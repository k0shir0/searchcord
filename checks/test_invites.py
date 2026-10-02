import asyncio
import unittest
from types import SimpleNamespace
from unittest.mock import patch

import httpx
from fastapi import FastAPI

import invite_api


class ParsingTest(unittest.TestCase):
    def test_mixed_forms_preserve_order_case_and_remove_duplicate_codes(self):
        self.assertEqual(invite_api.parse_invites(
            "<https://discord.gg/Ab_C-12?utm_source=test>, discord.com/invite/second;\n"
            "Ab_C-12 https://www.discordapp.com/invite/third/ fourth"),
            ["Ab_C-12", "second", "third", "fourth"])

    def test_rejects_other_hosts_channel_links_and_empty_lists(self):
        for text in ("", " ", "https://discord.gg.evil.test/a", "https://example.test/invite/a",
                     "https://discord.com/channels/10/20", "https://discord.gg/a/extra"):
            with self.subTest(text=text), self.assertRaises(ValueError):
                invite_api.parse_invites(text)


class InviteTest(unittest.IsolatedAsyncioTestCase):
    async def fixture(self, discord):
        async def token():
            return "synthetic-token"
        router, shutdown = invite_api.invite_router(token, discord)
        app = FastAPI()
        app.include_router(router)
        self.addAsyncCleanup(shutdown)
        client = httpx.AsyncClient(transport=httpx.ASGITransport(app=app), base_url="http://test")
        self.addAsyncCleanup(client.aclose)
        return client

    async def until(self, client, predicate):
        for _ in range(100):
            job = (await client.get("/api/invites/status")).json()["job"]
            if predicate(job):
                return job
            await asyncio.sleep(0)
        self.fail("Invite job did not reach expected state")

    async def test_thirty_second_intervals_failure_continuation_and_same_invite_rate_retry(self):
        now = [0.0]
        calls = []
        limited = [False]

        async def sleep(seconds):
            now[0] += seconds
            await asyncio.sleep(0)

        async def discord(method, path, token, **kwargs):
            calls.append((method, path, now[0]))
            if path.endswith("expired"):
                return httpx.Response(404, json={})
            if method == "POST" and path.endswith("limited") and not limited[0]:
                limited[0] = True
                return httpx.Response(429, json={"retry_after": 75})
            return httpx.Response(200, json={"guild": {"id": "10", "name": "Synthetic"}})

        clock = SimpleNamespace(monotonic=lambda: now[0])
        scheduler = SimpleNamespace(sleep=sleep, create_task=asyncio.create_task, CancelledError=asyncio.CancelledError)
        with patch.object(invite_api, "time", clock), patch.object(invite_api, "asyncio", scheduler):
            client = await self.fixture(discord)
            response = await client.post("/api/invites/start", json={"invites": "first expired limited last"})
            self.assertEqual(response.status_code, 200)
            job = await self.until(client, lambda j: not j["running"])
            self.assertEqual([i["status"] for i in job["items"]], ["joined", "failed", "joined", "joined"])
            posts = [(path, at) for method, path, at in calls if method == "POST"]
            self.assertEqual(posts, [("/invites/first", 0), ("/invites/limited", 60),
                                     ("/invites/limited", 135), ("/invites/last", 165)])
            self.assertNotIn("synthetic-token", str(job))

    async def test_stop_interrupts_wait_and_next_queue_keeps_cooldown(self):
        calls = []

        async def discord(method, path, token, **kwargs):
            calls.append((method, path))
            return httpx.Response(200, json={"guild": {"id": "10", "name": "Synthetic"}})

        client = await self.fixture(discord)
        response = await client.post("/api/invites/start", json={"invites": "first second"})
        job_id = response.json()["job"]["id"]
        await self.until(client, lambda j: j["items"][0]["status"] == "joined")
        conflict = await client.post("/api/invites/start", json={"invites": "third"})
        self.assertEqual(conflict.status_code, 409)
        stopped = await client.post(f"/api/invites/{job_id}/stop")
        self.assertEqual(stopped.json()["job"]["status"], "stopped")
        await client.post("/api/invites/start", json={"invites": "third"})
        job = (await client.get("/api/invites/status")).json()["job"]
        self.assertGreater(job["wait_seconds"], 0)
        self.assertEqual([p for m, p in calls if m == "POST"], ["/invites/first"])

    async def test_verification_blocks_queue_and_no_sensitive_response_is_exposed(self):
        calls = []

        async def discord(method, path, token, **kwargs):
            calls.append((method, path))
            return (httpx.Response(200, json={"guild": {"id": "10"}}) if method == "GET"
                    else httpx.Response(400, json={"captcha_key": ["verification"],
                                                   "captcha_rqtoken": "never-return-this"}))

        client = await self.fixture(discord)
        await client.post("/api/invites/start", json={"invites": "first second"})
        job = await self.until(client, lambda j: not j["running"])
        self.assertEqual(job["status"], "blocked")
        self.assertEqual(len(calls), 2)
        self.assertNotIn("never-return-this", str(job))

    async def test_invalid_input_never_makes_a_discord_request(self):
        async def discord(*args, **kwargs):
            self.fail("Invalid input reached Discord")
        client = await self.fixture(discord)
        response = await client.post("/api/invites/start", json={"invites": "https://example.test/a"})
        self.assertEqual(response.status_code, 422)
        self.assertIsNone((await client.get("/api/invites/status")).json()["job"])

    async def test_account_errors_stop_the_whole_queue_with_actionable_messages(self):
        for code, message in ((40002, "verification"), (30001, "server limit"),
                              (20001, "user account"), (340015, "restricted")):
            with self.subTest(code=code):
                calls = []
                async def discord(method, path, token, **kwargs):
                    calls.append(method)
                    return (httpx.Response(200, json={"guild": {"id": "10"}}) if method == "GET"
                            else httpx.Response(403, json={"code": code, "message": "provider error"}))
                client = await self.fixture(discord)
                await client.post("/api/invites/start", json={"invites": "first second"})
                job = await self.until(client, lambda j: not j["running"])
                self.assertEqual(job["status"], "blocked")
                self.assertIn(message, job["message"])
                self.assertEqual(job["items"][0]["discord_code"], code)
                self.assertEqual(calls, ["GET", "POST"])


if __name__ == "__main__":
    unittest.main()
