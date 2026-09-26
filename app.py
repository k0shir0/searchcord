import os
import json
import asyncio
import uuid
import time
from contextlib import asynccontextmanager
from pathlib import Path
from typing import Annotated, Optional
from datetime import datetime, timezone, timedelta
from urllib.parse import parse_qs, urlsplit

import aiosqlite
import httpx
from fastapi import FastAPI, HTTPException, BackgroundTasks, Request, Query
from fastapi.staticfiles import StaticFiles
from fastapi.responses import StreamingResponse, Response, RedirectResponse
from fastapi.middleware.cors import CORSMiddleware
from fastapi.middleware.gzip import GZipMiddleware
from pydantic import BaseModel, Field
from storage import EPOCH_MS, image_urls, init_database

# Resolve paths against this file, not the process working directory, so the
# app behaves the same however it was launched.
BASE_DIR = Path(__file__).resolve().parent
DATA_DIR = Path(os.environ.get("SEARCHCORD_DATA_DIR", "data")).expanduser()
if not DATA_DIR.is_absolute():
    DATA_DIR = BASE_DIR / DATA_DIR
DB_PATH = str(DATA_DIR / "searchcord.db")
STATIC_DIR = BASE_DIR / "static"

DISCORD_API = "https://discord.com/api/v10"

# Give up rather than recursing forever if Discord keeps rate-limiting us.
MAX_RATE_LIMIT_RETRIES = 5
# Cap on buffered scrape-progress events, so a job whose SSE client never
# connects (or goes away mid-scrape) can't grow its queue without bound.
PROGRESS_QUEUE_MAX = 1000
# How long a finished job stays readable before it is discarded.
JOB_RETENTION_SECONDS = 60

# In-memory job tracker: job_id -> {queue, cancelled}
active_jobs: dict = {}

# DM clear job tracker: job_id -> {queue, cancelled}
delete_jobs: dict = {}

# Live monitor tracker: channel_id -> {channel_name, guild_name, guild_id, task}
live_monitors: dict = {}
# SSE subscriber queues for live feed
live_subscribers: list = []

# Shared across all Discord calls: one connection pool instead of a fresh TLS
# handshake per request. Set up in lifespan.
http_client: Optional[httpx.AsyncClient] = None
discord_request_lock = asyncio.Lock()
last_discord_request = 0.0


# ─── DB ──────────────────────────────────────────────────────

async def init_db():
    result = await asyncio.to_thread(init_database, DB_PATH)
    if result["backup"]:
        print(f"Verified storage migration; recovery backup retained: {result['backup']}")


@asynccontextmanager
async def lifespan(app: FastAPI):
    global http_client
    await init_db()
    http_client = httpx.AsyncClient(
        timeout=30,
        limits=httpx.Limits(max_connections=20, max_keepalive_connections=10),
    )
    try:
        yield
    finally:
        # Stop live pollers before tearing down the client they depend on,
        # otherwise they raise into the shutdown path.
        for info in list(live_monitors.values()):
            info["task"].cancel()
        live_monitors.clear()
        await http_client.aclose()
        http_client = None

app = FastAPI(lifespan=lifespan)

# The UI is served from this same origin, so cross-origin access is never
# needed. Allowing "*" would let any site you happen to visit read your
# scraped messages and DM list straight off localhost.
app.add_middleware(
    CORSMiddleware,
    allow_origin_regex=r"^https?://(localhost|127\.0\.0\.1)(:\d+)?$",
    allow_methods=["*"],
    allow_headers=["*"],
)
app.add_middleware(GZipMiddleware, minimum_size=1024, compresslevel=5)


# ─── Discord helpers ─────────────────────────────────────────

async def get_token() -> str:
    async with aiosqlite.connect(DB_PATH) as db:
        async with db.execute("SELECT value FROM settings WHERE key='token'") as cur:
            row = await cur.fetchone()
    if not row:
        raise HTTPException(401, "No token configured")
    return row[0]


def _retry_after(response) -> float:
    """Seconds to wait after a 429, tolerant of a non-JSON error body."""
    try:
        value = response.json().get("retry_after")
    except (ValueError, TypeError, AttributeError, json.JSONDecodeError):
        value = None
    try:
        return max(0.0, float(value if value is not None else
                              response.headers.get("Retry-After", 1.0)))
    except (ValueError, TypeError, AttributeError):
        return 1.0


async def discord(method: str, path: str, token: str, **kwargs):
    global last_discord_request
    headers = {
        "Authorization": token,
        "User-Agent": "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
        "Content-Type": "application/json",
    }
    url = f"{DISCORD_API}{path}"
    # One process-wide gate makes concurrent scrape, profile, live-monitor and
    # DM jobs respect a shared global 429 instead of retrying into it together.
    async with discord_request_lock:
        for attempt in range(MAX_RATE_LIMIT_RETRIES + 1):
            # Stay below the documented global request ceiling even when many
            # jobs are active. Route-specific headers and 429s take precedence.
            wait = last_discord_request + 0.025 - time.monotonic()
            if wait > 0:
                await asyncio.sleep(wait)
            last_discord_request = time.monotonic()
            r = await http_client.request(method, url, headers=headers, **kwargs)
            if r.status_code == 429:
                if attempt == MAX_RATE_LIMIT_RETRIES:
                    return r
                await asyncio.sleep(_retry_after(r))
                continue
            if r.headers.get("X-RateLimit-Remaining") == "0":
                try:
                    await asyncio.sleep(max(0.0, float(r.headers.get(
                        "X-RateLimit-Reset-After", 0))))
                except ValueError:
                    pass
            return r


# ─── Auth ────────────────────────────────────────────────────

@app.get("/api/token/validate")
async def validate_token():
    try:
        token = await get_token()
        r = await discord("GET", "/users/@me", token)
        if r.status_code == 200:
            u = r.json()
            return {"valid": True, "username": u.get("global_name") or u.get("username"), "id": u.get("id")}
        return {"valid": False}
    except HTTPException:
        return {"valid": False}


# ─── Guilds & Channels ───────────────────────────────────────

@app.get("/api/guilds")
async def get_guilds():
    token = await get_token()
    r = await discord("GET", "/users/@me/guilds", token)
    if r.status_code != 200:
        raise HTTPException(r.status_code, "Failed to fetch guilds")
    return [
        {
            "id": g["id"],
            "name": g["name"],
            "icon": f"https://cdn.discordapp.com/icons/{g['id']}/{g['icon']}.png?size=64"
                    if g.get("icon") else None,
        }
        for g in r.json()
    ]


