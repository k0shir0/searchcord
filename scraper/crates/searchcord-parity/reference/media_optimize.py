"""Audit, compact and benchmark a database's sibling media directory.

All commands are offline. Optimization is a dry run unless --apply is supplied;
reclaiming original binaries additionally requires --remove-originals.
"""
import argparse
from collections import Counter
import hashlib
import io
import json
from pathlib import Path
import re
import tempfile
import time
import warnings

from media_store import (INLINE_TYPES, MAX_BYTES, _record, _saved_file,
                         media_binary, media_directory, media_kind)

MAX_PIXELS = 32_000_000
EDGES = {'avatar': 128, 'banner': 1024, 'attachment': 1600}
QUALITY = 80
OWNED_FILE = re.compile(r'[a-f0-9]{64}(?:\.[a-f0-9]{64})?\.(?:bin|json|webp)')


def entries(directory):
    if directory.is_symlink():
        raise OSError('Media directory cannot be a symbolic link')
    if not directory.exists():
        return
    for manifest in directory.iterdir():
        if not re.fullmatch(r'[a-f0-9]{64}\.json', manifest.name):
            continue
        try:
            if manifest.is_symlink():
                raise ValueError('Unsafe manifest')
            data = json.loads(manifest.read_text(encoding='utf-8'))
            if not isinstance(data, dict):
                raise ValueError('Invalid manifest')
        except (OSError, ValueError):
            data = {'status': 'invalid'}
        yield manifest.stem, data


def audit(path):
    directory = media_directory(path)
    report = {'objects': 0, 'active_bytes': 0, 'disk_bytes': 0,
              'invalid': 0, 'unavailable': 0, 'by_kind': {},
              'by_content_type': {}, 'exact_duplicate_bytes': 0}
    digests = set()
    for key, data in entries(directory):
        report['objects'] += 1
        if data.get('status') != 'saved':
            report['invalid' if data.get('status') == 'invalid' else 'unavailable'] += 1
            continue
        try:
            if not _saved_file(directory, key, data):
                raise ValueError('Damaged file')
        except (OSError, ValueError, TypeError):
            report['invalid'] += 1
            continue
        size = data['bytes']
        report['active_bytes'] += size
        kind = media_kind(data.get('source_url'))
        report['by_kind'][kind] = report['by_kind'].get(kind, 0) + size
        content_type = data.get('content_type') or 'unknown'
        report['by_content_type'][content_type] = report['by_content_type'].get(content_type, 0) + size
        # This is an estimate from recorded digests, not a fresh content scan.
        if data.get('sha256') in digests:
            report['exact_duplicate_bytes'] += size
        digests.add(data.get('sha256'))
    if directory.exists():
        report['disk_bytes'] = sum(file.stat().st_size for file in directory.iterdir()
                                   if OWNED_FILE.fullmatch(file.name) and not file.is_symlink())
    return report


def webp_bytes(binary, kind):
    """Keep animation and unsupported/oversized images intact, never upscale."""
    from PIL import Image, ImageOps, UnidentifiedImageError
    try:
        with warnings.catch_warnings():
            warnings.simplefilter('error', Image.DecompressionBombWarning)
            with Image.open(binary) as image:
                if image.width * image.height > MAX_PIXELS:
                    return None, 'pixel_limit'
                if getattr(image, 'is_animated', False) or getattr(image, 'n_frames', 1) > 1:
                    return None, 'animated'
                if image.mode not in ('RGB', 'RGBA', 'P', 'L', 'LA', '1'):
                    return None, 'unsupported_color_mode'
                icc = image.info.get('icc_profile', b'')
                image = ImageOps.exif_transpose(image)
                image.thumbnail((EDGES[kind], EDGES[kind]), Image.Resampling.LANCZOS,
                                reducing_gap=3.0)
                alpha = 'A' in image.getbands() or 'transparency' in image.info
                image = image.convert('RGBA' if alpha else 'RGB')
                output = io.BytesIO()
                image.save(output, 'WEBP', quality=QUALITY, method=4, icc_profile=icc)
                encoded = output.getvalue()
                # Decode the completed representation before it may be published.
                with Image.open(io.BytesIO(encoded)) as check:
                    check.load()
                return encoded, None
    except (OSError, ValueError, UnidentifiedImageError,
            Image.DecompressionBombError, Image.DecompressionBombWarning):
        return None, 'unsupported_or_damaged_image'


