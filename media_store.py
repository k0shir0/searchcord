"""Bounded, credential-free copies of Discord media beside the archive."""
import asyncio
from datetime import datetime, timezone
from functools import lru_cache
import hashlib
import json
from pathlib import Path
import re
import shutil
import tempfile
import time
from urllib.parse import parse_qsl, urlencode, urlsplit, urlunsplit

import httpx
from fastapi import APIRouter, HTTPException
from fastapi.responses import FileResponse

MAX_BYTES = 25 * 1024 * 1024
FILE_SECONDS = 15
BATCH_SECONDS = 30
PARALLEL_DOWNLOADS = 4
HOSTS = frozenset({'cdn.discordapp.com', 'media.discordapp.net'})
INLINE_TYPES = frozenset({'image/png', 'image/jpeg', 'image/gif', 'image/webp', 'image/avif'})


def canonical_url(url):
    """Only Discord's own HTTPS hosts; signatures do not identify the object."""
    if not isinstance(url, str) or len(url) > 8192 or any(ord(char) < 32 for char in url):
        return None
    try:
        parts = urlsplit(url)
        if (parts.scheme != 'https' or parts.hostname not in HOSTS or parts.port not in (None, 443)
                or parts.username or parts.password or not parts.path.startswith('/')):
            return None
        ignored = {'ex', 'is', 'hm'}
        if parts.path.startswith(('/avatars/', '/banners/')):
            ignored.add('size')
        query = urlencode(sorted((key, value) for key, value in parse_qsl(parts.query)
                                 if key.lower() not in ignored))
        return urlunsplit(('https', parts.hostname, parts.path, query, ''))
    except ValueError:
        return None


def media_key(url):
    canonical = canonical_url(url)
    return hashlib.sha256(canonical.encode()).hexdigest() if canonical else None


def media_directory(path):
    return Path(path).resolve().parent / 'media'


@lru_cache(maxsize=1024)
def _file_digest(binary, fingerprint):
    """The stat fingerprint invalidates prior verification after file changes."""
    with binary.open('rb') as source:
        return hashlib.file_digest(source, 'sha256').hexdigest()


def _saved_file(directory, key, data, *, verify=False, fresh=False):
    binary = directory / (key + '.bin')
    if directory.is_symlink() or binary.is_symlink():
        return False
    stat = binary.stat()
    if data.get('status') != 'saved' or stat.st_size != data.get('bytes'):
        return False
    if verify:
        digest = _file_digest.__wrapped__ if fresh else _file_digest
        return digest(binary, (stat.st_size, stat.st_mtime_ns, stat.st_ctime_ns, stat.st_ino)) == data.get('sha256')
    return True


def media_status(path, url, *, verify=False, fresh=False):
    key = media_key(url)
    if not key:
        return {'status': 'unavailable', 'reason': 'unsupported_host'}
    directory = media_directory(path)
    try:
        manifest = directory / (key + '.json')
        if directory.is_symlink() or manifest.is_symlink():
            return {'status': 'unavailable', 'reason': 'unsafe_path'}
        data = json.loads(manifest.read_text(encoding='utf-8'))
        if data.get('status') == 'saved' and not _saved_file(directory, key, data, verify=verify, fresh=fresh):
            return {'status': 'unavailable', 'reason': 'damaged_file'}
        return data
    except (OSError, ValueError, AttributeError):
        return {'status': 'unavailable', 'reason': 'not_collected'}


def local_url(path, url):
    return '/api/media/' + media_key(url) if media_status(path, url)['status'] == 'saved' else url


def _record(directory, key, data):
    if directory.is_symlink():
        raise OSError('Media directory cannot be a symbolic link')
    directory.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(mode='w', encoding='utf-8', dir=directory, prefix='.searchcord-media-', suffix='.tmp', delete=False) as file:
        temporary = Path(file.name)
        try:
            json.dump(data, file, ensure_ascii=False)
            file.close()
            temporary.replace(directory / (key + '.json'))
        finally:
            temporary.unlink(missing_ok=True)


