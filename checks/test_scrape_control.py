"""Pause/resume against disposable SQLite, and request pacing with a fake clock."""
import asyncio
from contextlib import ExitStack, closing
from pathlib import Path
import sqlite3
import tempfile
import unittest
from unittest.mock import AsyncMock, patch

import httpx
from fastapi import BackgroundTasks
import app
from storage import init_database

CHANNEL = {"id": "20", "name": "fixture", "guild_id": "10", "guild_name": "Fixture"}
BASE = 1000000000000000000


def message(number):
    return {"id": str(BASE + number), "author": {"id": "30", "username": "fixture"},
            "content": "synthetic", "attachments": []}


class ScrapeControlTest(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.path = str(Path(self.directory.name) / "archive.db")
        init_database(self.path)
        self.patches = ExitStack()
        for key, value in {"DB_PATH": self.path, "active_jobs": {},
                           "discord_request_lock": asyncio.Lock(), "last_discord_request": 0,
                           "last_scrape_request": 0, "discord_ready_at": 0,
                           "scrape_break_until": 0, "scrape_confirmed_requests": 0,
                           "SCRAPE_INTERVAL_MIN_SECONDS": 0, "SCRAPE_INTERVAL_MAX_SECONDS": 0,
                           "JOB_RETENTION_SECONDS": 0}.items():
            self.patches.enter_context(patch.object(app, key, value))
        self.patches.enter_context(patch.object(app, "get_token", AsyncMock(return_value="synthetic")))
        self.requests = []
        self.replies = []
        self.pending = asyncio.Event()
        self.release = asyncio.Event()
        self.hold = False

        async def reply(request):
            self.requests.append(dict(request.url.params))
            if self.hold:
                self.hold = False
                self.pending.set()
                await self.release.wait()
            return self.replies.pop(0) if self.replies else httpx.Response(200, json=[])

        self.discord_client = httpx.AsyncClient(transport=httpx.MockTransport(reply))
        self.patches.enter_context(patch.object(app, "http_client", self.discord_client))
        self.client = httpx.AsyncClient(transport=httpx.ASGITransport(app=app.app), base_url="http://fixture")
        self.tasks = []

    async def asyncTearDown(self):
        for task in self.tasks:
            task.cancel()
        await asyncio.gather(*self.tasks, return_exceptions=True)
        await self.client.aclose()
        await self.discord_client.aclose()
        self.patches.close()
        self.directory.cleanup()

    async def start(self, *, limit=0, profiles=False):
        background = BackgroundTasks()
        result = await app.start_scrape(app.ScrapeRequest(channels=[CHANNEL], limit=limit,
                                                       harvest_profiles=profiles), background)
        job_id = result["job_id"]
        job = app.active_jobs[job_id]
        task = asyncio.create_task(background())
        self.tasks.append(task)
        return job_id, job, task

    async def until(self, predicate):
        async with asyncio.timeout(3):
            while not predicate():
                await asyncio.sleep(0.01)

    async def test_pause_commits_inflight_page_and_resumes_exact_cursor_and_limit(self):
        self.hold = True
        self.replies = [httpx.Response(200, json=[message(i) for i in range(300, 200, -1)]),
                        httpx.Response(200, json=[message(i) for i in range(200, 150, -1)])]
        job_id, job, task = await self.start(limit=150)
        await asyncio.wait_for(self.pending.wait(), 1)
        self.assertEqual((await self.client.post(f"/api/scrape/{job_id}/pause")).status_code, 200)
        self.assertFalse(job["paused"])
        self.release.set()
        await self.until(lambda: job["paused"])
        with closing(sqlite3.connect(self.path)) as db:
            self.assertEqual(db.execute("SELECT COUNT(*) FROM messages").fetchone()[0], 100)
            self.assertEqual(db.execute("SELECT oldest_message_id FROM scrape_cursors").fetchone()[0], message(201)["id"])
        await asyncio.sleep(0.15)
        self.assertEqual(len(self.requests), 1)
        snapshot = (await self.client.get("/api/scrape/active")).json()[0]
        self.assertTrue(snapshot["paused"])
        self.assertEqual(snapshot["total_messages"], 100)
        self.assertEqual(snapshot["rows"][0]["messages"], 100)
        self.assertEqual((await self.client.post("/api/scrape/start", json={"channels": [CHANNEL]})).status_code, 409)
        await self.client.post(f"/api/scrape/{job_id}/pause")  # idempotent
        await self.client.post(f"/api/scrape/{job_id}/resume")
        await asyncio.wait_for(task, 2)
        self.assertEqual(self.requests, [{"limit": "100"}, {"limit": "50", "before": message(201)["id"]}])
        with closing(sqlite3.connect(self.path)) as db:
            self.assertEqual(db.execute("SELECT COUNT(*) FROM messages").fetchone()[0], 150)
            self.assertEqual(db.execute("PRAGMA integrity_check").fetchone()[0], "ok")

    async def test_pause_newer_gap_keeps_anchor_and_before_cursor(self):
        db = await app._write_db()
        await app._save_messages(db, [message(200)], "20", "fixture", "10", "Fixture")
        await app._mark_history_complete(db, "20")
        await db.close()
        self.hold = True
        self.replies = [httpx.Response(200, json=[message(i) for i in range(400, 300, -1)]),
                        httpx.Response(200, json=[message(i) for i in range(300, 200, -1)]),
                        httpx.Response(200, json=[])]
        job_id, job, task = await self.start()
        await self.pending.wait()
        await self.client.post(f"/api/scrape/{job_id}/pause")
        self.release.set()
        await self.until(lambda: job["paused"])
        with closing(sqlite3.connect(self.path)) as db:
            self.assertEqual(db.execute("SELECT pending_after_message_id,pending_before_message_id FROM scrape_cursors").fetchone(),
                             (message(200)["id"], message(301)["id"]))
        await self.client.post(f"/api/scrape/{job_id}/resume")
        await asyncio.wait_for(task, 2)
        self.assertEqual(self.requests[1]["before"], message(301)["id"])
        with closing(sqlite3.connect(self.path)) as db:
            self.assertEqual(db.execute("SELECT COUNT(*) FROM messages").fetchone()[0], 201)

    async def test_stop_while_paused_and_validation(self):
        self.hold = True
        job_id, job, task = await self.start()
        await self.pending.wait()
        await self.client.post(f"/api/scrape/{job_id}/pause")
        self.replies = [httpx.Response(200, json=[message(i) for i in range(300, 200, -1)])]
        self.release.set()
        await self.until(lambda: job["paused"])
        await self.client.post(f"/api/scrape/{job_id}/stop")
        self.assertEqual((await self.client.post(f"/api/scrape/{job_id}/resume")).status_code, 409)
        await asyncio.wait_for(task, 1)
        self.assertEqual(job["last_event"]["type"], "cancelled")
        self.assertEqual(len(self.requests), 1)
        self.assertEqual((await self.client.post("/api/scrape/missing/pause")).status_code, 404)

    async def test_pause_and_stop_during_mandatory_break_without_holding_global_gate(self):
        app.scrape_break_until = app.time.monotonic() + 60
        job_id, job, task = await self.start()
        await self.until(lambda: job.get("on_break"))
        await self.client.post(f"/api/scrape/{job_id}/pause")
        await self.until(lambda: job["paused"])
        await asyncio.wait_for(app.discord("GET", "/users/@me", "synthetic"), 1)
        self.assertEqual(len(self.requests), 1)  # unrelated requests remain usable
        await self.client.post(f"/api/scrape/{job_id}/resume")
        await asyncio.sleep(0.15)
        self.assertEqual(len(self.requests), 1)  # resume cannot skip the break
        await self.client.post(f"/api/scrape/{job_id}/stop")
        await asyncio.wait_for(task, 1)
        self.assertGreater(app.scrape_break_until, app.time.monotonic() + 59)

    async def test_pause_during_fetch_retry(self):
        self.replies = [httpx.Response(503)]
        job_id, job, task = await self.start()
        await self.until(lambda: job.get("last_event", {}).get("type") == "retry_wait")
        await self.client.post(f"/api/scrape/{job_id}/pause")
        await self.until(lambda: job["paused"])
        await self.client.post(f"/api/scrape/{job_id}/stop")
        await asyncio.wait_for(task, 1)
        self.assertEqual(len(self.requests), 1)

    async def test_progress_clients_each_receive_snapshot_and_pause_events(self):
        result = await app.start_scrape(app.ScrapeRequest(channels=[CHANNEL]), BackgroundTasks())
        job_id = result["job_id"]
        job = app.active_jobs[job_id]

        class Connected:
            async def is_disconnected(self):
                return False

        streams = [(await app.scrape_progress(job_id, Connected())).body_iterator for _ in range(2)]
        try:
            for stream in streams:
                self.assertIn('"type": "snapshot"', await anext(stream))
            app._emit(job["queue"], {"type": "paused"})
            for stream in streams:
                self.assertIn('"type": "paused"', await asyncio.wait_for(anext(stream), 1))
            self.assertEqual(len(job["subscribers"]), 2)
        finally:
            for stream in streams:
                await stream.aclose()
        self.assertEqual(len(job["subscribers"]), 0)

    async def test_profiles_use_same_pacing_and_pause_during_profile_retry(self):
        self.replies = [httpx.Response(200, json=[message(300)]), httpx.Response(503)]
        job_id, job, task = await self.start(profiles=True)
        await self.until(lambda: job.get("last_event", {}).get("type") == "profile_retry")
        self.assertEqual(app.scrape_confirmed_requests, 1)  # failed profile does not count
        await self.client.post(f"/api/scrape/{job_id}/pause")
        await self.until(lambda: job["paused"])
        await self.client.post(f"/api/scrape/{job_id}/stop")
        await asyncio.wait_for(task, 1)
        self.assertEqual(len(self.requests), 2)


class ScrapePacingTest(unittest.IsolatedAsyncioTestCase):
    async def test_random_intervals_retries_and_100_success_break_are_shared(self):
        now = [1000.0]
        sent = []
        waits = []
        replies = [httpx.Response(429, json={"retry_after": 0}), httpx.Response(503)]

        class Client:
            async def request(self, *args, **kwargs):
                sent.append(now[0])
                return replies.pop(0) if replies else httpx.Response(200, json=[])

        async def wait(job_id, seconds, **kwargs):
            waits.append(seconds)
            now[0] += seconds

        jobs = {key: {"queue": asyncio.Queue(), "cancelled": False} for key in ("a", "b")}
        with ExitStack() as patches:
            for key, value in {"http_client": Client(), "active_jobs": jobs,
                               "discord_request_lock": asyncio.Lock(), "last_discord_request": 0,
                               "last_scrape_request": 0, "discord_ready_at": 0,
                               "scrape_break_until": 0, "scrape_confirmed_requests": 0,
                               "SCRAPE_INTERVAL_MIN_SECONDS": 0.5, "SCRAPE_INTERVAL_MAX_SECONDS": 1.0}.items():
                patches.enter_context(patch.object(app, key, value))
            patches.enter_context(patch.object(app.time, "monotonic", side_effect=lambda: now[0]))
            patches.enter_context(patch.object(app, "_wait_for_scrape_retry", side_effect=wait))
            draw = patches.enter_context(patch.object(app.random, "uniform", side_effect=lambda low, high: low if len(sent) % 2 else high))
            # An internal 429 retry, a failed response, and 101 confirmations.
            for index in range(102):
                await app.discord("GET", "/channels/20/messages", "synthetic", scrape_job_id="a" if index % 2 else "b")
            self.assertEqual(len(sent), 103)
            self.assertEqual(draw.call_count, 103)
            self.assertTrue(all(call.args == (0.5, 1.0) for call in draw.call_args_list))
            intervals = [b - a for a, b in zip(sent, sent[1:])]
            self.assertTrue(all(0.5 <= interval <= 1.0 for interval in intervals[:-1]))
            self.assertGreaterEqual(intervals[-1], 60)
            self.assertEqual(app.scrape_confirmed_requests, 1)
            self.assertTrue(any(event["type"] == "scrape_break" for job in jobs.values() for event in list(job["queue"]._queue)))

    async def test_concurrent_jobs_obey_pacing_with_real_time(self):
        sent = []
        finished = []

        class Client:
            async def request(self, *args, **kwargs):
                sent.append(app.time.monotonic())
                await asyncio.sleep(0.6)
                finished.append(app.time.monotonic())
                return httpx.Response(200, json=[])

        with ExitStack() as patches:
            for key, value in {"http_client": Client(), "active_jobs": {
                    key: {"queue": asyncio.Queue(), "cancelled": False} for key in ("a", "b")},
                    "discord_request_lock": asyncio.Lock(), "last_discord_request": 0,
                    "last_scrape_request": 0, "discord_ready_at": 0, "scrape_break_until": 0,
                    "scrape_confirmed_requests": 0}.items():
                patches.enter_context(patch.object(app, key, value))
            await asyncio.gather(*(app.discord("GET", "/channels/20/messages", "synthetic", scrape_job_id=key)
                                   for key in ("a", "b", "a")))
            self.assertTrue(all(start - end >= 0.5 for end, start in zip(finished, sent[1:])))
            self.assertEqual(app.scrape_confirmed_requests, 3)


if __name__ == "__main__":
    unittest.main()
