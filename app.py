import os
import json
import asyncio
import logging
import math
import sqlite3
import uuid
import time
import random
from http.client import HTTPConnection, HTTPException as HTTPClientException
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
from storage import EPOCH_MS, image_urls, init_database, save_message_payloads, mark_messages_deleted
from profile_api import profile_router, collect_profile, ProfileStopped
from search_app import Archive, MAX_ID
from cli import local_url, open_url, print_banner
from channel_access import readable_channels
from invite_api import invite_router
from collection_state import save_job, persist_job, load_jobs, remove_job
from durable_scrape import Scrapes, TransientScrapeError
from credential_store import (load_credentials, save_credentials, clear_credentials,
                              credential_metadata, CredentialError)
from media_store import archive_message_media, local_url as archived_media_url, cache_media, clear_media, media_router
from storage import read_message_archive
from chat_export import export_names, chatml_lines
from profile_store import image_url as profile_image_url

# Resolve paths against this file, not the process working directory, so the
# app behaves the same however it was launched.
BASE_DIR = Path(__file__).resolve().parent
DATA_DIR = Path(os.environ.get("SEARCHCORD_DATA_DIR", "data")).expanduser()
if not DATA_DIR.is_absolute():
    DATA_DIR = BASE_DIR / DATA_DIR
DB_PATH = str(DATA_DIR / "searchcord.db")
# Production web files belong in static/; design trials belong in static/trials/.
STATIC_DIR = BASE_DIR / "static"

DISCORD_API = "https://discord.com/api/v10"

# Give up rather than recursing forever if Discord keeps rate-limiting us.
MAX_RATE_LIMIT_RETRIES = 5
SCRAPE_RETRY_BASE_SECONDS = 2
SCRAPE_RETRY_MAX_SECONDS = 60
DISCORD_INTERVAL_SECONDS = 0.025
LIVE_POLL_SECONDS = 3
SCRAPE_INTERVAL_MIN_SECONDS = 0.5
SCRAPE_INTERVAL_MAX_SECONDS = 1.0
SCRAPE_BREAK_REQUESTS = 100
SCRAPE_BREAK_SECONDS = 60
# Cap on buffered scrape-progress events, so a job whose SSE client never
# connects (or goes away mid-scrape) can't grow its queue without bound.
PROGRESS_QUEUE_MAX = 1000
# How long a finished job stays readable before it is discarded.
JOB_RETENTION_SECONDS = 60

scrape_log = logging.getLogger("searchcord.scrape")
if not scrape_log.handlers:
    handler = logging.StreamHandler()
    handler.setFormatter(logging.Formatter("%(asctime)s %(levelname)s [scrape] %(message)s"))
    scrape_log.addHandler(handler)
scrape_log.setLevel(logging.INFO)
scrape_log.propagate = False

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
discord_ready_at = 0.0
last_scrape_request = 0.0
scrape_confirmed_requests = 0
scrape_break_until = 0.0
archive_clearing = False
credentials_clearing = False
collection_write_lock = asyncio.Lock()
settings_lock = asyncio.Lock()
collection_persistence_ready = False


async def _credential_id():
    _require_credentials_available()
    async with aiosqlite.connect(DB_PATH) as db:
        settings = await _token_settings(db)
    tokens = _saved_tokens(settings)
    _require_credentials_available()
    return settings.get("active_token_id") or (tokens[0]["id"] if tokens else None)


async def _job_token(job):
    credential_id, token = await _selected_credentials()
    if job.get("credential_id") != credential_id:
        raise HTTPException(409, "Select the account used to start this job before resuming")
    return token


async def _selected_credentials():
    async with settings_lock:
        return await _credential_id(), await get_token()


def _require_archive_available():
    if archive_clearing:
        raise HTTPException(409, "Archive clear is in progress; retry after it finishes")
    _require_credentials_available()


def _require_credentials_available():
    if credentials_clearing:
        raise HTTPException(409, "Credential removal is in progress; retry after it finishes")


async def _persist_pacing():
    if not collection_persistence_ready:
        return
    now = time.monotonic()
    wall = time.time()
    await persist_job(DB_PATH, "discord-pacing", "pacing", {
        "discord_ready_at": wall + max(0, discord_ready_at - now),
        "last_scrape_request": wall + last_scrape_request - now if last_scrape_request else 0,
        "scrape_break_until": wall + max(0, scrape_break_until - now),
        "scrape_confirmed_requests": scrape_confirmed_requests})


async def _recover_collection_jobs():
    global discord_ready_at, last_scrape_request, scrape_break_until, scrape_confirmed_requests, collection_persistence_ready
    now, wall = time.monotonic(), time.time()
    discord_ready_at = last_scrape_request = scrape_break_until = 0.0
    scrape_confirmed_requests = 0
    for job_id, kind, state in await load_jobs(DB_PATH):
        if kind == "pacing":
            discord_ready_at = now + max(0, state.get("discord_ready_at", 0) - wall)
            scrape_break_until = now + max(0, state.get("scrape_break_until", 0) - wall)
            last_scrape_request = now + state.get("last_scrape_request", 0) - wall
            scrape_confirmed_requests = state.get("scrape_confirmed_requests", 0)
        elif kind == "monitor" and state.get("running"):
            live_monitors[job_id] = {**state, "paused": True, "task": None}
    await scrapes.recover()
    collection_persistence_ready = True


async def _suspend_collection_jobs():
    tasks = []
    await scrapes.suspend()
    for cid, info in list(live_monitors.items()):
        info["suspending"] = True
        info["paused"] = True
        if info.get("task"):
            info["task"].cancel()
            tasks.append(info["task"])
    for job in delete_jobs.values():
        job.update(cancelled=True, suspending=True)
        if job.get("task") and not job["task"].done():
            job["task"].cancel()
            tasks.append(job["task"])
    if tasks:
        await asyncio.gather(*tasks, return_exceptions=True)


def discord_avatar_url(user_id: str, avatar_hash: Optional[str]) -> Optional[str]:
    if not isinstance(avatar_hash, str) or not str(user_id).isdigit() or not avatar_hash.removeprefix("a_").isalnum():
        return None
    return profile_image_url(user_id, avatar_hash, size=64)


