"""A stoppable, server-owned invite queue with one attempt per 30 seconds."""

import asyncio
import math
import re
import time
import uuid

import httpx
from fastapi import APIRouter, HTTPException
from pydantic import BaseModel, Field
from collection_state import persist_job, load_jobs


JOIN_INTERVAL_SECONDS = 30
MAX_INVITE_RETRIES = 5
CODE = re.compile(r"[A-Za-z0-9_-]{1,100}\Z")
LINK = re.compile(r"(?:https?://)?(?:www\.)?(?:discord\.gg/|(?:(?:canary|ptb)\.)?discord(?:app)?\.com/invite/)([A-Za-z0-9_-]{1,100})/?(?:[?#][^\s]*)?\Z", re.I)


def parse_invites(text):
    codes = []
    seen = set()
    for index, item in enumerate(re.split(r"[\s,;]+", text.strip()), 1):
        item = item.strip("<>\"'`[]()").rstrip(".")
        if not item:
            continue
        match = LINK.fullmatch(item)
        code = match.group(1) if match else item
        if not (match or CODE.fullmatch(code)):
            raise ValueError(f"Item {index} is not a Discord invite link or code.")
        if code not in seen:
            seen.add(code)
            codes.append(code)
    if not codes:
        raise ValueError("Enter at least one Discord invite link or code.")
    if len(codes) > 200:
        raise ValueError("Use at most 200 unique invites per queue.")
    return codes


class InviteRequest(BaseModel):
    invites: str = Field(min_length=1, max_length=50000)


