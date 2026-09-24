"""Audit local Git objects without printing secret values or archived content.

Includes unreachable objects, refs, and reflogs. Optional --database inputs are
opened immutable/read-only to compare known saved credentials, never displayed.
This is a targeted local audit, not a guarantee about unavailable remote clones.
"""
import argparse
import json
import re
import sqlite3
import subprocess
from contextlib import closing
from pathlib import Path


def contains_messages(value):
    if isinstance(value, dict):
        if 'content' in value and any(key in value for key in ('author_id', 'author', 'role')):
            return True
        return any(contains_messages(child) for child in value.values())
    if isinstance(value, list):
        return any(contains_messages(child) for child in value)
    return False


def audit(databases=()):
    known = []
    for item in databases:
        path = Path(item).resolve(strict=True)
        wal = Path(str(path) + '-wal')
        if wal.exists() and wal.stat().st_size:
            raise RuntimeError('Refusing immutable credential comparison with an active WAL')
        with closing(sqlite3.connect(path.as_uri() + '?mode=ro&immutable=1', uri=True)) as db:
            for (value,) in db.execute("SELECT value FROM settings WHERE key='token'"):
                if value and len(value) > 20:
                    known.append(value.encode())
    paths = {}
    for line in subprocess.check_output(['git', 'rev-list', '--objects', '--all', '--reflog'], text=True).splitlines():
        oid, _, name = line.partition(' ')
        if name:
            paths.setdefault(oid, []).append(name)
    objects = subprocess.check_output(['git', 'cat-file', '--batch-all-objects', '--batch-check=%(objectname) %(objecttype) %(objectsize)'], text=True).splitlines()
    patterns = {
        'discord credential pattern': rb'(?<![\w-])(?:mfa\.[\w-]{70,}|[A-Za-z0-9_-]{23,28}\.[A-Za-z0-9_-]{6}\.[A-Za-z0-9_-]{27,})(?![\w-])',
        'github credential pattern': rb'\b(?:gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{60,})\b',
        'private key': rb'-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----',
    }
    findings = []
    blobs = commits = 0
    for info in objects:
        oid, kind, size = info.split()
        if kind not in ('blob', 'commit', 'tag'):
            continue
        payload = subprocess.check_output(['git', 'cat-file', kind, oid])
        blobs += kind == 'blob'
        commits += kind == 'commit'
        reasons = []
        if any(secret in payload for secret in known):
            reasons.append('matches a locally saved credential')
        for label, pattern in patterns.items():
            if re.search(pattern, payload):
                reasons.append(label)
        if payload.startswith(b'SQLite format 3\x00'):
            reasons.append('SQLite database object')
        names = paths.get(oid, [])
        for name in names:
            if re.search(r'(^|/)(data|\.personal)/|\.(?:db|sqlite3?)(?:-wal|-shm|-journal)?$|\.jsonl$|(^|/)(?:token\.txt|\.env(?:\.(?!example$).+)?)$', name, re.I):
                reasons.append('private-data path')
        # Inspect structure even when an unreachable object has no recoverable
        # filename. JSONL and wrapped Discord/ChatML exports are also covered.
        if kind == 'blob' and payload.lstrip().startswith((b'{', b'[')):
            try:
                records = [json.loads(payload)]
            except (ValueError, UnicodeDecodeError):
                records = []
                for line in payload.splitlines():
                    try:
                        records.append(json.loads(line))
                    except (ValueError, UnicodeDecodeError):
                        continue
            if any(contains_messages(record) for record in records):
                reasons.append('message export structure')
        if reasons:
            findings.append({'object': oid, 'paths': names, 'reasons': sorted(set(reasons))})
    return {'objects': len(objects), 'blobs': blobs, 'commits': commits,
            'known_credentials_compared': len(known), 'findings': findings,
            'scope': 'all local Git objects, reachable and unreachable; no remote mutation'}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--database', action='append', default=[])
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    report = audit(args.database)
    encoded = json.dumps(report, indent=2)
    if args.output:
        args.output.write_text(encoded, encoding='utf-8')
    print(encoded)
    raise SystemExit(bool(report['findings']))
