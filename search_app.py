"""Read-only archive search. Run separately from the collection application."""
import argparse
from pathlib import Path
import os
from typing import Literal, Optional

from fastapi import FastAPI, HTTPException, Query
from fastapi.middleware.gzip import GZipMiddleware
from fastapi.responses import FileResponse
from fastapi.staticfiles import StaticFiles
from archive_reader import Archive, MAX_ID, like_literal
from profile_store import search_label
from cli import local_url, print_banner
from media_store import media_router

ROOT = Path(__file__).resolve().parent


def create_app(path=None, immutable=None):
    directory = Path(os.environ.get("SEARCHCORD_DATA_DIR", "data")).expanduser()
    if not directory.is_absolute():
        directory = ROOT / directory
    database = Path(path or os.environ.get("SEARCHCORD_DB") or directory / "searchcord.db").expanduser()
    if not database.is_absolute():
        database = ROOT / database
    archive = Archive(database, immutable if immutable is not None else os.environ.get("SEARCHCORD_IMMUTABLE") == "1")
    app = FastAPI(title="Searchcord display", docs_url=None, redoc_url=None, openapi_url=None)
    app.add_middleware(GZipMiddleware, minimum_size=1024, compresslevel=5)
    app.include_router(media_router(database))

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
               limit: int = Query(40, ge=1, le=100), scan: bool = False):
        return archive.search(q, guild_id, channel_id, author_id, date_from, date_to, before, limit, scan=scan)

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
        return archive.profile(uid)

    @app.get('/api/profiles/{uid}/servers')
    def servers(uid: str, q: str = Query('',max_length=100), offset: int = Query(0,ge=0), limit: int = Query(30,ge=1,le=100)):
        return archive.profile_servers(uid, q, offset, limit)

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
    source = parser.add_mutually_exclusive_group()
    source.add_argument("--data-dir", type=Path, help="Directory containing searchcord.db")
    source.add_argument("--db", type=Path, help="Path to an existing archive .db file")
    parser.add_argument("--snapshot", action="store_true", help="Read a closed, unchanging archive without creating SQLite sidecars")
    parser.add_argument("--port", type=int, default=8001)
    parser.add_argument("--host", default="127.0.0.1", help="Listen address (default: loopback only)")
    args = parser.parse_args()
    import uvicorn
    print_banner()
    print(f"Searchcord: {local_url(args.host, args.port)}", flush=True)
    uvicorn.run(create_app(args.db or (args.data_dir / "searchcord.db" if args.data_dir else None), True if args.snapshot else None), host=args.host, port=args.port, access_log=False)
