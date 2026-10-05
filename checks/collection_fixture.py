"""Run the real app against synthetic Discord and a disposable SQLite archive.

Usage: python checks/collection_fixture.py --port 8016
Only this test process exposes /__checks/state. No production data is opened.
"""

import argparse
import asyncio
from contextlib import asynccontextmanager, closing
from pathlib import Path
import sqlite3
import sys
import tempfile
import time

import httpx
import uvicorn

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import app
from channel_access import READABLE, READ_MESSAGE_HISTORY, VIEW_CHANNEL


parser = argparse.ArgumentParser()
parser.add_argument("--port", type=int, default=8016)
args = parser.parse_args()
directory = tempfile.TemporaryDirectory(prefix="searchcord-collection-check-")
app.DB_PATH = str(Path(directory.name) / "searchcord.db")
requests = []
join_times = []
slow_scrapes = False
fail_probes = False
page_scrapes = False


def channel(cid, name, deny=0, guild_id="10"):
    return {"id": cid, "name": name, "type": 0, "position": int(cid),
            "permission_overwrites": [{"id": guild_id, "type": 0, "allow": "0", "deny": str(deny)}]}


async def reply(request):
    path = request.url.path.removeprefix("/api/v10")
    params = dict(request.url.params)
    requests.append({"method": request.method, "path": path, "params": params, "at": time.monotonic()})
    if path == "/users/@me":
        body = {"id": "30", "username": "fixture-account"}
    elif path == "/users/@me/guilds":
        body = [{"id": "10", "name": "Fixture server", "icon": None},
                {"id": "11", "name": "Second server", "icon": None}]
        if join_times:
            body.append({"id": "12", "name": "Joined server", "icon": None})
    elif path == "/users/@me/channels":
        body = [{"id": "90", "type": 1, "recipients": [{"id": "31", "username": "fixture-dm"}]}]
    elif path in ("/guilds/10", "/guilds/11", "/guilds/12"):
        gid = path.rsplit("/", 1)[1]
        body = {"id": gid, "owner_id": "99", "roles": [{"id": gid, "permissions": str(READABLE)}]}
    elif "/members/30" in path:
        body = {"user": {"id": "30"}, "roles": []}
    elif path == "/guilds/10/channels":
        body = [channel("20", "general"), channel("21", "empty"), channel("22", "view-only", READ_MESSAGE_HISTORY),
                channel("23", "hidden", VIEW_CHANNEL), channel("24", "RoBOT-commands"), channel("25", "denied")]
    elif path == "/guilds/11/channels":
        body = [channel("40", "second-general", guild_id="11")]
    elif path == "/guilds/12/channels":
        body = [channel("50", "joined-general", guild_id="12")]
    elif path.endswith("/threads/active"):
        body = {"threads": []}
    elif "/threads/archived/" in path:
        body = {"threads": [], "has_more": False}
    elif path.endswith("/messages"):
        cid = path.split("/")[2]
        if cid == "25":
            return httpx.Response(403, json={"message": "Missing Access"})
        if params.get("limit") == "1":
            if fail_probes:
                return httpx.Response(503, json={})
            body = []
        else:
            if slow_scrapes:
                await asyncio.sleep(1.5)
            if page_scrapes and cid == "20":
                first = int(params.get("before", 1000000000000000501)) - 1
                body = [{"id": str(first - index), "content": "synthetic page check",
                         "author": {"id": "30", "username": "fixture-author"}, "attachments": []}
                        for index in range(int(params["limit"]))]
            else:
                body = [] if cid == "21" or "before" in params or "after" in params else [
                    {"id": str(1000000000000000000 + int(cid)), "content": "synthetic collection check",
                     "author": {"id": "30", "username": "fixture-author"}, "attachments": []}]
    elif path.startswith("/invites/"):
        if path.endswith("expired"):
            return httpx.Response(404, json={"message": "Unknown Invite"})
        if request.method == "POST":
            join_times.append(time.monotonic())
        body = {"guild": {"id": "12", "name": "Joined server"}}
    else:
        return httpx.Response(404, json={"message": "Unexpected fixture endpoint"})
    return httpx.Response(200, json=body)


production_lifespan = app.app.router.lifespan_context


@asynccontextmanager
async def fixture_lifespan(asgi_app):
    async with production_lifespan(asgi_app):
        await app.http_client.aclose()
        app.http_client = httpx.AsyncClient(transport=httpx.MockTransport(reply))
        with closing(sqlite3.connect(app.DB_PATH)) as db, db:
            db.execute("INSERT INTO settings VALUES ('token','synthetic-token')")
        try:
            yield
        finally:
            for job in app.active_jobs.values():
                job["cancelled"] = True


app.app.router.lifespan_context = fixture_lifespan
production_run_scrape = app.run_scrape


async def delayed_scrape(*args, **kwargs):
    # A controlled waiting job lets the browser exercise queue changes while
    # a batch is active, independently of the shared Discord request lock.
    if slow_scrapes:
        await asyncio.sleep(3)
    await production_run_scrape(*args, **kwargs)


app.run_scrape = delayed_scrape


@app.app.get("/__checks/state")
async def state():
    with closing(sqlite3.connect(app.DB_PATH)) as db:
        stored = dict(db.execute("SELECT channel_id, COUNT(*) FROM messages GROUP BY channel_id"))
    return {"requests": requests, "join_intervals": [b - a for a, b in zip(join_times, join_times[1:])],
            "joins": len(join_times), "stored": stored,
            "running_jobs": sum(bool(j.get("running")) for j in app.active_jobs.values())}


@app.app.post("/__checks/scenario")
async def scenario(body: dict):
    global slow_scrapes, fail_probes, page_scrapes
    slow_scrapes = body.get("slow_scrapes", False)
    fail_probes = body.get("fail_probes", False)
    page_scrapes = body.get("page_scrapes", False)
    if body.get("break_next"):
        app.scrape_confirmed_requests = 99
    return {"ok": True}


# Keep fixture endpoints ahead of the production static catch-all.
for route in list(app.app.router.routes):
    if getattr(route, "path", "").startswith("/__checks/"):
        app.app.router.routes.remove(route)
        app.app.router.routes.insert(0, route)


if __name__ == "__main__":
    try:
        uvicorn.run(app.app, host="127.0.0.1", port=args.port, access_log=False)
    finally:
        directory.cleanup()