async def cache_media(path, url, *, client=None, cancelled=None, seconds=FILE_SECONDS, deadline=None):
    """A failed binary never prevents its original message/profile being saved."""
    key = media_key(url)
    if not key:
        return {'status': 'unavailable', 'reason': 'unsupported_host'}
    existing = await asyncio.to_thread(media_status, path, url, verify=True)
    if existing['status'] == 'saved':
        return existing
    if client is None:
        async with httpx.AsyncClient(timeout=FILE_SECONDS, follow_redirects=False, trust_env=False) as client:
            return await cache_media(path, url, client=client, cancelled=cancelled,
                                     seconds=seconds, deadline=deadline)
    directory = media_directory(path)
    data = {'source_url': canonical_url(url), 'status': 'unavailable', 'reason': None,
            'observed_at': datetime.now(timezone.utc).isoformat()}
    temporary = None
    try:
        if cancelled and cancelled():
            raise asyncio.CancelledError
        if directory.is_symlink():
            raise OSError('Media directory cannot be a symbolic link')
        directory.mkdir(parents=True, exist_ok=True)
        if shutil.disk_usage(directory).free < MAX_BYTES:
            data['reason'] = 'disk_space'
        else:
            # Cache verification and filesystem checks may consume the page budget.
            if deadline is not None:
                seconds = min(seconds, deadline - time.monotonic())
                if seconds <= 0:
                    data['reason'] = 'batch_time_limit'
                    raise TimeoutError
            async with asyncio.timeout(max(.001, seconds)), client.stream('GET', url, follow_redirects=False) as response:
                if response.status_code != 200:
                    data['reason'] = 'http_' + str(response.status_code)
                else:
                    try:
                        length = int(response.headers.get('content-length', 0))
                    except ValueError:
                        length = 0
                    if length > MAX_BYTES:
                        data['reason'] = 'size_limit'
                    else:
                        digest = hashlib.sha256()
                        size = 0
                        with tempfile.NamedTemporaryFile(dir=directory, prefix='.searchcord-media-', suffix='.tmp', delete=False) as file:
                            temporary = Path(file.name)
                            async for chunk in response.aiter_bytes(64 * 1024):
                                if cancelled and cancelled():
                                    raise asyncio.CancelledError
                                size += len(chunk)
                                if size > MAX_BYTES:
                                    data['reason'] = 'size_limit'
                                    break
                                file.write(chunk)
                                digest.update(chunk)
                        if data['reason'] is None:
                            temporary.replace(directory / (key + '.bin'))
                            data.update(status='saved', bytes=size, sha256=digest.hexdigest(),
                                        content_type=response.headers.get('content-type', '').split(';')[0].lower())
    except (TimeoutError, httpx.TimeoutException):
        data['reason'] = data['reason'] or 'time_limit'
    except (httpx.RequestError, httpx.InvalidURL):
        data['reason'] = 'network_error'
    except OSError:
        data['reason'] = 'storage_error'
    finally:
        if temporary:
            try:
                temporary.unlink(missing_ok=True)
            except OSError:
                pass
    # Another collection job may have completed this same object meanwhile.
    if data['status'] != 'saved':
        concurrent = await asyncio.to_thread(media_status, path, url, verify=True)
        if concurrent['status'] == 'saved':
            return concurrent
    try:
        _record(directory, key, data)
    except OSError:
        data.update(status='unavailable', reason='storage_error')
    return data


def message_media_urls(messages):
    from profile_store import image_url
    urls = []
    for message in messages:
        for attachment in message.get('attachments') or ():
            if isinstance(attachment, dict):
                urls.append(attachment.get('url'))
        for embed in message.get('embeds') or ():
            if isinstance(embed, dict):
                for kind in ('image', 'thumbnail', 'video'):
                    asset = embed.get(kind)
                    if isinstance(asset, dict):
                        urls.append(asset.get('proxy_url') or asset.get('url'))
        user = message.get('author') or {}
        if isinstance(user, dict) and str(user.get('id', '')).isdigit():
            urls += [image_url(user['id'], user.get('avatar')),
                     image_url(user['id'], user.get('banner'), 'banners', 1024)]
    return list({canonical_url(url): url for url in urls if canonical_url(url)}.values())