@app.get("/api/guilds/{guild_id}/channels")
async def get_channels(guild_id: str):
    token = await get_token()
    r = await discord("GET", f"/guilds/{guild_id}/channels", token)
    if r.status_code != 200:
        raise HTTPException(r.status_code, "Failed to fetch channels")

    all_ch = r.json()
    categories = {c["id"]: c["name"] for c in all_ch if c["type"] == 4}
    text_types = {0, 5, 10, 11, 12}
    result = []
    for c in sorted(all_ch, key=lambda x: (x.get("parent_id") or "", x.get("position", 0))):
        if c["type"] in text_types:
            result.append({
                "id": c["id"],
                "name": c["name"],
                "type": c["type"],
                "category": categories.get(c.get("parent_id"), ""),
                "category_id": c.get("parent_id"),
                "nsfw": c.get("nsfw", False),
            })
    return result


@app.get("/api/dms")
async def get_dms():
    token = await get_token()
    r = await discord("GET", "/users/@me/channels", token)
    if r.status_code != 200:
        raise HTTPException(r.status_code, "Failed to fetch DMs")

    result = []
    for c in r.json():
        recipients = c.get("recipients", [])
        if c["type"] == 1:  # DM
            name = (recipients[0].get("global_name") or recipients[0].get("username")) \
                   if recipients else "Unknown User"
            result.append({"id": c["id"], "name": name, "type": 1})
        elif c["type"] == 3:  # Group DM
            names = [rec.get("global_name") or rec.get("username") for rec in recipients]
            name = c.get("name") or ", ".join(names) or "Group DM"
            result.append({"id": c["id"], "name": name, "type": 3})
    return result


# ─── Scraping ────────────────────────────────────────────────

class ScrapeRequest(BaseModel):
    channels: list
    limit: int = Field(default=0, ge=0)  # 0 = all; >0 = at most N per channel
    harvest_profiles: bool = False


@app.post("/api/scrape/start")
async def start_scrape(req: ScrapeRequest, bg: BackgroundTasks):
    channel_ids = {channel.get("id") for channel in req.channels
                   if isinstance(channel, dict) and channel.get("id")}
    if any(channel_ids & job.get("channel_ids", set())
           for job in active_jobs.values() if job.get("running")):
        raise HTTPException(409, "A selected channel is still being scraped")
    job_id = str(uuid.uuid4())
    queue: asyncio.Queue = asyncio.Queue(maxsize=PROGRESS_QUEUE_MAX)
    active_jobs[job_id] = {"queue": queue, "cancelled": False,
                           "running": True, "channel_ids": channel_ids}
    bg.add_task(run_scrape, job_id, req.channels, req.limit, req.harvest_profiles)
    return {"job_id": job_id}


@app.post("/api/scrape/{job_id}/stop")
async def stop_scrape(job_id: str):
    if job_id not in active_jobs:
        raise HTTPException(404, "Job not found")
    active_jobs[job_id]["cancelled"] = True
    return {"ok": True}


@app.get("/api/scrape/progress/{job_id}")
async def scrape_progress(job_id: str, request: Request):
    job = active_jobs.get(job_id)
    if job is None:
        raise HTTPException(404, "Job not found")

    async def stream():
        queue: asyncio.Queue = job["queue"]
        while True:
            if await request.is_disconnected():
                break
            try:
                evt = await asyncio.wait_for(queue.get(), timeout=25.0)
                yield f"data: {json.dumps(evt)}\n\n"
                if evt.get("type") in ("complete", "cancelled", "error"):
                    break
            except asyncio.TimeoutError:
                # Keep-alive, and a chance to notice the job was reaped.
                if job_id not in active_jobs and queue.empty():
                    break
                yield "data: {\"type\":\"ping\"}\n\n"

    return StreamingResponse(stream(), media_type="text/event-stream",
                             headers={"Cache-Control": "no-cache", "X-Accel-Buffering": "no"})


def _emit(q: asyncio.Queue, event: dict):
    """Queue a progress event without ever blocking the scrape.

    Progress events are disposable: if the consumer has stalled or gone away,
    drop the oldest rather than letting a full queue back-pressure the loop
    that is actually doing the work.
    """
    while True:
        try:
            q.put_nowait(event)
            return
        except asyncio.QueueFull:
            try:
                q.get_nowait()
            except asyncio.QueueEmpty:
                return


profile_fetch_lock = asyncio.Lock()


async def _write_db():
    db = await aiosqlite.connect(DB_PATH, timeout=30)
    await db.execute("PRAGMA synchronous=NORMAL")
    await db.execute("PRAGMA busy_timeout=30000")
    return db


