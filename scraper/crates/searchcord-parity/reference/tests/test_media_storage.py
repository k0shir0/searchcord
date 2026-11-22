"""Media policies and migration use disposable, synthetic images only."""
import asyncio
import hashlib
import io
import json
import os
from pathlib import Path
import tempfile
import threading
import unittest
from unittest.mock import patch

import httpx
from fastapi import FastAPI
from fastapi.testclient import TestClient
from PIL import Image

import media_optimize
from media_store import (archive_message_media, cache_media, clear_media,
                         copy_referenced_media, local_url, media_binary,
                         media_directory, media_key, media_router, media_status, _record)
from profile_store import image_url

ATTACHMENT = 'https://cdn.discordapp.com/attachments/20/30/example.png'
AVATAR = 'https://cdn.discordapp.com/avatars/30/abc.png?size=256'


def png(size=(2400, 1200), mode='RGB'):
    image = Image.new(mode, size, (40, 130, 200, 90) if mode == 'RGBA' else (40, 130, 200))
    output = io.BytesIO()
    image.save(output, 'PNG', compress_level=0)
    return output.getvalue()


class MediaTest(unittest.TestCase):
    def test_same_key_compact_publishers_and_cancelled_waiter_are_serialized(self):
        async def scenario():
            requested = asyncio.Event()
            release = asyncio.Event()
            count = 0

            async def download(request):
                nonlocal count
                count += 1
                requested.set()
                await release.wait()
                return httpx.Response(200, content=png((512, 256)), headers={'content-type': 'image/png'})

            with patch.dict(os.environ, {'SEARCHCORD_MEDIA_MODE': 'compact'}):
                async with httpx.AsyncClient(transport=httpx.MockTransport(download)) as client:
                    first = asyncio.create_task(cache_media(self.path, ATTACHMENT, client=client))
                    self.assertTrue(await asyncio.wait_for(requested.wait(), 5))
                    second = asyncio.create_task(cache_media(self.path, ATTACHMENT, client=client))
                    cancelled = asyncio.create_task(cache_media(self.path, ATTACHMENT, client=client))
                    await asyncio.sleep(.05)
                    self.assertEqual(count, 1)
                    cancelled.cancel()
                    with self.assertRaises(asyncio.CancelledError):
                        await cancelled
                    release.set()
                    a, b = await asyncio.wait_for(asyncio.gather(first, second), 5)
                    self.assertEqual(a, b)
                    self.assertIn('storage_file', a)
                    self.assertEqual(count, 1)
                    self.assertEqual(media_status(self.path, ATTACHMENT, verify=True)['status'], 'saved')
                    self.assertFalse((self.directory / (media_key(ATTACHMENT) + '.bin')).exists())
                    self.assertEqual((await cache_media(self.path, ATTACHMENT, client=client))['status'], 'saved')
        asyncio.run(scenario())

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name) / 'archive.db'
        self.path.touch()
        self.directory = media_directory(self.path)
        self.environment = patch.dict(os.environ, {'SEARCHCORD_MEDIA_MODE': 'original',
                                                   'SEARCHCORD_PROFILE_IMAGES': 'original'})
        self.environment.start()
        self.addCleanup(self.environment.stop)

    def save(self, content=None, url=ATTACHMENT, content_type='image/png'):
        self.directory.mkdir(exist_ok=True)
        content = content if content is not None else png()
        key = media_key(url)
        (self.directory / (key + '.bin')).write_bytes(content)
        data = {'source_url': url, 'status': 'saved', 'bytes': len(content),
                'sha256': hashlib.sha256(content).hexdigest(), 'content_type': content_type}
        _record(self.directory, key, data)
        return key, data

    def client(self):
        app = FastAPI()
        app.include_router(media_router(self.path))
        client = TestClient(app)
        self.addCleanup(client.close)
        return client

    def test_dry_run_reports_savings_without_writing_or_changing_database(self):
        self.save()
        before = {file.name: file.read_bytes() for file in self.directory.iterdir()}
        report = media_optimize.optimize(self.path)
        self.assertGreater(report['active_saving_percent'], 50)
        self.assertEqual(report['results'], {'would_convert': 1})
        self.assertEqual(before, {file.name: file.read_bytes() for file in self.directory.iterdir()})
        self.assertEqual(self.path.read_bytes(), b'')

    def test_apply_retains_original_until_explicit_retirement_and_is_resumable(self):
        key, original = self.save()
        media_optimize.optimize(self.path, apply=True)
        data = media_status(self.path, ATTACHMENT, verify=True, fresh=True)
        self.assertEqual(data['content_type'], 'image/webp')
        self.assertEqual(data['original_sha256'], original['sha256'])
        self.assertTrue((self.directory / (key + '.bin')).exists())
        with Image.open(media_binary(self.directory, key, data)) as image:
            self.assertEqual(image.size, (1600, 800))
        report = media_optimize.optimize(self.path, apply=True, remove_originals=True)
        self.assertEqual(report['retired_bytes'], original['bytes'])
        self.assertFalse((self.directory / (key + '.bin')).exists())
        self.assertEqual(media_status(self.path, ATTACHMENT, verify=True)['status'], 'saved')
        self.assertEqual(media_optimize.optimize(self.path, apply=True)['results'], {'already_compact': 1})

    def test_serving_export_and_cleanup_understand_compact_and_legacy_files(self):
        key, _ = self.save()
        self.save(png((256, 256)), AVATAR)
        media_optimize.optimize_one(self.directory, key, media_status(self.path, ATTACHMENT),
                                    apply=True, remove_originals=True)
        client = self.client()
        response = client.get(local_url(self.path, ATTACHMENT))
        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.headers['content-type'], 'image/webp')
        self.assertEqual(response.headers['cache-control'], 'private, no-store')
        self.assertEqual(client.get(local_url(self.path, AVATAR)).headers['content-type'], 'image/png')
        destination = Path(self.temp.name) / 'snapshot' / 'search.db'
        destination.parent.mkdir()
        report = copy_referenced_media(self.path, destination, [ATTACHMENT, AVATAR, ATTACHMENT])
        self.assertEqual(report['copied'], 2)
        self.assertEqual(media_status(destination, ATTACHMENT, verify=True)['status'], 'saved')
        self.assertEqual(media_status(destination, AVATAR, verify=True)['status'], 'saved')
        (self.directory / 'personal.txt').write_text('keep')
        self.assertEqual(clear_media(self.path), 4)
        self.assertEqual([file.name for file in self.directory.iterdir()], ['personal.txt'])

    def test_corrupted_compact_binary_is_not_served_or_exported(self):
        key, data = self.save()
        media_optimize.optimize_one(self.directory, key, data, apply=True)
        data = media_status(self.path, ATTACHMENT)
        binary = media_binary(self.directory, key, data)
        content = bytearray(binary.read_bytes())
        content[-1] ^= 1  # Same size: digest verification must catch it.
        binary.write_bytes(content)
        self.assertEqual(self.client().get('/api/media/' + key).status_code, 404)
        destination = Path(self.temp.name) / 'snapshot' / 'search.db'
        destination.parent.mkdir()
        self.assertEqual(copy_referenced_media(self.path, destination, [ATTACHMENT])['unavailable'], 1)

    def test_manifest_cannot_select_an_arbitrary_file(self):
        key, data = self.save()
        _record(self.directory, key, dict(data, storage_file='../private.webp'))
        self.assertEqual(self.client().get('/api/media/' + key).status_code, 404)

    def test_publication_failure_preserves_verified_original(self):
        key, data = self.save()
        with patch('media_optimize._record', side_effect=OSError('synthetic disk failure')):
            with self.assertRaises(OSError):
                media_optimize.optimize_one(self.directory, key, data, apply=True, remove_originals=True)
        self.assertEqual(media_status(self.path, ATTACHMENT, verify=True, fresh=True), data)
        self.assertEqual(self.client().get('/api/media/' + key).content,
                         (self.directory / (key + '.bin')).read_bytes())

    def test_changed_original_is_never_retired(self):
        key, data = self.save()
        media_optimize.optimize_one(self.directory, key, data, apply=True)
        original = self.directory / (key + '.bin')
        content = bytearray(original.read_bytes())
        content[-1] ^= 1
        original.write_bytes(content)
        media_optimize.optimize(self.path, apply=True, remove_originals=True)
        self.assertTrue(original.exists())

    def test_transparency_and_orientation_survive_conversion(self):
        key, data = self.save(png((300, 200), 'RGBA'))
        media_optimize.optimize_one(self.directory, key, data, apply=True)
        data = media_status(self.path, ATTACHMENT)
        with Image.open(media_binary(self.directory, key, data)) as image:
            self.assertEqual(image.convert('RGBA').getpixel((1, 1))[3], 90)
        output = io.BytesIO()
        exif = Image.Exif()
        exif[274] = 6
        Image.new('RGB', (700, 200), 'orange').save(output, 'JPEG', quality=100, exif=exif)
        key, data = self.save(output.getvalue(), content_type='image/jpeg')
        media_optimize.optimize_one(self.directory, key, data, apply=True)
        with Image.open(media_binary(self.directory, key, media_status(self.path, ATTACHMENT))) as image:
            self.assertEqual(image.size, (200, 700))
            self.assertIsNone(image.getexif().get(274))

    def test_animation_non_images_and_oversized_pixel_dimensions_stay_original(self):
        output = io.BytesIO()
        Image.new('RGB', (200, 100), 'red').save(output, 'GIF', save_all=True,
            append_images=[Image.new('RGB', (200, 100), 'blue')], duration=100, loop=0)
        content = output.getvalue()
        key, data = self.save(content, content_type='image/gif')
        result = media_optimize.optimize_one(self.directory, key, data, apply=True, remove_originals=True)
        self.assertEqual(result['reason'], 'animated')
        self.assertEqual(self.client().get('/api/media/' + key).content, content)
        key, data = self.save(b'not an image', content_type='application/zip')
        self.assertEqual(media_optimize.optimize_one(self.directory, key, data)['reason'], 'non_image_or_size_limit')
        key, data = self.save(png((20, 20)))
        with patch.object(media_optimize, 'MAX_PIXELS', 100):
            self.assertEqual(media_optimize.optimize_one(self.directory, key, data)['reason'], 'pixel_limit')

    def test_image_that_would_grow_is_not_replaced(self):
        output = io.BytesIO()
        Image.effect_noise((128, 128), 100).convert('RGB').save(output, 'JPEG', quality=1)
        key, data = self.save(output.getvalue(), content_type='image/jpeg')
        result = media_optimize.optimize_one(self.directory, key, data, apply=True, remove_originals=True)
        self.assertEqual(result['reason'], 'not_smaller')
        self.assertEqual(media_status(self.path, ATTACHMENT, verify=True), data)

    def test_audit_separates_profiles_attachments_and_retained_disk_usage(self):
        _, attachment = self.save()
        _, avatar = self.save(png((256, 256)), AVATAR)
        report = media_optimize.audit(self.path)
        self.assertEqual(report['by_kind'], {'attachment': attachment['bytes'], 'avatar': avatar['bytes']})
        media_optimize.optimize(self.path, apply=True)
        report = media_optimize.audit(self.path)
        self.assertGreater(report['disk_bytes'], report['active_bytes'] + attachment['bytes'])
        self.assertEqual(media_optimize.benchmark(self.path)['decoded'], 2)

    def test_shapes_policy_hides_profile_images(self):
        self.save(url=AVATAR)
        with patch.dict(os.environ, {'SEARCHCORD_PROFILE_IMAGES': 'shapes'}):
            self.assertIsNone(image_url('30', 'abc'))
            self.assertIsNone(image_url('30', 'abc', 'banners'))
            self.assertIsNone(local_url(self.path, AVATAR))
            self.assertEqual(local_url(self.path, ATTACHMENT), ATTACHMENT)


