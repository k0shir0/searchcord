"""Offline benchmark worker; Rust generates inputs and verifies all outputs."""
import asyncio
import json
import os
from pathlib import Path
import resource
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))


def offline(event, args):
    if event == 'subprocess.Popen' and sys.argv[1] in {'native-snapshot', 'native-save'} and args[0] == sys.argv[4]:
        return
    if event in {'socket.connect', 'socket.getaddrinfo', 'subprocess.Popen', 'os.system'}:
        raise RuntimeError('benchmark workers are offline')


sys.addaudithook(offline)
operation, source, destination = sys.argv[1:4]
if operation == 'native-snapshot':
    import subprocess
    report = str(destination) + '.report.json'
    started = time.perf_counter()
    completed = subprocess.run([sys.argv[4], source, destination, '--report', report], capture_output=True)
    if completed.returncode:
        raise RuntimeError(completed.stderr.decode('utf-8'))
    result = json.loads(Path(report).read_text(encoding='utf-8'))
    if result.get('snapshot_pages') is None:
        import sqlite3
        with sqlite3.connect(Path(destination).resolve().as_uri() + '?mode=ro&immutable=1', uri=True) as db:
            try:
                result['snapshot_pages'] = dict(db.execute('SELECT name,SUM(pgsize) FROM dbstat GROUP BY name'))
            except sqlite3.OperationalError:
                pass
elif operation == 'snapshot':
    from search_snapshot import build_snapshot
    started = time.perf_counter()
    result = build_snapshot(source, destination)
elif operation == 'native-save':
    import subprocess
    started = time.perf_counter()
    completed = subprocess.run([sys.argv[4], source, destination], capture_output=True)
    if completed.returncode:
        raise RuntimeError(completed.stderr.decode('utf-8'))
    measured = json.loads(completed.stdout)
    result = measured['result']
elif operation == 'save':
    import app
    import aiosqlite
    messages = json.loads(Path(destination).read_text(encoding='utf-8'))
    async def save():
        count = 0
        async with aiosqlite.connect(source) as db:
            for start in range(0, len(messages), 100):
                count += await app._save_messages(db, messages[start:start+100], '20', 'Fixture', '10', 'Fixture')
        return {'messages': count}
    started = time.perf_counter()
    result = asyncio.run(save())
else:
    raise ValueError('unsupported benchmark operation')
elapsed = measured['seconds'] if operation == 'native-save' else time.perf_counter() - started
if operation in {'snapshot', 'native-snapshot'}:
    import hashlib
    import sqlite3
    import zlib
    with sqlite3.connect(Path(destination).resolve().as_uri() + '?mode=ro&immutable=1', uri=True) as db:
        count, total, smallest, largest = db.execute('SELECT COUNT(*),SUM(length(payload)),MIN(length(payload)),MAX(length(payload)) FROM message_blocks').fetchone()
        first = db.execute('SELECT payload FROM message_blocks ORDER BY block LIMIT 1').fetchone()
        raw = zlib.decompress(first[0]) if first else b''
        result['snapshot_blocks'] = {'count': count, 'bytes': total, 'min_bytes': smallest, 'max_bytes': largest,
            'first_raw_sha256': hashlib.sha256(raw).hexdigest(), 'first_recompressed_bytes': len(zlib.compress(raw, 9))}
print(json.dumps({'seconds': elapsed, 'result': result,
    'peak_rss_bytes': resource.getrusage(resource.RUSAGE_CHILDREN if operation.startswith('native-') else resource.RUSAGE_SELF).ru_maxrss * (1 if sys.platform == 'darwin' else 1024),
    'python': sys.version.split()[0]}))
