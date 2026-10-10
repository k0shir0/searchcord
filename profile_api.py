"""Profile collection stays explicitly initiated in the collection app."""
import asyncio
from datetime import datetime, timedelta, timezone
import json
import logging
import math
import sqlite3
import time

import aiosqlite
import httpx
from fastapi import APIRouter, HTTPException, Query

from profile_store import from_file, read_profile, profile_servers, user_id, STALE_AFTER_SECONDS
from media_store import archive_message_media

RETRY_BASE_SECONDS = 2
RETRY_MAX_SECONDS = 60
log = logging.getLogger('searchcord.profiles')


class ProfileStopped(Exception):
    pass


def retry_seconds(response):
    try:
        value = response.json().get('retry_after')
    except (ValueError, AttributeError):
        value = None
    try:
        value = float(value if value is not None else response.headers.get('Retry-After', 0))
        return max(0, value) if math.isfinite(value) else 0
    except (ValueError, TypeError):
        return 0


async def collect_profile(db, uid, token, discord, cancelled, progress, *, checkpoint=None):
    """Retry the same user until saved, unavailable, or explicitly stopped."""
    attempts = 0
    while not cancelled():
        operation = asyncio.create_task(fetch_profile(db, uid, token, discord,
            cancelled=cancelled, on_wait=lambda seconds: progress({
                'user_id': uid, 'reason': 'Discord cooldown', 'wait_seconds': seconds,
                'attempt': attempts})))
        try:
            while not operation.done():
                await asyncio.wait({operation}, timeout=0.25)
                if cancelled():
                    raise ProfileStopped
            await operation
            return
        except (HTTPException, httpx.RequestError, TimeoutError, sqlite3.OperationalError) as exc:
            if isinstance(exc, HTTPException):
                if exc.status_code not in (408, 425, 429) and not 500 <= exc.status_code <= 599:
                    raise
                reason = f'HTTP {exc.status_code}'
                delay = float((exc.headers or {}).get('Retry-After', 0))
            elif isinstance(exc, sqlite3.OperationalError):
                if not any(word in str(exc).lower() for word in ('locked', 'busy')):
                    raise
                reason, delay = 'database busy', 0
            else:
                reason, delay = type(exc).__name__, 0
            attempts += 1
            delay = max(delay, min(RETRY_MAX_SECONDS, RETRY_BASE_SECONDS * 2 ** min(attempts - 1, 10)))
            log.warning('user=%s profile retry=%s reason=%s wait=%.1fs', uid, attempts, reason, delay)
            progress({'user_id': uid, 'reason': reason, 'attempt': attempts, 'wait_seconds': delay})
            deadline = time.monotonic() + delay
            while time.monotonic() < deadline:
                if cancelled():
                    raise ProfileStopped
                if checkpoint and not isinstance(exc, sqlite3.OperationalError):
                    await checkpoint()
                await asyncio.sleep(min(0.25, max(0, deadline - time.monotonic())))
        finally:
            if not operation.done():
                operation.cancel()
            await asyncio.gather(operation, return_exceptions=True)
    raise ProfileStopped


async def fetch_profile(db, uid, token, discord, *, cancelled=None, on_wait=None):
    uid = user_id(uid)
    response = await discord('GET', f'/users/{uid}/profile', token,
                             params={'with_mutual_guilds': 'true', 'with_mutual_friends_count': 'false'},
                             retry_rate_limits=False, cancelled=cancelled, on_wait=on_wait)
    if response.status_code != 200:
        raise HTTPException(response.status_code if response.status_code in (401,403,404,408,425,429) or
                            500 <= response.status_code <= 599 else 502,
                            f'Discord could not provide this profile (HTTP {response.status_code}). Saved data was kept.',
                            headers={'Retry-After': str(retry_seconds(response))})
    try:
        payload = response.json()
    except ValueError as exc:
        raise HTTPException(502, 'Discord returned an invalid profile response. Saved data was kept.') from exc
    user = payload.get('user') if isinstance(payload, dict) else None
    if not isinstance(user, dict) or str(user.get('id')) != uid:
        raise HTTPException(502, 'Discord returned a mismatched profile. Saved data was kept.')
    now = datetime.now(timezone.utc).isoformat()
    try:
        await save_profile(db, uid, user, payload, now)
        await db.commit()
    except BaseException:
        await db.rollback()
        raise
    async with db.execute('PRAGMA database_list') as cur:
        path = (await cur.fetchone())[2]
    if path:
        # Cache after the profile commit; a failed binary must not discard identity.
        details = payload.get('user_profile') or {}
        media_user = dict(user, banner=details.get('banner') or user.get('banner'))
        await archive_message_media(path, [{'author': media_user}], cancelled=cancelled)


async def save_profile(db, uid, user, payload, now):
    await db.execute('''INSERT INTO profiles
        (user_id,username,global_name,avatar_hash,banner_hash,accent_color,bot,public_flags,fetched_at)
        VALUES (?,?,?,?,?,?,?,?,?) ON CONFLICT(user_id) DO UPDATE SET
        username=excluded.username,global_name=excluded.global_name,avatar_hash=excluded.avatar_hash,
        banner_hash=excluded.banner_hash,accent_color=excluded.accent_color,bot=excluded.bot,
        public_flags=excluded.public_flags,fetched_at=excluded.fetched_at''',
        (uid,user.get('username') or '',user.get('global_name'),user.get('avatar'),user.get('banner'),
         user.get('accent_color'),int(bool(user.get('bot'))),user.get('public_flags'),now))
    await db.execute('''INSERT INTO profile_details(user_id,payload,fetched_at) VALUES (?,?,?)
        ON CONFLICT(user_id) DO UPDATE SET payload=excluded.payload,fetched_at=excluded.fetched_at''',
        (uid,json.dumps(payload,ensure_ascii=False),now))


