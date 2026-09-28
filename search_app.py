"""Read-only archive search. Run separately from the collection application."""
import argparse
from contextlib import contextmanager
from datetime import date, datetime, timedelta, timezone
from pathlib import Path
import os
import re
import sqlite3
import time
from typing import Literal, Optional

from fastapi import FastAPI, HTTPException, Query
from fastapi.middleware.gzip import GZipMiddleware
from fastapi.responses import FileResponse
from fastapi.staticfiles import StaticFiles
from profile_store import read_profile, profile_servers, search_label

ROOT = Path(__file__).resolve().parent
EPOCH_MS = 1420070400000
MAX_ID = 9223372036854775807
WINDOW = 16384


def snowflake_day(value: str, end=False) -> int:
    try:
        moment = datetime.combine(date.fromisoformat(value), datetime.min.time(), timezone.utc)
        if end:
            moment += timedelta(days=1)
        return max(0, min(MAX_ID, (int(moment.timestamp() * 1000) - EPOCH_MS) << 22))
    except (ValueError, OverflowError) as exc:
        raise HTTPException(400, "Enter a valid UTC date.") from exc


def like_literal(value):
    return value.replace("!", "!!").replace("%", "!%").replace("_", "!_")


class Archive:
    def __init__(self, path, immutable=False):
        self.path = Path(path).resolve()
        self.immutable = immutable

    @contextmanager
    def connect(self):
        if not self.path.is_file():
            raise HTTPException(503, "Archive not found. Set SEARCHCORD_DATA_DIR to an existing archive.")
        wal = Path(str(self.path) + "-wal")
        if self.immutable and wal.exists() and wal.stat().st_size:
            raise HTTPException(503, "Snapshot mode needs a closed archive with no pending WAL. Use normal read-only mode for a live archive.")
        db = None
        try:
            uri = self.path.as_uri() + "?mode=ro" + ("&immutable=1" if self.immutable else "")
            db = sqlite3.connect(uri, uri=True, timeout=1)
            db.row_factory = sqlite3.Row
            db.execute("PRAGMA query_only=ON")
            deadline = time.monotonic() + 4
            db.set_progress_handler(lambda: time.monotonic() > deadline, 1000)
            if db.execute("PRAGMA user_version").fetchone()[0] != 9:
                raise HTTPException(503, "This display app needs a schema v9 archive. Upgrade a copy with the collection app first.")
            db.execute("BEGIN")
            yield db
        except sqlite3.OperationalError as exc:
            if "interrupt" in str(exc):
                raise HTTPException(408, "This search is too broad. Add a server, channel, author, or date filter and try again.") from exc
            raise HTTPException(503, "The archive could not be read. Check its location and access permissions.") from exc
        finally:
            if db is not None:
                db.close()

    def search(self, q="", guild_id=None, channel_id=None, author_id=None,
               date_from=None, date_to=None, before=None, limit=40):
        started = time.perf_counter()
        q = q.strip()
        # A literal run preserves trigram acceleration, including queries with % or _.
        runs = re.findall(r"[^%_]+", q)
        anchor = max(runs, key=len, default="")
        conditions, params = [], []
        for column, value in (("guild_id", guild_id), ("channel_id", channel_id), ("author_id", author_id)):
            if value:
                if not value.isascii() or not value.isdigit() or not 0 < int(value) <= MAX_ID:
                    raise HTTPException(400, "Choose a filter suggestion or enter a valid Discord ID.")
                conditions.append(column + "=?")
                params.append(int(value))
        lower = snowflake_day(date_from) if date_from else None
        upper = snowflake_day(date_to, True) if date_to else None
        if lower is not None and upper is not None and lower >= upper:
            raise HTTPException(400, "From date must be on or before the to date.")
        for clause, value in (("id>=?", lower), ("id<?", upper), ("id<?", before)):
            if value is not None:
                conditions.append(clause)
                params.append(value)
        where = " AND ".join(conditions) or "1"
        take = limit + 1
        with self.connect() as db:
            if q:
                boundary = db.execute(f"SELECT id FROM messages WHERE {where} ORDER BY id DESC LIMIT 1 OFFSET ?", params + [WINDOW-1]).fetchone()
                floor = boundary[0] if boundary else 0
                literal = "%" + like_literal(q) + "%"
                ids = [r[0] for r in db.execute(
                    f"SELECT id FROM messages WHERE {where} AND id>=? AND content LIKE ? ESCAPE '!' ORDER BY id DESC LIMIT ?",
                    params + [floor, literal, take])]
                if len(ids) < take and boundary:
                    indexed = len(anchor) >= 3
                    candidate = "AND rowid IN (SELECT rowid FROM messages_fts WHERE content LIKE ?) " if indexed else ""
                    ids += [r[0] for r in db.execute(
                        f"SELECT id FROM messages WHERE {where} AND id<? " + candidate +
                        "AND content LIKE ? ESCAPE '!' ORDER BY id DESC LIMIT ?",
                        params + [floor] + (["%" + anchor + "%"] if indexed else []) + [literal, take-len(ids)])]
            else:
                ids = [r[0] for r in db.execute(f"SELECT id FROM messages WHERE {where} ORDER BY id DESC LIMIT ?", params + [take])]
            has_more = len(ids) > limit
            ids = ids[:limit]
            messages = []
            if ids:
                author_ids = [r[0] for r in db.execute(
                    "SELECT DISTINCT author_id FROM messages WHERE id IN (" + ",".join("?" for _ in ids) + ")", ids)]
                avatars = {str(r[0]): r[1] for r in db.execute(
                    "SELECT user_id, avatar_hash FROM profiles WHERE user_id IN (" + ",".join("?" for _ in author_ids) + ")", author_ids)}
                rows = db.execute("SELECT * FROM message_records WHERE id IN (" + ",".join("?" for _ in ids) + ") ORDER BY id DESC", ids)
                for row in rows:
                    message = dict(row)
                    message["id"] = str(message["id"])
                    avatar = avatars.get(message["author_id"])
                    message["avatar_url"] = (
                        f"https://cdn.discordapp.com/avatars/{message['author_id']}/{avatar}.png?size=64"
                        if isinstance(avatar, str) and re.fullmatch(r"(?:a_)?[A-Za-z0-9]+", avatar)
                        and str(message['author_id']).isdigit() else None)
                    # No credential lookup, external fetch or expiring-link refresh in display mode.
                    urls = (message.pop("image_urls") or "").splitlines()
                    message["attachments"] = [url for url in urls if re.match(r"^https://(?:cdn|media)\.discordapp\.(?:com|net)/", url)]
                    messages.append(message)
        return {"messages": messages, "has_more": has_more,
                "next_cursor": str(ids[-1]) if has_more else None,
                "elapsed_ms": round((time.perf_counter()-started)*1000, 1)}


