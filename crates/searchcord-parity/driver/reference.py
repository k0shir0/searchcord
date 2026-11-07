"""Test-only JSONL adapter. No scenarios, expected values or assertions live here.

Rust owns the temporary root and fixtures. Only explicitly enumerated operations
are exposed. Never import/run the old tests or enter the production lifespan.
"""
import asyncio
from contextlib import closing
import json
import os
from pathlib import Path
import socket
import sys
import threading
import time

DRIVER = Path(__file__).resolve().parent
ROOT = Path(sys.argv[1]).resolve()
sys.path.insert(0, str(DRIVER.parent / 'reference'))
sys.path.insert(0, str(DRIVER))
sys.dont_write_bytecode = True

# No inherited archive/credential paths or user-site imports. Rust also sets these.
os.environ['SEARCHCORD_DATA_DIR'] = str(ROOT / 'archive')
os.environ['SEARCHCORD_CREDENTIAL_FILE'] = str(ROOT / 'credentials/synthetic.vault')
os.environ['SEARCHCORD_CREDENTIAL_KEY_FILE'] = str(ROOT / 'credentials/synthetic.key')
os.environ.pop('SEARCHCORD_DB', None)


def offline(event, args):
    # Windows asyncio may use a numeric loopback socket pair for its wakeup pipe.
    if event == 'socket.connect' and isinstance(args[1], tuple) and args[1][0] in {'127.0.0.1', '::1'}:
        return
    if event in {'socket.connect', 'socket.getaddrinfo', 'subprocess.Popen', 'os.system'}:
        raise RuntimeError('network and subprocess access disabled by parity driver')


sys.addaudithook(offline)

import aiosqlite
import credential_store as credentials
import storage

ARCHIVE = ROOT / 'archive/synthetic.db'
server = None
worker = None
listener = None
event_body = ''
job_loop = None
job_thread = None
jobs = None


def archive_path(name):
    path = (ROOT / 'archive' / name).resolve()
    if not path.is_relative_to(ROOT / 'archive'):
        raise ValueError('archive must stay inside the disposable archive directory')
    return path


async def write(request):
    async with aiosqlite.connect(ARCHIVE) as db:
        await db.execute('BEGIN')
        try:
            if request['op'] == 'save':
                result = await storage.save_message_payloads(
                    db, request['messages'], request['channel_id'], request['channel_name'],
                    request['guild_id'], request['guild_name'])
            else:
                result = await storage.mark_messages_deleted(
                    db, request['ids'], source=request.get('source', 'confirmed_delete'))
            if request.get('rollback'):
                await db.rollback()
            else:
                await db.commit()
            return result
        except BaseException:
            await db.rollback()
            raise


def start_viewer(request):
    global server, worker, listener
    if server is not None:
        raise ValueError('viewer already running')
    import archive_reader
    import uvicorn
    from fastapi.responses import Response
    from search_app import create_app
    if request.get('window') is not None:
        archive_reader.WINDOW = request['window']
    app = create_app(archive_path(request['archive']), request['immutable'])

    # Finite transport fixture only; production SSE semantics belong to later chunks.
    @app.get('/__parity/events', include_in_schema=False)
    def events():
        return Response(event_body, media_type='text/event-stream')
    app.router.routes.insert(0, app.router.routes.pop())

    listener = socket.socket()
    listener.bind(('127.0.0.1', 0))
    listener.listen(128)
    server = uvicorn.Server(uvicorn.Config(app, host='127.0.0.1', log_config=None,
        log_level='error', access_log=False, loop='asyncio', lifespan='off', ws='none'))
    worker = threading.Thread(target=server.run, kwargs={'sockets': [listener]}, daemon=True)
    worker.start()
    deadline = time.monotonic() + 10
    while not server.started:
        if not worker.is_alive() or time.monotonic() >= deadline:
            raise RuntimeError('viewer failed to start')
        time.sleep(.01)
    return {'port': listener.getsockname()[1]}