async def _save_messages(db, msgs: list, cid: str, cname: str, gid: str,
                         gname: str, history_page: bool = False,
                         pending: tuple = None, clear_pending: bool = False) -> int:
    if not msgs:
        return 0
    author_names = [m["author"].get("global_name") or
                    m["author"].get("username") or "" for m in msgs]
    latest_authors = {}
    for m, author_name in zip(msgs, author_names):
        author_id = m["author"]["id"]
        if author_id not in latest_authors or int(m["id"]) > int(latest_authors[author_id][1]):
            latest_authors[author_id] = (author_name, m["id"])
    real_authors = {(cid, m["author"]["id"]) for m in msgs
                    if not m.get("webhook_id")}
    newest = str(max(int(m["id"]) for m in msgs))
    oldest = str(min(int(m["id"]) for m in msgs)) if history_page else None
    await db.execute("BEGIN")
    try:
        rows = [
            (int(m["id"]), int(cid) if str(cid).isdigit() else cid,
             int(gid) if gid is not None and str(gid).isdigit() else gid,
             int(m["author"]["id"]) if str(m["author"]["id"]).isdigit()
             else m["author"]["id"], m.get("content") or "",
             "\n".join(image_urls(m.get("attachments"))) or None)
            for m in msgs
        ]
        if gid is not None:
            await db.execute("""INSERT INTO guilds(id, name) VALUES (?, ?)
                ON CONFLICT(id) DO UPDATE SET name=excluded.name
                WHERE name IS NOT excluded.name""", (gid, gname or ""))
        await db.execute("""INSERT INTO channels(id, guild_id, name) VALUES (?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET guild_id=excluded.guild_id,
                                           name=excluded.name
            WHERE guild_id IS NOT excluded.guild_id OR name IS NOT excluded.name""",
            (cid, gid, cname or ""))
        await db.executemany("""INSERT INTO authors(id, name, last_message_id)
            VALUES (?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET name=excluded.name,
                last_message_id=excluded.last_message_id
            WHERE CAST(excluded.last_message_id AS INTEGER) >
                  CAST(authors.last_message_id AS INTEGER)""",
            [(author_id, name, last_id) for author_id, (name, last_id)
             in latest_authors.items()])
        insert_cursor = await db.executemany(
            """INSERT OR IGNORE INTO messages
               (id, channel_id, guild_id, author_id, content, image_urls)
               VALUES (?,?,?,?,?,?)""",
            rows,
        )
        # total_changes includes FTS and stats trigger writes, so it cannot
        # measure saved messages or enforce the user's per-channel limit.
        inserted = insert_cursor.rowcount
        await db.executemany("""INSERT OR IGNORE INTO channel_authors
            (channel_id, author_id) VALUES (?, ?)""", list(real_authors))
        await db.execute("""INSERT INTO scrape_cursors
            (channel_id, guild_id, newest_message_id, oldest_message_id)
            VALUES (?, ?, ?, ?)
            ON CONFLICT(channel_id) DO UPDATE SET
                guild_id=excluded.guild_id,
                newest_message_id=CASE
                    WHEN CAST(excluded.newest_message_id AS INTEGER) >
                         CAST(scrape_cursors.newest_message_id AS INTEGER)
                    THEN excluded.newest_message_id ELSE scrape_cursors.newest_message_id END,
                oldest_message_id=CASE
                    WHEN excluded.oldest_message_id IS NULL THEN scrape_cursors.oldest_message_id
                    WHEN scrape_cursors.oldest_message_id IS NULL OR
                         CAST(excluded.oldest_message_id AS INTEGER) <
                         CAST(scrape_cursors.oldest_message_id AS INTEGER)
                    THEN excluded.oldest_message_id ELSE scrape_cursors.oldest_message_id END""",
            (cid, gid, newest, oldest))
        if pending:
            await db.execute("""UPDATE scrape_cursors SET
                pending_after_message_id=?, pending_before_message_id=?
                WHERE channel_id=?""", (pending[0], pending[1], cid))
        elif clear_pending:
            await db.execute("""UPDATE scrape_cursors SET
                pending_after_message_id=NULL, pending_before_message_id=NULL
                WHERE channel_id=?""", (cid,))
        await db.commit()
        return inserted
    except Exception:
        await db.rollback()
        raise


async def _mark_history_complete(db, cid: str):
    await db.execute("UPDATE scrape_cursors SET history_complete=1 WHERE channel_id=?", (cid,))
    await db.commit()


async def _clear_pending(db, cid: str):
    await db.execute("""UPDATE scrape_cursors SET
        pending_after_message_id=NULL, pending_before_message_id=NULL
        WHERE channel_id=?""", (cid,))
    await db.commit()


async def _harvest_profiles(db, cid: str, token: str, job_id: str, q: asyncio.Queue) -> int:
    async with db.execute("""SELECT ca.author_id FROM channel_authors ca
        LEFT JOIN profiles p ON p.user_id=ca.author_id
        WHERE ca.channel_id=? AND p.user_id IS NULL""", (cid,)) as cur:
        missing = [row[0] for row in await cur.fetchall()]
    saved = 0
    for user_id in missing:
        if active_jobs.get(job_id, {}).get("cancelled"):
            break
        # Two scrape jobs can see the same missing author. Check again under the
        # lock so each stable user ID is fetched at most once per process.
        async with profile_fetch_lock:
            async with db.execute("SELECT 1 FROM profiles WHERE user_id=?", (user_id,)) as cur:
                if await cur.fetchone():
                    continue
            r = await discord("GET", f"/users/{user_id}", token)
            if r.status_code != 200:
                _emit(q, {"type": "profile_warning", "message": f"Profile HTTP {r.status_code}"})
                if r.status_code in (401, 403, 429):
                    break
                continue
            profile = r.json()
            if profile.get("id") != user_id:
                _emit(q, {"type": "profile_warning", "message": "Profile ID mismatch"})
                continue
            await db.execute("""INSERT OR IGNORE INTO profiles
                (user_id, username, global_name, avatar_hash, banner_hash,
                 accent_color, bot, public_flags, fetched_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)""",
                (user_id, profile.get("username") or "",
                 profile.get("global_name"), profile.get("avatar"),
                 profile.get("banner"), profile.get("accent_color"),
                 int(bool(profile.get("bot"))), profile.get("public_flags"),
                 datetime.now(timezone.utc).isoformat()))
            await db.commit()
            saved += 1
            _emit(q, {"type": "profile_progress", "channel_id": cid, "saved": saved})
    return saved


async def _fetch_messages(cid: str, token: str, params: dict) -> list:
    r = await discord("GET", f"/channels/{cid}/messages", token, params=params)
    if r.status_code == 403:
        raise PermissionError("No access (403)")
    if r.status_code != 200:
        raise RuntimeError(f"HTTP {r.status_code}")
    return r.json()


