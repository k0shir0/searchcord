"""Aggregate-only read benchmark and exact result comparison, including live WAL."""
import argparse
from datetime import datetime, timedelta, timezone
import json
from pathlib import Path
import statistics
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from fastapi import HTTPException
from search_app import Archive


def compare(label, params, source, snapshot, rounds):
    item = {'class': label, 'source_ms': [], 'snapshot_ms': [],
            'same_fields': None, 'timeouts': {}}
    for _ in range(rounds):
        results = {}
        for name, archive in (('source', source), ('snapshot', snapshot)):
            started = time.perf_counter()
            try:
                results[name] = archive.search(**params)
            except HTTPException as exc:
                failures = item['timeouts'].setdefault(name, [])
                failures.append(exc.status_code)
                continue
            item[name + '_ms'].append(round((time.perf_counter() - started) * 1000, 2))
        if len(results) == 2:
            item['same_fields'] = results['source']['messages'] == results['snapshot']['messages']
            if not item['same_fields']:
                raise RuntimeError('Result fields differ for ' + label)
            assert results['source']['has_more'] == results['snapshot']['has_more']
            assert results['source']['next_cursor'] == results['snapshot']['next_cursor']
            item['returned'] = len(results['snapshot']['messages'])
    for name in ('source', 'snapshot'):
        values = item[name + '_ms']
        if values:
            item[name + '_median_ms'] = round(statistics.median(values), 2)
            item[name + '_max_ms'] = max(values)
    return item


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source', type=Path)
    parser.add_argument('snapshot', type=Path)
    parser.add_argument('--rounds', type=int, default=5)
    parser.add_argument('--report', type=Path)
    args = parser.parse_args()
    if not 1 <= args.rounds <= 20:
        parser.error('rounds must be between 1 and 20')
    if args.report and args.report.resolve() in (args.source.resolve(), args.snapshot.resolve()):
        parser.error('The report must be separate from both archive files')
    source, snapshot = Archive(args.source, False), Archive(args.snapshot, True)
    with source.connect() as db:
        chosen = {kind: db.execute(
            'SELECT key FROM stats_counts WHERE kind=? AND key!=? ORDER BY count DESC LIMIT 1',
            (kind, '')).fetchone() for kind in ('guild', 'channel', 'author')}
        newest = db.execute('SELECT id FROM messages ORDER BY id DESC LIMIT 1').fetchone()
    if newest is None or any(value is None for value in chosen.values()):
        parser.error('Benchmark needs a nonempty archive with server, channel and author counts.')
    last_day = datetime.fromtimestamp(((newest[0] >> 22) + 1420070400000) / 1000, timezone.utc).date()
    scenarios = [
        ('browse', {}), ('common', {'q': 'hello'}), ('very_common', {'q': 'the'}),
        ('phrase', {'q': 'release notes'}), ('absent', {'q': 'zzzzunlikelymatch'}),
        ('literal_wildcards', {'q': '%_'}), ('short', {'q': 'he'}),
        ('server_text', {'q': 'hello', 'guild_id': chosen['guild'][0]}),
        ('channel_text', {'q': 'hello', 'channel_id': chosen['channel'][0]}),
        ('author', {'author_id': chosen['author'][0]}),
        ('date_text', {'q': 'hello', 'date_from': str(last_day - timedelta(days=7)),
                       'date_to': str(last_day)}),
    ]
    report = {
        'rounds': args.rounds, 'page_size': 40,
        'cache': 'fresh SQLite connections, bounded decoded-block cache, OS cache not flushed',
        'queries': [],
    }
    for label, params in scenarios:
        item = compare(label, params, source, snapshot, args.rounds)
        report['queries'].append(item)
        print(json.dumps(item), flush=True)
    cursor, pages_traversed = None, 0
    started = time.perf_counter()
    for _ in range(100):
        result = snapshot.search(q='hello', before=int(cursor) if cursor else None)
        pages_traversed += 1
        if not result['has_more']:
            break
        cursor = result['next_cursor']
    boundary = int(cursor) if cursor else None
    deep_source = source.search(q='hello', before=boundary)
    deep_snapshot = snapshot.search(q='hello', before=boundary)
    if deep_source['messages'] != deep_snapshot['messages']:
        raise RuntimeError('Deep page differs')
    report['deep_page'] = {
        'pages_traversed': pages_traversed, 'same_fields': True,
        'snapshot_ms': deep_snapshot['elapsed_ms'], 'source_ms': deep_source['elapsed_ms'],
        'traversal_seconds': round(time.perf_counter() - started, 2),
    }
    if args.report:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(json.dumps(report, indent=2) + '\n', encoding='utf-8')
    print(json.dumps(report['deep_page']), flush=True)
    if any(item['timeouts'] for item in report['queries']):
        raise SystemExit('Some benchmark requests failed; inspect the aggregate report.')


if __name__ == '__main__':
    main()
