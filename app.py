import os
import json
import asyncio
import uuid
from contextlib import asynccontextmanager
from pathlib import Path
from typing import Optional

import aiosqlite
import httpx
from fastapi import FastAPI, HTTPException, BackgroundTasks, Request
from fastapi.staticfiles import StaticFiles
from fastapi.responses import StreamingResponse, Response
from fastapi.middleware.cors import CORSMiddleware
from pydantic import BaseModel

# Resolve paths against this file, not the process working directory, so the
# app behaves the same however it was launched.
BASE_DIR = Path(__file__).resolve().parent
DATA_DIR = BASE_DIR / "data"
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

# Live monitor tracker: channel_id -> {channel_name, guild_name, guild_id, task}
live_monitors: dict = {}
# SSE subscriber queues for live feed
live_subscribers: list = []

# Shared across all Discord calls: one connection pool instead of a fresh TLS
# handshake per request. Set up in lifespan.
http_client: Optional[httpx.AsyncClient] = None


# ─── DB ──────────────────────────────────────────────────────

async def init_db():
    os.makedirs(DATA_DIR, exist_ok=True)
    async with aiosqlite.connect(DB_PATH) as db:
        # WAL lets the live monitors read while a scrape is writing, and
        # NORMAL sync is the right trade-off for a local archival tool.
        await db.execute("PRAGMA journal_mode=WAL")
        await db.execute("PRAGMA synchronous=NORMAL")
        await db.execute("""
            CREATE TABLE IF NOT EXISTS messages (
                id           TEXT PRIMARY KEY,
                channel_id   TEXT,
                channel_name TEXT,
                guild_id     TEXT,
                guild_name   TEXT,
                author_id    TEXT,
                author_name  TEXT,
                content      TEXT,
                timestamp    TEXT,
                attachments  TEXT
            )
        """)
        await db.execute("CREATE INDEX IF NOT EXISTS idx_ch   ON messages(channel_id)")
        await db.execute("CREATE INDEX IF NOT EXISTS idx_gd   ON messages(guild_id)")
        await db.execute("CREATE INDEX IF NOT EXISTS idx_au   ON messages(author_id)")
        await db.execute("CREATE INDEX IF NOT EXISTS idx_ts   ON messages(timestamp)")
        await db.execute("""
            CREATE TABLE IF NOT EXISTS settings (
                key   TEXT PRIMARY KEY,
                value TEXT
            )
        """)
        await db.commit()


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
        return float(response.json().get("retry_after", 1.0))
    except (ValueError, TypeError, AttributeError, json.JSONDecodeError):
        return 1.0


async def discord(method: str, path: str, token: str, **kwargs):
    headers = {
        "Authorization": token,
        "User-Agent": "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
        "Content-Type": "application/json",
    }
    url = f"{DISCORD_API}{path}"
    for attempt in range(MAX_RATE_LIMIT_RETRIES + 1):
        r = await http_client.request(method, url, headers=headers, **kwargs)
        if r.status_code != 429 or attempt == MAX_RATE_LIMIT_RETRIES:
            return r
        await asyncio.sleep(min(_retry_after(r), 60.0))
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
    limit: int = 0  # 0 = all messages; >0 = scrape at most N messages per channel


@app.post("/api/scrape/start")
async def start_scrape(req: ScrapeRequest, bg: BackgroundTasks):
    job_id = str(uuid.uuid4())
    queue: asyncio.Queue = asyncio.Queue(maxsize=PROGRESS_QUEUE_MAX)
    active_jobs[job_id] = {"queue": queue, "cancelled": False}
    bg.add_task(run_scrape, job_id, req.channels, req.limit)
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