def dispatch(request):
    global event_body, job_loop, job_thread, jobs
    op = request['op']
    if op == 'permissions':
        from channel_access import permissions_for
        return permissions_for(request['guild'], request['member'], request['channel'], request['channels'])
    if op == 'job':
        if jobs is None:
            from jobs import Jobs
            job_loop = asyncio.new_event_loop()
            job_thread = threading.Thread(target=job_loop.run_forever, daemon=True)
            job_thread.start()
            jobs = Jobs(ARCHIVE)
            asyncio.run_coroutine_threadsafe(jobs.setup(), job_loop).result(timeout=10)
            jobs.hold = False
        return asyncio.run_coroutine_threadsafe(jobs.invoke(request), job_loop).result(timeout=10)
    if op == 'credential_key':
        import hashlib
        return hashlib.sha256(os.path.normcase(str(ARCHIVE.resolve())).encode()).hexdigest()[:24]
    if op == 'init':
        return storage.init_database(str(ARCHIVE))
    if op in {'save', 'delete'}:
        return asyncio.run(write(request))
    if op == 'read_message':
        return storage.read_message_archive(ARCHIVE, request['id'],
            limit=request.get('limit', 100), after_revision=request.get('after_revision', 0))
    if op == 'backup':
        import sqlite3
        with closing(sqlite3.connect(ARCHIVE)) as db:
            db.execute('BEGIN IMMEDIATE')
            try:
                return storage._backup(ARCHIVE)
            finally:
                db.rollback()
    if op in {'load_credentials', 'save_credentials', 'clear_credentials'}:
        from unittest.mock import patch
        path = archive_path(request.get('archive', 'synthetic.db'))
        vault = archive_path(request['vault_in_archive']) if 'vault_in_archive' in request else ROOT / 'credentials/synthetic.vault'
        with patch.dict(os.environ, {'SEARCHCORD_CREDENTIAL_FILE': str(vault)}):
            from contextlib import nullcontext
            with patch.object(credentials, '_USE_DPAPI', False) if request.get('portable') else nullcontext():
                if op == 'load_credentials':
                    return credentials.load_credentials(path)
                if op == 'save_credentials':
                    return credentials.save_credentials(path, request['tokens'], request['active_token_id'])
                return credentials.clear_credentials(path)
    if op == 'start_viewer':
        return start_viewer(request)
    if op == 'archive_read':
        if 'window' in request:
            import archive_reader
            archive_reader.WINDOW = request['window']
        from archive_reader import Archive
        archive = Archive(archive_path(request.get('archive', 'synthetic.db')), request.get('immutable', False))
        if request['method'] not in {'search', 'profile', 'profile_servers'}:
            raise ValueError('unsupported archive method')
        return getattr(archive, request['method'])(**request.get('args', {}))
    if op == 'snapshot':
        from contextlib import ExitStack
        from unittest.mock import patch
        import search_snapshot
        with ExitStack() as patches:
            destination = archive_path(request['destination'])
            if request.get('fail_compress_at'):
                original_compress = search_snapshot.zlib.compress
                calls = 0
                def compress(*args):
                    nonlocal calls
                    calls += 1
                    if calls == request['fail_compress_at']:
                        raise OSError('injected compression failure')
                    return original_compress(*args)
                patches.enter_context(patch.object(search_snapshot.zlib, 'compress', compress))
            if request.get('race'):
                original_link = search_snapshot.os.link
                def link(source, target):
                    Path(target).write_bytes(b'concurrent exporter')
                    return original_link(source, target)
                patches.enter_context(patch.object(search_snapshot.os, 'link', link))
            return search_snapshot.build_snapshot(archive_path(request.get('source', 'synthetic.db')),
                destination, block_rows=request.get('block_rows', search_snapshot.BLOCK_ROWS))
    if op == 'export_media':
        from search_snapshot import export_media
        return export_media(archive_path(request.get('source', 'synthetic.db')), archive_path(request['destination']))
    if op == 'snapshot_cli':
        from contextlib import redirect_stdout, redirect_stderr
        from unittest.mock import patch
        from io import StringIO
        import search_snapshot
        args = ['search_snapshot.py', str(archive_path(request.get('source', 'synthetic.db'))),
                str(archive_path(request['destination']))]
        if 'report' in request:
            args += ['--report', str(archive_path(request['report']))]
        if request.get('with_media'):
            args += ['--with-media']
        output, errors, code = StringIO(), StringIO(), 0
        with patch.object(sys, 'argv', args), redirect_stdout(output), redirect_stderr(errors):
            try:
                search_snapshot.main()
            except SystemExit as exc:
                code = exc.code
        return {'code': code, 'stdout': output.getvalue(), 'stderr': errors.getvalue()}
    if op == 'media_key':
        from media_store import media_key
        return media_key(request['url'])
    if op == 'presentation':
        from profile_store import search_label
        from media_store import canonical_url, media_status
        if request['method'] == 'search_label':
            return search_label(request['value'])
        if request['method'] == 'canonical_url':
            return canonical_url(request['value'])
        if request['method'] == 'media_status':
            return media_status(ARCHIVE, request['value'], verify=request.get('verify', False))
        raise ValueError('unsupported presentation operation')
    if op == 'process_metrics':
        import resource
        return {'peak_rss_bytes': resource.getrusage(resource.RUSAGE_SELF).ru_maxrss * (1 if sys.platform == 'darwin' else 1024)}
    if op == 'readonly_execute':
        from archive_reader import Archive
        failure = None
        with Archive(ARCHIVE, True).connect() as db:
            # Capture at the connection boundary, like an assertion inside the
            # caller's context. Archive translates errors that escape its body.
            try:
                result = db.execute(request['sql']).fetchall()
            except Exception as exc:
                failure = exc
        if failure is not None:
            raise failure
        return result
    if op == 'sse_fixture':
        event_body = request['body']
        return None
    if op == 'network_probe':
        if request.get('dns'):
            return socket.getaddrinfo('discord.com', 443)
        with socket.socket() as probe:
            return probe.connect(('162.159.135.232', 443))
    raise ValueError(f'unknown adapter operation: {op}')


print(json.dumps({'ready': True, 'protocol': 1}), flush=True)
try:
    for line in sys.stdin:
        request = json.loads(line)
        if request.get('op') == 'shutdown':
            break
        try:
            from contextlib import redirect_stdout
            with redirect_stdout(sys.stderr):
                result = {'ok': True, 'result': dispatch(request)}
        except Exception as exc:
            result = {'ok': False, 'error': {'type': type(exc).__name__, 'message': str(exc)}}
            if exc.__cause__ is not None:
                result['error']['cause'] = str(exc.__cause__)
        print(json.dumps(result, ensure_ascii=False), flush=True)
finally:
    if jobs is not None:
        asyncio.run_coroutine_threadsafe(jobs.close(), job_loop).result(timeout=10)
        job_loop.call_soon_threadsafe(job_loop.stop)
        job_thread.join(timeout=2)
        job_loop.close()
    if server is not None:
        server.should_exit = True
        worker.join(timeout=2)
        listener.close()
