"""Read-only timing probe for a legacy Searchcord database.

Prints aggregate timings only. It never reads settings or returns message rows.
Usage: python benchmarks/large_readonly.py PATH_TO_SEARCHCORD_DB
"""

import argparse
import sqlite3
import tempfile
import time
from contextlib import closing
from pathlib import Path


def timed(db, label, sql, params=()):
    start = time.perf_counter()
    db.execute(sql, params).fetchall()
    print(f"{label}: {time.perf_counter() - start:.3f}s", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("database", type=Path)
    args = parser.parse_args()
    path = args.database.resolve(strict=True)
    wal = Path(f"{path}-wal")
    if wal.exists() and wal.stat().st_size:
        parser.error("Database has a nonempty WAL; immutable reads could miss data")

    # Immutable skips Windows locking and guarantees SQLite cannot modify the
    # database or create sidecars beside a database in another checkout.
    with closing(sqlite3.connect(path.as_uri() + "?mode=ro&immutable=1", uri=True)) as db:
        db.execute("PRAGMA query_only=ON")
        db.execute("PRAGMA temp_store=FILE")
        print(f"database_bytes: {path.stat().st_size}")
        timed(db, "count_all", "SELECT COUNT(*) FROM messages")
        timed(db, "count_guilds", "SELECT COUNT(DISTINCT guild_id) FROM messages")
        timed(db, "count_channels", "SELECT COUNT(DISTINCT channel_id) FROM messages")
        timed(db, "count_authors", "SELECT COUNT(DISTINCT author_id) FROM messages")
        timed(db, "top_authors", """SELECT author_id, COUNT(*) FROM messages
            GROUP BY author_id ORDER BY COUNT(*) DESC LIMIT 3""")
        timed(db, "top_guilds", """SELECT guild_id, COUNT(*) FROM messages
            GROUP BY guild_id ORDER BY COUNT(*) DESC LIMIT 10""")
        timed(db, "by_day_30", """SELECT DATE(timestamp), COUNT(*) FROM messages
            WHERE timestamp >= DATE('now', '-30 days')
            GROUP BY DATE(timestamp)""")
        timed(db, "by_hour", """SELECT strftime('%H', timestamp), COUNT(*)
            FROM messages GROUP BY 1""")
        timed(db, "search_first_page", """SELECT id FROM messages
            ORDER BY timestamp DESC LIMIT 50""")
        timed(db, "search_unmatched_count", """SELECT COUNT(*) FROM messages
            WHERE content LIKE '%zzzzzzzzzzzzzzzzzzzz%'""")
        timed(db, "filter_guilds", """SELECT DISTINCT guild_id, guild_name
            FROM messages ORDER BY guild_name""")
        timed(db, "filter_channels", """SELECT DISTINCT channel_id,
            channel_name, guild_id FROM messages ORDER BY guild_id, channel_name""")
        # Return one row to Python while SQLite performs the full distinct/
        # sort. This measures the endpoint's expensive author lookup without
        # reading the resulting names or IDs into the benchmark process.
        start = time.perf_counter()
        cursor = db.execute("""SELECT DISTINCT author_id, author_name
            FROM messages ORDER BY author_name""")
        cursor.fetchone()
        print(f"filter_authors_start: {time.perf_counter() - start:.3f}s")

    # Materialize only aggregate counts into a disposable database. Source
    # messages and settings remain in the original read-only file throughout.
    with tempfile.TemporaryDirectory() as temp:
        aggregate_path = Path(temp) / "aggregate.db"
        with closing(sqlite3.connect(aggregate_path, uri=True)) as aggregate:
            aggregate.execute("PRAGMA temp_store=FILE")
            aggregate.execute("ATTACH DATABASE ? AS source",
                              (path.as_uri() + "?mode=ro&immutable=1",))
            aggregate.execute("""CREATE TABLE stats_counts (
                kind TEXT NOT NULL, key TEXT NOT NULL, count INTEGER NOT NULL,
                PRIMARY KEY(kind, key)) WITHOUT ROWID""")
            start = time.perf_counter()
            aggregate.execute("""INSERT INTO stats_counts
                SELECT 'total', '', COUNT(*) FROM source.messages""")
            for kind, expression in (
                ("guild", "COALESCE(guild_id, '')"),
                ("channel", "channel_id"),
                ("author", "author_id"),
                ("day", "COALESCE(DATE(timestamp), '')"),
                ("hour", "COALESCE(strftime('%H', timestamp), '')"),
            ):
                aggregate.execute(f"""INSERT INTO stats_counts
                    SELECT ?, {expression}, COUNT(*) FROM source.messages
                    GROUP BY {expression}""", (kind,))
            aggregate.commit()
            print(f"aggregate_build: {time.perf_counter() - start:.3f}s")
            print(f"aggregate_bytes: {aggregate_path.stat().st_size}")
            timed(aggregate, "cached_count_all", """SELECT count FROM stats_counts
                WHERE kind='total' AND key=''""")
            timed(aggregate, "cached_distincts", """SELECT kind, COUNT(*)
                FROM stats_counts WHERE kind IN ('guild','channel','author')
                AND key<>'' AND count>0 GROUP BY kind""")
            timed(aggregate, "cached_top_authors", """SELECT key, count
                FROM stats_counts WHERE kind='author' ORDER BY count DESC LIMIT 3""")
            timed(aggregate, "cached_top_guilds", """SELECT key, count
                FROM stats_counts WHERE kind='guild' ORDER BY count DESC LIMIT 10""")
            timed(aggregate, "cached_by_day_30", """SELECT key, count
                FROM stats_counts WHERE kind='day'
                AND key >= DATE('now','-30 days') ORDER BY key""")
            timed(aggregate, "cached_by_hour", """SELECT key, count
                FROM stats_counts WHERE kind='hour' ORDER BY key""")


if __name__ == "__main__":
    main()