# ─── DB ──────────────────────────────────────────────────────

async def init_db():
    print("Preparing local database. A first upgrade of a large archive may take several minutes.", flush=True)
    task = asyncio.create_task(asyncio.to_thread(init_database, DB_PATH))
    while True:
        try:
            result = await asyncio.wait_for(asyncio.shield(task), timeout=30)
            break
        except asyncio.TimeoutError:
            print("Still preparing local database; the web port opens when it is ready.", flush=True)
    if result["backup"]:
        print(f"Verified storage migration; recovery backup retained: {result['backup']}", flush=True)
    print("Local database ready.", flush=True)


@asynccontextmanager
async def lifespan(app: FastAPI):
    global http_client, collection_persistence_ready
    print_banner()
    scrape_log.info("timing save_interval=each fetched page (up to 100 messages); "
                    "scrape_interval=%g-%gs after each response; "
                    "scrape_break=%gs after %s successful responses; "
                    "request_interval_min=%gs; retry_interval=%g-%gs; live_poll_interval=%gs",
                    SCRAPE_INTERVAL_MIN_SECONDS, SCRAPE_INTERVAL_MAX_SECONDS,
                    SCRAPE_BREAK_SECONDS, SCRAPE_BREAK_REQUESTS,
                    DISCORD_INTERVAL_SECONDS, SCRAPE_RETRY_BASE_SECONDS,
                    SCRAPE_RETRY_MAX_SECONDS, LIVE_POLL_SECONDS)
    await init_db()
    await _recover_collection_jobs()
    await recover_invite_jobs()
    http_client = httpx.AsyncClient(
        timeout=30,
        limits=httpx.Limits(max_connections=20, max_keepalive_connections=10),
    )
    try:
        yield
    finally:
        # Stop live pollers before tearing down the client they depend on,
        # otherwise they raise into the shutdown path.
        await _suspend_collection_jobs()
        await shutdown_invite_jobs(preserve=True)
        await shutdown_profile_jobs()
        await http_client.aclose()
        http_client = None
        collection_persistence_ready = False
        scrapes.reset()
        live_monitors.clear()

app = FastAPI(lifespan=lifespan)
APP_INSTANCE_ID = uuid.uuid4().hex

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


@app.middleware("http")
async def archive_profile_barrier(request: Request, call_next):
    path = request.url.path
    if ((request.method == "POST" and (path.startswith("/api/profiles/") or
                                      path == "/api/profile-backfill" or
                                      (path.startswith("/api/messages/") and path.endswith("/refresh")))) or
            (request.method == "GET" and path.startswith("/api/images/"))):
        if archive_clearing:
            return Response(content='{"detail":"Archive clear is in progress"}',
                            status_code=409, media_type="application/json")
        async with collection_write_lock:
            if archive_clearing:
                return Response(content='{"detail":"Archive clear is in progress"}',
                                status_code=409, media_type="application/json")
            return await call_next(request)
    return await call_next(request)


@app.get("/api/health", include_in_schema=False)
async def health():
    return Response(content="searchcord-ready", media_type="text/plain",
                    headers={"X-Searchcord-Instance": APP_INSTANCE_ID})


def open_browser_when_ready(host: str, port: int):
    """Open this app only after its startup has completed and it answers HTTP."""
    browser_host = {"0.0.0.0": "127.0.0.1", "::": "::1"}.get(host, host)
    while True:
        connection = HTTPConnection(browser_host, port, timeout=1)
        try:
            connection.request("GET", "/api/health")
            response = connection.getresponse()
            if (response.status == 200
                    and response.getheader("X-Searchcord-Instance") == APP_INSTANCE_ID
                    and response.read() == b"searchcord-ready"):
                open_url(local_url(host, port))
                return
        except (OSError, HTTPClientException):
            pass
        finally:
            connection.close()
        time.sleep(0.5)


# ─── Discord helpers ─────────────────────────────────────────

async def get_token() -> str:
    _require_credentials_available()
    async with aiosqlite.connect(DB_PATH) as db:
        settings = await _token_settings(db)
    tokens = _saved_tokens(settings)
    active = settings.get("active_token_id") or (tokens[0]["id"] if tokens else None)
    token = next((item["token"] for item in tokens if item["id"] == active), None)
    _require_credentials_available()
    if not token:
        raise HTTPException(401, "No token configured")
    return token


async def _token_settings(db) -> dict:
    async with db.execute("""SELECT key, value FROM settings
        WHERE key IN ('token', 'saved_tokens', 'active_token_id')""") as cur:
        legacy = dict(await cur.fetchall())
    try:
        state = await asyncio.to_thread(load_credentials, DB_PATH)
    except CredentialError:
        raise HTTPException(503, "Could not unlock saved credentials; the credential vault was kept") from None
    # Startup migrates legacy credentials. This fallback supports isolated legacy
    # archives inspected before lifespan startup without writing plaintext again.
    try:
        saved = json.loads(legacy.get("saved_tokens", "[]"))
        has_plaintext = isinstance(saved, list) and any(
            isinstance(item, dict) and "token" in item for item in saved)
    except (TypeError, ValueError):
        has_plaintext = False
    if not state["tokens"] and (legacy.get("token") or has_plaintext):
        return legacy
    return {"saved_tokens": json.dumps(state["tokens"]),
            "active_token_id": state["active_token_id"]}


def _saved_tokens(settings: dict) -> list[dict]:
    raw = settings.get("saved_tokens")
    if raw is None:
        legacy = settings.get("token")
        return [{"id": "legacy", "label": "Saved token", "token": legacy}] if legacy else []
    try:
        tokens = json.loads(raw)
        if not isinstance(tokens, list) or any(
            not isinstance(item, dict) or
            not all(isinstance(item.get(key), str) and item[key] for key in ("id", "label", "token"))
            for item in tokens
        ):
            raise ValueError
        return tokens
    except (ValueError, TypeError):
        raise HTTPException(500, "Saved token settings are invalid") from None


