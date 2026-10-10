"""Read-only archive interface shared by collector and search-only callers.

Format adapters own candidate selection, public-ID translation and payload shape.
Importing this module creates no application and opens no archive or credentials.
"""
import json
from contextlib import contextmanager
from datetime import date, datetime, timedelta, timezone
from pathlib import Path
import re
import sqlite3
import time

from fastapi import HTTPException
from profile_store import read_profile, profile_servers, search_label, user_id, image_url
from media_store import local_url as archived_media_url, media_status
from snapshot_codec import VERSION, ASCII_FOLD, install_reader, short_token, trigram_query

EPOCH_MS = 1420070400000
MAX_ID = 9223372036854775807
WINDOW = 16384
QUERY_SECONDS = 4


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


def _packed_ids(db, q, filters, lower, upper, before, take, literal):
    conditions, params = [], []
    selective, multi_source, has_author = False, False, False
    for column, value in filters:
        if value is None:
            continue
        if column == 'author_id':
            ref = db.execute('SELECT ref FROM ref_values WHERE value=?', (str(value),)).fetchone()
            if ref is None:
                return []
            conditions.append('m.author_id=?')
            params.append(ref[0])
            has_author = True
        else:
            refs = [r[0] for r in db.execute('SELECT ref FROM message_sources WHERE ' + column + '=?', (str(value),))]
            if not refs:
                return []
            conditions.append('m.source_id IN (' + ','.join('?' for _ in refs) + ')')
            params.extend(refs)
            multi_source = multi_source or len(refs)>1
        count = db.execute('SELECT count FROM stats_counts WHERE kind=? AND key=?',
                           ({'guild_id':'guild', 'channel_id':'channel', 'author_id':'author'}[column], str(value))).fetchone()
        selective = selective or bool(count and count[0] <= 4096)
    for value, low in ((lower, True), (upper, False), (before, False)):
        if value is None:
            continue
        boundary = db.execute('SELECT row_before_id(?)', (value,)).fetchone()[0]
        conditions.append('m.rowid' + ('>=?' if low else '<=?'))
        params.append(boundary+1 if low else boundary)
    short = q.split('\0', 1)[0].translate(ASCII_FOLD)
    if q and 1 <= len(short) <= 2 and not selective:
        block_rows = int(db.execute("SELECT value FROM snapshot_info WHERE key='block_rows'").fetchone()[0])
        lower_row = db.execute('SELECT row_before_id(?)', (lower,)).fetchone()[0]+1 if lower is not None else 1
        last_id = min(v for v in (upper,before) if v is not None) if upper is not None or before is not None else None
        upper_row = db.execute('SELECT row_before_id(?)', (last_id,)).fetchone()[0] if last_id is not None else int(db.execute("SELECT value FROM snapshot_info WHERE key='messages'").fetchone()[0])
        candidates = db.execute('SELECT rowid FROM message_short_fts WHERE message_short_fts MATCH ? AND rowid BETWEEN ? AND ? ORDER BY rowid DESC',
            (short_token(short), (lower_row-1)//block_rows+1, (upper_row-1)//block_rows+1))
        result = []
        where = ' AND '.join(conditions + ["m.rowid BETWEEN ? AND ?", "record_content(m.rowid) LIKE ? ESCAPE '!'"])
        for block in candidates:
            result.extend(r[0] for r in db.execute('SELECT m.rowid FROM message_index m WHERE ' + where + ' ORDER BY m.rowid DESC LIMIT ?',
                params + [(block[0]-1)*block_rows+1,block[0]*block_rows,literal,take-len(result)]))
            if len(result) == take:
                break
        return result
    match = trigram_query(q) if q and not selective else None
    if match:
        relation = 'messages_fts f JOIN message_index m ON m.rowid=f.rowid'
        conditions.insert(0, 'f.messages_fts MATCH ?')
        params.insert(0, match)
        order = 'f.rowid'
    else:
        relation, order = 'message_index m', 'm.rowid'
        if not q and multi_source and not selective and not has_author:
            relation += ' NOT INDEXED'
    if q:
        conditions.append("record_content(m.rowid) LIKE ? ESCAPE '!'")
        params.append(literal)
    return [r[0] for r in db.execute(f'SELECT m.rowid FROM {relation} WHERE ' +
        (' AND '.join(conditions) or '1') + f' ORDER BY {order} DESC LIMIT ?', params + [take])]


def _packed_servers(db, uid, q, offset, limit, normalize):
    ref = db.execute('SELECT ref FROM ref_values WHERE value=?', (uid,)).fetchone()
    if ref is None:
        return {'servers': [], 'has_more': False, 'offset': offset,
                'observation_kind':'archived_messages','current_membership':None}
    db.create_function('search_label', 1, normalize, deterministic=True)
    value = normalize(q).replace('!', '!!').replace('%', '!%').replace('_', '!_')
    rows = db.execute("""SELECT s.guild_id id, COALESCE(g.name,'Unknown server') name,
        SUM(m.messages) messages, CAST(record_id(MAX(m.latest)) AS TEXT) latest_id
        FROM (SELECT source_id, COUNT(*) messages, MAX(rowid) latest
              FROM message_index WHERE author_id=? GROUP BY source_id) m
        JOIN message_sources s ON s.ref=m.source_id
        LEFT JOIN guilds g ON g.id=s.guild_id
        WHERE s.guild_id IS NOT NULL
          AND (search_label(g.name) LIKE ? ESCAPE '!' OR s.guild_id=?)
        GROUP BY s.guild_id ORDER BY messages DESC, CAST(s.guild_id AS INTEGER)
        LIMIT ? OFFSET ?""", (ref[0], '%' + value + '%', q.strip(), limit+1, offset)).fetchall()
    return {'servers': [dict(r) for r in rows[:limit]], 'has_more': len(rows)>limit, 'offset': offset,
            'observation_kind':'archived_messages','current_membership':None}


class _StandardReader:
    """Standard v9/v10 adapter; candidates are public IDs."""
    row_key = 'id'
    payload_table = 'message_payloads'

    def __init__(self, db):
        self.db = db

    def candidates(self, q, filters, lower, upper, before, take, scan):
        db = self.db
        bounds, bound_params = [], []
        for clause, value in (("id>=?", lower), ("id<?", upper), ("id<?", before)):
            if value is not None:
                bounds.append(clause)
                bound_params.append(value)
        conditions = [column + '=?' for column, _ in filters] + bounds
        params = [value for _, value in filters] + bound_params
        where = ' AND '.join(conditions) or '1'
        literal = '%' + like_literal(q) + '%'
        scan_cursor = None
        if scan:
            window = [r[0] for r in db.execute('SELECT id FROM messages WHERE ' +
                (' AND '.join(bounds) or '1') + ' ORDER BY id DESC LIMIT ?', bound_params+[WINDOW+1])]
            floor = window[min(WINDOW,len(window))-1] if window else None
            text_clause = "AND content LIKE ? ESCAPE '!' " if q else ''
            ids = [r[0] for r in db.execute(f"SELECT id FROM messages WHERE {where} AND id>=? " +
                text_clause + "ORDER BY id DESC LIMIT ?", params+[floor]+([literal] if q else [])+[take])] if window else []
            if len(window) > WINDOW and len(ids) < take:
                scan_cursor = str(floor)
        elif q:
            boundary = db.execute(f"SELECT id FROM messages WHERE {where} ORDER BY id DESC LIMIT 1 OFFSET ?", params + [WINDOW-1]).fetchone()
            floor = boundary[0] if boundary else 0
            ids = [r[0] for r in db.execute(
                f"SELECT id FROM messages WHERE {where} AND id>=? AND content LIKE ? ESCAPE '!' ORDER BY id DESC LIMIT ?",
                params + [floor, literal, take])]
            if len(ids) < take and boundary:
                anchor = max(re.findall(r"[^%_]+", q), key=len, default='')
                indexed = len(anchor) >= 3
                candidate = "AND rowid IN (SELECT rowid FROM messages_fts WHERE content LIKE ?) " if indexed else ''
                ids += [r[0] for r in db.execute(
                    f"SELECT id FROM messages WHERE {where} AND id<? " + candidate +
                    "AND content LIKE ? ESCAPE '!' ORDER BY id DESC LIMIT ?",
                    params + [floor] + (["%" + anchor + "%"] if indexed else []) + [literal, take-len(ids)])]
        else:
            ids = [r[0] for r in db.execute(f"SELECT id FROM messages WHERE {where} ORDER BY id DESC LIMIT ?", params + [take])]
        return ids, scan_cursor

    def records(self, ids):
        return self.db.execute(f"SELECT * FROM message_records WHERE {self.row_key} IN (" +
            ','.join('?' for _ in ids) + f") ORDER BY {self.row_key} DESC", ids).fetchall()

    def attachments(self, rows):
        db = self.db
        if not db.execute("SELECT 1 FROM sqlite_master WHERE name=? AND type='table'", (self.payload_table,)).fetchone():
            return {}
        public_ids = [row['id'] for row in rows]
        return {str(r[0]): self.attachment_data(json.loads(r[1])) for r in db.execute(
            f'SELECT message_id,payload FROM {self.payload_table} WHERE message_id IN (' +
            ','.join('?' for _ in public_ids)+')', public_ids)}

    @staticmethod
    def attachment_data(payload):
        return payload.get('attachments', [])

    def servers(self, uid, q, offset, limit):
        return profile_servers(self.db, uid, q, offset, limit)


class _PackedReader(_StandardReader):
    """Packed v101 adapter; storage references never escape Archive results."""
    row_key = 'storage_rowid'
    payload_table = 'message_attachments'

    def __init__(self, db):
        self.db = db
        install_reader(db)

    def candidates(self, q, filters, lower, upper, before, take, scan):
        db = self.db
        scan_cursor = None
        if scan:
            bounds, params = [], []
            for value, low in ((lower, True), (upper, False), (before, False)):
                if value is not None:
                    boundary = db.execute('SELECT row_before_id(?)', (value,)).fetchone()[0]
                    bounds.append('rowid' + ('>=?' if low else '<=?'))
                    params.append(boundary+1 if low else boundary)
            window = [r[0] for r in db.execute('SELECT rowid FROM message_index WHERE ' +
                (' AND '.join(bounds) or '1') + ' ORDER BY rowid DESC LIMIT ?', params+[WINDOW+1])]
            floor = db.execute('SELECT record_id(?)', (window[min(WINDOW,len(window))-1],)).fetchone()[0] if window else None
            ids = _packed_ids(db, q, filters, floor, upper, before, take,
                              '%' + like_literal(q) + '%') if window else []
            if len(window) > WINDOW and len(ids) < take:
                scan_cursor = str(floor)
        else:
            ids = _packed_ids(db, q, filters, lower, upper, before, take,
                              '%' + like_literal(q) + '%')
        return ids, scan_cursor

    @staticmethod
    def attachment_data(payload):
        return payload

    def servers(self, uid, q, offset, limit):
        return _packed_servers(self.db, user_id(uid), q, offset, limit, search_label)


class Archive:
    """Read standard archives and verified packed snapshots through one interface.

    Search results and continuation cursors contain public Discord IDs. A partial
    page can be empty while older history remains; continue with its next_cursor
    and scan=True. Each operation pins one read transaction and SQL deadline.
    Immutable mode requires a closed archive that stays unchanged for the session.
    """

    def __init__(self, path, immutable=False):
        self.path = Path(path).resolve()
        self.immutable = immutable

    @contextmanager
    def _connect(self):
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
            deadline = time.monotonic() + QUERY_SECONDS
            db.set_progress_handler(lambda: time.monotonic() > deadline, 1000)
            db.execute("BEGIN")
            version = db.execute("PRAGMA user_version").fetchone()[0]
            if version not in (9, 10, VERSION):
                raise HTTPException(503, "This display app needs a schema v9/v10 archive or a verified search snapshot.")
            reader = _PackedReader(db) if version == VERSION else _StandardReader(db)
            yield db, reader
        except sqlite3.OperationalError as exc:
            if "interrupt" in str(exc):
                raise HTTPException(408, "This search is too broad. Add a server, channel, author, or date filter and try again.") from exc
            raise HTTPException(503, "The archive could not be read. Check its location and access permissions.") from exc
        finally:
            if db is not None:
                db.close()

    @contextmanager
    def connect(self):
        """A consistent read-only connection with format decoding already installed."""
        with self._connect() as (db, _):
            yield db

    def profile(self, uid):
        with self.connect() as db:
            return read_profile(db, uid)

    def profile_servers(self, uid, q='', offset=0, limit=30):
        with self._connect() as (db, reader):
            return reader.servers(uid, q, offset, limit)

    def search(self, q="", guild_id=None, channel_id=None, author_id=None,
               date_from=None, date_to=None, before=None, limit=40, refresh_images=False, scan=False):
        started = time.perf_counter()
        args = (q, guild_id, channel_id, author_id, date_from, date_to, before, limit, refresh_images)
        try:
            result = self._search(*args, scan=scan)
        except HTTPException as exc:
            if exc.status_code != 408 or scan:
                raise
            # Retry in a fresh transaction over one bounded public-ID window.
            # The continuation records examined rows, including an empty page.
            result = self._search(*args, scan=True)
        result['elapsed_ms'] = round((time.perf_counter()-started)*1000,1)
        return result

    def _search(self, q="", guild_id=None, channel_id=None, author_id=None,
                date_from=None, date_to=None, before=None, limit=40, refresh_images=False, scan=False):
        started = time.perf_counter()
        q = q.strip()
        selected = []
        for column, value in (("guild_id", guild_id), ("channel_id", channel_id), ("author_id", author_id)):
            if value:
                if not value.isascii() or not value.isdigit() or not 0 < int(value) <= MAX_ID:
                    raise HTTPException(400, "Choose a filter suggestion or enter a valid Discord ID.")
                selected.append((column, int(value)))
        lower = snowflake_day(date_from) if date_from else None
        upper = snowflake_day(date_to, True) if date_to else None
        if lower is not None and upper is not None and lower >= upper:
            raise HTTPException(400, "From date must be on or before the to date.")
        with self._connect() as (db, reader):
            ids, scan_cursor = reader.candidates(q, selected, lower, upper, before, limit+1, scan)
            has_more = len(ids) > limit or scan_cursor is not None
            ids = ids[:limit]
            messages = []
            if ids:
                rows = reader.records(ids)
                author_ids = list({row['author_id'] for row in rows})
                avatars = {str(r[0]): r[1] for r in db.execute(
                    "SELECT user_id, avatar_hash FROM profiles WHERE user_id IN (" + ",".join("?" for _ in author_ids) + ")", author_ids)}
                payloads = reader.attachments(rows)
                for row in rows:
                    message = dict(row)
                    message.pop('storage_rowid', None)
                    message["id"] = str(message["id"])
                    avatar = avatars.get(message["author_id"])
                    message["avatar_url"] = (
                        image_url(message['author_id'],avatar,size=64)
                        if isinstance(avatar, str) and re.fullmatch(r"(?:a_)?[A-Za-z0-9]+", avatar)
                        and str(message['author_id']).isdigit() else None)
                    if message['avatar_url']:
                        message['avatar_url'] = archived_media_url(self.path,message['avatar_url'])
                    # Display mode never fetches credentials or refreshes expiring links.
                    urls = (message.pop("image_urls") or "").splitlines()
                    message["attachments"] = [
                        archived_media_url(self.path,url) if media_status(self.path,url)['status'] == 'saved' else
                        (f"/api/images/{message['id']}/{i}" if refresh_images else url)
                        for i, url in enumerate(urls)
                        if re.match(r"^https://(?:cdn|media)\.discordapp\.(?:com|net)/", url)]
                    attachment_data = payloads.get(message['id'], [])
                    message['attachment_files'] = [{
                        'filename':attachment.get('filename') or 'attachment',
                        'url':archived_media_url(self.path,attachment['url']),
                        'media':media_status(self.path,attachment['url']),
                    } for attachment in attachment_data if isinstance(attachment,dict) and
                        isinstance(attachment.get('url'),str) and
                        re.match(r'^https://(?:cdn|media)\.discordapp\.(?:com|net)/',attachment['url'])]
                    messages.append(message)
        return {"messages": messages, "has_more": has_more,
                "next_cursor": scan_cursor or (messages[-1]['id'] if has_more else None),
                "scan": scan, "partial": scan_cursor is not None,
                "elapsed_ms": round((time.perf_counter()-started)*1000, 1)}
