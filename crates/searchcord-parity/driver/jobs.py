"""Mechanical controls for the real collector; Rust supplies scenarios and checks.

No imports from the Python test suite. All network responses and fault/barrier
settings come from the Rust process. A single loop preserves accepted writes
between commands, including while Rust inspects SQLite from another connection.
"""
import asyncio
from contextlib import ExitStack
from copy import deepcopy
import sqlite3
import time
from types import SimpleNamespace
from unittest.mock import AsyncMock, patch

import httpx
import app
import durable_scrape


class Jobs:
    def __init__(self, path):
        self.path = path
        self.patches = ExitStack()
        self.replies = []
        self.requests = []
        self.tasks = {}
        self.barriers = {}
        self.attempts = 0
        self.server = None
        self.subscriptions = {}

    def barrier(self, name):
        return self.barriers.setdefault(name, (asyncio.Event(), asyncio.Event()))

    async def setup(self):
        for key, value in {
            'DB_PATH': str(self.path), 'discord_request_lock': asyncio.Lock(),
            'last_discord_request': 0, 'last_scrape_request': 0, 'discord_ready_at': 0,
            'scrape_break_until': 0, 'scrape_confirmed_requests': 0,
            'collection_persistence_ready': True, 'archive_clearing': False,
            'credentials_clearing': False, 'settings_lock': asyncio.Lock(),
            'profile_fetch_lock': asyncio.Lock(), 'collection_write_lock': asyncio.Lock(),
            'SCRAPE_INTERVAL_MIN_SECONDS': 0, 'SCRAPE_INTERVAL_MAX_SECONDS': 0,
            'SCRAPE_RETRY_BASE_SECONDS': .01, 'SCRAPE_RETRY_MAX_SECONDS': .02,
            'PROGRESS_QUEUE_MAX': 20,
        }.items():
            self.patches.enter_context(patch.object(app, key, value))
        self.selected = self.patches.enter_context(patch.object(app, '_selected_credentials',
            AsyncMock(return_value=('account', 'synthetic'))))
        self.patches.enter_context(patch.object(app, 'get_token', AsyncMock(return_value='synthetic')))
        self.patches.enter_context(patch.object(app, 'archive_message_media', AsyncMock()))
        self.owner = app.create_scrapes()
        self.patches.enter_context(patch.object(app, 'scrapes', self.owner))
        self.transport = httpx.AsyncClient(transport=httpx.MockTransport(self.reply))
        self.patches.enter_context(patch.object(app, 'http_client', self.transport))
        # Rebind routers that capture archive paths/callbacks at construction.
        # These are the production router factories, with this disposable DB.
        profile_routes, self.shutdown_profiles = app.profile_router(str(self.path),
            app.get_token, app.discord, app.profile_fetch_lock)
        invite_routes, self.shutdown_invites, self.recover_invites = app.invite_router(
            app.get_token, app.discord, path=lambda: str(self.path),
            credential_id=app._credential_id, credential_context=app._selected_credentials)
        replaced = {route.path for router in (profile_routes, invite_routes) for route in router.routes}
        self.patches.enter_context(patch.object(app.app.router, 'routes',
            profile_routes.routes + invite_routes.routes +
            [route for route in app.app.router.routes if getattr(route, 'path', None) not in replaced
             and getattr(route, 'original_router', None) not in (app.profile_routes, app.invite_routes)]))
        self.patches.enter_context(patch.object(app, 'shutdown_profile_jobs', self.shutdown_profiles))
        self.patches.enter_context(patch.object(app, 'shutdown_invite_jobs', self.shutdown_invites))
        self.patches.enter_context(patch.object(app, 'recover_invite_jobs', self.recover_invites))
        import profile_api
        self.patches.enter_context(patch.object(profile_api, 'archive_message_media', AsyncMock()))
        self.client = httpx.AsyncClient(transport=httpx.ASGITransport(app=app.app), base_url='http://fixture')

    async def reply(self, request):
        self.requests.append({'path': request.url.path, 'params': dict(request.url.params),
                              'method': request.method, 'invite_time': getattr(self, 'invite_now', None)})
        if self.hold:
            self.hold = False
            entered, release = self.barrier('fetch')
            entered.set()
            await release.wait()
        response = self.replies.pop(0) if self.replies else {'status': 200, 'body': []}
        if response.get('error'):
            raise httpx.ReadTimeout(response['error'])
        return httpx.Response(response['status'], json=response.get('body'), headers=response.get('headers'))

    async def invoke(self, r):
        action = r['action']
        if action == 'configure':
            self.replies = deepcopy(r.get('replies', self.replies))
            self.hold = r.get('hold', False)
            return None
        if action == 'commit_barrier':
            original = durable_scrape.save_job
            failures = r.get('failures', 0)
            async def save(db, job_id, kind, state):
                if (state.get('last_event') or {}).get('type') == 'progress':
                    self.attempts += 1
                    if self.attempts == 1:
                        entered, release = self.barrier('commit')
                        entered.set()
                        await release.wait()
                    if self.attempts <= failures:
                        raise sqlite3.OperationalError('database busy')
                await original(db, job_id, kind, state)
            self.patches.enter_context(patch.object(durable_scrape, 'save_job', save))
            return None
        if action == 'credentials':
            self.selected.side_effect = [tuple(v) for v in r['sequence']] if 'sequence' in r else None
            self.selected.return_value = (r.get('id', 'account'), 'synthetic')
            return None
        if action == 'credentials_barrier':
            async def credentials():
                entered, release = self.barrier('credentials')
                entered.set()
                await release.wait()
                return 'account', 'synthetic'
            self.patches.enter_context(patch.object(app, '_selected_credentials', credentials))
            return None
        if action == 'wait_barrier':
            await asyncio.wait_for(self.barrier(r['name'])[0].wait(), 4)
            return None
        if action == 'release':
            self.barrier(r['name'])[1].set()
            return None
        if action == 'http':
            response = await self.client.request(r.get('method', 'POST'), r['path'], json=r.get('body'))
            try:
                body = response.json()
            except ValueError:
                body = response.text
            return {'status': response.status_code, 'body': body, 'headers': dict(response.headers)}
        if action == 'concurrent_http':
            return await asyncio.gather(*(self.invoke({'action': 'http', **v}) for v in r['requests']))
        if action == 'control':
            method = getattr(self.owner, r['method'])
            return await method(*([r['id']] if 'id' in r else []))
        if action == 'spawn':
            self.tasks[r['name']] = asyncio.create_task(self.invoke(r['call']))
            # Yield a turn so the accepted operation has entered its first await.
            await asyncio.sleep(0)
            return None
        if action == 'task':
            task = self.tasks[r['name']]
            if r.get('join'):
                return await asyncio.wait_for(asyncio.shield(task), 4)
            return {'done': task.done()}
        if action == 'snapshot':
            return self.owner.snapshot(r['id'])
        if action == 'wait_snapshot':
            async with asyncio.timeout(4):
                while True:
                    snapshot = self.owner.snapshot(r['id'])
                    if all(snapshot.get(k) == v for k, v in r['fields'].items()):
                        return snapshot
                    await asyncio.sleep(.001)
        if action == 'recover':
            self.owner.reset()
            await self.owner.recover()
            return None
        if action == 'start_suspend':
            job_id = await self.owner.start(r['channels'])
            await self.owner.suspend()
            return job_id
        if action == 'inspect':
            return {'requests': deepcopy(self.requests), 'attempts': self.attempts,
                    'has_work': self.owner.has_work}
        if action == 'block_fetch':
            async def fetch(*args):
                await asyncio.Event().wait()
            self.patches.enter_context(patch.object(app, '_fetch_messages', fetch))
            return None
        if action == 'virtual_clock':
            self.now = time.monotonic() + 1
            self.sent, self.finished, self.waits, self.draws = [], [], [], []
            self.random_values = list(r['draws'])
            self.response_seconds = r.get('response_seconds', 0)
            async def sleep(seconds):
                self.waits.append(seconds)
                self.now += seconds
            def uniform(low, high):
                self.draws.append([low, high])
                return self.random_values.pop(0)
            class Client:
                async def request(inner, *args, **kwargs):
                    self.sent.append(self.now)
                    self.now += self.response_seconds
                    self.finished.append(self.now)
                    response = self.replies.pop(0) if self.replies else {'status': 200, 'body': []}
                    if response.get('error'):
                        raise httpx.ReadTimeout(response['error'])
                    return httpx.Response(response['status'], json=response.get('body'), headers=response.get('headers'))
            self.owner._clock = lambda: self.now
            self.owner._sleep = sleep
            self.patches.enter_context(patch.object(app, 'time', SimpleNamespace(monotonic=lambda: self.now, time=time.time)))
            self.patches.enter_context(patch.object(app, 'http_client', Client()))
            self.patches.enter_context(patch.object(app, 'SCRAPE_INTERVAL_MIN_SECONDS', r['bounds'][0]))
            self.patches.enter_context(patch.object(app, 'SCRAPE_INTERVAL_MAX_SECONDS', r['bounds'][1]))
            self.patches.enter_context(patch.object(app.random, 'uniform', uniform))
            return None
        if action == 'pacing_requests':
            subscriptions, queues = [], []
            for job_id in set(r['ids']):
                subscription = self.owner.subscribe(job_id)
                _, queue = await subscription.__aenter__()
                subscriptions.append(subscription)
                queues.append(queue)
            async def request(job_id):
                try:
                    response = await app.discord('GET', '/channels/20/messages', 'synthetic', scrape_job_id=job_id)
                    return {'status': response.status_code}
                except httpx.ReadTimeout as exc:
                    return {'error': type(exc).__name__}
            try:
                if r.get('concurrent'):
                    results = await asyncio.gather(*(request(i) for i in r['ids']))
                else:
                    results = [await request(i) for i in r['ids']]
                events = []
                for queue in queues:
                    while not queue.empty():
                        events.append(queue.get_nowait())
                return {'results': results, 'events': events}
            finally:
                for subscription in subscriptions:
                    await subscription.__aexit__(None, None, None)
        if action == 'set_counter':
            app.scrape_confirmed_requests = r['value']
            return None
        if action == 'recover_pacing':
            self.owner.reset()
            await app._recover_collection_jobs()
            return None
        if action == 'pacing_inspect':
            now = app.time.monotonic()
            return {'sent': self.sent, 'finished': self.finished, 'waits': self.waits, 'draws': self.draws,
                    'counter': app.scrape_confirmed_requests,
                    'break_seconds': app.scrape_break_until - now,
                    'cooldown_seconds': app.discord_ready_at - now,
                    'wall': time.time()}
        if action == 'options':
            allowed = {'DISCORD_INTERVAL_SECONDS', 'LIVE_POLL_SECONDS', 'archive_clearing',
                       'credentials_clearing', 'JOB_RETENTION_SECONDS'}
            for key, value in r['values'].items():
                if key not in allowed:
                    raise ValueError('unsupported test option')
                self.patches.enter_context(patch.object(app, key, value))
            return None
        if action == 'wait_http':
            async with asyncio.timeout(4):
                while True:
                    reply = await self.invoke({'action': 'http', 'method': 'GET', 'path': r['path']})
                    value = reply['body']
                    for key in r['keys']:
                        value = value[key]
                    if value == r['value']:
                        return reply
                    await asyncio.sleep(.001)
        if action == 'wait_database':
            async with asyncio.timeout(4):
                while True:
                    with sqlite3.connect(self.path) as db:
                        value = db.execute(r['sql']).fetchone()[0]
                    if value == r['value']:
                        return value
                    await asyncio.sleep(.001)
        if action == 'wait_requests':
            async with asyncio.timeout(4):
                while len(self.requests) < r['count']:
                    await asyncio.sleep(.001)
            return deepcopy(self.requests)
        if action == 'suspend_all':
            await app._suspend_collection_jobs()
            await self.shutdown_invites(preserve=True)
            await self.shutdown_profiles()
            return None
        if action == 'recover_all':
            self.owner.reset()
            app.live_monitors.clear()
            await app._recover_collection_jobs()
            await self.recover_invites()
            return None
        if action == 'route_inventory':
            def routes(router):
                for route in router.routes:
                    if hasattr(route, 'original_router'):
                        yield from routes(route.original_router)
                    elif getattr(route, 'methods', None):
                        yield {'path': route.path, 'methods': sorted(route.methods)}
            return sorted(routes(app.app), key=lambda r: (r['path'], r['methods']))
        if action == 'lifespan':
            async with app.lifespan(app.app):
                during = {'ready': app.collection_persistence_ready, 'client': app.http_client is not None}
            return {'during': during, 'after': {'ready': app.collection_persistence_ready,
                    'client': app.http_client is not None, 'has_work': self.owner.has_work}}
        if action == 'clear_media_fault':
            self.patches.enter_context(patch.object(app, 'clear_media', side_effect=OSError('injected media cleanup failure')))
            return None
        if action == 'drain_dm':
            async with asyncio.timeout(4):
                while app.delete_jobs[r['id']]['running']:
                    await asyncio.sleep(.001)
            queue = app.delete_jobs[r['id']]['queue']
            events = []
            while not queue.empty():
                events.append(queue.get_nowait())
            if r.get('retain'):
                for event in events:
                    queue.put_nowait(event)
            return events
        if action == 'invite_clock':
            import invite_api
            self.invite_now, self.invite_waits = 1000.0, []
            async def sleep(seconds):
                self.invite_waits.append(seconds)
                self.invite_now += seconds
                await asyncio.sleep(0)
            fake_asyncio = SimpleNamespace(**{k: getattr(asyncio, k) for k in dir(asyncio) if not k.startswith('__')})
            fake_asyncio.sleep = sleep
            self.patches.enter_context(patch.object(invite_api, 'asyncio', fake_asyncio))
            self.patches.enter_context(patch.object(invite_api, 'time',
                SimpleNamespace(monotonic=lambda: self.invite_now, time=time.time)))
            self.patches.enter_context(patch.object(app, 'time',
                SimpleNamespace(monotonic=lambda: self.invite_now, time=time.time)))
            self.patches.enter_context(patch.object(app, 'asyncio', fake_asyncio))
            return None
        if action == 'invite_waits':
            return self.invite_waits
        if action == 'subscribe':
            subscription = self.owner.subscribe(r['id'])
            snapshot, queue = await subscription.__aenter__()
            self.subscriptions[r['id']] = (subscription, queue)
            return snapshot
        if action == 'report':
            for event in r['events']:
                self.owner.report(r['id'], event)
            return None
        if action == 'subscription_read':
            _, queue = self.subscriptions[r['id']]
            events = []
            while not queue.empty():
                events.append(queue.get_nowait())
            return {'capacity': queue.maxsize, 'events': events}
        if action == 'start_http':
            import socket
            import uvicorn
            listener = socket.socket()
            listener.bind(('127.0.0.1', 0))
            listener.listen(128)
            self.listener = listener
            self.server = uvicorn.Server(uvicorn.Config(app.app, host='127.0.0.1', log_config=None,
                log_level='error', access_log=False, loop='asyncio', lifespan='off', ws='none'))
            self.server_task = asyncio.create_task(self.server.serve(sockets=[listener]))
            async with asyncio.timeout(4):
                while not self.server.started:
                    if self.server_task.done():
                        self.server_task.result()
                    await asyncio.sleep(.001)
            return {'port': listener.getsockname()[1]}
        if action == 'scrape_events':
            return [event async for event in self.owner.events(r['id'])]
        if action == 'patch_media_fetch':
            self.patches.enter_context(patch.object(app, 'cache_media', AsyncMock()))
            return None
        raise ValueError(f'unknown job control: {action}')

    async def close(self):
        if self.server is not None:
            self.server.should_exit = True
            await asyncio.wait_for(self.server_task, 4)
            self.listener.close()
        for _, release in self.barriers.values():
            release.set()
        await app._suspend_collection_jobs()
        await self.shutdown_invites()
        await self.shutdown_profiles()
        for subscription, _ in self.subscriptions.values():
            await subscription.__aexit__(None, None, None)
        self.owner.reset()
        for task in self.tasks.values():
            if not task.done():
                task.cancel()
        await asyncio.gather(*self.tasks.values(), return_exceptions=True)
        await self.client.aclose()
        await self.transport.aclose()
        self.patches.close()