def remove_original(directory, key, data):
    """Only retire the exact input whose digest was saved before conversion."""
    original = directory / (key + '.bin')
    if not original.exists() or original.is_symlink():
        return 0
    if original.stat().st_size != data.get('original_bytes'):
        return 0
    with original.open('rb') as source:
        if hashlib.file_digest(source, 'sha256').hexdigest() != data.get('original_sha256'):
            return 0
    size = original.stat().st_size
    original.unlink()
    return size


def optimize_one(directory, key, data, *, apply=False, remove_originals=False):
    """Publish a verified immutable binary, then its manifest, then retire input.

    An interruption before manifest publication leaves the original usable.
    After publication, the new representation is independently verifiable.
    """
    if not re.fullmatch('[a-f0-9]{64}', key):
        raise ValueError('Invalid media key')
    result = {'status': 'skipped', 'before_bytes': 0, 'after_bytes': 0,
              'retired_bytes': 0}
    if data.get('status') != 'saved':
        return dict(result, reason='not_saved')
    if not _saved_file(directory, key, data, verify=True, fresh=True):
        raise ValueError('Media digest mismatch')
    size = data['bytes']
    result.update(before_bytes=size, after_bytes=size)
    if data.get('storage_file'):
        if apply and remove_originals:
            result['retired_bytes'] = remove_original(directory, key, data)
        return dict(result, reason='already_compact')
    if size > MAX_BYTES or data.get('content_type') not in INLINE_TYPES:
        return dict(result, reason='non_image_or_size_limit')
    binary = media_binary(directory, key, data)
    encoded, reason = webp_bytes(binary, media_kind(data.get('source_url')))
    if encoded is None:
        return dict(result, reason=reason)
    if len(encoded) >= size:
        return dict(result, reason='not_smaller')
    result.update(status='converted' if apply else 'would_convert', after_bytes=len(encoded))
    if not apply:
        return result
    digest = hashlib.sha256(encoded).hexdigest()
    target = directory / (key + '.' + digest + '.webp')
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(dir=directory, prefix='.searchcord-media-',
                                         suffix='.tmp', delete=False) as output:
            temporary = Path(output.name)
            output.write(encoded)
        temporary.replace(target)
        updated = dict(data, storage_file=target.name, bytes=len(encoded), sha256=digest,
                       content_type='image/webp', original_bytes=size,
                       original_sha256=data['sha256'], original_content_type=data.get('content_type'),
                       encoding={'codec': 'webp', 'quality': QUALITY,
                                 'max_edge': EDGES[media_kind(data.get('source_url'))]})
        if not _saved_file(directory, key, updated, verify=True, fresh=True):
            raise OSError('Compressed media digest mismatch')
        _record(directory, key, updated)
        if remove_originals:
            result['retired_bytes'] = remove_original(directory, key, updated)
    finally:
        if temporary:
            temporary.unlink(missing_ok=True)
    return result


def optimize(path, *, apply=False, remove_originals=False, limit=0):
    directory = media_directory(path)
    report = {'apply': apply, 'remove_originals': remove_originals, 'processed': 0,
              'before_bytes': 0, 'after_bytes': 0, 'retired_bytes': 0, 'results': Counter()}
    started = time.perf_counter()
    report['disk_bytes_before'] = audit(path)['disk_bytes']
    for key, data in entries(directory):
        if limit and report['processed'] >= limit:
            break
        try:
            result = optimize_one(directory, key, data, apply=apply, remove_originals=remove_originals)
        except (OSError, ValueError, TypeError):
            result = {'status': 'error', 'reason': 'invalid_or_unwritable'}
        report['processed'] += 1
        report['results'][result.get('reason') or result['status']] += 1
        for name in ('before_bytes', 'after_bytes', 'retired_bytes'):
            report[name] += result.get(name, 0)
    report['active_saving_percent'] = round(
        100 * (1 - report['after_bytes'] / report['before_bytes']), 2) if report['before_bytes'] else 0
    report['seconds'] = round(time.perf_counter() - started, 3)
    report['disk_bytes_after'] = audit(path)['disk_bytes']
    return report


