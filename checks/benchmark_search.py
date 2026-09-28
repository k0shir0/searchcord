"""Aggregate-only benchmark of a closed v9 archive; never prints message values.

python checks/benchmark_search.py /path/to/searchcord.db --rounds 5
"""
import argparse
from contextlib import closing
import json
from pathlib import Path
import sqlite3
import statistics
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from search_app import Archive


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('database', type=Path)
    parser.add_argument('--rounds', type=int, default=5)
    args = parser.parse_args()
    if not 1 <= args.rounds <= 20:
        parser.error('rounds must be between 1 and 20')
    path = args.database.resolve()
    archive = Archive(path, True)
    before = (path.stat().st_size, path.stat().st_mtime_ns)
    report = {'archive_bytes': before[0], 'queries': []}
    with archive.connect() as db:
        report['messages'] = db.execute("SELECT count FROM stats_counts WHERE kind='total' AND key=''").fetchone()[0]
    for query in ['', 'hello', 'the', 'release notes', 'zzzzunlikelymatch']:
        old, new = [], []
        for _ in range(args.rounds):
            # This reproduces the old candidate/sort path with the same 40 rows,
            # even omitting its separate total count and row hydration work.
            with closing(sqlite3.connect(path.as_uri()+'?mode=ro&immutable=1',uri=True)) as db:
                started = time.perf_counter()
                where = 'WHERE rowid IN (SELECT rowid FROM messages_fts WHERE content LIKE ?)' if query else ''
                reference = [str(r[0]) for r in db.execute(f'SELECT id FROM messages {where} ORDER BY id DESC LIMIT 40', ['%'+query+'%'] if query else [])]
                old.append((time.perf_counter()-started)*1000)
            result = archive.search(q=query)
            new.append(result['elapsed_ms'])
            assert [m['id'] for m in result['messages']] == reference, 'Result ordering differs from the reference query'
        report['queries'].append({'query':query or '(browse)', 'old_candidate_median_ms':round(statistics.median(old),1),
                                  'new_full_search_median_ms':round(statistics.median(new),1),
                                  'new_full_search_max_ms':max(new),'returned':len(reference),'same_ids':True})
    report['archive_unchanged'] = before == (path.stat().st_size,path.stat().st_mtime_ns)
    assert report['archive_unchanged']
    print(json.dumps(report,indent=2))


if __name__ == '__main__':
    main()
