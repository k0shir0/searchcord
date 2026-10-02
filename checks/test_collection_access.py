import unittest

import httpx
from fastapi import HTTPException

from channel_access import ADMINISTRATOR, READABLE, READ_MESSAGE_HISTORY, VIEW_CHANNEL, permissions_for, readable_channels


def channel(channel_id, name="general", overwrites=None, channel_type=0, **extra):
    return {"id": channel_id, "name": name, "type": channel_type,
            "permission_overwrites": overwrites or [], **extra}


def overwrite(target, deny=0, allow=0, kind=0):
    return {"id": target, "type": kind, "deny": str(deny), "allow": str(allow)}


GUILD = {"id": "10", "owner_id": "99", "roles": [
    {"id": "10", "permissions": str(READABLE)},
    {"id": "40", "permissions": "0"}, {"id": "41", "permissions": "0"}]}
MEMBER = {"user": {"id": "30"}, "roles": ["40", "41"]}


class PermissionsTest(unittest.TestCase):
    def test_role_allows_win_over_combined_role_denies_then_member_wins(self):
        c = channel("20", overwrites=[overwrite("10", deny=READABLE),
                                     overwrite("40", deny=READABLE), overwrite("41", allow=READABLE)])
        self.assertEqual(permissions_for(GUILD, MEMBER, c, {"20": c}) & READABLE, READABLE)
        c["permission_overwrites"].append(overwrite("30", deny=READ_MESSAGE_HISTORY, kind=1))
        self.assertEqual(permissions_for(GUILD, MEMBER, c, {"20": c}) & READABLE, VIEW_CHANNEL)

    def test_owner_and_administrator_bypass_overwrites(self):
        c = channel("20", overwrites=[overwrite("10", deny=READABLE)])
        owner = {"user": {"id": "99"}, "roles": []}
        self.assertEqual(permissions_for(GUILD, owner, c, {"20": c}) & READABLE, READABLE)
        guild = {**GUILD, "roles": GUILD["roles"] + [{"id": "42", "permissions": str(ADMINISTRATOR)}]}
        admin = {"user": {"id": "30"}, "roles": ["42"]}
        self.assertEqual(permissions_for(guild, admin, c, {"20": c}) & READABLE, READABLE)

    def test_thread_uses_parent_not_its_own_overwrites(self):
        parent = channel("20", overwrites=[overwrite("30", deny=READ_MESSAGE_HISTORY, kind=1)])
        thread = channel("21", channel_type=11, parent_id="20")
        self.assertEqual(permissions_for(GUILD, MEMBER, thread, {"20": parent}) & READABLE, VIEW_CHANNEL)


class AccessTest(unittest.IsolatedAsyncioTestCase):
    async def test_filters_private_and_view_only_but_keeps_empty_readable_and_bots(self):
        channels = [channel("20"), channel("21", "view-only", [overwrite("10", deny=READ_MESSAGE_HISTORY)]),
                    channel("22", "hidden", [overwrite("10", deny=VIEW_CHANNEL)]),
                    channel("23", "bot-commands"), channel("24", "stale-access"),
                    channel("25", "private-thread", channel_type=12, parent_id="20")]
        probes = []

        async def discord(method, path, token, **kwargs):
            if path == "/guilds/10/channels":
                body = channels
            elif path == "/guilds/10":
                body = GUILD
            elif path == "/users/@me":
                body = {"id": "30"}
            elif path == "/guilds/10/members/30":
                body = MEMBER
            elif path.endswith("/threads/active"):
                body = {"threads": []}
            elif "/threads/archived/" in path:
                body = {"threads": [], "has_more": False}
            elif path.endswith("/messages"):
                self.assertEqual(kwargs["params"], {"limit": 1})
                cid = path.split("/")[2]
                probes.append(cid)
                return httpx.Response(403 if cid in ("24", "25") else 200, json=[])
            else:
                self.fail(path)
            return httpx.Response(200, json=body)

        result = await readable_channels("10", "synthetic-token", discord)
        self.assertEqual({c["id"] for c in result}, {"20", "23"})
        self.assertNotIn("21", probes)
        self.assertNotIn("22", probes)

    async def test_discovers_and_paginates_threads_without_duplicates(self):
        parent = channel("20")
        active = channel("21", "active", channel_type=11, parent_id="20")
        archived = channel("22", "archive", channel_type=11, parent_id="20",
                           thread_metadata={"archive_timestamp": "2026-01-01T00:00:00Z"})
        last = channel("23", "older", channel_type=11, parent_id="20")
        cursors = []

        async def discord(method, path, token, **kwargs):
            body = {"/guilds/10/channels": [parent], "/guilds/10": GUILD,
                    "/users/@me": {"id": "30"}, "/guilds/10/members/30": MEMBER}.get(path)
            if path.endswith("/threads/active"):
                body = {"threads": [active]}
            elif path.endswith("/archived/public"):
                cursors.append(kwargs["params"].copy())
                body = ({"threads": [active, archived], "has_more": True}
                        if "before" not in kwargs["params"] else {"threads": [last], "has_more": False})
            elif path.endswith("/archived/private"):
                body = {"threads": [archived], "has_more": False}
            elif path.endswith("/messages"):
                body = []
            return httpx.Response(200, json=body)

        result = await readable_channels("10", "synthetic-token", discord, include_threads=True)
        self.assertEqual({c["id"] for c in result}, {"20", "21", "22", "23"})
        self.assertEqual(len(result), 4)
        self.assertEqual(cursors, [{"limit": 100}, {"limit": 100, "before": "2026-01-01T00:00:00Z"}])

    async def test_transient_probe_does_not_misreport_a_partial_scan(self):
        async def discord(method, path, token, **kwargs):
            if path.endswith("/messages"):
                return httpx.Response(503, json={})
            body = {"/guilds/10/channels": [channel("20")], "/guilds/10": GUILD,
                    "/users/@me": {"id": "30"}, "/guilds/10/members/30": MEMBER,
                    "/guilds/10/threads/active": {"threads": []}}.get(path, {"threads": [], "has_more": False})
            return httpx.Response(200, json=body)

        with self.assertRaises(HTTPException) as raised:
            await readable_channels("10", "synthetic-token", discord)
        self.assertEqual(raised.exception.status_code, 503)


if __name__ == "__main__":
    unittest.main()