async def run_scrape(job_id: str, channels: list, limit: int = 0,
                     harvest_profiles: bool = False):
    job = active_jobs[job_id]
    q: asyncio.Queue = job["queue"]
    total_messages = 0

    # Whatever happens below, the job must not be left behind in active_jobs.
    try:
        try:
            token = await get_token()
        except Exception:
            _emit(q, {"type": "error", "message": "No token configured"})
            return

        db = await _write_db()
        try:
            for idx, channel in enumerate(channels):
                if active_jobs.get(job_id, {}).get("cancelled"):
                    break

                cid = channel["id"]
                cname = channel["name"]
                gid = channel.get("guild_id")
                gname = channel.get("guild_name")
                _emit(q, {"type": "channel_start", "channel": cname, "guild": gname,
                          "index": idx + 1, "total": len(channels)})
                ch_count = 0

                try:
                    async with db.execute("""SELECT newest_message_id, oldest_message_id,
                        history_complete, pending_after_message_id,
                        pending_before_message_id
                        FROM scrape_cursors WHERE channel_id=?""", (cid,)) as cur:
                        cursor = await cur.fetchone()

                    # An indexed channel needs only messages newer than its
                    # saved high-water mark. A full page leaves a pending gap;
                    # if a capped or stopped job exits, the next job walks the
                    # gap down to its original lower bound before moving on.
                    if cursor:
                        anchor = int(cursor[3] or cursor[0])
                        before = cursor[4] if cursor[3] else None
                        resuming_pending = bool(cursor[3])
                        while not active_jobs.get(job_id, {}).get("cancelled"):
                            fetch = min(100, limit - ch_count) if limit else 100
                            if fetch <= 0:
                                break
                            params = {"limit": fetch}
                            params["before" if before else "after"] = before or cursor[0]
                            msgs = await _fetch_messages(cid, token, params)
                            fresh = [m for m in msgs if int(m["id"]) > anchor]
                            more = bool(fresh) and len(msgs) == fetch and len(fresh) == len(msgs)
                            if fresh:
                                next_before = str(min(int(m["id"]) for m in fresh))
                                inserted = await _save_messages(
                                    db, fresh, cid, cname, gid, gname,
                                    pending=(str(anchor), next_before) if more else None,
                                    clear_pending=not more)
                                ch_count += inserted
                                total_messages += inserted
                                _emit(q, {"type": "progress", "channel": cname,
                                          "messages": ch_count, "total_messages": total_messages})
                            elif before:
                                await _clear_pending(db, cid)
                            if not more:
                                if resuming_pending and (not limit or ch_count < limit):
                                    resuming_pending = False
                                    anchor = int(cursor[0])
                                    before = None
                                    continue
                                break
                            before = next_before

                    # A first scrape can be limited or interrupted. Retain its
                    # oldest cursor so a later job continues the old history
                    # after collecting any new arrivals above.
                    if not cursor or not cursor[2]:
                        before = cursor[1] if cursor else None
                        while not active_jobs.get(job_id, {}).get("cancelled"):
                            fetch = min(100, limit - ch_count) if limit else 100
                            if fetch <= 0:
                                break
                            params = {"limit": fetch}
                            if before:
                                params["before"] = before
                            msgs = await _fetch_messages(cid, token, params)
                            if not msgs:
                                if before:
                                    await _mark_history_complete(db, cid)
                                break
                            inserted = await _save_messages(db, msgs, cid, cname, gid, gname,
                                                            history_page=True)
                            ch_count += inserted
                            total_messages += inserted
                            before = str(min(int(m["id"]) for m in msgs))
                            _emit(q, {"type": "progress", "channel": cname,
                                      "messages": ch_count, "total_messages": total_messages})
                            if len(msgs) < fetch:
                                await _mark_history_complete(db, cid)
                                break

                    if harvest_profiles and not active_jobs.get(job_id, {}).get("cancelled"):
                        profiles = await _harvest_profiles(db, cid, token, job_id, q)
                        _emit(q, {"type": "profiles", "channel": cname, "saved": profiles})
                except asyncio.CancelledError:
                    raise
                except Exception as e:
                    _emit(q, {"type": "channel_error", "channel": cname, "message": str(e)})

                _emit(q, {"type": "channel_complete", "channel": cname,
                          "messages": ch_count})
        finally:
            await db.close()

        was_cancelled = active_jobs.get(job_id, {}).get("cancelled", False)
        _emit(q, {"type": "cancelled" if was_cancelled else "complete",
                  "total_messages": total_messages, "channels": len(channels)})
        job["running"] = False

        # Leave the finished job readable briefly so a reconnecting client can
        # still pick up the terminal event.
        await asyncio.sleep(JOB_RETENTION_SECONDS)
    finally:
        active_jobs.pop(job_id, None)


# ─── DM Clearer ──────────────────────────────────────────────


class DmClearStartRequest(BaseModel):
    channel_id: str


@app.post("/api/dm-clear/start")
async def start_dm_clear(req: DmClearStartRequest, bg: BackgroundTasks):
    job_id = str(uuid.uuid4())
    queue: asyncio.Queue = asyncio.Queue(maxsize=PROGRESS_QUEUE_MAX)
    delete_jobs[job_id] = {"queue": queue, "cancelled": False}
    bg.add_task(run_dm_clear, job_id, req.channel_id)
    return {"job_id": job_id}


@app.post("/api/dm-clear/{job_id}/stop")
async def stop_dm_clear(job_id: str):
    if job_id not in delete_jobs:
        raise HTTPException(404, "Job not found")
    delete_jobs[job_id]["cancelled"] = True
    return {"ok": True}


@app.get("/api/dm-clear/progress/{job_id}")
async def dm_clear_progress(job_id: str, request: Request):
    job = delete_jobs.get(job_id)
    if job is None:
        raise HTTPException(404, "Job not found")

    async def stream():
        queue: asyncio.Queue = job["queue"]
        while True:
            if await request.is_disconnected():
                break
            try:
                evt = await asyncio.wait_for(queue.get(), timeout=25.0)
                yield f"data: {json.dumps(evt)}\n\n"
                if evt.get("type") in ("complete", "cancelled", "error"):
                    break
            except asyncio.TimeoutError:
                # Keep-alive, and a chance to notice the job was reaped.
                if job_id not in delete_jobs and queue.empty():
                    break
                yield "data: {\"type\":\"ping\"}\n\n"

    return StreamingResponse(stream(), media_type="text/event-stream",
                             headers={"Cache-Control": "no-cache", "X-Accel-Buffering": "no"})