def _retry_after(response) -> float:
    """Seconds to wait after a 429, tolerant of a non-JSON error body."""
    try:
        value = response.json().get("retry_after")
    except (ValueError, TypeError, AttributeError, json.JSONDecodeError):
        value = None
    try:
        seconds = float(value if value is not None else response.headers.get("Retry-After", 1.0))
        return max(0.0, seconds) if math.isfinite(seconds) else 1.0
    except (ValueError, TypeError, AttributeError):
        return 1.0


def _unknown_message(response) -> bool:
    """A channel/authentication 404 does not confirm a message deletion."""
    if response.status_code != 404:
        return False
    try:
        payload = response.json()
    except ValueError:
        return False
    return (isinstance(payload, dict) and type(payload.get("code")) is int
            and payload["code"] == 10008)


@asynccontextmanager
async def _discord_gate(scrape_job_id, cancelled):
    # Check controls while waiting for another request, without acquiring the
    # gate on behalf of a paused scrape.
    while True:
        if scrape_job_id:
            await scrapes.checkpoint(scrape_job_id)
        if cancelled and cancelled():
            raise ProfileStopped
        try:
            await asyncio.wait_for(discord_request_lock.acquire(), timeout=0.1)
            break
        except asyncio.TimeoutError:
            continue
    try:
        yield
    finally:
        discord_request_lock.release()


async def discord(method: str, path: str, token: str, *, retry_rate_limits=True,
                  cancelled=None, on_wait=None, scrape_job_id=None, **kwargs):
    global last_discord_request, discord_ready_at, last_scrape_request
    global scrape_confirmed_requests, scrape_break_until
    headers = {
        "Authorization": token,
        "User-Agent": "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
        "Content-Type": "application/json",
    }
    url = f"{DISCORD_API}{path}"
    # One process-wide gate makes concurrent scrape, profile, live-monitor and
    # DM jobs respect a shared global 429 instead of retrying into it together.
    for attempt in range(MAX_RATE_LIMIT_RETRIES + 1):
        # Draw once per attempt, including retries. Waiting never holds the
        # request lock, so a paused job cannot block unrelated Discord work.
        interval = random.uniform(SCRAPE_INTERVAL_MIN_SECONDS, SCRAPE_INTERVAL_MAX_SECONDS) if scrape_job_id else 0
        while True:
            if scrape_job_id:
                await scrapes.checkpoint(scrape_job_id)
            if cancelled and cancelled():
                raise ProfileStopped
            async with _discord_gate(scrape_job_id, cancelled):
                if cancelled and cancelled():
                    raise ProfileStopped
                if scrape_job_id and not scrapes.request_allowed(scrape_job_id):
                    continue
                ready_at = max(last_discord_request + DISCORD_INTERVAL_SECONDS, discord_ready_at)
                if scrape_job_id:
                    ready_at = max(ready_at, last_scrape_request + interval, scrape_break_until)
                wait = ready_at - time.monotonic()
                if wait <= 0:
                    last_discord_request = time.monotonic()
                    try:
                        try:
                            r = await http_client.request(method, url, headers=headers, **kwargs)
                        finally:
                            if scrape_job_id:
                                # Failures and slow responses also get the full
                                # scrape interval before another request.
                                last_scrape_request = time.monotonic()
                    except BaseException:
                        await _persist_pacing()
                        raise
                    if scrape_job_id and 200 <= r.status_code < 300:
                        scrape_confirmed_requests += 1
                        if scrape_confirmed_requests == SCRAPE_BREAK_REQUESTS:
                            scrape_confirmed_requests = 0
                            scrape_break_until = time.monotonic() + SCRAPE_BREAK_SECONDS
                    # Publish cooldowns before releasing the shared gate.
                    if r.status_code == 429:
                        discord_ready_at = max(discord_ready_at, time.monotonic() + _retry_after(r))
                    elif r.headers.get("X-RateLimit-Remaining") == "0":
                        try:
                            seconds = float(r.headers.get("X-RateLimit-Reset-After", 0))
                            if math.isfinite(seconds):
                                discord_ready_at = max(discord_ready_at, time.monotonic() + max(0.0, seconds))
                        except ValueError:
                            pass
                    await _persist_pacing()
                    break
            if scrape_job_id and scrape_break_until > time.monotonic():
                scrapes.break_wait(scrape_job_id, math.ceil(scrape_break_until - time.monotonic()))
            if on_wait and wait >= 0.1:
                on_wait(wait)
            if scrape_job_id:
                await scrapes.wait(scrape_job_id, min(0.25, wait))
            else:
                await asyncio.sleep(min(0.25, wait))
        if scrape_job_id:
            scrapes.break_finished(scrape_job_id)
        if r.status_code == 429:
            if not retry_rate_limits or attempt == MAX_RATE_LIMIT_RETRIES:
                return r
            continue
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
async def get_channels(guild_id: str, include_threads: bool = False):
    token = await get_token()
    return await readable_channels(guild_id, token, discord, include_threads=include_threads)


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


def _validated_channels(channels):
    if not 1 <= len(channels) <= 2000:
        raise HTTPException(422, "Select between 1 and 2000 channels")
    result, seen = [], set()
    for channel in channels:
        if not isinstance(channel, dict):
            raise HTTPException(422, "Each channel must provide its ID and name")
        cid, gid = channel.get("id"), channel.get("guild_id")
        name, guild_name = channel.get("name"), channel.get("guild_name", "")
        if (not isinstance(cid, str) or not cid.isascii() or not cid.isdigit() or
                not 1 <= int(cid) <= MAX_ID or not isinstance(name, str) or len(name) > 200 or
                not isinstance(guild_name, str) or len(guild_name) > 200 or
                (gid is not None and (not isinstance(gid, str) or not gid.isascii() or
                                     not gid.isdigit() or not 1 <= int(gid) <= MAX_ID))):
            raise HTTPException(422, "Channel IDs must be Discord IDs and names must contain at most 200 characters")
        if cid not in seen:
            result.append({"id": cid, "name": name, "guild_id": gid, "guild_name": guild_name})
            seen.add(cid)
    return result


