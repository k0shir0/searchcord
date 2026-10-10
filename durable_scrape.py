"""Own scrape lifetime and publish progress only after its checkpoint commits.

HTTP controls and the Discord request gate use this interface. Job dictionaries,
cursor stages and commit ordering are private implementation details. Existing
collection_jobs scrape records remain the on-disk representation.
"""
import asyncio
from contextlib import asynccontextmanager
from copy import deepcopy
from dataclasses import dataclass, field
import logging
import sqlite3
import time
import uuid

import httpx
from fastapi import HTTPException

from collection_state import load_jobs, persist_job, save_job
from profile_api import ProfileStopped


class TransientScrapeError(RuntimeError):
    def __init__(self, reason, retry_after=0):
        super().__init__(reason)
        self.retry_after = retry_after


class ScrapeStopped(Exception):
    pass


@dataclass
class _Job:
    id: str
    state: dict
    lock: asyncio.Lock = field(default_factory=asyncio.Lock)
    control: asyncio.Lock = field(default_factory=asyncio.Lock)
    wake: asyncio.Event = field(default_factory=asyncio.Event)
    subscribers: set = field(default_factory=set)
    task: object = None
    pause_requested: bool = False
    stop_requested: bool = False
    suspending: bool = False
    recovered: bool = False
    on_break: bool = False
    notice: object = None
    expiry: object = None


def _project(state, event=None, **changes):
    """The single projection for both durable state and subscriber progress."""
    result = deepcopy(state)
    result.update(changes)
    if event is None:
        return result
    event = deepcopy(event)
    result["last_event"] = event
    kind = event["type"]
    if kind == "channel_start":
        result["index"] = event["index"]
    if "total_messages" in event:
        result["total_messages"] = event["total_messages"]
    if kind == "channel_complete":
        result["completed"] += 1
    if kind in ("progress", "channel_start", "channel_complete", "channel_error"):
        row = result["rows"][result["index"] - 1]
        if kind == "channel_complete" and row.get("type") == "channel_error":
            row["messages"] = event["messages"]
        else:
            result["rows"][result["index"] - 1] = event
    return result


async def _join_commit(operation):
    """Cancellation joins the write, including its committed-state publication."""
    task = asyncio.create_task(operation)
    cancellation = None
    while not task.done():
        try:
            await asyncio.shield(task)
        except asyncio.CancelledError as exc:
            cancellation = exc
    result = task.result()
    if cancellation:
        raise cancellation
    return result