async def _save_messages(msgs: list, cid: str, cname: str, gid: str, gname: str):
    rows = [
        (m["id"], cid, cname, gid, gname,
         m["author"]["id"],
         m["author"].get("global_name") or m["author"]["username"],
         m.get("content", ""),
         m["timestamp"],
         json.dumps([a["url"] for a in m.get("attachments", [])]))
        for m in msgs
    ]
    async with aiosqlite.connect(DB_PATH) as db:
        await db.executemany(
            """INSERT OR IGNORE INTO messages
               (id, channel_id, channel_name, guild_id, guild_name,
                author_id, author_name, content, timestamp, attachments)
               VALUES (?,?,?,?,?,?,?,?,?,?)""",
            rows,
        )
        await db.commit()


async def run_scrape(job_id: str, channels: list, limit: int = 0):
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

        for idx, channel in enumerate(channels):
            if active_jobs.get(job_id, {}).get("cancelled"):
                break

            cid = channel["id"]
            cname = channel["name"]
            gid = channel.get("guild_id")
            gname = channel.get("guild_name")

            _emit(q, {"type": "channel_start", "channel": cname, "guild": gname,
                      "index": idx + 1, "total": len(channels)})

            before = None
            ch_count = 0

            while True:
                if active_jobs.get(job_id, {}).get("cancelled"):
                    break

                # How many to fetch this batch, respecting optional limit
                if limit > 0:
                    remaining = limit - ch_count
                    if remaining <= 0:
                        break
                    fetch = min(100, remaining)
                else:
                    fetch = 100

                params = {"limit": fetch}
                if before:
                    params["before"] = before

                try:
                    r = await discord("GET", f"/channels/{cid}/messages", token, params=params)

                    if r.status_code == 403:
                        _emit(q, {"type": "channel_error", "channel": cname,
                                  "message": "No access (403)"})
                        break
                    if r.status_code != 200:
                        _emit(q, {"type": "channel_error", "channel": cname,
                                  "message": f"HTTP {r.status_code}"})
                        break

                    msgs = r.json()
                    if not msgs:
                        break

                    await _save_messages(msgs, cid, cname, gid, gname)

                    ch_count += len(msgs)
                    total_messages += len(msgs)
                    before = msgs[-1]["id"]

                    _emit(q, {"type": "progress", "channel": cname,
                              "messages": ch_count, "total_messages": total_messages})

                    # Stop if we got fewer messages than requested (no more history)
                    if len(msgs) < fetch:
                        break

                    await asyncio.sleep(0.4)

                except asyncio.CancelledError:
                    raise
                except Exception as e:
                    _emit(q, {"type": "channel_error", "channel": cname, "message": str(e)})
                    break

            _emit(q, {"type": "channel_complete", "channel": cname, "messages": ch_count})

        was_cancelled = active_jobs.get(job_id, {}).get("cancelled", False)
        _emit(q, {"type": "cancelled" if was_cancelled else "complete",
                  "total_messages": total_messages, "channels": len(channels)})

        # Leave the finished job readable briefly so a reconnecting client can
        # still pick up the terminal event.
        await asyncio.sleep(JOB_RETENTION_SECONDS)
    finally:
        active_jobs.pop(job_id, None)


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
    try:
        try:
            token = await get_token()
        except Exception:
            return

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
                await _save_messages(msgs, channel_id, channel_name, guild_id, guild_name)

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
                        "attachments": [a["url"] for a in m.get("attachments", [])],
                    })
                last_id = msgs[-1]["id"]
            except asyncio.CancelledError:
                raise
            except Exception:
                pass
    except asyncio.CancelledError:
        pass
    finally:
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
            "SELECT DISTINCT author_id, author_name FROM messages WHERE channel_id = ? ORDER BY author_name",
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
            "SELECT author_id, author_name, content, timestamp FROM messages "
            "WHERE channel_id = ? ORDER BY timestamp ASC",
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

