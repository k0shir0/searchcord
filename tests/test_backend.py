import asyncio
import sqlite3
import tempfile
import unittest
from contextlib import closing
from datetime import datetime, timezone
from pathlib import Path
from unittest.mock import AsyncMock, patch

import httpx
from fastapi import BackgroundTasks, HTTPException
import app
from storage import EPOCH_MS, init_database


def message(number, user="u1", image=False):
    return {
        "id": str(number), "author": {"id": user, "username": user},
        "content": f"message {number}", "timestamp": "2026-09-22T00:00:00+00:00",
        "attachments": ([{"url": f"https://example.test/{number}.png", "content_type": "image/png"},
                         {"url": f"https://example.test/{number}.pdf", "content_type": "application/pdf"}]
                        if image else []),
    }


class FakeResponse:
    status_code = 200

    def __init__(self, data):
        self.data = data

    def json(self):
        return self.data


class ScrapeTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.db_path = str(Path(self.temp.name) / "searchcord.db")
        init_database(self.db_path)
        self.patch_path = patch.object(app, "DB_PATH", self.db_path)
        self.patch_token = patch.object(app, "get_token", return_value="test-token")
        self.patch_retention = patch.object(app, "JOB_RETENTION_SECONDS", 0)
        self.patch_path.start()
        self.patch_token.start()
        self.patch_retention.start()
        self.messages = [message(i, f"u{i % 3}", image=i == 120) for i in range(1, 121)]
        self.message_calls = []
        self.profile_calls = []

        async def fake_discord(method, path, token, **kwargs):
            if path.endswith("/messages"):
                params = kwargs["params"]
                self.message_calls.append(dict(params))
                found = sorted(self.messages, key=lambda m: int(m["id"]), reverse=True)
                if "after" in params:
                    found = [m for m in found if int(m["id"]) > int(params["after"])]
                if "before" in params:
                    found = [m for m in found if int(m["id"]) < int(params["before"])]
                return FakeResponse(found[:params["limit"]])
            user_id = path.split("/")[-1]
            self.profile_calls.append(user_id)
            return FakeResponse({"id": user_id, "username": user_id,
                                 "global_name": "Display " + user_id,
                                 "avatar": "hash", "banner": None,
                                 "public_flags": 0})

        self.patch_discord = patch.object(app, "discord", side_effect=fake_discord)
        self.patch_discord.start()

    async def asyncTearDown(self):
        self.patch_discord.stop()
        self.patch_retention.stop()
        self.patch_token.stop()
        self.patch_path.stop()
        self.temp.cleanup()

    async def scrape(self, profiles=False):
        job_id = "test-job"
        app.active_jobs[job_id] = {"queue": asyncio.Queue(), "cancelled": False}
        await app.run_scrape(job_id, [{"id": "c1", "name": "general",
                                       "guild_id": "g1", "guild_name": "Guild"}],
                             harvest_profiles=profiles)
        self.assertNotIn(job_id, app.active_jobs)

    async def test_profile_toggle_reaches_background_job(self):
        tasks = BackgroundTasks()
        response = await app.start_scrape(app.ScrapeRequest(
            channels=[], harvest_profiles=True), tasks)
        self.assertIs(tasks.tasks[0].args[-1], True)
        app.active_jobs.pop(response["job_id"], None)

    async def test_same_channel_cannot_start_while_stop_is_finishing(self):
        request = app.ScrapeRequest(channels=[{"id": "c1"}])
        first = await app.start_scrape(request, BackgroundTasks())
        with self.assertRaises(HTTPException) as caught:
            await app.start_scrape(request, BackgroundTasks())
        self.assertEqual(caught.exception.status_code, 409)
        app.active_jobs.pop(first["job_id"])

    async def test_stopped_first_scrape_resumes_from_saved_oldest_message(self):
        job_id = "stopped-first"
        app.active_jobs[job_id] = {"queue": asyncio.Queue(), "cancelled": False}
        save = app._save_messages

        async def stop_after_page(*args, **kwargs):
            inserted = await save(*args, **kwargs)
            app.active_jobs[job_id]["cancelled"] = True
            return inserted

        with patch.object(app, "_save_messages", side_effect=stop_after_page):
            await app.run_scrape(job_id, [{"id": "c1", "name": "general",
                                           "guild_id": "g1", "guild_name": "Guild"}])
        with closing(sqlite3.connect(self.db_path)) as db:
            self.assertEqual(db.execute("SELECT COUNT(*) FROM messages").fetchone()[0], 100)
            self.assertEqual(db.execute("""SELECT oldest_message_id, history_complete
                FROM scrape_cursors""").fetchone(), ("21", 0))

        self.message_calls.clear()
        await self.scrape()
        self.assertEqual(self.message_calls[0], {"limit": 100, "after": "120"})
        self.assertEqual(self.message_calls[1], {"limit": 100, "before": "21"})
        with closing(sqlite3.connect(self.db_path)) as db:
            self.assertEqual(db.execute("SELECT COUNT(*) FROM messages").fetchone()[0], 120)

    async def test_stopped_new_message_gap_resumes_without_rescanning_history(self):
        await self.scrape()
        self.messages.extend(message(i, "u4") for i in range(121, 351))
        self.message_calls.clear()
        job_id = "stopped-gap"
        app.active_jobs[job_id] = {"queue": asyncio.Queue(), "cancelled": False}
        save = app._save_messages

        async def stop_after_page(*args, **kwargs):
            inserted = await save(*args, **kwargs)
            app.active_jobs[job_id]["cancelled"] = True
            return inserted

        with patch.object(app, "_save_messages", side_effect=stop_after_page):
            await app.run_scrape(job_id, [{"id": "c1", "name": "general",
                                           "guild_id": "g1", "guild_name": "Guild"}])
        with closing(sqlite3.connect(self.db_path)) as db:
            self.assertEqual(db.execute("""SELECT pending_after_message_id,
                pending_before_message_id FROM scrape_cursors""").fetchone(),
                             ("120", "251"))

        self.message_calls.clear()
        await self.scrape()
        self.assertEqual(self.message_calls[0], {"limit": 100, "before": "251"})
        with closing(sqlite3.connect(self.db_path)) as db:
            self.assertEqual(db.execute("SELECT COUNT(*) FROM messages").fetchone()[0], 350)
            self.assertEqual(db.execute("""SELECT pending_after_message_id
                FROM scrape_cursors""").fetchone()[0], None)

    async def test_incremental_scrape_and_profile_toggle(self):
        self.messages[0]["author"]["username"] = "Old name"
        self.messages[-1]["author"]["username"] = "Newest name"
        await self.scrape()
        self.assertEqual(len(self.message_calls), 2)
        self.assertEqual(self.profile_calls, [])
        with closing(sqlite3.connect(self.db_path)) as db:
            self.assertEqual(db.execute("SELECT COUNT(*) FROM messages").fetchone()[0], 120)
            row = db.execute("""SELECT author_id, content, image_urls FROM messages
                WHERE id='120'""").fetchone()
            self.assertEqual(row, ("u0", "message 120", 'https://example.test/120.png'))
            self.assertEqual(db.execute("SELECT history_complete FROM scrape_cursors").fetchone()[0], 1)
        result = await app.search(q="message 120")
        self.assertEqual(result["total"], 1)
        self.assertEqual(result["messages"][0]["author_name"], "Newest name")
        self.assertEqual(result["messages"][0]["attachments"],
                         ["/api/images/120/0"])
        with closing(sqlite3.connect(self.db_path)) as db:
            self.assertEqual(db.execute("""SELECT author_name FROM message_records
                WHERE id='1'""").fetchone()[0], "u1")

        self.message_calls.clear()
        await self.scrape(profiles=True)
        self.assertEqual(self.message_calls, [{"limit": 100, "after": "120"}])
        self.assertCountEqual(self.profile_calls, ["u0", "u1", "u2"])
        with closing(sqlite3.connect(self.db_path)) as db:
            self.assertEqual(db.execute("SELECT COUNT(*) FROM profiles").fetchone()[0], 3)
            self.assertEqual(db.execute("SELECT COUNT(*) FROM messages").fetchone()[0], 120)

        self.message_calls.clear()
        self.profile_calls.clear()
        await self.scrape(profiles=True)
        self.assertEqual(self.message_calls, [{"limit": 100, "after": "120"}])
        self.assertEqual(self.profile_calls, [])

        self.messages.extend(message(i, "u4") for i in range(121, 251))
        self.message_calls.clear()
        await self.scrape(profiles=True)
        self.assertEqual(self.message_calls[0], {"limit": 100, "after": "120"})
        self.assertEqual(self.message_calls[1], {"limit": 100, "before": "151"})
        with closing(sqlite3.connect(self.db_path)) as db:
            self.assertEqual(db.execute("SELECT COUNT(*) FROM messages").fetchone()[0], 250)
            self.assertEqual(db.execute("SELECT COUNT(*) FROM profiles").fetchone()[0], 4)
            self.assertEqual(db.execute("SELECT newest_message_id FROM scrape_cursors").fetchone()[0], "250")

    async def test_limited_first_scrape_resumes_history(self):
        job_id = "limited"
        app.active_jobs[job_id] = {"queue": asyncio.Queue(), "cancelled": False}
        await app.run_scrape(job_id, [{"id": "c1", "name": "general",
                                       "guild_id": "g1", "guild_name": "Guild"}], limit=100)
        with closing(sqlite3.connect(self.db_path)) as db:
            self.assertEqual(db.execute("SELECT COUNT(*) FROM messages").fetchone()[0], 100)
            self.assertEqual(db.execute("SELECT history_complete FROM scrape_cursors").fetchone()[0], 0)
        self.message_calls.clear()
        await self.scrape()
        self.assertEqual(self.message_calls[0], {"limit": 100, "after": "120"})
        self.assertEqual(self.message_calls[1], {"limit": 100, "before": "21"})
        with closing(sqlite3.connect(self.db_path)) as db:
            self.assertEqual(db.execute("SELECT COUNT(*) FROM messages").fetchone()[0], 120)
            self.assertEqual(db.execute("SELECT history_complete FROM scrape_cursors").fetchone()[0], 1)

    async def test_limited_incremental_scrape_resumes_new_message_gap(self):
        await self.scrape()
        self.messages.extend(message(i, "u4") for i in range(121, 251))
        self.message_calls.clear()
        job_id = "limited-new"
        app.active_jobs[job_id] = {"queue": asyncio.Queue(), "cancelled": False}
        await app.run_scrape(job_id, [{"id": "c1", "name": "general",
                                       "guild_id": "g1", "guild_name": "Guild"}], limit=100)
        with closing(sqlite3.connect(self.db_path)) as db:
            self.assertEqual(db.execute("SELECT COUNT(*) FROM messages").fetchone()[0], 220)
            self.assertEqual(db.execute("""SELECT pending_after_message_id,
                pending_before_message_id FROM scrape_cursors""").fetchone(), ("120", "151"))

        self.message_calls.clear()
        await self.scrape()
        self.assertEqual(self.message_calls[0], {"limit": 100, "before": "151"})
        with closing(sqlite3.connect(self.db_path)) as db:
            self.assertEqual(db.execute("SELECT COUNT(*) FROM messages").fetchone()[0], 250)
            self.assertEqual(db.execute("""SELECT pending_after_message_id,
                pending_before_message_id FROM scrape_cursors""").fetchone(), (None, None))

    async def test_limit_counts_only_saved_messages(self):
        await self.scrape()
        self.messages.extend(message(i, "u4") for i in range(121, 351))
        job_id = "limit-150"
        app.active_jobs[job_id] = {"queue": asyncio.Queue(), "cancelled": False}
        await app.run_scrape(job_id, [{"id": "c1", "name": "general",
                                       "guild_id": "g1", "guild_name": "Guild"}], limit=150)
        with closing(sqlite3.connect(self.db_path)) as db:
            self.assertEqual(db.execute("SELECT COUNT(*) FROM messages").fetchone()[0], 270)
        await self.scrape()
        with closing(sqlite3.connect(self.db_path)) as db:
            self.assertEqual(db.execute("SELECT COUNT(*) FROM messages").fetchone()[0], 350)

    async def test_stats_and_bounded_author_suggestions(self):
        self.messages = [message(i, f"u{i}") for i in range(1, 31)]
        for m in self.messages:
            m["author"]["username"] = "Alpha " + m["id"]
        await self.scrape()
        stats = await app.get_stats()
        self.assertEqual(stats["total_messages"], 30)
        self.assertEqual(stats["total_users"], 30)
        self.assertEqual(stats["total_servers"], 1)
        self.assertEqual(stats["messages_by_hour"][0]["count"], 30)
        self.assertEqual(len(await app.search_authors("Alpha")), 20)
        self.assertEqual(await app.search_authors("Al%"), [])
        self.assertEqual((await app.search_filters(include_users=False))["users"], [])

    async def test_date_filter_and_derived_timestamp(self):
        day_ms = int(datetime(2026, 9, 22, tzinfo=timezone.utc).timestamp() * 1000)
        first = (day_ms - EPOCH_MS) << 22
        second = (day_ms + 86400000 - EPOCH_MS) << 22
        db = await app._write_db()
        try:
            await app._save_messages(db, [message(first), message(second)],
                                     "c1", "general", "g1", "Guild")
        finally:
            await db.close()
        result = await app.search(date_from="2026-09-22", date_to="2026-09-22")
        self.assertEqual(result["total"], 1)
        self.assertEqual(result["messages"][0]["id"], str(first))
        self.assertEqual(result["messages"][0]["timestamp"], "2026-09-22T00:00:00.000Z")

    async def test_search_export_stats_and_clear_still_work(self):
        await self.scrape(profiles=True)
        self.assertEqual((await app.get_stats())["total_messages"], 120)
        self.assertEqual((await app.search())["total"], 120)
        self.assertEqual((await app.search(guild_id="g1"))["total"], 120)
        self.assertEqual((await app.search(channel_id="c1"))["total"], 120)
        self.assertEqual((await app.search(author_id="u0"))["total"], 40)
        self.assertEqual((await app.search(guild_id="missing"))["total"], 0)
        filters = await app.search_filters()
        self.assertEqual(filters["guilds"], [{"guild_id": "g1", "guild_name": "Guild"}])
        self.assertEqual(len(filters["users"]), 3)
        self.assertEqual(len(await app.export_participants("c1")), 3)
        exported = await app.export_chatml(app.ChatMLExportRequest(
            channel_id="c1", target_id="u0"))
        self.assertEqual(exported.media_type, "application/x-ndjson")

        await app.clear_messages()
        with closing(sqlite3.connect(self.db_path)) as db:
            for table in ("messages", "profiles", "scrape_cursors",
                          "channel_authors", "stats_counts"):
                self.assertEqual(db.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0], 0)
            self.assertEqual(db.execute("""SELECT COUNT(*) FROM messages_fts
                WHERE content LIKE '%message%'""").fetchone()[0], 0)
        self.message_calls.clear()
        await self.scrape()
        self.assertEqual(self.message_calls[0], {"limit": 100})

    async def test_search_response_is_gzipped_for_browser(self):
        await self.scrape()
        transport = httpx.ASGITransport(app=app.app)
        async with httpx.AsyncClient(transport=transport,
                                     base_url="http://localhost") as client:
            response = await client.get("/api/search", headers={"Accept-Encoding": "gzip"})
        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.headers.get("content-encoding"), "gzip")
        self.assertLess(int(response.headers["content-length"]), len(response.content))
        self.assertEqual(response.json()["total"], 120)

    async def test_expired_image_link_is_refreshed_on_open(self):
        old = "https://cdn.discordapp.com/attachments/1/2/image.png?ex=00000001"
        fresh = "https://cdn.discordapp.com/attachments/1/2/image.png?ex=ffffffff"
        self.messages[-1]["attachments"] = [{"url": old, "content_type": "image/png"}]
        await self.scrape()
        with patch.object(app, "discord", return_value=FakeResponse({
                "attachments": [{"url": fresh, "content_type": "image/png"}]
        })) as fetch:
            response = await app.open_image(120, 0)
        self.assertEqual(response.headers["location"], fresh)
        fetch.assert_awaited_once_with("GET", "/channels/c1/messages/120", "test-token")

    async def test_profile_harvest_stops_after_exhausted_rate_limit(self):
        await self.scrape()
        calls = []

        async def limited(method, path, token):
            calls.append(path)
            response = FakeResponse({})
            response.status_code = 429
            return response

        app.active_jobs["limited-profile"] = {"cancelled": False}
        db = await app._write_db()
        try:
            with patch.object(app, "discord", side_effect=limited):
                saved = await app._harvest_profiles(
                    db, "c1", "test-token", "limited-profile", asyncio.Queue())
        finally:
            await db.close()
            app.active_jobs.pop("limited-profile", None)
        self.assertEqual(saved, 0)
        self.assertEqual(len(calls), 1)