def benchmark(path, limit=200):
    """Measure sampled read+digest+decode, excluding browser/network/rendering.

    Deterministic hash selection samples the full manifest list. OS caches are
    not flushed; the result must not be described as a cold disk benchmark.
    """
    import heapq
    from PIL import Image
    directory = media_directory(path)
    samples = heapq.nsmallest(limit, entries(directory), key=lambda item: item[0])
    timings = []
    skipped = 0
    for key, data in samples:
        if data.get('status') != 'saved' or data.get('content_type') not in INLINE_TYPES:
            skipped += 1
            continue
        started = time.perf_counter()
        try:
            if not _saved_file(directory, key, data, verify=True, fresh=True):
                raise ValueError('Digest mismatch')
            binary = media_binary(directory, key, data)
            if binary.stat().st_size > MAX_BYTES:
                raise ValueError('Size limit')
            with Image.open(binary) as image:
                if image.width * image.height > MAX_PIXELS:
                    raise ValueError('Pixel limit')
                image.load()
        except (OSError, ValueError, Image.DecompressionBombError):
            skipped += 1
            continue
        timings.append((time.perf_counter() - started) * 1000)
    timings.sort()
    def percentile(fraction):
        if not timings:
            return None
        position = (len(timings) - 1) * fraction
        lower = int(position)
        upper = min(lower + 1, len(timings) - 1)
        return round(timings[lower] + (timings[upper] - timings[lower]) * (position - lower), 3)
    return {'sampled': len(samples), 'decoded': len(timings), 'skipped': skipped,
            'p50_ms': percentile(.5), 'p95_ms': percentile(.95), 'max_ms': percentile(1),
            'over_300ms': sum(value >= 300 for value in timings),
            'scope': 'read + uncached SHA-256 verification + first-frame Pillow decode; OS cache uncontrolled; excludes browser/network'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('database', type=Path, help='Archive DB path (database is never opened or changed)')
    commands = parser.add_subparsers(dest='command', required=True)
    commands.add_parser('audit', help='Report disk usage by media kind and recorded content type')
    compact = commands.add_parser('optimize', help='Estimate WebP savings; stop collection before --apply')
    compact.add_argument('--apply', action='store_true')
    compact.add_argument('--remove-originals', action='store_true', help='Retire verified input binaries after publication; requires --apply and a backup')
    compact.add_argument('--limit', type=int, default=0, help='Process at most N manifests; zero means all')
    bench = commands.add_parser('benchmark', help='Measure local read+digest+decode, excluding the browser')
    bench.add_argument('--limit', type=int, default=200)
    args = parser.parse_args()
    if not args.database.is_file():
        parser.error('Choose an existing database file')
    if getattr(args, 'limit', 1) < 0 or (args.command == 'benchmark' and args.limit == 0):
        parser.error('--limit must be positive (or zero for an unlimited optimize run)')
    if args.command == 'optimize' and args.remove_originals and not args.apply:
        parser.error('--remove-originals requires --apply')
    if args.command == 'audit':
        report = audit(args.database)
    elif args.command == 'optimize':
        report = optimize(args.database, apply=args.apply, remove_originals=args.remove_originals, limit=args.limit)
    else:
        report = benchmark(args.database, limit=args.limit)
    print(json.dumps(report, indent=2))
    return 1 if report.get('results', {}).get('invalid_or_unwritable') else 0


if __name__ == '__main__':
    raise SystemExit(main())
