"""Actual scrape owner and HTTP adapters with disposable storage and transport."""
import asyncio
from contextlib import ExitStack, closing
import os
from pathlib import Path
import sqlite3
import tempfile
import unittest
from unittest.mock import AsyncMock, patch

import httpx
import app
from storage import init_database

CHANNEL = {"id": "20", "name": "fixture", "guild_id": "10", "guild_name": "Fixture"}
BASE = 1450000000000000000

def message(number, uid="30"):
    return {"id": str(BASE + number), "author": {"id": uid, "username": "fixture"},
            "content": "synthetic", "attachments": []}

class ScrapeCase(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        self.path = str(self.root / "archive" / "fixture.db")
        self.patches = ExitStack()
        self.patches.enter_context(patch.dict(os.environ, {
            "SEARCHCORD_CREDENTIAL_FILE": str(self.root / "vault" / "fixture.vault"),
            "SEARCHCORD_CREDENTIAL_KEY_FILE": str(self.root / "vault" / "fixture.key")}))
        init_database(self.path)
        for key, value in {"DB_PATH": self.path, "discord_request_lock": asyncio.Lock(),
                "last_discord_request": 0, "last_scrape_request": 0, "discord_ready_at": 0,
                "scrape_break_until": 0, "scrape_confirmed_requests": 0,
                "collection_persistence_ready": True, "archive_clearing": False,
                "credentials_clearing": False, "settings_lock": asyncio.Lock(),
                "profile_fetch_lock": asyncio.Lock(), "collection_write_lock": asyncio.Lock(),
                "SCRAPE_INTERVAL_MIN_SECONDS": 0, "SCRAPE_INTERVAL_MAX_SECONDS": 0,
                "SCRAPE_RETRY_BASE_SECONDS": 0.01, "SCRAPE_RETRY_MAX_SECONDS": 0.02,
                "PROGRESS_QUEUE_MAX": 20}.items():
            self.patches.enter_context(patch.object(app, key, value))
        self.selected = self.patches.enter_context(patch.object(app, "_selected_credentials",
            AsyncMock(return_value=("account", "synthetic"))))
        self.patches.enter_context(patch.object(app, "get_token", AsyncMock(return_value="synthetic")))
        self.patches.enter_context(patch.object(app, "archive_message_media", AsyncMock()))
        self.owner = app.create_scrapes()
        self.patches.enter_context(patch.object(app, "scrapes", self.owner))
        self.requests = []
        self.replies = []
        self.pending = asyncio.Event()
        self.release = asyncio.Event()
        self.hold = False
        async def reply(request):
            self.requests.append((request.url.path, dict(request.url.params)))
            if self.hold:
                self.hold = False
                self.pending.set()
                await self.release.wait()
            response = self.replies.pop(0) if self.replies else httpx.Response(200, json=[])
            if isinstance(response, Exception):
                raise response
            return response
        self.discord_client = httpx.AsyncClient(transport=httpx.MockTransport(reply))
        self.patches.enter_context(patch.object(app, "http_client", self.discord_client))
        self.client = httpx.AsyncClient(transport=httpx.ASGITransport(app=app.app), base_url="http://fixture")

    async def asyncTearDown(self):
        await self.owner.suspend()
        self.owner.reset()
        await self.client.aclose()
        await self.discord_client.aclose()
        self.patches.close()
        self.directory.cleanup()

    async def start(self, *, limit=0, profiles=False, channels=None):
        response = await self.client.post('/api/scrape/start', json={"channels": channels or [CHANNEL],
            "limit": limit, "harvest_profiles": profiles})
        self.assertEqual(response.status_code, 200, response.text)
        return response.json()['job_id']

    async def until(self, predicate):
        async with asyncio.timeout(4):
            while not predicate():
                await asyncio.sleep(0.005)

    async def finished(self, job_id):
        await self.until(lambda: not self.owner.snapshot(job_id)['running'])
        return self.owner.snapshot(job_id)

    def query(self, sql):
        with closing(sqlite3.connect(self.path)) as db:
            return db.execute(sql).fetchall()

    async def seed(self, messages, *, complete=False):
        db = await app._write_db()
        try:
            await app._save_messages(db, messages, '20', 'fixture', '10', 'Fixture', history_complete=complete)
        finally:
            await db.close()