class Scrapes:
    def __init__(self, *, path, credentials, available, connect, fetch, save,
                 profile, media, break_seconds, queue_size=1000, retention=60,
                 retry_base=2, retry_max=60, clock=time.monotonic,
                 sleep=asyncio.sleep, log=None):
        self._path = path
        self._credentials = credentials
        self._available = available
        self._connect = connect
        self._fetch = fetch
        self._save = save
        self._profile = profile
        self._media = media
        self._break_seconds = break_seconds
        self._queue_size = queue_size
        self._retention = retention
        self._retry_base = retry_base
        self._retry_max = retry_max
        self._clock = clock
        self._sleep = sleep
        self._log = log or logging.getLogger("searchcord.scrape")
        self._jobs = {}
        self._closing = False

    @property
    def has_work(self):
        # Remains true until connections, shielded writes and media work finish.
        return any(job.state["running"] for job in self._jobs.values())

    def _job(self, job_id, *, running=False):
        job = self._jobs.get(job_id)
        if job is None:
            raise HTTPException(404, "Job not found")
        if running and (not job.state["running"] or job.stop_requested):
            raise HTTPException(409, "Scrape is no longer running")
        return job

    def snapshot(self, job_id):
        job = self._job(job_id)
        return {"type": "snapshot", "job_id": job_id,
                **deepcopy({key: job.state.get(key) for key in (
                    "running", "cancelled", "pause_requested", "paused", "channels",
                    "limit", "harvest_profiles", "index", "total_messages", "completed", "rows")}),
                "recovered": job.recovered, "break_seconds": self._break_seconds(),
                "last_event": deepcopy(job.notice or job.state.get("last_event"))}

    def active(self):
        return [self.snapshot(job.id) for job in self._jobs.values() if job.state["running"]]

    def _publish(self, job, event):
        for queue in job.subscribers:
            if queue.full():
                queue.get_nowait()
            queue.put_nowait(deepcopy(event))

    def report(self, job_id, event):
        """Transient request/retry notices do not change committed progress."""
        job = self._job(job_id)
        job.notice = deepcopy(event)
        self._publish(job, event)

    def break_wait(self, job_id, seconds):
        job = self._job(job_id)
        if not job.on_break:
            job.on_break = True
            self.report(job_id, {"type": "scrape_break", "wait_seconds": seconds})

    def break_finished(self, job_id):
        job = self._job(job_id)
        if job.on_break:
            job.on_break = False
            self.report(job_id, {"type": "scrape_break_end"})

    @asynccontextmanager
    async def subscribe(self, job_id):
        job = self._job(job_id)
        queue = asyncio.Queue(maxsize=self._queue_size)
        job.subscribers.add(queue)
        try:
            # Registration and snapshot have no await: no missing-event window.
            yield self.snapshot(job_id), queue
        finally:
            job.subscribers.discard(queue)

    async def events(self, job_id):
        terminal = ("complete", "cancelled", "error")
        async with self.subscribe(job_id) as (snapshot, queue):
            yield snapshot
            event = snapshot.get("last_event") or {}
            if event.get("type") in terminal:
                yield event
                return
            while True:
                try:
                    event = await asyncio.wait_for(queue.get(), timeout=25)
                except asyncio.TimeoutError:
                    if job_id not in self._jobs:
                        return
                    event = {"type": "ping"}
                yield event
                if event["type"] in terminal:
                    return

    def _apply(self, job, state, event):
        job.state = state
        if event:
            job.notice = None
            self._publish(job, event)

    async def _transition(self, job, event=None, **changes):
        async with job.lock:
            state = _project(job.state, event, **changes)
            async def commit():
                await persist_job(self._path(), job.id, "scrape", state)
                self._apply(job, state, event)
            await _join_commit(commit())

    async def _token(self, job):
        credential_id, token = await self._credentials()
        if credential_id != job.state["credential_id"]:
            raise HTTPException(409, "Select the account used to start this job before resuming")
        return token

    async def start(self, channels, limit=0, harvest_profiles=False):
        self._available()
        credential_id, _ = await self._credentials()
        self._available()
        if self._closing:
            raise HTTPException(409, "Collector is shutting down")
        ids = {channel["id"] for channel in channels}
        if any(ids & {channel["id"] for channel in job.state["channels"]}
               for job in self._jobs.values() if job.state["running"]):
            raise HTTPException(409, "A selected channel is still being scraped")
        job = _Job(str(uuid.uuid4()), {
            "running": True, "cancelled": False, "pause_requested": False, "paused": False,
            "channels": deepcopy(channels), "limit": limit, "harvest_profiles": harvest_profiles,
            "index": 0, "total_messages": 0, "completed": 0, "rows": [{} for _ in channels],
            "last_event": None, "next_index": 0, "channel_state": None,
            "failed_channels": 0, "credential_id": credential_id, "profile_auth_failed": False})
        self._jobs[job.id] = job
        async with job.control:
            try:
                await self._transition(job)
            except BaseException:
                self._jobs.pop(job.id, None)
                raise
            if not self._closing:
                job.task = asyncio.create_task(self._run(job))
        return job.id

    async def pause(self, job_id):
        job = self._job(job_id, running=True)
        async with job.control:
            return await self._pause(job)

    async def _pause(self, job):
        self._job(job.id, running=True)
        job.pause_requested = True
        try:
            await self._transition(job, pause_requested=True)
        except Exception:
            job.pause_requested = job.state["pause_requested"]
            raise
        job.wake.set()
        return {"ok": True, "paused": job.state["paused"]}

    async def resume(self, job_id):
        job = self._job(job_id, running=True)
        async with job.control:
            return await self._resume(job)

    async def _resume(self, job):
        job_id = job.id
        self._available()
        await self._token(job)
        self._available()
        self._job(job_id, running=True)
        if self._closing:
            raise HTTPException(409, "Collector is shutting down")
        event = {"type": "resumed", "break_seconds": self._break_seconds()} if job.pause_requested else None
        await self._transition(job, event, pause_requested=False, paused=False)
        job.pause_requested = job.recovered = job.suspending = False
        if not job.task or job.task.done():
            job.task = asyncio.create_task(self._run(job))
        job.wake.set()
        return {"ok": True}

    async def stop(self, job_id):
        job = self._job(job_id)
        async with job.control:
            return await self._stop(job)

    async def _stop(self, job):
        job.stop_requested = True
        job.wake.set()
        if job.task and not job.task.done():
            if not job.task.cancelling():
                job.task.cancel()
            await asyncio.gather(job.task, return_exceptions=True)
        # Also handles a worker cancelled before its first turn.
        if job.state["running"]:
            await self._transition(job, {"type": "cancelled", "total_messages": job.state["total_messages"]},
                                   running=False, cancelled=True, paused=False, pause_requested=False)
        return {"ok": True}

    async def recover(self):
        self._closing = False
        for job_id, _, state in await load_jobs(self._path(), "scrape"):
            if state.get("running") and not state.get("cancelled"):
                state.update(paused=True, pause_requested=True)
                self._jobs[job_id] = _Job(job_id, state, pause_requested=True, recovered=True)

    async def suspend(self):
        self._closing = True
        jobs = list(self._jobs.values())
        for job in jobs:
            async with job.control:
                job.suspending = True
                job.pause_requested = True
                job.wake.set()
                if job.task and not job.task.done() and not job.task.cancelling():
                    job.task.cancel()
        await asyncio.gather(*(job.task for job in jobs if job.task), return_exceptions=True)
        for job in jobs:
            if job.state["running"]:
                await self._transition(job, {"type": "paused"}, paused=True, pause_requested=True)

    def reset(self):
        if any(job.task and not job.task.done() for job in self._jobs.values()):
            raise RuntimeError("Suspend scrapes before resetting the owner")
        for job in self._jobs.values():
            if job.expiry:
                job.expiry.cancel()
        self._jobs.clear()

    def request_allowed(self, job_id):
        job = self._job(job_id)
        if job.stop_requested:
            raise ScrapeStopped
        return not job.pause_requested

    async def checkpoint(self, job_id):
        job = self._job(job_id)
        while True:
            if job.stop_requested:
                raise ScrapeStopped
            if not job.pause_requested:
                return
            if not job.state["paused"]:
                await self._transition(job, {"type": "paused"}, paused=True, pause_requested=True)
            job.wake.clear()
            if job.pause_requested and not job.stop_requested:
                await job.wake.wait()

    async def wait(self, job_id, seconds, *, pause=True):
        deadline = self._clock() + seconds
        while True:
            job = self._job(job_id)
            if job.stop_requested:
                raise ScrapeStopped
            if pause:
                await self.checkpoint(job_id)
            remaining = deadline - self._clock()
            if remaining <= 0:
                return
            await self._sleep(min(0.1, remaining))

    async def _retry(self, job, cid, params, stage, operation):
        attempts = 0
        position = next((f"{key}={params[key]}" for key in ("before", "after") if key in params), "start")
        while True:
            if job.stop_requested:
                raise ScrapeStopped
            if stage == "fetch":
                await self.checkpoint(job.id)
            try:
                result = await operation()
                if attempts:
                    self._log.info("job=%s channel=%s %s %s recovered after %s retries", job.id, cid, stage, position, attempts)
                    self.report(job.id, {"type": "retry_resumed", "channel_id": cid, "stage": stage, "attempts": attempts})
                return result
            except Exception as exc:
                if isinstance(exc, TransientScrapeError):
                    reason, upstream_delay = str(exc), exc.retry_after
                elif isinstance(exc, (httpx.RequestError, TimeoutError)):
                    reason, upstream_delay = type(exc).__name__, 0
                elif isinstance(exc, sqlite3.OperationalError) and any(word in str(exc).lower() for word in ("locked", "busy")):
                    reason, upstream_delay = "database busy", 0
                else:
                    raise
                attempts += 1
                delay = max(upstream_delay, min(self._retry_max, self._retry_base * 2 ** min(attempts - 1, 10)))
                self._log.warning("job=%s channel=%s %s %s failed: %s; attempt=%s waiting=%.1fs", job.id, cid, stage, position, reason, attempts, delay)
                self.report(job.id, {"type": "retry_wait", "channel_id": cid, "stage": stage,
                                    "attempt": attempts, "wait_seconds": round(delay, 1), "reason": reason})
                await self.wait(job.id, delay, pause=stage == "fetch")

    async def _page(self, job, db, channel, messages, next_state, *, history, more):
        async with job.lock:
            event = None
            checkpoint = None
            async def save_checkpoint(inserted):
                nonlocal checkpoint, event
                channel_state = {**next_state, "messages": job.state["channel_state"]["messages"] + inserted}
                event = {"type": "progress", "channel": channel["name"], "messages": channel_state["messages"],
                         "total_messages": job.state["total_messages"] + inserted}
                checkpoint = _project(job.state, event, channel_state=channel_state)
                await save_job(db, job.id, "scrape", checkpoint)

            async def commit():
                if messages:
                    await self._save(db, messages, channel["id"], channel["name"],
                        channel.get("guild_id"), channel.get("guild_name"),
                        history_page=history, history_complete=history and not more,
                        pending=(job.state["channel_state"]["anchor"], next_state["before"]) if not history and more else None,
                        clear_pending=not history and not more, checkpoint=save_checkpoint)
                else:
                    await db.execute("BEGIN")
                    try:
                        if history:
                            await db.execute("UPDATE scrape_cursors SET history_complete=1 WHERE channel_id=?", (channel["id"],))
                        elif not more:
                            await db.execute("UPDATE scrape_cursors SET pending_after_message_id=NULL, pending_before_message_id=NULL WHERE channel_id=?", (channel["id"],))
                        await save_checkpoint(0)
                        await db.commit()
                    except BaseException:
                        await db.rollback()
                        raise
                self._apply(job, checkpoint, event)
            await _join_commit(commit())

    async def _profiles(self, job, db, channel, token):
        while job.state["channel_state"].get("pending_profiles") and not job.state.get("profile_auth_failed"):
            await self.checkpoint(job.id)
            state = job.state["channel_state"]
            uid = state["pending_profiles"][0]
            try:
                saved = await self._profile(db, uid, token, job_id=job.id,
                    cancelled=lambda: job.stop_requested,
                    checkpoint=lambda: self.checkpoint(job.id),
                    progress=lambda info: self.report(job.id, {
                        "type": "profile_retry", "channel_id": channel["id"], **info}))
            except HTTPException as exc:
                if exc.status_code == 401:
                    await self._transition(job, {"type": "profile_warning", "message": "Profile HTTP 401"}, profile_auth_failed=True)
                    return
                self.report(job.id, {"type": "profile_warning", "message": f"Profile HTTP {exc.status_code}"})
                saved = False
            state = job.state["channel_state"]
            next_state = {**state, "profiles": state["profiles"] + int(saved),
                          "pending_profiles": state["pending_profiles"][1:]}
            await self._transition(job, {"type": "profile_progress", "channel_id": channel["id"],
                "user_id": uid, "saved": next_state["profiles"]}, channel_state=next_state)

    async def _channel(self, job, db, channel, token):
        if job.state["channel_state"] is None:
            async with db.execute("""SELECT newest_message_id,oldest_message_id,history_complete,
                pending_after_message_id,pending_before_message_id FROM scrape_cursors WHERE channel_id=?""", (channel["id"],)) as cursor:
                saved = await cursor.fetchone()
            state = {"stage": "newer" if saved else "history", "messages": 0, "profiles": 0,
                     "pending_profiles": [], "anchor": str(saved[3] or saved[0]) if saved else None,
                     "before": saved[4] if saved and saved[3] else None,
                     "newest": saved[0] if saved else None, "oldest": saved[1] if saved else None,
                     "history_complete": bool(saved and saved[2]), "resuming_pending": bool(saved and saved[3])}
            await self._transition(job, channel_state=state)
        while True:
            if job.state["harvest_profiles"]:
                await self._profiles(job, db, channel, token)
            await self.checkpoint(job.id)
            state = job.state["channel_state"]
            limit = job.state["limit"]
            fetch = min(100, limit - state["messages"]) if limit else 100
            if fetch <= 0 or state["stage"] == "done":
                return
            params = {"limit": fetch}
            if state["before"]:
                params["before"] = state["before"]
            elif state["stage"] == "newer":
                params["after"] = state["newest"]
            messages = await self._retry(job, channel["id"], params, "fetch",
                lambda: self._fetch(channel["id"], token, params, job.id))
            history = state["stage"] == "history"
            fresh = messages if history else [message for message in messages if int(message["id"]) > int(state["anchor"])]
            more = bool(fresh) and len(messages) == fetch and len(fresh) == len(messages)
            next_state = deepcopy(state)
            next_state["pending_profiles"] = list(dict.fromkeys(message["author"]["id"] for message in fresh
                if not message.get("webhook_id"))) if job.state["harvest_profiles"] else []
            before = str(min(int(message["id"]) for message in fresh)) if fresh else None
            if history:
                next_state.update(before=before or state["before"], stage="history" if more else "done")
            elif more:
                next_state["before"] = before
            elif state["resuming_pending"]:
                next_state.update(anchor=state["newest"], before=None, resuming_pending=False)
            else:
                next_state.update(stage="done" if state["history_complete"] else "history", before=state["oldest"])
            await self._retry(job, channel["id"], params, "save" if fresh else "cursor",
                lambda: self._page(job, db, channel, fresh, next_state, history=history, more=more))
            if fresh:
                await self._media(fresh, cancelled=lambda: job.stop_requested or job.pause_requested)

    async def _run(self, job):
        event = None
        try:
            try:
                token = await self._token(job)
            except HTTPException:
                job.pause_requested = True
                await self._transition(job, {"type": "paused", "message": "Select this job's saved account before resuming."},
                                       paused=True, pause_requested=True)
                return
            db = await self._connect()
            try:
                channels = job.state["channels"]
                for index in range(job.state["next_index"], len(channels)):
                    await self.checkpoint(job.id)
                    channel = channels[index]
                    await self._transition(job, {"type": "channel_start", "channel": channel["name"],
                        "guild": channel.get("guild_name"), "index": index + 1, "total": len(channels),
                        "messages": (job.state["channel_state"] or {}).get("messages", 0)})
                    try:
                        await self._channel(job, db, channel, token)
                    except (ScrapeStopped, ProfileStopped):
                        raise ScrapeStopped
                    except Exception as exc:
                        self._log.exception("job=%s channel=%s failed; committed checkpoint retained", job.id, channel["id"])
                        if isinstance(exc, PermissionError) or (
                                isinstance(exc, RuntimeError) and str(exc).startswith("HTTP ")):
                            message = str(exc)
                        else:
                            message = f"{type(exc).__name__}; see terminal"
                        await self._transition(job, {"type": "channel_error", "channel": channel["name"], "message": message},
                            failed_channels=job.state["failed_channels"] + 1)
                    state = job.state["channel_state"] or {}
                    await self._transition(job, {"type": "channel_complete", "channel": channel["name"], "messages": state.get("messages", 0)},
                        next_index=index + 1, channel_state=None)
            finally:
                await db.close()
            event = {"type": "complete", "total_messages": job.state["total_messages"],
                     "channels": len(job.state["channels"]), "failed_channels": job.state["failed_channels"]}
        except (ScrapeStopped, ProfileStopped):
            event = {"type": "cancelled", "total_messages": job.state["total_messages"]}
        except asyncio.CancelledError:
            if not job.suspending:
                event = {"type": "cancelled", "total_messages": job.state["total_messages"]}
        except Exception:
            self._log.exception("job=%s stopped; committed checkpoints retained", job.id)
            event = {"type": "error", "message": "Scrape stopped; see terminal for details. Saved cursors remain available."}
        finally:
            if job.suspending:
                await self._transition(job, {"type": "paused"}, paused=True, pause_requested=True)
            elif event:
                await self._transition(job, event, running=False, cancelled=event["type"] == "cancelled", paused=False, pause_requested=False)
        if event and not job.suspending:
            job.expiry = asyncio.get_running_loop().call_later(self._retention, self._jobs.pop, job.id, None)
