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
    if event == 'subprocess.Popen' and sys.argv[1] == 'native-snapshot' and args[0] == sys.argv[4]:
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
elif operation == 'snapshot':
    from search_snapshot import build_snapshot
    started = time.perf_counter()
    result = build_snapshot(source, destination)
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
print(json.dumps({'seconds': time.perf_counter() - started, 'result': result,
    'peak_rss_bytes': resource.getrusage(resource.RUSAGE_CHILDREN if operation == 'native-snapshot' else resource.RUSAGE_SELF).ru_maxrss * (1 if sys.platform == 'darwin' else 1024),
    'python': sys.version.split()[0]}))