@app.get("/api/search")
async def search(
    q: Optional[str] = None,
    guild_id: Optional[str] = None,
    channel_id: Optional[str] = None,
    author_id: Optional[str] = None,
    date_from: Optional[str] = None,
    date_to: Optional[str] = None,
    page: int = 1,
    limit: int = 50,
):
    conditions, params = [], []
    if q:
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
        conditions.append("timestamp >= ?")
        params.append(date_from)
    if date_to:
        conditions.append("timestamp <= ?")
        params.append(date_to)

    where = ("WHERE " + " AND ".join(conditions)) if conditions else ""
    offset = (page - 1) * limit

    async with aiosqlite.connect(DB_PATH) as db:
        db.row_factory = aiosqlite.Row
        async with db.execute(f"SELECT COUNT(*) FROM messages {where}", params) as cur:
            total = (await cur.fetchone())[0]
        async with db.execute(
            f"SELECT * FROM messages {where} ORDER BY timestamp DESC LIMIT ? OFFSET ?",
            params + [limit, offset]
        ) as cur:
            rows = [dict(r) for r in await cur.fetchall()]

    for r in rows:
        r["attachments"] = json.loads(r["attachments"] or "[]")

    return {"total": total, "page": page, "limit": limit,
            "pages": max(1, (total + limit - 1) // limit), "messages": rows}


@app.get("/api/stats")
async def get_stats():
    async with aiosqlite.connect(DB_PATH) as db:
        db.row_factory = aiosqlite.Row

        async with db.execute("SELECT COUNT(*) FROM messages") as cur:
            total_messages = (await cur.fetchone())[0]
        async with db.execute("SELECT COUNT(DISTINCT guild_id) FROM messages") as cur:
            total_servers = (await cur.fetchone())[0]
        async with db.execute("SELECT COUNT(DISTINCT channel_id) FROM messages") as cur:
            total_channels = (await cur.fetchone())[0]
        async with db.execute("SELECT COUNT(DISTINCT author_id) FROM messages") as cur:
            total_users = (await cur.fetchone())[0]

        async with db.execute("""
            SELECT author_id, author_name, COUNT(*) as count
            FROM messages GROUP BY author_id ORDER BY count DESC LIMIT 3
        """) as cur:
            top_users = [dict(r) for r in await cur.fetchall()]

        async with db.execute("""
            SELECT guild_name, guild_id, COUNT(*) as count
            FROM messages GROUP BY guild_id ORDER BY count DESC LIMIT 10
        """) as cur:
            by_server = [dict(r) for r in await cur.fetchall()]

        async with db.execute("""
            SELECT DATE(timestamp) as date, COUNT(*) as count
            FROM messages
            WHERE timestamp >= DATE('now', '-30 days')
            GROUP BY DATE(timestamp) ORDER BY date ASC
        """) as cur:
            by_day = [dict(r) for r in await cur.fetchall()]

        async with db.execute("""
            SELECT CAST(strftime('%H', timestamp) AS INTEGER) as hour, COUNT(*) as count
            FROM messages GROUP BY hour ORDER BY hour ASC
        """) as cur:
            by_hour = [dict(r) for r in await cur.fetchall()]

    try:
        db_size = os.path.getsize(DB_PATH)
    except OSError:
        db_size = 0

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
    }


@app.get("/api/search/filters")
async def search_filters():
    async with aiosqlite.connect(DB_PATH) as db:
        db.row_factory = aiosqlite.Row
        async with db.execute(
            "SELECT DISTINCT guild_id, guild_name FROM messages ORDER BY guild_name"
        ) as cur:
            guilds = [dict(r) for r in await cur.fetchall()]
        async with db.execute(
            "SELECT DISTINCT channel_id, channel_name, guild_id FROM messages ORDER BY guild_id, channel_name"
        ) as cur:
            channels = [dict(r) for r in await cur.fetchall()]
        async with db.execute(
            "SELECT DISTINCT author_id, author_name FROM messages ORDER BY author_name"
        ) as cur:
            users = [dict(r) for r in await cur.fetchall()]
        async with db.execute("SELECT COUNT(*) FROM messages") as cur:
            total = (await cur.fetchone())[0]

    return {"guilds": guilds, "channels": channels, "users": users, "total_messages": total}


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
        await db.execute("DELETE FROM messages")
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
