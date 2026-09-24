"""Disposable, synthetic write benchmark. Run with the project requirements installed."""

import asyncio
import sqlite3
import statistics
import tempfile
import time
from unittest.mock import patch
from contextlib import closing
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import app
from storage import init_database


COUNT = 10_000
PAGE = 100
MESSAGES = [
    {"id": str(10**18 + i), "author": {"id": f"u{i % 250}",
     "username": f"user{i % 250}"}, "content": f"synthetic message {i}",
     "timestamp": "2026-09-22T00:00:00+00:00", "attachments": []}
    for i in range(COUNT)
]


async def legacy(path):
    import aiosqlite

    with closing(sqlite3.connect(path)) as db:
        db.execute("PRAGMA journal_mode=WAL")
        db.execute("""CREATE TABLE messages (id TEXT PRIMARY KEY, channel_id TEXT,
            channel_name TEXT, guild_id TEXT, guild_name TEXT, author_id TEXT,
            author_name TEXT, content TEXT, timestamp TEXT, attachments TEXT)""")
        for name, column in (("ch", "channel_id"), ("gd", "guild_id"),
                             ("au", "author_id"), ("ts", "timestamp")):
            db.execute(f"CREATE INDEX idx_{name} ON messages({column})")
        db.commit()

    start = time.perf_counter()
    for i in range(0, COUNT, PAGE):
        rows = [(m["id"], "c1", "general", "g1", "Guild", m["author"]["id"],
                 m["author"]["username"], m["content"], m["timestamp"], "[]")
                for m in MESSAGES[i:i + PAGE]]
        async with aiosqlite.connect(path) as db:
            await db.executemany("INSERT OR IGNORE INTO messages VALUES (?,?,?,?,?,?,?,?,?,?)", rows)
            await db.commit()
    elapsed = time.perf_counter() - start
    with closing(sqlite3.connect(path)) as db:
        repeated_bytes = db.execute("""SELECT SUM(LENGTH(channel_name) +
            LENGTH(guild_name) + LENGTH(author_name)) FROM messages""").fetchone()[0]
    return elapsed, Path(path).stat().st_size, repeated_bytes


async def current(path):
    init_database(path)
    app.DB_PATH = str(path)
    db = await app._write_db()
    try:
        start = time.perf_counter()
        for i in range(0, COUNT, PAGE):
            await app._save_messages(db, MESSAGES[i:i + PAGE], "c1", "general",
                                     "g1", "Guild", history_page=True)
        elapsed = time.perf_counter() - start
    finally:
        await db.close()
    return elapsed, Path(path).stat().st_size


async def profiles(path):
    init_database(path)
    with closing(sqlite3.connect(path)) as db:
        db.executemany("INSERT INTO channel_authors VALUES ('c1', ?)",
                       [(f"u{i}",) for i in range(1000)])
        db.commit()
    app.DB_PATH = str(path)
    app.active_jobs["benchmark"] = {"cancelled": False}
    requests = 0

    async def fake_discord(method, endpoint, token):
        nonlocal requests
        requests += 1
        user_id = endpoint.split("/")[-1]
        return type("Response", (), {"status_code": 200, "json": lambda self: {
            "id": user_id, "username": user_id, "global_name": None}})()

    db = await app._write_db()
    try:
        with patch.object(app, "discord", side_effect=fake_discord):
            start = time.perf_counter()
            first = await app._harvest_profiles(db, "c1", "synthetic", "benchmark",
                                                asyncio.Queue(maxsize=1000))
            first_seconds = time.perf_counter() - start
            start = time.perf_counter()
            second = await app._harvest_profiles(db, "c1", "synthetic", "benchmark",
                                                 asyncio.Queue(maxsize=1000))
            second_seconds = time.perf_counter() - start
    finally:
        await db.close()
        app.active_jobs.pop("benchmark", None)
    return first, first_seconds, second, second_seconds, requests


async def profile_connection_proxy(path):
    """Apply the old per-write connection pattern to profile rows."""
    import aiosqlite

    init_database(path)
    start = time.perf_counter()
    for i in range(1000):
        async with aiosqlite.connect(path) as db:
            await db.execute("""INSERT INTO profiles
                (user_id, username, fetched_at) VALUES (?, ?, ?)""",
                (f"u{i}", f"u{i}", "2026-09-22T00:00:00+00:00"))
            await db.commit()
    return time.perf_counter() - start


async def main():
    results = {"legacy": [], "current": []}
    for _ in range(5):
        with tempfile.TemporaryDirectory() as temp:
            results["legacy"].append(await legacy(Path(temp) / "legacy.db"))
        with tempfile.TemporaryDirectory() as temp:
            results["current"].append(await current(Path(temp) / "current.db"))
    for name, runs in results.items():
        seconds = statistics.median(run[0] for run in runs)
        print(f"{name}: {COUNT / seconds:,.0f} messages/s, {seconds:.3f} s / {COUNT}, "
              f"file {statistics.median(run[1] for run in runs):,.0f} bytes")
    print(f"legacy repeated name bytes: {results['legacy'][0][2]:,}")
    with tempfile.TemporaryDirectory() as temp:
        first, first_seconds, second, second_seconds, requests = await profiles(
            Path(temp) / "profiles.db")
    with tempfile.TemporaryDirectory() as temp:
        proxy_seconds = await profile_connection_proxy(Path(temp) / "profile-proxy.db")
    print(f"profiles old connection-pattern proxy: {1000 / proxy_seconds:,.0f} saves/s "
          f"(1000 saves, {proxy_seconds:.3f} s; no network or lookup)")
    print(f"profiles (mocked API): first pass {first / first_seconds:,.0f} saves/s "
          f"({first} saves, {first_seconds:.3f} s); repeat {second} saves, "
          f"{second_seconds * 1000:.2f} ms; {requests} total fetches")


if __name__ == "__main__":
    asyncio.run(main())