class DownloadTest(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name) / 'archive.db'
        self.calls = []
        self.content = png((300, 200))
        async def respond(request):
            self.calls.append(str(request.url))
            return httpx.Response(200, content=self.content, headers={'content-type': 'image/png'})
        self.client = httpx.AsyncClient(transport=httpx.MockTransport(respond))
        self.addAsyncCleanup(self.client.aclose)

    async def test_none_mode_does_not_download_or_write_even_on_direct_cache_call(self):
        with patch.dict(os.environ, {'SEARCHCORD_MEDIA_MODE': 'none'}):
            self.assertEqual((await cache_media(self.path, ATTACHMENT, client=self.client))['reason'], 'disabled_by_policy')
            await archive_message_media(self.path, [{'attachments': [{'url': ATTACHMENT}]}], client=self.client)
        self.assertEqual(self.calls, [])
        self.assertFalse(media_directory(self.path).exists())

    async def test_shapes_skips_profile_downloads_but_archives_attachments(self):
        message = {'author': {'id': '30', 'avatar': 'abc', 'banner': 'def'},
                   'attachments': [{'url': ATTACHMENT}]}
        with patch.dict(os.environ, {'SEARCHCORD_PROFILE_IMAGES': 'shapes', 'SEARCHCORD_MEDIA_MODE': 'original'}):
            await archive_message_media(self.path, [message], client=self.client)
        self.assertEqual(self.calls, [ATTACHMENT])
        self.assertEqual(media_status(self.path, ATTACHMENT, verify=True)['status'], 'saved')

    async def test_compact_download_serves_webp_and_retires_verified_input(self):
        with patch.dict(os.environ, {'SEARCHCORD_MEDIA_MODE': 'compact', 'SEARCHCORD_PROFILE_IMAGES': 'original'}):
            result = await cache_media(self.path, AVATAR, client=self.client)
        self.assertEqual(result['content_type'], 'image/webp')
        self.assertFalse((media_directory(self.path) / (media_key(AVATAR) + '.bin')).exists())
        with Image.open(media_binary(media_directory(self.path), media_key(AVATAR), result)) as image:
            self.assertEqual(image.size, (128, 85))

    async def test_compact_encoding_failure_keeps_original_download_available(self):
        with patch.dict(os.environ, {'SEARCHCORD_MEDIA_MODE': 'compact'}), \
                patch('media_optimize.webp_bytes', side_effect=OSError('synthetic encoder failure')):
            with self.assertLogs('media_store', level='ERROR'):
                result = await cache_media(self.path, ATTACHMENT, client=self.client)
        self.assertEqual(result['content_type'], 'image/png')
        self.assertEqual(media_status(self.path, ATTACHMENT, verify=True)['status'], 'saved')

    async def test_cancel_joins_encoding_before_writer_exits(self):
        entered, release = threading.Event(), threading.Event()
        original = media_optimize.optimize_one
        def held(*args, **kwargs):
            entered.set()
            release.wait(5)
            return original(*args, **kwargs)
        with patch.dict(os.environ, {'SEARCHCORD_MEDIA_MODE': 'compact'}), \
                patch('media_optimize.optimize_one', held):
            task = asyncio.create_task(cache_media(self.path, ATTACHMENT, client=self.client))
            try:
                async with asyncio.timeout(3):
                    while not entered.is_set():
                        await asyncio.sleep(.005)
                task.cancel()
                await asyncio.sleep(.02)
                self.assertFalse(task.done())
            finally:
                release.set()
            with self.assertRaises(asyncio.CancelledError):
                await task
        data = media_status(self.path, ATTACHMENT, verify=True, fresh=True)
        self.assertEqual(data['content_type'], 'image/webp')
        self.assertEqual(clear_media(self.path), 2)


if __name__ == '__main__':
    unittest.main()
