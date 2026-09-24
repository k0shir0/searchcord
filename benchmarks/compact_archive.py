"""Migrate a disposable copy of a legacy archive and print aggregate timings.

The source is opened only as immutable/read-only and is never modified. The
scratch copy, migration backup, and all message contents are deleted on exit.
Usage: python benchmarks/compact_archive.py PATH_TO_DB
"""

import argparse
import asyncio
import shutil
import sqlite3
import sys
import tempfile
import time
from contextlib import closing
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import app  # noqa: E402
import httpx  # noqa: E402
from storage import init_database  # noqa: E402


def timed(db, sql, params=()):
    start = time.perf_counter()
    result = db.execute(sql, params).fetchall()
    return round(time.perf_counter() - start, 4), result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("database", type=Path)
    args = parser.parse_args()
    source = args.database.resolve(strict=True)
    wal = Path(f"{source}-wal")
    if wal.exists() and wal.stat().st_size:
        parser.error("Source has a nonempty WAL; immutable reads could miss data")
    scratch = Path(__file__).resolve().parents[1] / "data"
    scratch.mkdir(exist_ok=True)
    with closing(sqlite3.connect(source.as_uri() + "?mode=ro&immutable=1", uri=True)) as old:
        old.execute("PRAGMA query_only=ON")
        print("source_bytes", source.stat().st_size, flush=True)
        print("source_count", old.execute("SELECT COUNT(*) FROM messages").fetchone()[0], flush=True)
        seconds, _ = timed(old, """SELECT COUNT(DISTINCT author_id),
            COUNT(DISTINCT channel_id), COUNT(DISTINCT guild_id) FROM messages""")
        print("source_distinct_stats_sql_seconds", seconds, flush=True)

    with tempfile.TemporaryDirectory(prefix="compact-benchmark-", dir=scratch) as temp:
        copy = Path(temp) / "searchcord.db"
        start = time.perf_counter()
        shutil.copyfile(source, copy)
        print("copy_seconds", round(time.perf_counter() - start, 3), flush=True)
        start = time.perf_counter()
        migration = init_database(str(copy))
        print("migration_seconds", round(time.perf_counter() - start, 3), flush=True)
        print("migrated", migration["migrated"], flush=True)
        print("compact_bytes", copy.stat().st_size, flush=True)
        with closing(sqlite3.connect(copy)) as db:
            db.execute("PRAGMA query_only=ON")
            print("integrity", db.execute("PRAGMA integrity_check").fetchone()[0], flush=True)
            print("compact_count", db.execute("SELECT COUNT(*) FROM messages").fetchone()[0], flush=True)
            print("compact_content_bytes", db.execute("SELECT SUM(LENGTH(content)) FROM messages").fetchone()[0], flush=True)
            print("image_message_count", db.execute("SELECT COUNT(*) FROM messages WHERE image_urls IS NOT NULL").fetchone()[0], flush=True)
            print("component_bytes", db.execute("""SELECT name,SUM(pgsize) FROM dbstat
                GROUP BY name ORDER BY SUM(pgsize) DESC""").fetchall(), flush=True)
            for label, sql in (
                    ("stats_count_sql", "SELECT count FROM stats_counts WHERE kind='total' AND key=''"),
                    ("stats_distinct_sql", """SELECT kind,COUNT(*) FROM stats_counts
                        WHERE kind IN ('guild','channel','author') AND key<>'' AND count>0 GROUP BY kind"""),
                    ("stats_top_authors_sql", """SELECT key,count FROM stats_counts
                        WHERE kind='author' AND count>0 ORDER BY count DESC LIMIT 3"""),
                    ("search_first_page_sql", "SELECT id FROM messages ORDER BY id DESC LIMIT 50"),
                    ("search_no_match_sql", """SELECT COUNT(*) FROM messages WHERE id IN
                        (SELECT rowid FROM messages_fts WHERE content LIKE '%zzzzzzzzzzzzzzzzzzzz%')""")):
                seconds, _ = timed(db, sql)
                print(label, seconds, flush=True)

        async def stats_api():
            with patch.object(app, "DB_PATH", str(copy)):
                start = time.perf_counter()
                result = await app.get_stats()
                return round(time.perf_counter() - start, 4), result

        seconds, result = asyncio.run(stats_api())
        print("stats_api_seconds", seconds, flush=True)
        print("stats_api_counts", result["total_messages"], result["total_users"],
              result["total_channels"], result["total_servers"], flush=True)

        async def http_probe():
            transport = httpx.ASGITransport(app=app.app)
            with patch.object(app, "DB_PATH", str(copy)):
                async with httpx.AsyncClient(transport=transport,
                                             base_url="http://localhost") as client:
                    for route in ("/api/stats", "/api/search?limit=50"):
                        start = time.perf_counter()
                        plain = await client.get(route, headers={"Accept-Encoding": "identity"})
                        plain_time = time.perf_counter() - start
                        start = time.perf_counter()
                        zipped = await client.get(route, headers={"Accept-Encoding": "gzip"})
                        gzip_time = time.perf_counter() - start
                        print("http", route, "plain_seconds", round(plain_time, 4),
                              "gzip_seconds", round(gzip_time, 4),
                              "plain_bytes", len(plain.content),
                              "wire_bytes", int(zipped.headers.get("content-length", 0)),
                              "encoding", zipped.headers.get("content-encoding", "identity"),
                              flush=True)

        asyncio.run(http_probe())


if __name__ == "__main__":
    main()
