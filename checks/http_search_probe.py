"""Measure local search HTTP responses without emitting archived values."""
import argparse
from concurrent.futures import ThreadPoolExecutor
import json
from pathlib import Path
import statistics
import time
from urllib.parse import urlsplit

import httpx


def request(client, query):
    started = time.perf_counter()
    response = client.get('/api/search', params={'q': query, 'limit': 50})
    response.raise_for_status()
    result = response.json()
    messages = result['messages']
    assert len(messages) <= 50
    assert [int(m['id']) for m in messages] == sorted(
        (int(m['id']) for m in messages), reverse=True)
    return {
        'http_ms': round((time.perf_counter() - started) * 1000, 2),
        'search_ms': result['elapsed_ms'],
        'returned': len(messages),
        'encoding': response.headers.get('content-encoding'),
        'wire_bytes': int(response.headers['content-length']),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('base')
    parser.add_argument('--report', type=Path)
    args = parser.parse_args()
    if urlsplit(args.base).hostname not in ('localhost', '127.0.0.1', '::1'):
        parser.error('Use a local preview server.')
    report = {'rounds': 5, 'page_size': 50, 'queries': []}
    with httpx.Client(base_url=args.base, timeout=10) as client:
        for label, query in (
            ('browse', ''), ('common', 'hello'), ('very_common', 'the'),
            ('phrase', 'release notes'), ('absent', 'zzzzunlikelymatch'),
            ('literal_wildcards', '%_'), ('short', 'he'),
        ):
            samples = [request(client, query) for _ in range(5)]
            item = {
                'class': label,
                'first_http_ms': samples[0]['http_ms'],
                'http_median_ms': statistics.median(s['http_ms'] for s in samples),
                'http_max_ms': max(s['http_ms'] for s in samples),
                **{k: samples[-1][k] for k in ('returned', 'encoding', 'wire_bytes')},
            }
            report['queries'].append(item)
            print(json.dumps(item), flush=True)
        started = time.perf_counter()
        with ThreadPoolExecutor(max_workers=10) as pool:
            concurrent = list(pool.map(lambda _: request(client, 'hello'), range(10)))
        report['concurrent'] = {
            'requests': 10,
            'all_successful': True,
            'batch_ms': round((time.perf_counter() - started) * 1000, 2),
            'http_median_ms': statistics.median(s['http_ms'] for s in concurrent),
            'http_max_ms': max(s['http_ms'] for s in concurrent),
        }
        print(json.dumps(report['concurrent']), flush=True)
    if args.report:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(json.dumps(report, indent=2) + '\n', encoding='utf-8')


if __name__ == '__main__':
    main()