def profile_router(path, token_provider, discord, lock):
    router = APIRouter()
    state = {'running':False,'saved':0,'failed':0,'cancelled':False,'status':'idle',
             'current_user':None,'retries':0,'wait_seconds':0,'last_error':None}
    task = None

    @router.get('/api/profiles/{uid}')
    def profile(uid: str):
        return from_file(path, read_profile, uid)

    @router.get('/api/profiles/{uid}/servers')
    def servers(uid: str, q: str = Query('',max_length=100), offset: int = Query(0,ge=0), limit: int = Query(30,ge=1,le=100)):
        return from_file(path, profile_servers, uid, q, offset, limit)

    @router.post('/api/profiles/{uid}/fetch')
    @router.post('/api/profiles/{uid}/refresh')
    async def fetch(uid: str):
        uid = user_id(uid)
        # Only archived authors can be collected through this surface.
        await asyncio.to_thread(from_file, path, read_profile, uid)
        token = await token_provider()
        async with lock, aiosqlite.connect(path) as db:
            await fetch_profile(db, uid, token, discord)
        return await asyncio.to_thread(from_file, path, read_profile, uid)

    async def backfill(token, refresh_stale=False):
        try:
            # Keyset batches bound memory even on archives with millions of authors.
            after = ''
            cutoff = (datetime.now(timezone.utc) - timedelta(seconds=STALE_AFTER_SECONDS)).isoformat()
            while not state['cancelled']:
                async with aiosqlite.connect(path) as db:
                    async with db.execute('''SELECT a.id FROM authors a LEFT JOIN profile_details p ON p.user_id=a.id
                        WHERE (p.user_id IS NULL OR (? AND p.fetched_at<=?)) AND a.id>?
                        ORDER BY a.id LIMIT 100''',(refresh_stale, cutoff, after)) as cur:
                        ids = [row[0] for row in await cur.fetchall()]
                if not ids: break
                for uid in ids:
                    if state['cancelled']: break
                    after = uid
                    state.update(current_user=uid,wait_seconds=0,status='Refreshing archived profiles…' if refresh_stale else 'Collecting missing extended profiles…')
                    async with lock, aiosqlite.connect(path) as db:
                        # A simultaneous manual fetch may already have filled this author.
                        async with db.execute('SELECT 1 FROM profile_details WHERE user_id=? AND (NOT ? OR fetched_at>?)',
                                              (uid,refresh_stale,cutoff)) as cur:
                            if await cur.fetchone(): continue
                        try:
                            def retry(info):
                                state.update(retries=state['retries'] + 1, wait_seconds=info['wait_seconds'],
                                    status=f"{info['reason']}; retrying user {uid} in {info['wait_seconds']:.1f}s…")
                            await collect_profile(db, uid, token, discord, lambda: state['cancelled'], retry)
                            state['saved'] += 1
                        except HTTPException as exc:
                            state['failed'] += 1
                            state['last_error'] = f'HTTP {exc.status_code} for user {uid}'
                            if exc.status_code == 401:
                                state['status'] = f'Stopped after Discord HTTP {exc.status_code}. Check account access before retrying.'
                                return
                    # Give a scrape waiting on the shared profile lock its turn.
                    await asyncio.sleep(0)
                await asyncio.sleep(0)
            state['status'] = 'Stopped. Saved profiles are kept.' if state['cancelled'] else 'Complete.'
        except (asyncio.CancelledError, ProfileStopped):
            state['status'] = 'Stopped. Saved profiles are kept.'
        except Exception:
            log.exception('Profile backfill failed; saved profiles remain available')
            state['status'] = 'Collection failed. Saved profiles are kept; retry to resume.'
        finally:
            state.update(running=False,current_user=None,wait_seconds=0)

    @router.get('/api/profile-backfill')
    def progress():
        return dict(state)

    @router.post('/api/profile-backfill')
    async def start(refresh_stale: bool = Query(False)):
        nonlocal task
        if state['running']:
            raise HTTPException(409, 'Profile collection is already running.')
        token = await token_provider()
        # Check again after the awaited token lookup to prevent duplicate starts.
        if state['running']:
            raise HTTPException(409, 'Profile collection is already running.')
        state.update(running=True,saved=0,failed=0,cancelled=False,current_user=None,retries=0,
                     wait_seconds=0,last_error=None,status='Refreshing archived profiles…' if refresh_stale else 'Collecting missing extended profiles…')
        task = asyncio.create_task(backfill(token, refresh_stale))
        return dict(state)

    @router.post('/api/profile-backfill/stop')
    async def stop():
        state['cancelled'] = True
        if task and not task.done():
            task.cancel()
            await asyncio.gather(task, return_exceptions=True)
            # A task stopped before its first turn never enters backfill's
            # finally block. Always clear the running state after joining it.
            state.update(running=False,current_user=None,wait_seconds=0,
                         status='Stopped. Saved profiles are kept.')
        return dict(state)

    async def shutdown():
        if task and not task.done():
            task.cancel()
            await asyncio.gather(task, return_exceptions=True)

    return router, shutdown
