"""Exercise the HTTP scrape path with synthetic Discord pages and a real SQLite archive."""

import sqlite3
import tempfile
import unittest
from contextlib import closing
from pathlib import Path
from unittest.mock import patch

import httpx
from fastapi.testclient import TestClient

import app


BASE_ID = 1000000000000000000
CHANNEL = {"id": "20", "name": "synthetic-channel",
           "guild_id": "10", "guild_name": "synthetic-guild"}


def message(number, content=None):
    return {"id": str(BASE_ID + number),
            "author": {"id": "30", "username": "synthetic-author"},
            "content": content or f"synthetic message {number}",
            "attachments": []}


class ScrapeDedupTest(unittest.TestCase):
    def test_replayed_discord_messages_are_not_saved_twice(self):
        with tempfile.TemporaryDirectory() as directory:
            db_path = Path(directory) / "searchcord.db"
            with patch.object(app, "DB_PATH", str(db_path)), TestClient(app.app) as client:
                with closing(sqlite3.connect(db_path)) as db, db:
                    db.execute("INSERT INTO settings(key,value) VALUES ('token',?)",
                               ("synthetic-test-token",))

                pages = {
                    1: [message(102), message(101),
                        message(101, "changed duplicate in one page"), message(100)],
                    2: [message(103), message(102, "changed replay"),
                        message(101, "changed replay")],
                    3: [message(103)],
                }
                phase = 1
                requests = []
                events = []
                original_emit = app._emit

                def discord_reply(method, url, **kwargs):
                    self.assertEqual(method, "GET")
                    self.assertEqual(url, "https://discord.com/api/v10/channels/20/messages")
                    self.assertEqual(kwargs["headers"]["Authorization"],
                                     "synthetic-test-token")
                    requests.append((phase, dict(kwargs["params"])))
                    return httpx.Response(200, json=pages[phase])

                def record_event(queue, event):
                    events.append(event.copy())
                    original_emit(queue, event)

                def scrape(expected_saved):
                    events.clear()
                    response = client.post("/api/scrape/start", json={"channels": [CHANNEL]})
                    self.assertEqual(response.status_code, 200, response.text)
                    complete = [event for event in events if event["type"] == "complete"]
                    self.assertEqual(len(complete), 1)
                    self.assertEqual(complete[0]["total_messages"], expected_saved)

                def check_archive(expected_total):
                    with closing(sqlite3.connect(db_path)) as db:
                        self.assertEqual(db.execute("SELECT COUNT(*) FROM messages").fetchone()[0],
                                         expected_total)
                        self.assertEqual(db.execute("""SELECT count FROM stats_counts
                            WHERE kind='total' AND key=''""").fetchone()[0], expected_total)
                        self.assertEqual(db.execute("SELECT COUNT(*) FROM messages_fts").fetchone()[0],
                                         expected_total)
                        self.assertEqual(db.execute("PRAGMA integrity_check").fetchone()[0], "ok")
                    search = client.get("/api/search", params={"q": "synthetic"}).json()
                    self.assertEqual(search["total"], expected_total)
                    self.assertEqual(len({item["id"] for item in search["messages"]}),
                                     expected_total)
                    self.assertEqual(client.get("/api/search/filters",
                                                params={"include_users": "false"}).json()
                                     ["total_messages"], expected_total)

                with patch.object(app.http_client, "request", side_effect=discord_reply), \
                        patch.object(app, "_emit", side_effect=record_event), \
                        patch.object(app, "JOB_RETENTION_SECONDS", 0):
                    scrape(3)
                    check_archive(3)

                    # Simulate a saved cursor lagging behind the stored rows.
                    # The next Discord page replays IDs 101 and 102 and adds 103.
                    with closing(sqlite3.connect(db_path)) as db, db:
                        db.execute("""UPDATE scrape_cursors SET newest_message_id=?,
                            history_complete=1 WHERE channel_id=?""",
                                   (str(BASE_ID + 100), CHANNEL["id"]))
                    phase = 2
                    scrape(1)
                    check_archive(4)
                    with closing(sqlite3.connect(db_path)) as db:
                        stored = dict(db.execute("SELECT id, content FROM messages"))
                    self.assertEqual(stored[BASE_ID + 101], "synthetic message 101")
                    self.assertEqual(stored[BASE_ID + 102], "synthetic message 102")

                    phase = 3
                    scrape(0)
                    check_archive(4)

                self.assertEqual(requests, [
                    (1, {"limit": 100}),
                    (2, {"limit": 100, "after": str(BASE_ID + 100)}),
                    (3, {"limit": 100, "after": str(BASE_ID + 103)}),
                ])


if __name__ == "__main__":
    unittest.main()