async def run_dm_clear(job_id: str, channel_id: str):
    job = delete_jobs[job_id]
    q: asyncio.Queue = job["queue"]
    deleted_count = 0
    scanned_count = 0
    error_occurred = False
    error_message = None

    try:
        try:
            token = await get_token()
        except Exception:
            _emit(q, {"type": "error", "message": "No token configured"})
            error_occurred = True
            error_message = "No token configured"
            # Don't return here - let it fall through to cleanup with retention

        if not error_occurred:
            # Fetch the current user's own id
            try:
                r = await discord("GET", "/users/@me", token)
                if r.status_code != 200:
                    _emit(q, {"type": "error", "message": f"Failed to fetch user: HTTP {r.status_code}"})
                    error_occurred = True
                    error_message = f"Failed to fetch user: HTTP {r.status_code}"
                else:
                    self_id = r.json()["id"]
            except Exception as e:
                _emit(q, {"type": "error", "message": f"Failed to fetch user: {e}"})
                error_occurred = True
                error_message = f"Failed to fetch user: {e}"

        if not error_occurred:
            before = None

            while True:
                if delete_jobs.get(job_id, {}).get("cancelled"):
                    break

                params = {"limit": 100}
                if before:
                    params["before"] = before

                try:
                    r = await discord("GET", f"/channels/{channel_id}/messages", token, params=params)

                    if r.status_code == 403:
                        _emit(q, {"type": "error", "message": "No access (403)"})
                        break
                    if r.status_code != 200:
                        _emit(q, {"type": "error", "message": f"HTTP {r.status_code}"})
                        break

                    msgs = r.json()
                    if not msgs:
                        break

                    page_size = len(msgs)
                    scanned_count += page_size

                    for msg in msgs:
                        if delete_jobs.get(job_id, {}).get("cancelled"):
                            break

                        if msg["author"]["id"] == self_id:
                            msg_id = msg["id"]
                            if delete_jobs.get(job_id, {}).get("cancelled"):
                                break
                            try:
                                del_r = await discord("DELETE", f"/channels/{channel_id}/messages/{msg_id}", token)
                                if del_r.status_code == 404:
                                    # Already gone — treat as success
                                    pass
                                elif del_r.status_code != 204:
                                    _emit(q, {"type": "warning", "message": f"Failed to delete {msg_id}: HTTP {del_r.status_code}"})
                                deleted_count += 1
                                _emit(q, {"type": "progress", "deleted": deleted_count, "scanned": scanned_count})
                            except Exception as e:
                                _emit(q, {"type": "warning", "message": f"Failed to delete {msg_id}: {e}"})

                            # Small delay between deletes to avoid rate limiting
                            await asyncio.sleep(0.45)

                    # Advance cursor using oldest message in page
                    before = msgs[-1]["id"]

                    # If page was smaller than requested, we've hit the start of history
                    if page_size < 100:
                        break

                except asyncio.CancelledError:
                    raise
                except Exception as e:
                    _emit(q, {"type": "error", "message": str(e)})
                    error_occurred = True
                    error_message = str(e)
                    break

        was_cancelled = delete_jobs.get(job_id, {}).get("cancelled", False)
        _emit(q, {"type": "cancelled" if was_cancelled else "complete", "deleted": deleted_count})

        # Leave the finished job readable briefly so a reconnecting client can
        # still pick up the terminal event.
        await asyncio.sleep(JOB_RETENTION_SECONDS)
    finally:
        delete_jobs.pop(job_id, None)


# ─── Live monitoring ──────────────────────────────────────────

async def _broadcast(event: dict):
    for sub_q in live_subscribers:
        try:
            sub_q.put_nowait(event)
        except asyncio.QueueFull:
            pass


async def poll_channel(channel_id: str, channel_name: str, guild_id: str, guild_name: str):
    # Any exit path — clean stop, network failure, bad response — has to clear
    # the registry entry, or the channel can never be monitored again.
    db = None
    try:
        try:
            token = await get_token()
        except Exception:
            return
        db = await _write_db()

        # Anchor to the current newest message so we only stream NEW ones
        last_id = None
        try:
            r = await discord("GET", f"/channels/{channel_id}/messages",
                              token, params={"limit": 1})
            if r.status_code == 200:
                anchor = r.json()
                if anchor:
                    last_id = anchor[0]["id"]
        except asyncio.CancelledError:
            raise
        except Exception:
            pass  # start from scratch rather than refusing to monitor

        await _broadcast({"type": "monitor_start", "channel_id": channel_id,
                          "channel": channel_name, "guild": guild_name, "guild_id": guild_id})

        while channel_id in live_monitors:
            await asyncio.sleep(3)
            if channel_id not in live_monitors:
                break

            params = {"limit": 50}
            if last_id:
                params["after"] = last_id

            try:
                r = await discord("GET", f"/channels/{channel_id}/messages", token, params=params)
                if r.status_code != 200:
                    continue
                msgs = r.json()
                if not msgs:
                    continue

                msgs.sort(key=lambda m: int(m["id"]))  # oldest first (snowflakes are numeric)
                await _save_messages(db, msgs, channel_id, channel_name, guild_id, guild_name)

                for m in msgs:
                    await _broadcast({
                        "type": "message",
                        "id": m["id"],
                        "channel_id": channel_id,
                        "channel": channel_name,
                        "guild_id": guild_id,
                        "guild": guild_name,
                        "author_id": m["author"]["id"],
                        "author": m["author"].get("global_name") or m["author"]["username"],
                        "content": m.get("content", ""),
                        "timestamp": m["timestamp"],
                        "attachments": [f"/api/images/{m['id']}/{i}" for i, _ in
                                        enumerate(image_urls(m.get("attachments")))],
                    })
                last_id = msgs[-1]["id"]
            except asyncio.CancelledError:
                raise
            except Exception:
                pass
    except asyncio.CancelledError:
        pass
    finally:
        if db is not None:
            await db.close()
        # Drop the registry entry here too: if this task died on its own the
        # entry would otherwise linger and block any restart of the channel.
        live_monitors.pop(channel_id, None)
        await _broadcast({"type": "monitor_stop", "channel_id": channel_id,
                          "channel": channel_name})


class LiveStartRequest(BaseModel):
    channels: list


@app.post("/api/live/start")
async def live_start(req: LiveStartRequest):
    for ch in req.channels:
        cid = ch["id"]
        if cid not in live_monitors:
            task = asyncio.create_task(
                poll_channel(cid, ch["name"], ch["guild_id"], ch["guild_name"])
            )
            live_monitors[cid] = {
                "channel_name": ch["name"],
                "guild_name":   ch["guild_name"],
                "guild_id":     ch["guild_id"],
                "task":         task,
            }
    return {"active": list(live_monitors.keys())}


class LiveStopRequest(BaseModel):
    channel_ids: list = []  # empty list = stop all


@app.post("/api/live/stop")
async def live_stop(req: LiveStopRequest):
    to_stop = req.channel_ids if req.channel_ids else list(live_monitors.keys())
    for cid in to_stop:
        if cid in live_monitors:
            live_monitors[cid]["task"].cancel()
            del live_monitors[cid]
    return {"stopped": to_stop}


@app.get("/api/live/status")
async def live_status():
    return {
        "channels": [
            {"channel_id": cid, "channel_name": info["channel_name"],
             "guild_name": info["guild_name"], "guild_id": info["guild_id"]}
            for cid, info in live_monitors.items()
        ]
    }