def invite_router(get_token, discord, *, path=None, credential_id=None, credential_context=None):
    router = APIRouter()
    job = None
    task = None
    next_attempt_at = 0.0

    async def persist():
        if path and job:
            await persist_job(path(), "invite-queue", "invite", {
                "job": job, "next_attempt_at": time.time() + max(0, next_attempt_at - time.monotonic())})

    async def recover():
        nonlocal job, next_attempt_at
        if not path:
            return
        records = await load_jobs(path(), "invite")
        if not records:
            return
        state = records[-1][2]
        job = state["job"]
        next_attempt_at = time.monotonic() + max(0, state.get("next_attempt_at", 0) - time.time())
        if job.get("running"):
            for item in job["items"]:
                if item["status"] == "joining":
                    item.update(status="uncertain", message="Request interrupted. Check Discord; this invite will not be replayed.")
            job.update(paused=True, status="paused", message="Recovered after restart. Resume pending invites when ready.")
            await persist()

    def snapshot():
        if job is None:
            return {"job": None}
        return {"job": {**job, "items": [dict(item) for item in job["items"]],
                        "wait_seconds": max(0, math.ceil(next_attempt_at - time.monotonic()))
                        if job["running"] else 0}}

    async def run(token):
        nonlocal next_attempt_at
        current = None
        try:
            for item in job["items"]:
                if item["status"] not in ("pending", "waiting"):
                    continue
                current = item
                attempts = item.get("attempts", 0)
                while True:
                    if next_attempt_at > time.monotonic():
                        job["message"] = "Waiting before the next join."
                        await asyncio.sleep(max(0, next_attempt_at - time.monotonic()))
                    item["status"] = "joining"
                    job["message"] = "Checking invite and joining server."
                    # Reserve the interval before network I/O so failed attempts,
                    # stopping, and starting another queue cannot bypass it.
                    next_attempt_at = time.monotonic() + JOIN_INTERVAL_SECONDS
                    await persist()
                    invite = await discord("GET", f"/invites/{item['code']}", token,
                                           retry_rate_limits=False)
                    response = invite
                    if invite.status_code == 200:
                        try:
                            guild = invite.json().get("guild")
                        except (ValueError, AttributeError):
                            guild = None
                        if not isinstance(guild, dict) or not guild.get("id"):
                            item.update(status="failed", message="This invite does not identify a server.")
                            break
                        item.update(guild_id=guild["id"], guild_name=guild.get("name", "Server"))
                        # The interval applies to actual POST attempts as well as
                        # queue items. A slow preflight must never bunch joins.
                        next_attempt_at = time.monotonic() + JOIN_INTERVAL_SECONDS
                        await persist()
                        response = await discord("POST", f"/invites/{item['code']}", token,
                                                 retry_rate_limits=False, json={})
                        next_attempt_at = time.monotonic() + JOIN_INTERVAL_SECONDS
                    try:
                        payload = response.json()
                    except ValueError:
                        payload = {}
                    if not isinstance(payload, dict):
                        payload = {}
                    code = payload.get("code")
                    if isinstance(code, int):
                        item["discord_code"] = code
                    else:
                        code = None
                    account_errors = {20001: "Invite joining requires a user account, not a bot token.",
                                      30001: "This account has reached its server limit. Free a server slot in Discord, then retry.",
                                      40001: "Discord rejected the selected token.",
                                      40002: "Complete account verification in Discord, then retry.",
                                      50014: "Discord rejected the selected token.",
                                      340015: "Discord has restricted this account from joining new servers. Check Account Standing in Discord before retrying."}
                    if "captcha_key" in payload or response.status_code == 401 or code in account_errors:
                        item.update(status="blocked", message=("Complete Discord verification, then retry."
                                    if "captcha_key" in payload else account_errors.get(code, "Discord rejected the selected token.")))
                        job.update(status="blocked", message=item["message"])
                        return
                    if response.status_code == 429:
                        attempts += 1
                        item["attempts"] = attempts
                        if attempts <= MAX_INVITE_RETRIES:
                            try:
                                wait = float(payload.get("retry_after", response.headers.get("Retry-After", 1)))
                                if math.isfinite(wait):
                                    next_attempt_at = max(next_attempt_at, time.monotonic() + max(0, wait))
                            except (ValueError, TypeError):
                                pass
                            item.update(status="waiting", message="Discord rate limit; retrying this invite.")
                            await persist()
                            continue
                    if response.status_code in (200, 201, 204):
                        item.update(status="joined", message="Joined server."
                                    if not payload.get("pending") else "Joined; complete server screening in Discord.")
                    else:
                        messages = {400: "Discord could not accept this invite.",
                                    403: "Discord denied this join. Check access or server limits.",
                                    404: "Invite is invalid or expired.",
                                    429: "Invite is still rate limited. Retry it later."}
                        message = ("This account is banned from the invited server." if code == 40007
                                   else messages.get(response.status_code,
                                   f"Could not confirm join (HTTP {response.status_code}); check Discord before retrying."))
                        item.update(status="failed", message=message)
                    break
                await persist()
                current = None
            failures = sum(item["status"] != "joined" for item in job["items"])
            job.update(status="complete", message="Invite queue finished."
                       if not failures else f"Invite queue finished; {failures} invite(s) need attention.")
        except asyncio.CancelledError:
            if current and current["status"] == "joining":
                current.update(status="stopped", message="Stopped during request; check Discord before retrying.")
            if job.get("paused"):
                if current and current["status"] == "stopped":
                    current.update(status="uncertain", message="Request interrupted. Check Discord; this invite will not be replayed.")
                job.update(status="paused", message="Recovered queue paused. Resume pending invites when ready.")
            else:
                job.update(status="stopped", message="Invite queue stopped. Completed joins are kept.")
        except httpx.RequestError:
            if current:
                current.update(status="failed", message="Connection lost; check Discord before retrying.")
            job.update(status="failed", message="Invite queue stopped after a connection failure.")
        except Exception:
            if current:
                current.update(status="failed", message="Could not confirm join; check Discord before retrying.")
            job.update(status="failed", message="Invite queue stopped after an unexpected response.")
        finally:
            job["running"] = bool(job.get("paused"))
            await persist()

    @router.post("/api/invites/start")
    async def start(req: InviteRequest):
        nonlocal job, task
        if job and job["running"]:
            raise HTTPException(409, "An invite queue is already running.")
        try:
            codes = parse_invites(req.invites)
        except ValueError as error:
            raise HTTPException(422, str(error)) from None
        reference, token = (await credential_context() if credential_context else
                            (await credential_id() if credential_id else None, await get_token()))
        # Another request may have started while reading settings.
        if job and job["running"]:
            raise HTTPException(409, "An invite queue is already running.")
        job = {"id": uuid.uuid4().hex, "running": True, "status": "running",
               "paused": False, "credential_id": reference,
               "message": "Starting invite queue.",
               "items": [{"code": code, "status": "pending", "message": "Waiting."} for code in codes]}
        await persist()
        task = asyncio.create_task(run(token))
        return snapshot()

    @router.get("/api/invites/status")
    async def status():
        return snapshot()

    @router.post("/api/invites/{job_id}/stop")
    async def stop(job_id: str):
        if not job or job["id"] != job_id:
            raise HTTPException(404, "Invite queue not found.")
        job["paused"] = False
        if task and not task.done():
            if job.get("running"):
                task.cancel()
            try:
                await task
            except asyncio.CancelledError:
                job.update(running=False, status="stopped", message="Invite queue stopped.")
        job.update(running=False, paused=False, status="stopped", message="Invite queue stopped. Completed joins are kept.")
        await persist()
        return snapshot()

    @router.post("/api/invites/{job_id}/resume")
    async def resume(job_id: str):
        nonlocal task
        if not job or job["id"] != job_id:
            raise HTTPException(404, "Invite queue not found.")
        if not job.get("paused"):
            raise HTTPException(409, "Invite queue is not paused.")
        reference, token = (await credential_context() if credential_context else
                            (await credential_id() if credential_id else None, await get_token()))
        if credential_id and job.get("credential_id") != reference:
            raise HTTPException(409, "Select the account used to start this queue before resuming.")
        if not job.get("paused"):
            raise HTTPException(409, "Invite queue is no longer paused.")
        job.update(paused=False, running=True, status="running", message="Resuming pending invites.")
        await persist()
        task = asyncio.create_task(run(token))
        return snapshot()

    async def shutdown(preserve=False):
        if job and job.get("running"):
            job["paused"] = preserve
        if task and not task.done():
            if job.get("running"):
                task.cancel()
            try:
                await task
            except asyncio.CancelledError:
                job.update(running=False, status="stopped", message="Invite queue stopped.")

        if job and preserve and job.get("running"):
            job.update(paused=True, status="paused")
            await persist()

    return (router, shutdown, recover) if path else (router, shutdown)
