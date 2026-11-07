"""Durable collection checkpoints. Records never contain credentials."""

import asyncio
import json
import time

import aiosqlite


async def save_job(db, job_id, kind, state):
    await db.execute("""INSERT INTO collection_jobs(id,kind,state_json,updated_at)
        VALUES (?,?,?,?) ON CONFLICT(id) DO UPDATE SET
        kind=excluded.kind,state_json=excluded.state_json,updated_at=excluded.updated_at""",
        (job_id, kind, json.dumps(state, separators=(",", ":")), time.time()))


async def persist_job(path, job_id, kind, state):
    async def persist():
        async with aiosqlite.connect(path, timeout=30) as db:
            await save_job(db, job_id, kind, state)
            await db.commit()

    # Join checkpoint writes before cancellation can abandon an opening
    # connection or tear down the archive path during shutdown.
    task = asyncio.create_task(persist())
    try:
        await asyncio.shield(task)
    except asyncio.CancelledError:
        await task
        raise


async def load_jobs(path, kind=None):
    async with aiosqlite.connect(path, timeout=30) as db:
        query = "SELECT id,kind,state_json FROM collection_jobs"
        async with db.execute(query + (" WHERE kind=?" if kind else ""),
                              (kind,) if kind else ()) as cursor:
            rows = await cursor.fetchall()
    return [(job_id, job_kind, json.loads(raw)) for job_id, job_kind, raw in rows]


async def remove_job(path, job_id):
    async with aiosqlite.connect(path, timeout=30) as db:
        await db.execute("DELETE FROM collection_jobs WHERE id=?", (job_id,))
        await db.commit()