class MigrationTests(unittest.TestCase):
    def test_prior_normalized_schema_migrates_without_losing_metadata(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "searchcord.db"
            with closing(sqlite3.connect(path)) as db:
                db.execute("""CREATE TABLE messages (
                    id TEXT PRIMARY KEY, channel_id TEXT NOT NULL, guild_id TEXT,
                    author_id TEXT NOT NULL, content TEXT NOT NULL,
                    timestamp TEXT NOT NULL, attachments TEXT NOT NULL,
                    guild_name_id INTEGER, channel_name_id INTEGER,
                    author_name_id INTEGER)""")
                db.execute("""INSERT INTO messages VALUES
                    ('123','10','20','30','the entire message','2026-09-22T00:00:00Z',
                     '["https://example.test/image.webp"]',1,2,3)""")
                db.execute("CREATE TABLE guilds(id TEXT PRIMARY KEY,name TEXT NOT NULL)")
                db.execute("INSERT INTO guilds VALUES ('20','Current guild')")
                db.execute("CREATE TABLE channels(id TEXT PRIMARY KEY,guild_id TEXT,name TEXT NOT NULL)")
                db.execute("INSERT INTO channels VALUES ('10','20','Current channel')")
                db.execute("""CREATE TABLE authors(id TEXT PRIMARY KEY,name TEXT NOT NULL,
                    last_message_id TEXT NOT NULL DEFAULT '0')""")
                db.execute("INSERT INTO authors VALUES ('30','Current author','123')")
                db.execute("CREATE TABLE name_values(id INTEGER PRIMARY KEY,value TEXT UNIQUE)")
                db.execute("INSERT INTO name_values VALUES (1,'Old guild')")
                db.commit()
            migrated = init_database(str(path))
            self.assertTrue(migrated["migrated"])
            with closing(sqlite3.connect(path)) as db:
                self.assertEqual(db.execute("PRAGMA integrity_check").fetchone()[0], "ok")
                self.assertEqual(db.execute("SELECT id,content,image_urls FROM messages").fetchone(),
                                 (123, "the entire message", "https://example.test/image.webp"))
                self.assertEqual(db.execute("""SELECT guild_name,channel_name,author_name
                    FROM message_records""").fetchone(),
                                 ("Current guild", "Current channel", "Current author"))
                self.assertIsNone(db.execute("""SELECT 1 FROM sqlite_master
                    WHERE type='table' AND name='name_values'""").fetchone())

    def test_backup_and_normalization_preserve_rows(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "searchcord.db"
            with closing(sqlite3.connect(path)) as db:
                db.execute("""CREATE TABLE messages (
                    id TEXT PRIMARY KEY, channel_id TEXT, channel_name TEXT,
                    guild_id TEXT, guild_name TEXT, author_id TEXT,
                    author_name TEXT, content TEXT, timestamp TEXT, attachments TEXT)""")
                db.executemany("INSERT INTO messages VALUES (?,?,?,?,?,?,?,?,?,?)", [
                    (str(i), "c1", "general-new" if i == 3 else "general",
                     "g1", "Guild New" if i == 3 else "Guild", "u1",
                     "User New" if i == 3 else "User",
                     f"message {i}", "2026-09-22T00:00:00+00:00",
                     '["https://example.test/3.png", "https://example.test/3.pdf"]'
                     if i == 3 else "[]")
                    for i in range(1, 4)
                ])
                db.execute("CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT)")
                db.execute("INSERT INTO settings VALUES ('token', 'fixture')")
                db.commit()
            result = init_database(str(path))
            self.assertTrue(result["migrated"])
            self.assertIsNone(result["backup"])
            self.assertEqual(list(path.parent.glob("*.pre-compact-*.bak")), [])
            with closing(sqlite3.connect(path)) as db:
                self.assertEqual(db.execute("PRAGMA integrity_check").fetchone()[0], "ok")
                self.assertEqual(db.execute("SELECT COUNT(*) FROM messages").fetchone()[0], 3)
                self.assertEqual(db.execute("""SELECT guild_name, channel_name,
                    author_name FROM message_records WHERE id='1'""").fetchone(),
                                 ("Guild New", "general-new", "User New"))
                self.assertEqual(db.execute("""SELECT guild_name, channel_name,
                    author_name FROM message_records WHERE id='3'""").fetchone(),
                                 ("Guild New", "general-new", "User New"))
                self.assertNotIn("guild_name", [row[1] for row in db.execute("PRAGMA table_info(messages)")])
                self.assertEqual(db.execute("SELECT image_urls FROM messages WHERE id=3").fetchone()[0],
                                 "https://example.test/3.png")
                self.assertEqual(db.execute("SELECT newest_message_id FROM scrape_cursors").fetchone()[0], "3")
                self.assertEqual(db.execute("SELECT history_complete FROM scrape_cursors").fetchone()[0], 0)
                self.assertEqual(db.execute("""SELECT count FROM stats_counts
                    WHERE kind='total' AND key=''""").fetchone()[0], 3)
                self.assertEqual(db.execute("SELECT value FROM settings WHERE key='token'").fetchone()[0], "fixture")
                self.assertEqual(db.execute("SELECT COUNT(*) FROM messages_fts WHERE content LIKE '%message 2%'").fetchone()[0], 1)
            self.assertFalse(init_database(str(path))["migrated"])


class RateLimitTests(unittest.IsolatedAsyncioTestCase):
    async def test_retry_after_over_sixty_seconds_is_respected(self):
        limited = FakeResponse({"retry_after": 65.0})
        limited.status_code = 429
        limited.headers = {"Retry-After": "65"}
        ok = FakeResponse([])
        ok.headers = {}
        client = type("Client", (), {"request": AsyncMock(side_effect=[limited, ok])})()
        with patch.object(app, "http_client", client), patch.object(
                app.asyncio, "sleep", new_callable=AsyncMock) as sleep:
            response = await app.discord("GET", "/channels/c1/messages", "test-token")
        self.assertIs(response, ok)
        self.assertIn(65.0, [call.args[0] for call in sleep.await_args_list])


if __name__ == "__main__":
    unittest.main()