@app.get("/api/live/events")
async def live_events(request: Request):
    q: asyncio.Queue = asyncio.Queue(maxsize=500)
    live_subscribers.append(q)

    # Immediately tell the new subscriber about currently-active monitors.
    # put_nowait, not put: a full queue here must not block the handler, and
    # the stream's finally clause is not yet in play to unregister us.
    for cid, info in list(live_monitors.items()):
        try:
            q.put_nowait({"type": "monitor_start", "channel_id": cid,
                          "channel": info["channel_name"], "guild": info["guild_name"],
                          "guild_id": info["guild_id"]})
        except asyncio.QueueFull:
            break

    async def stream():
        try:
            while True:
                if await request.is_disconnected():
                    break
                try:
                    evt = await asyncio.wait_for(q.get(), timeout=20.0)
                    yield f"data: {json.dumps(evt)}\n\n"
                except asyncio.TimeoutError:
                    yield "data: {\"type\":\"ping\"}\n\n"
        finally:
            if q in live_subscribers:
                live_subscribers.remove(q)

    return StreamingResponse(stream(), media_type="text/event-stream",
                             headers={"Cache-Control": "no-cache", "X-Accel-Buffering": "no"})


# ─── ChatML Export ───────────────────────────────────────────

DEFAULT_SYSTEM_TEMPLATE = "You are {target}. You are texting {other}. Respond in your natural texting style."


@app.get("/api/export/participants")
async def export_participants(channel_id: str):
    async with aiosqlite.connect(DB_PATH) as db:
        db.row_factory = aiosqlite.Row
        async with db.execute(
            "SELECT DISTINCT author_id, author_name FROM message_records WHERE channel_id = ? ORDER BY author_name",
            (channel_id,)
        ) as cur:
            rows = [dict(r) for r in await cur.fetchall()]
    for r in rows:
        if not r["author_name"]:
            r["author_name"] = r["author_id"]
    return rows


class ChatMLExportRequest(BaseModel):
    channel_id: str
    target_id: str
    other_name: Optional[str] = None
    system_prompt: Optional[str] = None


@app.post("/api/export/chatml")
async def export_chatml(req: ChatMLExportRequest):
    async with aiosqlite.connect(DB_PATH) as db:
        db.row_factory = aiosqlite.Row
        async with db.execute(
            "SELECT author_id, author_name, content, timestamp FROM message_records "
            "WHERE channel_id = ? ORDER BY id ASC",
            (req.channel_id,)
        ) as cur:
            rows = [dict(r) for r in await cur.fetchall()]

    if not rows:
        raise HTTPException(404, "No messages found for this channel")

    target_name = next((r["author_name"] for r in rows if r["author_id"] == req.target_id), None)
    if not target_name:
        raise HTTPException(404, "Target user not found in this channel's messages")

    other_name = req.other_name or next(
        (r["author_name"] for r in rows if r["author_id"] != req.target_id), "Them"
    )

    system_prompt = (req.system_prompt or DEFAULT_SYSTEM_TEMPLATE).format(
        target=target_name, other=other_name
    )

    # Merge consecutive same-side messages into single turns (handles
    # double-texting / message bursts from either side).
    turns = []
    for r in rows:
        text = (r["content"] or "").strip()
        if not text:
            continue
        side = "target" if r["author_id"] == req.target_id else "other"
        if turns and turns[-1]["side"] == side:
            turns[-1]["text"] += "\n" + text
        else:
            turns.append({"side": side, "text": text})

    # One JSONL line per target reply, using the immediately preceding
    # "other" turn as context. Leading target turns with no prior context
    # are skipped (no user message to respond to).
    lines = []
    for i in range(1, len(turns)):
        if turns[i]["side"] == "target" and turns[i - 1]["side"] == "other":
            example = {
                "messages": [
                    {"role": "system", "content": system_prompt},
                    {"role": "user", "content": f"{other_name}: {turns[i - 1]['text']}"},
                    {"role": "assistant", "content": turns[i]["text"]},
                ]
            }
            lines.append(json.dumps(example, ensure_ascii=False))

    jsonl = "\n".join(lines) + ("\n" if lines else "")

    return Response(
        content=jsonl,
        media_type="application/x-ndjson",
        headers={"Content-Disposition": f'attachment; filename="chatml_{req.channel_id}.jsonl"'},
    )


# ─── Search ──────────────────────────────────────────────────

def _snowflake_bound(value: str, upper: bool = False) -> int:
    """Convert a UTC date/time filter to a Discord snowflake boundary."""
    try:
        moment = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError as exc:
        raise HTTPException(400, "Invalid date filter") from exc
    if moment.tzinfo is None:
        moment = moment.replace(tzinfo=timezone.utc)
    if upper:
        moment += timedelta(days=1) if len(value) == 10 else (
            timedelta(seconds=1) if "." not in value else timedelta(milliseconds=1))
    return (int(moment.timestamp() * 1000) - EPOCH_MS) << 22