async def archive_message_media(path, messages, *, cancelled=None, client=None):
    """Four workers share a download deadline; active digests finish off-loop."""
    candidates = message_media_urls(messages)
    if not candidates:
        return []
    urls = iter(candidates)
    deadline = time.monotonic() + BATCH_SECONDS
    results = []
    async def worker():
        for url in urls:
            if cancelled and cancelled():
                return
            existing = await asyncio.to_thread(media_status, path, url, verify=True)
            remaining = deadline - time.monotonic()
            if existing['status'] == 'saved':
                data = existing
            elif remaining <= 0:
                data = {'source_url': canonical_url(url), 'status': 'unavailable',
                        'reason': 'batch_time_limit', 'observed_at': datetime.now(timezone.utc).isoformat()}
                try:
                    _record(media_directory(path), media_key(url), data)
                except OSError:
                    data['reason'] = 'storage_error'
            else:
                data = await cache_media(path, url, client=client, cancelled=cancelled,
                                         seconds=min(FILE_SECONDS, remaining), deadline=deadline)
            results.append(data)
    async def run_workers():
        async with asyncio.TaskGroup() as group:
            for _ in range(PARALLEL_DOWNLOADS):
                group.create_task(worker())
    if client is None:
        async with httpx.AsyncClient(timeout=FILE_SECONDS, follow_redirects=False, trust_env=False) as client:
            await run_workers()
    else:
        await run_workers()
    return results


def media_router(path):
    router = APIRouter()
    @router.get('/api/media/{key}')
    def media(key: str):
        if not re.fullmatch('[a-f0-9]{64}', key):
            raise HTTPException(404, 'Archived media not found.')
        directory = media_directory(path() if callable(path) else path)
        binary = directory / (key + '.bin')
        try:
            manifest = directory / (key + '.json')
            if directory.is_symlink() or manifest.is_symlink():
                raise ValueError
            data = json.loads(manifest.read_text(encoding='utf-8'))
            if not _saved_file(directory, key, data, verify=True):
                raise ValueError
        except (OSError, ValueError, AttributeError):
            raise HTTPException(404, 'Archived media not found.')
        content_type = data.get('content_type')
        inline = content_type in INLINE_TYPES
        return FileResponse(binary, media_type=content_type if inline else 'application/octet-stream',
                            filename=None if inline else 'attachment',
                            headers={'X-Content-Type-Options': 'nosniff', 'Cache-Control': 'private, no-store',
                                     'Content-Security-Policy': "default-src 'none'; sandbox"})
    return router


def clear_media(path):
    """Delete only files owned by this cache, after collection writers have stopped."""
    directory = media_directory(path)
    if not directory.exists():
        return 0
    if directory.is_symlink() or directory.resolve().parent != Path(path).resolve().parent:
        raise OSError('Media directory resolves outside the archive directory')
    removed = 0
    for file in directory.iterdir():
        if file.is_file() and (re.fullmatch('[a-f0-9]{64}\\.(?:bin|json)', file.name) or
                               re.fullmatch(r'\.searchcord-media-[A-Za-z0-9_-]+\.tmp', file.name)):
            file.unlink()
            removed += 1
    return removed


def copy_referenced_media(source_db, destination_db, urls):
    """Copy verified referenced objects to a new snapshot's sibling directory."""
    source = media_directory(source_db)
    destination = media_directory(destination_db)
    if destination.exists() or destination.is_symlink():
        raise FileExistsError('Destination media directory already exists; choose a new snapshot directory')
    destination.mkdir(mode=0o700)
    created = []
    result = {'copied': 0, 'unavailable': 0, 'bytes': 0}
    seen = set()
    try:
        for url in urls:
            key = media_key(url)
            if not key or key in seen:
                continue
            seen.add(key)
            data = media_status(source_db, url, verify=True, fresh=True)
            if data['status'] != 'saved':
                result['unavailable'] += 1
                continue
            binary = destination / (key + '.bin')
            created.append(binary)
            with (source / (key + '.bin')).open('rb') as original, binary.open('xb') as copy:
                shutil.copyfileobj(original, copy, 1024 * 1024)
            # A source writer changing an object during copying cannot publish
            # a mismatched snapshot binary or alter the source to repair it.
            if not _saved_file(destination, key, data, verify=True, fresh=True):
                raise OSError('Copied media did not match its recorded digest')
            manifest = destination / (key + '.json')
            created.append(manifest)
            with manifest.open('x', encoding='utf-8') as output:
                json.dump(data, output, ensure_ascii=False)
            result['copied'] += 1
            result['bytes'] += data['bytes']
        return result
    except BaseException:
        for file in created:
            file.unlink(missing_ok=True)
        destination.rmdir()
        raise