def create_app(path=None, immutable=None):
    directory = Path(os.environ.get("SEARCHCORD_DATA_DIR", "data")).expanduser()
    if not directory.is_absolute():
        directory = ROOT / directory
    archive = Archive(path or directory / "searchcord.db", immutable if immutable is not None else os.environ.get("SEARCHCORD_IMMUTABLE") == "1")
    app = FastAPI(title="Searchcord display", docs_url=None, redoc_url=None, openapi_url=None)
    app.add_middleware(GZipMiddleware, minimum_size=1024, compresslevel=5)

    @app.middleware("http")
    async def private_response(request, call_next):
        response = await call_next(request)
        response.headers["Cache-Control"] = "no-store" if request.url.path.startswith("/api/") else "no-cache"
        response.headers["X-Content-Type-Options"] = "nosniff"
        response.headers["Referrer-Policy"] = "no-referrer"
        response.headers["Content-Security-Policy"] = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data: https://cdn.discordapp.com; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'"
        return response

    @app.get("/api/summary")
    def summary():
        with archive.connect() as db:
            total = db.execute("SELECT count FROM stats_counts WHERE kind='total' AND key=''").fetchone()
            servers = db.execute("SELECT COUNT(*) FROM stats_counts WHERE kind='guild' AND key!='' AND count>0").fetchone()[0]
        return {"messages": total[0] if total else 0, "servers": servers}

    @app.get("/api/search")
    def search(q: str = Query("", max_length=200), guild_id: Optional[str] = None,
               channel_id: Optional[str] = None, author_id: Optional[str] = None,
               date_from: Optional[str] = None, date_to: Optional[str] = None,
               before: Optional[int] = Query(None, ge=1, le=MAX_ID),
               limit: int = Query(40, ge=1, le=100)):
        return archive.search(q, guild_id, channel_id, author_id, date_from, date_to, before, limit)

    @app.get("/api/suggestions/{kind}")
    def suggestions(kind: Literal["server", "channel", "author"], q: str = Query("", max_length=100), guild_id: Optional[str] = None):
        table = {"server": "guilds", "channel": "channels", "author": "authors"}[kind]
        if kind == "author" and len(q.strip()) < 2 and not q.strip().isdigit():
            return []
        conditions, params = ["name LIKE ? ESCAPE '!'"], [like_literal(q.strip()) + "%"]
        if kind == "channel" and guild_id:
            conditions.append("guild_id=?")
            params.append(guild_id)
        with archive.connect() as db:
            if kind == 'author':
                return [dict(r) for r in db.execute('''SELECT a.id,a.name,p.username
                    FROM authors a LEFT JOIN profiles p ON p.user_id=a.id WHERE a.id IN (
                    SELECT id FROM authors WHERE name LIKE ? ESCAPE '!'
                    UNION SELECT user_id FROM profiles WHERE username LIKE ? ESCAPE '!'
                    UNION SELECT id FROM authors WHERE id=?)
                    ORDER BY a.name COLLATE NOCASE,a.id LIMIT 20''', (params[0],params[0],q.strip()))]
            db.create_function('search_label', 1, search_label, deterministic=True)
            conditions[0] = "(search_label(name) LIKE ? ESCAPE '!' OR id=?)"
            params = [like_literal(search_label(q))+'%',q.strip()] + params[1:]
            return [dict(r) for r in db.execute(f"SELECT id, name FROM {table} WHERE {' AND '.join(conditions)} ORDER BY name COLLATE NOCASE, id LIMIT 20", params)]

    @app.get('/api/profiles/{uid}')
    def profile(uid: str):
        with archive.connect() as db:
            return read_profile(db, uid)

    @app.get('/api/profiles/{uid}/servers')
    def servers(uid: str, q: str = Query('',max_length=100), offset: int = Query(0,ge=0), limit: int = Query(30,ge=1,le=100)):
        with archive.connect() as db:
            return profile_servers(db, uid, q, offset, limit)

    @app.get('/{asset}', include_in_schema=False)
    def shared_asset(asset: str):
        if asset in ('bauhaus.css','profile-view.css','profile-view.js'):
            return FileResponse(ROOT / 'static' / asset)
        target = ROOT / 'static' / 'search' / asset
        if asset in ('index.html','search.css','search.js','privacy.html','privacy.css'):
            return FileResponse(target)
        raise HTTPException(404, 'Not found')

    app.mount("/", StaticFiles(directory=ROOT / "static" / "search", html=True), name="display")
    return app


app = create_app()

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--data-dir", type=Path, help="Directory containing searchcord.db")
    parser.add_argument("--snapshot", action="store_true", help="Read a closed, unchanging archive without creating SQLite sidecars")
    parser.add_argument("--port", type=int, default=8001)
    args = parser.parse_args()
    import uvicorn
    uvicorn.run(create_app(args.data_dir / "searchcord.db" if args.data_dir else None, True if args.snapshot else None), host="127.0.0.1", port=args.port, access_log=False)