@app.get("/api/search")
async def search(
    q: Optional[str] = None,
    guild_id: Optional[str] = None,
    channel_id: Optional[str] = None,
    author_id: Optional[str] = None,
    date_from: Optional[str] = None,
    date_to: Optional[str] = None,
    page: Annotated[int, Query(ge=1)] = 1,
    limit: Annotated[int, Query(ge=1, le=100)] = 50,
):
    conditions, params = [], []
    if q:
        if len(q) >= 3:
            conditions.append("rowid IN (SELECT rowid FROM messages_fts WHERE content LIKE ?)")
        else:
            conditions.append("content LIKE ?")
        params.append(f"%{q}%")
    if guild_id:
        conditions.append("guild_id = ?")
        params.append(guild_id)
    if channel_id:
        conditions.append("channel_id = ?")
        params.append(channel_id)
    if author_id:
        conditions.append("author_id = ?")
        params.append(author_id)
    if date_from:
        conditions.append("id >= ?")
        params.append(_snowflake_bound(date_from))
    if date_to:
        conditions.append("id < ?")
        params.append(_snowflake_bound(date_to, upper=True))
    if date_from and date_to and _snowflake_bound(date_from) >= _snowflake_bound(date_to, upper=True):
        raise HTTPException(400, "From date must be on or before the to date")

    where = ("WHERE " + " AND ".join(conditions)) if conditions else ""
    count_where = where
    offset = (page - 1) * limit
    cached_count = None
    if not q and not date_from and not date_to:
        filters = [("guild", guild_id), ("channel", channel_id),
                   ("author", author_id)]
        active = [(kind, value) for kind, value in filters if value]
        if not active:
            cached_count = ("total", "")
        elif len(active) == 1:
            cached_count = active[0]

    async with aiosqlite.connect(DB_PATH) as db:
        db.row_factory = aiosqlite.Row
        if cached_count:
            count_sql = "SELECT count FROM stats_counts WHERE kind=? AND key=? AND count>0"
            count_params = cached_count
        else:
            count_sql = f"SELECT COUNT(*) FROM messages {count_where}"
            count_params = params
        async with db.execute(count_sql, count_params) as cur:
            count_row = await cur.fetchone()
            total = count_row[0] if count_row else 0
        async with db.execute(
            f"""SELECT * FROM message_records WHERE id IN (
                SELECT id FROM messages {where}
                ORDER BY id DESC LIMIT ? OFFSET ?)
                ORDER BY id DESC""",
            params + [limit, offset]
        ) as cur:
            rows = [dict(r) for r in await cur.fetchall()]

        author_ids = list({r["author_id"] for r in rows})
        avatars = {}
        if author_ids:
            placeholders = ",".join("?" for _ in author_ids)
            async with db.execute(
                f"SELECT user_id, avatar_hash FROM profiles WHERE user_id IN ({placeholders})",
                author_ids,
            ) as cur:
                avatars = {r[0]: r[1] for r in await cur.fetchall()}

    for r in rows:
        r["id"] = str(r["id"])
        avatar = avatars.get(r["author_id"])
        r["avatar_url"] = (
            f"https://cdn.discordapp.com/avatars/{r['author_id']}/{avatar}.png?size=64"
            if avatar and r["author_id"].isdigit() and avatar.removeprefix("a_").isalnum()
            else None
        )
        urls = r.pop("image_urls")
        r["attachments"] = [f"/api/images/{r['id']}/{i}" for i, _ in
                            enumerate(urls.splitlines())] if urls else []

    return {"total": total, "page": page, "limit": limit,
            "pages": max(1, (total + limit - 1) // limit), "messages": rows}


@app.get("/api/images/{message_id}/{image_index}")
async def open_image(message_id: int, image_index: int):
    """Redirect to a current attachment URL, refreshing expired signatures."""
    async with aiosqlite.connect(DB_PATH) as db:
        async with db.execute("""SELECT channel_id, image_urls FROM messages
            WHERE id=?""", (message_id,)) as cur:
            row = await cur.fetchone()
    if not row or not row[1]:
        raise HTTPException(404, "Image not found")
    urls = row[1].splitlines()
    if image_index < 0 or image_index >= len(urls):
        raise HTTPException(404, "Image not found")
    url = urls[image_index]
    expiry = parse_qs(urlsplit(url).query).get("ex", [None])[0]
    if expiry is not None:
        try:
            expired = int(expiry, 16) <= time.time() + 60
        except ValueError:
            expired = True
        if expired:
            token = await get_token()
            response = await discord("GET", f"/channels/{row[0]}/messages/{message_id}", token)
            if response.status_code != 200:
                raise HTTPException(404, "Image is no longer available")
            original_path = urlsplit(url).path
            url = next((fresh for fresh in image_urls(response.json().get("attachments"))
                        if urlsplit(fresh).path == original_path), None)
            if not url:
                raise HTTPException(404, "Image is no longer available")
    return RedirectResponse(url, status_code=302)


@app.get("/api/stats")
async def get_stats(days: Annotated[int, Query(ge=0, le=3650)] = 90):
    async with aiosqlite.connect(DB_PATH) as db:
        db.row_factory = aiosqlite.Row

        async with db.execute("""SELECT count FROM stats_counts
            WHERE kind='total' AND key=''""") as cur:
            row = await cur.fetchone()
            total_messages = row[0] if row else 0
        async with db.execute("""SELECT kind, COUNT(*) AS count FROM stats_counts
            WHERE kind IN ('guild', 'channel', 'author') AND key<>'' AND count>0
            GROUP BY kind""") as cur:
            totals = {row["kind"]: row["count"] for row in await cur.fetchall()}
        total_servers = totals.get("guild", 0)
        total_channels = totals.get("channel", 0)
        total_users = totals.get("author", 0)

        async with db.execute("""
            SELECT ranked.author_id, authors.name AS author_name, ranked.count
            FROM (SELECT key AS author_id, count FROM stats_counts
                  WHERE kind='author' AND count>0 ORDER BY count DESC, key LIMIT 50) AS ranked
            LEFT JOIN authors ON authors.id=ranked.author_id
        """) as cur:
            top_users = [dict(r) for r in await cur.fetchall()]

        async with db.execute("""
            SELECT guilds.name AS guild_name, ranked.guild_id, ranked.count
            FROM (SELECT NULLIF(key, '') AS guild_id, count FROM stats_counts
                  WHERE kind='guild' AND count>0 ORDER BY count DESC, key LIMIT 20) AS ranked
            LEFT JOIN guilds ON guilds.id=ranked.guild_id
        """) as cur:
            by_server = [dict(r) for r in await cur.fetchall()]

        async with db.execute("""SELECT key AS date, count FROM stats_counts
            WHERE kind='day' AND count>0 ORDER BY key""") as cur:
            daily = [dict(r) for r in await cur.fetchall()]
        by_weekday = [0] * 7
        monthly = {}
        for row in daily:
            by_weekday[datetime.fromisoformat(row['date']).weekday()] += row['count']
            month = row['date'][:7]
            monthly[month] = monthly.get(month, 0) + row['count']
        range_end = daily[-1]['date'] if daily else None
        range_start = ((datetime.fromisoformat(range_end) - timedelta(days=days - 1)).date().isoformat()
                       if days and range_end else daily[0]['date'] if daily else None)
        by_day = [row for row in daily if row['date'] >= range_start] if range_start else []
        async with db.execute("""SELECT ranked.key AS channel_id,
            channels.name AS channel_name, guilds.name AS guild_name, ranked.count
            FROM (SELECT key, count FROM stats_counts WHERE kind='channel' AND count>0
                  ORDER BY count DESC, key LIMIT 20) ranked
            LEFT JOIN channels ON channels.id=ranked.key
            LEFT JOIN guilds ON guilds.id=channels.guild_id""") as cur:
            by_channel = [dict(r) for r in await cur.fetchall()]

        async with db.execute("""
            SELECT CAST(NULLIF(key, '') AS INTEGER) AS hour, count
            FROM stats_counts WHERE kind='hour' AND count>0 ORDER BY hour ASC
        """) as cur:
            by_hour = [dict(r) for r in await cur.fetchall()]

    db_size = 0
    for path in (DB_PATH, f"{DB_PATH}-wal"):
        try:
            db_size += os.path.getsize(path)
        except OSError:
            pass

    return {
        "total_messages":   total_messages,
        "total_servers":    total_servers,
        "total_channels":   total_channels,
        "total_users":      total_users,
        "db_size_bytes":    db_size,
        "top_users":        top_users,
        "messages_by_server": by_server,
        "messages_by_day":  by_day,
        "messages_by_hour": by_hour,
        "messages_by_channel": by_channel,
        "messages_by_weekday": [{"day": day, "count": count} for day, count in enumerate(by_weekday)],
        "messages_by_month": [{"month": month, "count": count} for month, count in monthly.items()],
        "range_start": range_start,
        "range_end": range_end,
    }


@app.get("/api/stats/contributors")
async def contributors(
    offset: Annotated[int, Query(ge=0)] = 0,
    limit: Annotated[int, Query(ge=1, le=100)] = 50,
    q: Annotated[str, Query(max_length=100)] = "",
):
    """Bounded, deterministic leaderboard pages over the cached author counts."""
    query = q.strip().replace('!', '!!').replace('%', '!%').replace('_', '!_')
    async with aiosqlite.connect(DB_PATH) as db:
        db.row_factory = aiosqlite.Row
        async with db.execute("""SELECT counts.key AS author_id, authors.name AS author_name,
            counts.count FROM stats_counts counts LEFT JOIN authors ON authors.id=counts.key
            WHERE counts.kind='author' AND counts.count>0
            AND (?='' OR authors.name LIKE ? ESCAPE '!' OR counts.key=?)
            ORDER BY counts.count DESC, counts.key LIMIT ? OFFSET ?""",
            (query, f'%{query}%', q.strip(), limit + 1, offset)) as cur:
            rows = [dict(row) for row in await cur.fetchall()]
    return {"users": rows[:limit], "has_more": len(rows) > limit,
            "offset": offset, "limit": limit}


@app.get("/api/search/filters")
async def search_filters(include_users: bool = True):
    async with aiosqlite.connect(DB_PATH) as db:
        db.row_factory = aiosqlite.Row
        async with db.execute(
            "SELECT id AS guild_id, name AS guild_name FROM guilds ORDER BY name"
        ) as cur:
            guilds = [dict(r) for r in await cur.fetchall()]
        async with db.execute(
            "SELECT id AS channel_id, name AS channel_name, guild_id FROM channels ORDER BY guild_id, name"
        ) as cur:
            channels = [dict(r) for r in await cur.fetchall()]
        users = []
        if include_users:
            async with db.execute(
                "SELECT id AS author_id, name AS author_name FROM authors ORDER BY name"
            ) as cur:
                users = [dict(r) for r in await cur.fetchall()]
        async with db.execute("""SELECT count FROM stats_counts
            WHERE kind='total' AND key=''""") as cur:
            row = await cur.fetchone()
            total = row[0] if row else 0

    return {"guilds": guilds, "channels": channels, "users": users, "total_messages": total}


@app.get("/api/search/authors")
async def search_authors(q: str = ""):
    """Return a small prefix match set for the author text filter."""
    q = q.strip()
    if len(q) < 2:
        return []
    pattern = q[:100].replace("!", "!!").replace("%", "!%").replace("_", "!_") + "%"
    async with aiosqlite.connect(DB_PATH) as db:
        db.row_factory = aiosqlite.Row
        async with db.execute("""SELECT id AS author_id, name AS author_name
            FROM authors WHERE name LIKE ? ESCAPE '!'
            ORDER BY name COLLATE NOCASE, id LIMIT 20""", (pattern,)) as cur:
            return [dict(row) for row in await cur.fetchall()]


# ─── Settings ────────────────────────────────────────────────

@app.get("/api/settings")
async def get_settings():
    async with aiosqlite.connect(DB_PATH) as db:
        db.row_factory = aiosqlite.Row
        async with db.execute("SELECT key, value FROM settings") as cur:
            rows = {r["key"]: r["value"] for r in await cur.fetchall()}
    result = dict(rows)
    if "token" in result:
        t = result.pop("token")
        result["token_set"] = True
        result["token_preview"] = (t[:8] + "…" + t[-4:]) if len(t) > 12 else "••••••••"
    return result


class SettingsIn(BaseModel):
    token: Optional[str] = None


@app.post("/api/settings")
async def update_settings(s: SettingsIn):
    async with aiosqlite.connect(DB_PATH) as db:
        if s.token is not None:
            await db.execute(
                "INSERT OR REPLACE INTO settings (key, value) VALUES ('token', ?)", (s.token,)
            )
        await db.commit()
    return {"ok": True}


@app.delete("/api/messages")
async def clear_messages():
    async with aiosqlite.connect(DB_PATH) as db:
        for table in ("messages", "message_history", "name_values", "stats_counts",
                      "scrape_cursors", "channel_authors", "profiles", "authors",
                      "channels", "guilds"):
            await db.execute(f"DELETE FROM {table}")
        await db.commit()
    return {"ok": True}


# ─── Static ──────────────────────────────────────────────────

app.mount("/", StaticFiles(directory=str(STATIC_DIR), html=True), name="static")

if __name__ == "__main__":
    import webbrowser
    import threading
    import uvicorn

    # Loopback by default. This app has no authentication and holds a Discord
    # token plus everything you have scraped, so binding 0.0.0.0 would hand
    # the whole archive to anyone on the same network. Override deliberately.
    host = os.environ.get("SEARCHCORD_HOST", "127.0.0.1")
    port = int(os.environ.get("SEARCHCORD_PORT", "8000"))

    def _open():
        import time
        time.sleep(1.2)
        webbrowser.open(f"http://localhost:{port}")

    threading.Thread(target=_open, daemon=True).start()
    uvicorn.run(app, host=host, port=port)
