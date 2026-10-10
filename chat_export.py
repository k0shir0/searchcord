"""Stream ChatML conversation exports without materializing the conversation."""
import json
import sqlite3
import time

import aiosqlite
from fastapi import HTTPException


async def export_names(db_path, channel_id, target_id):
    async with aiosqlite.connect(db_path, timeout=1) as db:
        deadline = time.monotonic() + 4
        await db.set_progress_handler(lambda: time.monotonic() > deadline, 1000)
        try:
            async with db.execute("SELECT author_name FROM message_records WHERE channel_id=? "
                                  "AND author_id=? ORDER BY id LIMIT 1", (channel_id,target_id)) as cursor:
                target = await cursor.fetchone()
            if target is None:
                raise HTTPException(404, "Target user not found in this channel's messages")
            async with db.execute("SELECT author_name FROM message_records WHERE channel_id=? "
                                  "AND author_id<>? ORDER BY id LIMIT 1", (channel_id,target_id)) as cursor:
                other = await cursor.fetchone()
        except sqlite3.OperationalError as exc:
            if 'interrupt' in str(exc):
                raise HTTPException(408, 'Export lookup took too long; try a smaller conversation') from None
            raise
    return target[0] or target_id, (other[0] if other else None) or 'Them'


async def chatml_lines(db_path, channel_id, target_id, other_name, system_prompt):
    # Only the preceding other turn and current target turn stay in memory.
    # Long same-side turns are assembled once, rather than repeatedly copied.
    async with aiosqlite.connect(db_path, timeout=1) as db:
        await db.execute('PRAGMA query_only=ON')
        await db.execute('BEGIN')
        deadline = time.monotonic() + 4
        await db.set_progress_handler(lambda: time.monotonic() > deadline, 1000)
        async with db.execute("SELECT author_id,content FROM messages WHERE channel_id=? "
                              "ORDER BY id ASC", (channel_id,)) as cursor:
            side, pieces, previous = None, [], None

            def line():
                return json.dumps({'messages': [
                    {'role':'system','content':system_prompt},
                    {'role':'user','content':f'{other_name}: {previous}'},
                    {'role':'assistant','content':'\n'.join(pieces)},
                ]}, ensure_ascii=False) + '\n'

            while True:
                deadline = time.monotonic() + 4
                batch = await cursor.fetchmany(1000)
                if not batch:
                    break
                for author, content in batch:
                    text = (content or '').strip()
                    if not text:
                        continue
                    current = 'target' if str(author) == target_id else 'other'
                    if side is not None and current != side:
                        if side == 'target' and previous is not None:
                            yield line()
                        if side == 'other':
                            previous = '\n'.join(pieces)
                        pieces = []
                    side = current
                    pieces.append(text)
            if side == 'target' and previous is not None:
                yield line()