@app.post("/api/scrape/start")
async def start_scrape(req: ScrapeRequest):
    channels = _validated_channels(req.channels)
    return {"job_id": await scrapes.start(channels, req.limit, req.harvest_profiles)}


@app.get("/api/scrape/active")
async def scrape_active():
    return scrapes.active()


@app.post("/api/scrape/{job_id}/pause")
async def pause_scrape(job_id: str):
    return await scrapes.pause(job_id)


@app.post("/api/scrape/{job_id}/resume")
async def resume_scrape(job_id: str):
    return await scrapes.resume(job_id)


@app.post("/api/scrape/{job_id}/stop")
async def stop_scrape(job_id: str):
    return await scrapes.stop(job_id)


@app.get("/api/scrape/progress/{job_id}")
async def scrape_progress(job_id: str, request: Request):
    scrapes.snapshot(job_id)

    async def stream():
        async for event in scrapes.events(job_id):
            if await request.is_disconnected():
                break
            yield f"data: {json.dumps(event)}\n\n"

    return StreamingResponse(stream(), media_type="text/event-stream",
                             headers={"Cache-Control": "no-cache", "X-Accel-Buffering": "no"})


def _emit(q: asyncio.Queue, event: dict):
    """Queue a progress event without blocking its worker.

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
profile_routes, shutdown_profile_jobs = profile_router(DB_PATH, get_token, discord, profile_fetch_lock)
app.include_router(profile_routes)
app.include_router(media_router(lambda: DB_PATH))
invite_routes, shutdown_invite_jobs, recover_invite_jobs = invite_router(
    get_token, discord, path=lambda: DB_PATH, credential_id=_credential_id,
    credential_context=_selected_credentials)
app.include_router(invite_routes)


async def _write_db():
    async def connect():
        db = await aiosqlite.connect(DB_PATH, timeout=30)
        try:
            await db.execute("PRAGMA synchronous=NORMAL")
            await db.execute("PRAGMA busy_timeout=30000")
            return db
        except BaseException:
            await db.close()
            raise

    task = asyncio.create_task(connect())
    try:
        return await asyncio.shield(task)
    except asyncio.CancelledError:
        db = await task
        await db.close()
        raise


async def _save_messages(db, msgs: list, cid: str, cname: str, gid: str,
                         gname: str, history_page: bool = False,
                         pending: tuple = None, clear_pending: bool = False,
                         checkpoint=None, history_complete=False, monitor_checkpoint=None) -> int:
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
        inserted = await save_message_payloads(db, msgs, cid, cname, gid, gname)
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
        if history_complete:
            await db.execute("UPDATE scrape_cursors SET history_complete=1 WHERE channel_id=?", (cid,))
        if checkpoint:
            await checkpoint(inserted)
        if monitor_checkpoint:
            channel_id, checkpoint = monitor_checkpoint
            await save_job(db, channel_id, "monitor", checkpoint())
        await db.commit()
        return inserted
    except BaseException:
        await db.rollback()
        raise


async def _collect_scrape_profile(db, user_id, token, *, job_id, cancelled, checkpoint, progress):
    # Storage/transport remain profile responsibilities; pending work belongs to
    # the scrape owner. Recheck after acquiring the shared profile lock.
    while True:
        await checkpoint()
        try:
            await asyncio.wait_for(profile_fetch_lock.acquire(), timeout=0.1)
            break
        except asyncio.TimeoutError:
            continue
    try:
        await checkpoint()
        async with db.execute("SELECT 1 FROM profile_details WHERE user_id=?", (user_id,)) as cursor:
            if await cursor.fetchone():
                return False
        async def request(*args, **kwargs):
            return await discord(*args, scrape_job_id=job_id, **kwargs)
        await collect_profile(db, user_id, token, request, cancelled, progress, checkpoint=checkpoint)
        return True
    finally:
        profile_fetch_lock.release()


async def _fetch_messages(cid: str, token: str, params: dict, job_id: str) -> list:
    r = await discord("GET", f"/channels/{cid}/messages", token, params=params,
                      scrape_job_id=job_id)
    if r.status_code in (408, 425, 429) or 500 <= r.status_code <= 599:
        raise TransientScrapeError(f"HTTP {r.status_code}",
                                    _retry_after(r) if r.status_code in (429, 503) else 0)
    if r.status_code == 401:
        raise PermissionError("Token rejected (401)")
    if r.status_code == 403:
        raise PermissionError("No access (403)")
    if r.status_code != 200:
        raise RuntimeError(f"HTTP {r.status_code}")
    try:
        messages = r.json()
    except (ValueError, TypeError) as exc:
        raise TransientScrapeError("Invalid JSON response") from exc
    if not isinstance(messages, list):
        raise TransientScrapeError("Unexpected messages response")
    return messages


def create_scrapes():
    return Scrapes(
        path=lambda: DB_PATH, credentials=lambda: _selected_credentials(),
        available=_require_archive_available, connect=lambda: _write_db(),
        fetch=lambda *args: _fetch_messages(*args),
        save=lambda *args, **kwargs: _save_messages(*args, **kwargs),
        profile=lambda *args, **kwargs: _collect_scrape_profile(*args, **kwargs),
        media=lambda *args, **kwargs: archive_message_media(DB_PATH, *args, **kwargs),
        break_seconds=lambda: max(0, math.ceil(scrape_break_until - time.monotonic())),
        queue_size=PROGRESS_QUEUE_MAX, retention=JOB_RETENTION_SECONDS,
        retry_base=SCRAPE_RETRY_BASE_SECONDS, retry_max=SCRAPE_RETRY_MAX_SECONDS,
        log=scrape_log)


scrapes = create_scrapes()


# ─── DM Clearer ──────────────────────────────────────────────


class DmClearStartRequest(BaseModel):
    channel_id: str = Field(pattern=r"^[0-9]{1,19}$")


@app.post("/api/dm-clear/start")
async def start_dm_clear(req: DmClearStartRequest, bg: BackgroundTasks):
    _require_archive_available()
    if not 1 <= int(req.channel_id) <= MAX_ID:
        raise HTTPException(422, "Channel ID must be a Discord ID")
    if any(job.get("running") and job.get("channel_id") == req.channel_id for job in delete_jobs.values()):
        raise HTTPException(409, "This DM is already being cleared")
    job_id = str(uuid.uuid4())
    queue: asyncio.Queue = asyncio.Queue(maxsize=PROGRESS_QUEUE_MAX)
    delete_jobs[job_id] = {"queue": queue, "cancelled": False, "running": True, "channel_id": req.channel_id}
    delete_jobs[job_id]["task"] = asyncio.create_task(run_dm_clear(job_id, req.channel_id))
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


async def _dm_wait(job, seconds):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if job.get("cancelled"):
            raise ProfileStopped
        await asyncio.sleep(min(0.1, max(0, deadline - time.monotonic())))


async def run_dm_clear(job_id: str, channel_id: str):
    job = delete_jobs[job_id]
    job["running"] = True
    q = job["queue"]
    counts = {"deleted": 0, "failed": 0, "already_missing": 0, "scanned": 0}
    error = None
    stopped = lambda: job.get("cancelled", False)
    try:
        token = await get_token()
        response = await discord("GET", "/users/@me", token, cancelled=stopped)
        if response.status_code != 200:
            raise RuntimeError(f"Failed to fetch user: HTTP {response.status_code}")
        self_id = response.json()["id"]
        before = None
        while not stopped():
            params = {"limit": 100}
            if before:
                params["before"] = before
            response = await discord("GET", f"/channels/{channel_id}/messages", token,
                                     params=params, cancelled=stopped)
            if response.status_code != 200:
                raise RuntimeError(f"Could not read messages: HTTP {response.status_code}")
            messages = response.json()
            if not isinstance(messages, list):
                raise RuntimeError("Discord returned an invalid message page")
            if not messages:
                break
            counts["scanned"] += len(messages)
            for msg in messages:
                if stopped():
                    break
                if msg["author"]["id"] != self_id:
                    continue
                msg_id = msg["id"]
                status = None
                missing = False
                for attempt in range(4):
                    try:
                        deletion = await discord("DELETE", f"/channels/{channel_id}/messages/{msg_id}",
                            token, retry_rate_limits=False, cancelled=stopped)
                        status = deletion.status_code
                        missing = _unknown_message(deletion)
                    except httpx.RequestError:
                        status = None
                    transient = status is None or status in (408, 425, 429) or 500 <= status <= 599
                    if not transient or attempt == 3:
                        break
                    _emit(q, {"type": "warning", "message": f"Retrying delete {msg_id} ({attempt + 1}/3)."})
                    await _dm_wait(job, min(8, 2 ** attempt))
                if status == 204:
                    counts["deleted"] += 1
                elif missing:
                    counts["already_missing"] += 1
                else:
                    counts["failed"] += 1
                    _emit(q, {"type": "warning", "message": f"Could not confirm delete {msg_id}: " +
                        (f"HTTP {status}" if status else "connection failure")})
                if status == 204 or missing:
                    async with aiosqlite.connect(DB_PATH, timeout=30) as db:
                        await mark_messages_deleted(db, [msg_id],
                            source="confirmed_delete" if status == 204 else "confirmed_missing")
                        await db.commit()
                _emit(q, {"type": "progress", **counts})
                if status in (401, 403):
                    raise RuntimeError(f"Discord denied deletion: HTTP {status}")
                await _dm_wait(job, 0.45)
            before = str(min(int(msg["id"]) for msg in messages))
            if len(messages) < 100:
                break
    except ProfileStopped:
        pass
    except asyncio.CancelledError:
        job["cancelled"] = True
        raise
    except Exception as exc:
        error = str(exc) if isinstance(exc, (HTTPException, RuntimeError)) else "Deletion stopped after an unexpected response; check Discord before retrying."
    finally:
        job["running"] = False
        terminal = {"type": "error" if error else "cancelled" if stopped() else "complete", **counts}
        if error:
            terminal["message"] = error
        _emit(q, terminal)
        try:
            if not job.get("suspending"):
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


async def _persist_monitor(channel_id):
    info = live_monitors[channel_id]
    await persist_job(DB_PATH, channel_id, "monitor", _monitor_state(info))


def _monitor_state(info):
    return {key: info.get(key) for key in ("channel_name", "guild_id", "guild_name", "running",
        "paused", "last_id", "credential_id", "pending_before", "pending_anchor", "pending_newest")}


async def poll_channel(channel_id: str, channel_name: str, guild_id: str, guild_name: str):
    # Any exit path — clean stop, network failure, bad response — has to clear
    # the registry entry, or the channel can never be monitored again.
    db = None
    info = live_monitors[channel_id]
    try:
        try:
            token = await _job_token(info)
        except Exception:
            info.update(paused=True, suspending=True)
            return
        db = await _write_db()

        # Anchor to the current newest message so we only stream NEW ones
        last_id = info.get("last_id")
        try:
            if last_id is None:
                r = await discord("GET", f"/channels/{channel_id}/messages",
                                  token, params={"limit": 1})
                if r.status_code != 200:
                    raise RuntimeError("Could not establish the monitor cursor")
                anchor = r.json()
                if anchor:
                    last_id = anchor[0]["id"]
                info["last_id"] = last_id
                await _persist_monitor(channel_id)
        except asyncio.CancelledError:
            raise
        except Exception:
            info["paused"] = True
            info["suspending"] = True
            return

        await _broadcast({"type": "monitor_start", "channel_id": channel_id,
                          "channel": channel_name, "guild": guild_name, "guild_id": guild_id})

        while channel_id in live_monitors:
            await asyncio.sleep(LIVE_POLL_SECONDS)
            if channel_id not in live_monitors:
                break

            params = {"limit": 50}
            draining = bool(info.get("pending_before"))
            if draining:
                params["before"] = info["pending_before"]

            try:
                r = await discord("GET", f"/channels/{channel_id}/messages", token, params=params)
                if r.status_code != 200:
                    continue
                msgs = r.json()
                if not msgs:
                    if draining:
                        info.update(last_id=info["pending_newest"], pending_before=None,
                                    pending_anchor=None, pending_newest=None)
                        last_id = info["last_id"]
                        await _persist_monitor(channel_id)
                    async with db.execute("""SELECT m.id FROM messages m LEFT JOIN message_payloads p
                        ON p.message_id=m.id WHERE m.channel_id=? AND p.deleted_at IS NULL
                        ORDER BY m.id DESC LIMIT 1""", (channel_id,)) as cur:
                        saved = await cur.fetchone()
                    if saved:
                        observed = await discord("GET", f"/channels/{channel_id}/messages/{saved[0]}", token)
                        if _unknown_message(observed):
                            await mark_messages_deleted(db, [saved[0]], source="upstream_not_found")
                            await db.commit()
                    continue
                msgs.sort(key=lambda m: int(m["id"]))
                anchor = info.get("pending_anchor") if draining else last_id
                fresh = [m for m in msgs if anchor is None or int(m["id"]) > int(anchor)]
                newest = info.get("pending_newest") if draining else msgs[-1]["id"]
                progress = {"last_id": last_id, "pending_anchor": None,
                            "pending_before": None, "pending_newest": None}
                if len(msgs) == 50 and len(fresh) == 50 and anchor is not None:
                    progress.update(pending_anchor=anchor, pending_before=msgs[0]["id"], pending_newest=newest)
                else:
                    progress["last_id"] = newest if last_id is None or int(newest) > int(last_id) else last_id
                task = asyncio.create_task(_save_messages(db, msgs, channel_id, channel_name, guild_id, guild_name,
                    monitor_checkpoint=(channel_id, lambda: {**_monitor_state(info), **progress})))
                cancellation = None
                try:
                    await asyncio.shield(task)
                except asyncio.CancelledError as exc:
                    await task
                    cancellation = exc
                info.update(progress)
                last_id = info["last_id"]
                if cancellation:
                    raise cancellation
                await archive_message_media(DB_PATH, msgs,
                    cancelled=lambda: channel_id not in live_monitors)

                for m in fresh:
                    await _broadcast({
                        "type": "message",
                        "id": m["id"],
                        "channel_id": channel_id,
                        "channel": channel_name,
                        "guild_id": guild_id,
                        "guild": guild_name,
                        "author_id": m["author"]["id"],
                        "author": m["author"].get("global_name") or m["author"]["username"],
                        "avatar_url": discord_avatar_url(
                            m["author"]["id"], m["author"].get("avatar")),
                        "content": m.get("content", ""),
                        "timestamp": m["timestamp"],
                        "attachments": [f"/api/images/{m['id']}/{i}" for i, _ in
                                        enumerate(image_urls(m.get("attachments")))],
                    })
                if not draining:
                    current_ids = {str(m["id"]) for m in msgs}
                    async with db.execute("""SELECT m.id FROM messages m LEFT JOIN message_payloads p
                        ON p.message_id=m.id WHERE m.channel_id=? AND m.id>=? AND m.id<=?
                        AND p.deleted_at IS NULL ORDER BY m.id DESC LIMIT 51""",
                        (channel_id, int(msgs[0]["id"]), max(int(msgs[-1]["id"]), int(last_id or 0)))) as cur:
                        absent = next((str(row[0]) for row in await cur.fetchall() if str(row[0]) not in current_ids), None)
                    if absent:
                        observed = await discord("GET", f"/channels/{channel_id}/messages/{absent}", token)
                        if _unknown_message(observed):
                            await mark_messages_deleted(db, [absent], source="upstream_not_found")
                            await db.commit()
                        elif observed.status_code == 200:
                            await _save_messages(db, [observed.json()], channel_id, channel_name, guild_id, guild_name)
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
        if info.get("suspending"):
            info["task"] = None
            info["paused"] = True
            await _persist_monitor(channel_id)
        else:
            live_monitors.pop(channel_id, None)
            await remove_job(DB_PATH, channel_id)
        await _broadcast({"type": "monitor_paused" if info.get("suspending") else "monitor_stop", "channel_id": channel_id,
                          "channel": channel_name})


class LiveStartRequest(BaseModel):
    channels: list


@app.post("/api/live/start")
async def live_start(req: LiveStartRequest):
    _require_archive_available()
    req.channels = _validated_channels(req.channels)
    credential_id, _ = await _selected_credentials()
    _require_archive_available()
    for ch in req.channels:
        cid = ch["id"]
        if cid not in live_monitors or live_monitors[cid].get("paused"):
            info = live_monitors.get(cid)
            if info and info.get("credential_id") != credential_id:
                raise HTTPException(409, "Select the account used to start this monitor before resuming")
            live_monitors[cid] = info or {
                "channel_name": ch["name"],
                "guild_name":   ch["guild_name"],
                "guild_id":     ch["guild_id"],
                "credential_id": credential_id, "last_id": None,
            }
            live_monitors[cid].update(running=True, paused=False, suspending=False)
            await _persist_monitor(cid)
            live_monitors[cid]["task"] = asyncio.create_task(
                poll_channel(cid, ch["name"], ch["guild_id"], ch["guild_name"]))
    return {"active": list(live_monitors.keys())}


class LiveStopRequest(BaseModel):
    channel_ids: list = []  # empty list = stop all


@app.post("/api/live/stop")
async def live_stop(req: LiveStopRequest):
    to_stop = req.channel_ids if req.channel_ids else list(live_monitors.keys())
    for cid in to_stop:
        if cid in live_monitors:
            info = live_monitors[cid]
            info["suspending"] = False
            if info.get("task"):
                info["task"].cancel()
                await asyncio.gather(info["task"], return_exceptions=True)
            live_monitors.pop(cid, None)
            await remove_job(DB_PATH, cid)
    return {"stopped": to_stop}


@app.get("/api/live/status")
async def live_status():
    return {
        "channels": [
            {"channel_id": cid, "channel_name": info["channel_name"],
             "guild_name": info["guild_name"], "guild_id": info["guild_id"]}
            | {"paused": info.get("paused", False)}
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
            q.put_nowait({"type": "monitor_paused" if info.get("paused") else "monitor_start", "channel_id": cid,
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
    target_name, default_other = await export_names(DB_PATH, req.channel_id, req.target_id)
    other_name = req.other_name or default_other
    try:
        system_prompt = (req.system_prompt or DEFAULT_SYSTEM_TEMPLATE).format(target=target_name, other=other_name)
    except (KeyError, ValueError):
        raise HTTPException(400, "System prompt supports only {target} and {other} placeholders") from None
    return StreamingResponse(chatml_lines(DB_PATH, req.channel_id, req.target_id, other_name, system_prompt),
        media_type="application/x-ndjson",
        headers={"Content-Disposition": f'attachment; filename="chatml_{req.channel_id}.jsonl"'})


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
    q: Annotated[Optional[str], Query(max_length=200)] = None,
    guild_id: Optional[str] = None,
    channel_id: Optional[str] = None,
    author_id: Optional[str] = None,
    date_from: Optional[str] = None,
    date_to: Optional[str] = None,
    page: Annotated[int, Query(ge=1)] = 1,
    limit: Annotated[int, Query(ge=1, le=100)] = 50,
    cursor: bool = False,
    scan: bool = False,
    before: Annotated[Optional[int], Query(ge=1, le=MAX_ID)] = None,
):
    if cursor:
        result = await asyncio.to_thread(
            Archive(DB_PATH).search, q or '', guild_id, channel_id, author_id,
            date_from, date_to, before, limit, True, scan)
        return {"total": None, "pages": None, "page": page, "limit": limit, **result}
    conditions, params = [], []
    if q:
        literal = q.replace("\\", "\\\\").replace("%", "\\%").replace("_", "\\_")
        if len(q) >= 3 and "\\" not in literal:
            conditions.append("rowid IN (SELECT rowid FROM messages_fts WHERE content LIKE ?)")
        else:
            conditions.append("content LIKE ? ESCAPE '\\'")
        params.append(f"%{literal}%")
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

    try:
        async with aiosqlite.connect(DB_PATH) as db:
            db.row_factory = aiosqlite.Row
            deadline = time.monotonic() + 4
            await db.set_progress_handler(lambda: time.monotonic() > deadline, 1000)
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

    except sqlite3.OperationalError as exc:
        if "interrupt" in str(exc):
            raise HTTPException(408, "Counted search exceeded its deadline; retry with cursor=true") from None
        raise

    for r in rows:
        r["id"] = str(r["id"])
        avatar = avatars.get(r["author_id"])
        r["avatar_url"] = discord_avatar_url(r["author_id"], avatar)
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
    cached = archived_media_url(DB_PATH, url)
    if cached != url:
        return RedirectResponse(cached, status_code=302)
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
    await cache_media(DB_PATH, url)
    return RedirectResponse(archived_media_url(DB_PATH, url), status_code=302)


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


@app.get("/api/stats/servers")
async def ranked_servers(
    offset: Annotated[int, Query(ge=0)] = 0,
    limit: Annotated[int, Query(ge=1, le=100)] = 50,
    q: Annotated[str, Query(max_length=100)] = "",
):
    """Page through all archived servers using cached message counts."""
    query = q.strip().replace('!', '!!').replace('%', '!%').replace('_', '!_')
    async with aiosqlite.connect(DB_PATH) as db:
        db.row_factory = aiosqlite.Row
        async with db.execute("""SELECT count FROM stats_counts
            WHERE kind='total' AND key=''""") as cur:
            row = await cur.fetchone()
            total_messages = row[0] if row else 0
        async with db.execute("""SELECT counts.key AS guild_id, guilds.name AS guild_name,
            counts.count FROM stats_counts counts LEFT JOIN guilds ON guilds.id=counts.key
            WHERE counts.kind='guild' AND counts.key<>'' AND counts.count>0
            AND (?='' OR guilds.name LIKE ? ESCAPE '!' OR counts.key=?)
            ORDER BY counts.count DESC, counts.key LIMIT ? OFFSET ?""",
            (query, f'%{query}%', q.strip(), limit + 1, offset)) as cur:
            rows = [dict(row) for row in await cur.fetchall()]
    return {"servers": rows[:limit], "has_more": len(rows) > limit,
            "offset": offset, "limit": limit, "total_messages": total_messages}


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
        async with db.execute("SELECT key, value FROM settings") as cur:
            rows = dict(await cur.fetchall())
        rows.update(await _token_settings(db))
    tokens = _saved_tokens(rows)
    result = {key: value for key, value in rows.items()
              if key not in {"token", "saved_tokens", "active_token_id"}}
    result["tokens"] = [{"id": item["id"], "label": item["label"]} for item in tokens]
    result["active_token_id"] = rows.get("active_token_id") or (
        tokens[0]["id"] if tokens else None)
    result["token_set"] = bool(tokens)
    return result


class SettingsIn(BaseModel):
    token: Optional[str] = None
    token_label: Optional[str] = None
    active_token_id: Optional[str] = None


@app.post("/api/settings")
async def update_settings(s: SettingsIn):
    if (s.token is None) == (s.active_token_id is None):
        raise HTTPException(400, "Provide a token or a saved token ID")
    token = s.token.strip() if s.token is not None else None
    label = (s.token_label or "").strip()
    if token is not None and (not token or len(token) > 4096):
        raise HTTPException(400, "Token must contain 1 to 4096 characters")
    if len(label) > 40:
        raise HTTPException(400, "Token name must be at most 40 characters")
    async with settings_lock, aiosqlite.connect(DB_PATH) as db:
        _require_credentials_available()
        await db.execute("BEGIN IMMEDIATE")
        settings = await _token_settings(db)
        tokens = _saved_tokens(settings)
        if token is not None:
            existing = next((item for item in tokens if item["token"] == token), None)
            if existing:
                active_id = existing["id"]
                if label:
                    existing["label"] = label
            else:
                active_id = uuid.uuid4().hex
                tokens.append({"id": active_id, "label": label or f"Token {len(tokens) + 1}", "token": token})
        else:
            if not any(item["id"] == s.active_token_id for item in tokens):
                raise HTTPException(404, "Saved token not found")
            active_id = s.active_token_id
        try:
            await asyncio.to_thread(save_credentials, DB_PATH, tokens, active_id)
        except CredentialError:
            raise HTTPException(503, "Could not save credentials; the credential vault was kept") from None
        await db.executemany(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?, ?)",
            [("saved_tokens", json.dumps(credential_metadata(tokens))), ("active_token_id", active_id)],
        )
        await db.execute("DELETE FROM settings WHERE key='token'")
        await db.commit()
    return {"ok": True, "active_token_id": active_id,
            "tokens": [{"id": item["id"], "label": item["label"]} for item in tokens]}


@app.delete("/api/settings/tokens")
async def clear_tokens(confirm: bool = False):
    if not confirm:
        raise HTTPException(400, "Confirm removal of all saved tokens")
    if (scrapes.has_work
            or delete_jobs or live_monitors):
        raise HTTPException(409, "Stop scrapes, DM cleanup and live monitors before removing tokens")
    global credentials_clearing
    _require_credentials_available()
    credentials_clearing = True
    try:
        async with settings_lock:
            await shutdown_invite_jobs()
            await shutdown_profile_jobs()
            async with aiosqlite.connect(DB_PATH, timeout=5) as db:
                await asyncio.to_thread(clear_credentials, DB_PATH)
                await db.execute("DELETE FROM settings WHERE key IN ('token', 'saved_tokens', 'active_token_id')")
                await db.commit()
    except (sqlite3.Error, CredentialError, OSError):
        raise HTTPException(503, "Could not remove saved tokens; retry when storage is available") from None
    finally:
        credentials_clearing = False
    return {"ok": True, "tokens": [], "active_token_id": None, "token_set": False}


@app.delete("/api/messages")
async def clear_messages(confirm: bool = False):
    global archive_clearing
    if not confirm:
        raise HTTPException(400, "Confirm removal of all archived messages, profiles, revisions and cached media")
    _require_archive_available()
    if (scrapes.has_work or live_monitors or
            any(job.get("running", True) for job in delete_jobs.values())):
        raise HTTPException(409, "Stop active and paused scrapes, live monitors and DM cleanup before clearing the archive")
    archive_clearing = True
    try:
        async with collection_write_lock:
            await shutdown_profile_jobs()
            async with profile_fetch_lock, aiosqlite.connect(DB_PATH, timeout=30) as db:
                await db.execute("BEGIN IMMEDIATE")
                for table in ("message_revisions", "message_payloads", "messages", "message_history", "name_values", "stats_counts",
                              "scrape_cursors", "channel_authors", "profile_details", "profiles", "authors", "channels", "guilds"):
                    await db.execute(f"DELETE FROM {table}")
                await db.execute("DELETE FROM collection_jobs WHERE kind IN ('scrape','monitor')")
                await db.commit()
                await asyncio.to_thread(clear_media, DB_PATH)
        return {"ok": True}
    except sqlite3.Error:
        raise HTTPException(503, "Could not clear the archive; retry when storage is available") from None
    except OSError:
        raise HTTPException(503, "Archive rows cleared, but cached media could not be removed; retry clear to finish") from None
    finally:
        archive_clearing = False


@app.get("/api/messages/{message_id}/archive")
async def message_archive(message_id: int,
                          after_revision: Annotated[int, Query(ge=0)] = 0,
                          limit: Annotated[int, Query(ge=1, le=100)] = 100):
    if not 1 <= message_id <= MAX_ID:
        raise HTTPException(400, "Invalid message ID")
    result = await asyncio.to_thread(read_message_archive, DB_PATH, message_id,
                                    after_revision=after_revision, limit=limit)
    if result is None:
        raise HTTPException(404, "Message not archived")
    return result


@app.post("/api/messages/{message_id}/refresh")
async def refresh_message(message_id: int):
    _require_archive_available()
    if not 1 <= message_id <= MAX_ID:
        raise HTTPException(400, "Invalid message ID")
    async with aiosqlite.connect(DB_PATH) as db:
        async with db.execute("""SELECT channel_id,channel_name,guild_id,guild_name
            FROM message_records WHERE id=?""", (message_id,)) as cursor:
            row = await cursor.fetchone()
    if row is None:
        raise HTTPException(404, "Message not archived")
    token = await get_token()
    response = await discord("GET", f"/channels/{row[0]}/messages/{message_id}", token)
    db = await _write_db()
    try:
        if response.status_code == 200:
            await _save_messages(db, [response.json()], *row)
            await archive_message_media(DB_PATH, [response.json()])
        elif _unknown_message(response):
            accessible = await discord("GET", f"/channels/{row[0]}", token)
            if accessible.status_code != 200:
                raise HTTPException(409, "Channel access could not be confirmed; deletion was not recorded")
            await mark_messages_deleted(db, [message_id], source="upstream_not_found")
            await db.commit()
        else:
            raise HTTPException(response.status_code, "Could not refresh this message; archived contents were kept")
    finally:
        await db.close()
    return await message_archive(message_id, 0, 100)


# ─── Static ──────────────────────────────────────────────────

app.mount("/", StaticFiles(directory=str(STATIC_DIR), html=True), name="static")

if __name__ == "__main__":
    import threading
    import uvicorn

    # Loopback by default. This app has no authentication and holds a Discord
    # token plus everything you have scraped, so binding 0.0.0.0 would hand
    # the whole archive to anyone on the same network. Override deliberately.
    host = os.environ.get("SEARCHCORD_HOST", "127.0.0.1")
    port = int(os.environ.get("SEARCHCORD_PORT", "8000"))

    threading.Thread(target=open_browser_when_ready, args=(host, port), daemon=True).start()
    uvicorn.run(app, host=host, port=port)
