"""HTTP contracts used by the promoted home page, with synthetic messages."""
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import aiosqlite
import httpx

import app
from storage import init_database


class HomeTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.path = str(Path(self.temp.name) / 'searchcord.db')
        init_database(self.path)
        self.patch = patch.object(app, 'DB_PATH', self.path)
        self.patch.start()
        self.client = httpx.AsyncClient(transport=httpx.ASGITransport(app=app.app), base_url='http://test')

    async def asyncTearDown(self):
        await self.client.aclose()
        self.patch.stop()
        self.temp.cleanup()

    async def test_search_rejects_unbounded_or_invalid_pagination(self):
        for query in ('page=0', 'page=-1', 'limit=0', 'limit=-1', 'limit=101'):
            response = await self.client.get('/api/search?' + query)
            self.assertEqual(response.status_code, 422)

    async def test_date_errors_and_empty_archive(self):
        for query in ('date_from=not-a-date', 'date_from=2026-09-24&date_to=2026-09-23'):
            self.assertEqual((await self.client.get('/api/search?' + query)).status_code, 400)
        response = await self.client.get('/api/search?date_from=2026-09-23&date_to=2026-09-23')
        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.json()['messages'], [])
        self.assertEqual(response.json()['pages'], 1)

    async def test_search_uses_profile_avatar_without_retrieving_discord(self):
        async with aiosqlite.connect(self.path) as db:
            await app._save_messages(db, [{
                'id': '1419914708582400000', 'author': {'id': '123456789', 'username': 'fixture'},
                'content': 'synthetic <script> text', 'attachments': [],
            }], '111', 'general', '222', 'fixture server')
            await db.execute("INSERT INTO profiles(user_id,username,avatar_hash,fetched_at) VALUES (?,?,?,?)", ('123456789', 'fixture', 'abc123', '2026-09-23'))
            await db.commit()
        response = await self.client.get('/api/search?channel_id=111&author_id=123456789&q=synthetic')
        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.json()['total'], 1)
        self.assertEqual(response.json()['messages'][0]['avatar_url'], 'https://cdn.discordapp.com/avatars/123456789/abc123.png?size=64')

    async def test_home_and_privacy_are_served(self):
        response = await self.client.get('/')
        self.assertEqual(response.status_code, 200)
        self.assertIn('id="queryForm"', response.text)
        self.assertNotIn('id="view-search"', response.text)
        self.assertEqual((await self.client.get('/privacy.html')).status_code, 200)
