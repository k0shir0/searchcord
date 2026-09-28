"""Profile collection stays explicitly initiated in the collection app."""
import asyncio
from datetime import datetime, timezone
import json

import aiosqlite
from fastapi import APIRouter, HTTPException, Query

from profile_store import from_file, read_profile, profile_servers, user_id


async def fetch_profile(db, uid, token, discord):
    uid = user_id(uid)
    response = await discord('GET', f'/users/{uid}/profile', token,
                             params={'with_mutual_guilds': 'true', 'with_mutual_friends_count': 'false'})
    if response.status_code != 200:
        raise HTTPException(response.status_code if response.status_code in (401,403,404,429) else 502,
                            f'Discord could not provide this profile (HTTP {response.status_code}). Saved data was kept.')
    payload = response.json()
    user = payload.get('user') if isinstance(payload, dict) else None
    if not isinstance(user, dict) or str(user.get('id')) != uid:
        raise HTTPException(502, 'Discord returned a mismatched profile. Saved data was kept.')
    now = datetime.now(timezone.utc).isoformat()
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
    await db.commit()


def profile_router(path, token_provider, discord, lock):
    router = APIRouter()
    state = {'running':False,'saved':0,'failed':0,'cancelled':False,'status':'idle'}
    task = None

    @router.get('/api/profiles/{uid}')
    def profile(uid: str):
        return from_file(path, read_profile, uid)

    @router.get('/api/profiles/{uid}/servers')
    def servers(uid: str, q: str = Query('',max_length=100), offset: int = Query(0,ge=0), limit: int = Query(30,ge=1,le=100)):
        return from_file(path, profile_servers, uid, q, offset, limit)

    @router.post('/api/profiles/{uid}/fetch')
    async def fetch(uid: str):
        uid = user_id(uid)
        # Only archived authors can be collected through this surface.
        await asyncio.to_thread(from_file, path, read_profile, uid)
        token = await token_provider()
        async with lock, aiosqlite.connect(path) as db:
            await fetch_profile(db, uid, token, discord)
        return await asyncio.to_thread(from_file, path, read_profile, uid)

    async def backfill(token):
        try:
            # Keyset batches bound memory even on archives with millions of authors.
            after = ''
            while not state['cancelled']:
                async with aiosqlite.connect(path) as db:
                    async with db.execute('''SELECT a.id FROM authors a LEFT JOIN profile_details p ON p.user_id=a.id
                        WHERE p.user_id IS NULL AND a.id>? ORDER BY a.id LIMIT 100''',(after,)) as cur:
                        ids = [row[0] for row in await cur.fetchall()]
                if not ids: break
                for uid in ids:
                    if state['cancelled']: break
                    after = uid
                    async with lock, aiosqlite.connect(path) as db:
                        # A simultaneous manual fetch may already have filled this author.
                        async with db.execute('SELECT 1 FROM profile_details WHERE user_id=?',(uid,)) as cur:
                            if await cur.fetchone(): continue
                        try:
                            await fetch_profile(db, uid, token, discord)
                            state['saved'] += 1
                        except HTTPException as exc:
                            state['failed'] += 1
                            if exc.status_code in (401,403,429):
                                state['status'] = f'Stopped after Discord HTTP {exc.status_code}. Check account access before retrying.'
                                return
                await asyncio.sleep(0)
            state['status'] = 'Stopped. Saved profiles are kept.' if state['cancelled'] else 'Complete.'
        except asyncio.CancelledError:
            state['status'] = 'Stopped.'
            raise
        except Exception:
            state['status'] = 'Collection failed. Saved profiles are kept; retry to resume.'
        finally:
            state['running'] = False

    @router.get('/api/profile-backfill')
    def progress():
        return dict(state)

    @router.post('/api/profile-backfill')
    async def start():
        nonlocal task
        if state['running']:
            raise HTTPException(409, 'Profile collection is already running.')
        token = await token_provider()
        # Check again after the awaited token lookup to prevent duplicate starts.
        if state['running']:
            raise HTTPException(409, 'Profile collection is already running.')
        state.update(running=True,saved=0,failed=0,cancelled=False,status='Collecting missing extended profiles…')
        task = asyncio.create_task(backfill(token))
        return dict(state)

    @router.post('/api/profile-backfill/stop')
    def stop():
        state['cancelled'] = True
        return dict(state)

    async def shutdown():
        if task and not task.done():
            task.cancel()
            await asyncio.gather(task, return_exceptions=True)

    return router, shutdown
