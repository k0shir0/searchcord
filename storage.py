"""Compact SQLite storage with verified message and history migrations."""

import gzip
import hashlib
import json
import os
import sqlite3
from contextlib import closing
from datetime import datetime, timezone
from pathlib import Path
from urllib.parse import unquote, urlsplit
from credential_store import migrate_credentials

COLLECTION_VERSION = 10

EPOCH_MS = 1420070400000
IMAGE_EXTENSIONS = frozenset({".png", ".jpg", ".jpeg", ".jfif", ".gif",
                              ".webp", ".avif", ".apng", ".bmp", ".ico",
                              ".svg", ".heic", ".heif", ".tif", ".tiff",
                              ".dng", ".exr"})


def _historical_timestamp_sql(message_id):
    """Rebuild the two exact UTC timestamp shapes in older Discord exports."""
    milliseconds = f"(({message_id} >> 22)+{EPOCH_MS})"
    seconds = f"{milliseconds}/1000.0"
    return f"""CASE WHEN {milliseconds}%1000=0 THEN
        strftime('%Y-%m-%dT%H:%M:%S',{seconds},'unixepoch')||'+00:00'
        ELSE strftime('%Y-%m-%dT%H:%M:%f',{seconds},'unixepoch')||'000+00:00' END"""


def image_urls(attachments):
    """Keep only image links from Discord attachments."""
    urls = []
    for attachment in attachments or ():
        url = attachment.get("url")
        if not url:
            continue
        mime = (attachment.get("content_type") or "").lower()
        extension = Path(unquote(urlsplit(url).path)).suffix.lower()
        if mime.startswith("image/") or (not mime and extension in IMAGE_EXTENSIONS):
            urls.append(url)
    return urls


def _legacy_images(raw):
    if not raw or raw == "[]":
        return None
    try:
        urls = json.loads(raw)
    except (ValueError, TypeError):
        return None
    if not isinstance(urls, list):
        return None
    return "\n".join(image_urls({"url": url} for url in urls
                                if isinstance(url, str))) or None


def _backup(path):
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S%fZ")
    target = path.with_name(f"{path.name}.pre-compact-{stamp}.bak")
    # The caller holds the write reservation. Read from another connection so
    # SQLite's backup API does not wait on its own uncommitted transaction.
    # Only publish the recovery copy after credential removal and a rebuild of
    # this disposable file. Earlier backups and the source file are untouched.
    temporary = target.with_suffix('.private-tmp')
    try:
        descriptor = os.open(temporary, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
        os.close(descriptor)
        with closing(sqlite3.connect(path.as_uri() + '?mode=ro', uri=True)) as source:
            with closing(sqlite3.connect(temporary)) as backup:
                source.backup(backup)
                backup.execute('PRAGMA journal_mode=DELETE')
                backup.execute('PRAGMA secure_delete=ON')
                if backup.execute("SELECT 1 FROM sqlite_master WHERE name='settings' AND type='table'").fetchone():
                    backup.execute("DELETE FROM settings WHERE key IN ('token','saved_tokens','active_token_id')")
                backup.commit()
                backup.execute('VACUUM')
                if backup.execute('PRAGMA integrity_check').fetchone()[0] != 'ok':
                    raise RuntimeError('Recovery backup failed integrity check')
        os.replace(temporary, target)
    except BaseException:
        temporary.unlink(missing_ok=True)
        raise
    return str(target)


def _pack_backup(path):
    """Compress a verified recovery copy; keep the raw copy on any failure."""
    source = Path(path)
    packed = Path(f"{path}.gz")
    original_hash = hashlib.sha256()
    try:
        with source.open('rb') as original, packed.open('xb') as output:
            with gzip.GzipFile(fileobj=output, mode='wb', compresslevel=6) as compressed:
                while block := original.read(1024 * 1024):
                    original_hash.update(block)
                    compressed.write(block)
        recovered_hash = hashlib.sha256()
        with gzip.open(packed, 'rb') as recovered:
            while block := recovered.read(1024 * 1024):
                recovered_hash.update(block)
        if recovered_hash.digest() != original_hash.digest():
            raise RuntimeError('Compressed recovery backup did not verify')
        source.unlink()
        return str(packed)
    except Exception:
        packed.unlink(missing_ok=True)
        raise


def _legacy_metadata(db):
    # Names are current UI labels. Stable IDs remain on every message.
    for table, ident, name, extra in (
            ("guilds", "guild_id", "guild_name", ""),
            ("channels", "channel_id", "channel_name", ", guild_id"),
            ("authors", "author_id", "author_name", ", id")):
        rows = db.execute(f"""SELECT {ident}, {name}{extra} FROM (
            SELECT {ident}, {name}{extra}, ROW_NUMBER() OVER (
                PARTITION BY {ident} ORDER BY CAST(id AS INTEGER) DESC) rn
            FROM messages WHERE {ident} IS NOT NULL) WHERE rn=1""")
        if table == "guilds":
            db.executemany("INSERT OR IGNORE INTO guilds(id,name) VALUES (?,?)",
                           ((ident, name or "") for ident, name in rows))
        elif table == "channels":
            db.executemany("INSERT OR IGNORE INTO channels(id,name,guild_id) VALUES (?,?,?)",
                           ((ident, name or "", guild) for ident, name, guild in rows))
        else:
            db.executemany("""INSERT OR IGNORE INTO authors
                (id,name,last_message_id) VALUES (?,?,?)""",
                           ((ident, name or "", last_id)
                            for ident, name, last_id in rows))
    db.execute("""INSERT OR IGNORE INTO channel_authors
        SELECT DISTINCT channel_id, author_id FROM messages
        WHERE channel_id IS NOT NULL AND author_id IS NOT NULL""")
    db.execute("""INSERT OR IGNORE INTO scrape_cursors
        (channel_id, guild_id, newest_message_id, oldest_message_id)
        SELECT channel_id, MAX(guild_id),
               CAST(MAX(CAST(id AS INTEGER)) AS TEXT),
               CAST(MIN(CAST(id AS INTEGER)) AS TEXT)
        FROM messages WHERE channel_id IS NOT NULL GROUP BY channel_id""")


def _migrate(db, legacy):
    columns = {row[1] for row in db.execute('PRAGMA table_info(messages)')}
    base = {'id', 'channel_id', 'guild_id', 'author_id', 'content'}
    historical = {'timestamp', 'attachments', 'guild_name', 'channel_name',
                  'author_name', 'guild_name_id', 'channel_name_id', 'author_name_id'}
    if not base <= columns or columns - base - historical - {'image_urls'}:
        raise RuntimeError('Unrecognized message columns; refusing a potentially lossy migration')
    db.execute("DROP VIEW IF EXISTS message_records")
    for trigger in ("messages_stats_insert", "messages_stats_delete",
                    "messages_fts_insert", "messages_fts_delete", "messages_fts_update"):
        db.execute(f"DROP TRIGGER IF EXISTS {trigger}")
    db.execute("DROP TABLE IF EXISTS messages_fts")
    if legacy:
        _legacy_metadata(db)
    db.execute("""CREATE TABLE messages_compact (
        rowid INTEGER PRIMARY KEY, id INTEGER NOT NULL UNIQUE,
        channel_id INTEGER, guild_id INTEGER, author_id INTEGER,
        content TEXT, image_urls TEXT)""")
    count = db.execute("SELECT COUNT(*) FROM messages").fetchone()[0]
    db.create_function('legacy_images', 1, _legacy_images)
    images = 'image_urls' if 'image_urls' in columns else 'legacy_images(attachments)'
    db.execute(f"""INSERT INTO messages_compact
        (id,channel_id,guild_id,author_id,content,image_urls)
        SELECT CAST(id AS INTEGER),channel_id,guild_id,author_id,content,{images}
        FROM messages ORDER BY CAST(id AS INTEGER)""")
    if db.execute("SELECT COUNT(*) FROM messages_compact").fetchone()[0] != count:
        raise RuntimeError("Message migration row count mismatch")
    if 'attachments' in columns:
        # Old compact formats cannot recover missing message JSON, but every
        # attachment value still present in older schemas remains available.
        db.execute("""INSERT OR IGNORE INTO legacy_message_attachments(message_id,attachments)
            SELECT CAST(id AS INTEGER),attachments FROM messages
            WHERE attachments IS NOT NULL""")
    # NULL-safe comparison of every existing message value, not just counts or
    # text lengths. ID strings must round-trip exactly through integer storage.
    comparisons = [f'CAST(old.{key} AS TEXT) IS CAST(new.{key} AS TEXT)'
                   for key in ('id', 'channel_id', 'guild_id', 'author_id')]
    comparisons.append('old.content IS new.content')
    if 'image_urls' in columns:
        comparisons.append('old.image_urls IS new.image_urls')
    if db.execute(f"""SELECT 1 FROM messages old JOIN messages_compact new
        ON new.id=CAST(old.id AS INTEGER) WHERE NOT ({' AND '.join(comparisons)})
        LIMIT 1""").fetchone():
        raise RuntimeError('Message values did not round-trip; migration rolled back')

    if columns & (historical - {'attachments'}):
        for name in ('guild_name', 'channel_name', 'author_name'):
            if name in columns:
                db.execute(f"""INSERT OR IGNORE INTO name_values(value)
                    SELECT DISTINCT {name} FROM messages WHERE {name} IS NOT NULL""")
        reconstructed = _historical_timestamp_sql('CAST(old.id AS INTEGER)')
        if 'timestamp' in columns:
            timestamp = f'CASE WHEN old.timestamp IS {reconstructed} THEN NULL ELSE old.timestamp END'
            timestamp_override = f'old.timestamp IS NOT {reconstructed}'
        else:
            timestamp, timestamp_override = 'NULL', '0'
        expressions = []
        for name in ('guild_name', 'channel_name', 'author_name'):
            if name in columns:
                expressions.append(f'(SELECT id FROM name_values WHERE value=old.{name})')
            else:
                expressions.append(f'old.{name}_id' if name + '_id' in columns else 'NULL')
        db.execute(f"""INSERT INTO message_history
            (rowid,timestamp,timestamp_override,guild_name_id,channel_name_id,author_name_id)
            SELECT new.rowid,{timestamp},{timestamp_override},{','.join(expressions)}
            FROM messages old
            JOIN messages_compact new ON new.id=CAST(old.id AS INTEGER)""")
        checks = []
        for name in columns & (historical - {'attachments'}):
            restored = (f'(SELECT value FROM name_values WHERE id=h.{name}_id)'
                        if name in ('guild_name', 'channel_name', 'author_name') else
                        f'CASE WHEN h.timestamp_override THEN h.timestamp ELSE {reconstructed} END'
                        if name == 'timestamp' else f'h.{name}')
            checks.append(f'old.{name} IS {restored}')
        if db.execute(f"""SELECT 1 FROM messages old JOIN messages_compact new
            ON new.id=CAST(old.id AS INTEGER) LEFT JOIN message_history h ON h.rowid=new.rowid
            WHERE h.rowid IS NULL OR NOT ({' AND '.join(checks)}) LIMIT 1""").fetchone():
            raise RuntimeError('Historical values did not round-trip; migration rolled back')
    db.execute("DROP TABLE messages")
    db.execute("ALTER TABLE messages_compact RENAME TO messages")


def init_database(path: str) -> dict:
    """Create or upgrade the database, preserving a backup before conversion."""
    db_path = Path(path).resolve()
    db_path.parent.mkdir(parents=True, exist_ok=True)
    with closing(sqlite3.connect(db_path, timeout=30)) as db:
        version = db.execute('PRAGMA user_version').fetchone()[0]
        if version > COLLECTION_VERSION:
            raise RuntimeError('This archive uses a newer storage format; update Searchcord first')
        credentials_migrated = migrate_credentials(db_path, db)
        db.execute("PRAGMA journal_mode=WAL")
        db.execute("PRAGMA synchronous=NORMAL")
        db.execute("PRAGMA busy_timeout=30000")
        columns = {row[1] for row in db.execute("PRAGMA table_info(messages)")}
        legacy = "channel_name" in columns
        migrate = bool(columns) and 'rowid' not in columns
        history_columns = {row[1] for row in db.execute('PRAGMA table_info(message_history)')}
        migrate_history = not migrate and 'attachments' in history_columns
        backup = None
        has_fts = db.execute("""SELECT 1 FROM sqlite_master
            WHERE type='table' AND name='messages_fts'""").fetchone() is not None
        has_stats = db.execute("""SELECT 1 FROM sqlite_master
            WHERE type='table' AND name='stats_counts'""").fetchone() is not None

        db.execute("BEGIN IMMEDIATE")
        try:
            if migrate or migrate_history:
                backup = _backup(db_path)
            db.execute("CREATE TABLE IF NOT EXISTS guilds (id TEXT PRIMARY KEY, name TEXT NOT NULL)")
            db.execute("""CREATE TABLE IF NOT EXISTS channels (
                id TEXT PRIMARY KEY, guild_id TEXT, name TEXT NOT NULL)""")
            db.execute("CREATE INDEX IF NOT EXISTS idx_channels_guild ON channels(guild_id)")
            db.execute("""CREATE TABLE IF NOT EXISTS authors (
                id TEXT PRIMARY KEY, name TEXT NOT NULL,
                last_message_id TEXT NOT NULL DEFAULT '0')""")
            author_cols = {row[1] for row in db.execute("PRAGMA table_info(authors)")}
            if "last_message_id" not in author_cols:
                db.execute("""ALTER TABLE authors ADD COLUMN last_message_id
                    TEXT NOT NULL DEFAULT '0'""")
            db.execute("""CREATE TABLE IF NOT EXISTS profiles (
                user_id TEXT PRIMARY KEY, username TEXT NOT NULL, global_name TEXT,
                avatar_hash TEXT, banner_hash TEXT, accent_color INTEGER,
                bot INTEGER, public_flags INTEGER, fetched_at TEXT NOT NULL)""")
            db.execute("""CREATE TABLE IF NOT EXISTS profile_details (
                user_id TEXT PRIMARY KEY, payload TEXT NOT NULL, fetched_at TEXT NOT NULL)""")
            db.execute("""CREATE TABLE IF NOT EXISTS collection_jobs (
                id TEXT PRIMARY KEY, kind TEXT NOT NULL,
                state_json TEXT NOT NULL, updated_at REAL NOT NULL)""")
            db.execute("""CREATE TABLE IF NOT EXISTS message_payloads (
                message_id INTEGER PRIMARY KEY, payload TEXT NOT NULL,
                first_seen_at TEXT NOT NULL, last_seen_at TEXT NOT NULL,
                deleted_at TEXT, complete INTEGER NOT NULL DEFAULT 1)""")
            db.execute("""CREATE TABLE IF NOT EXISTS message_revisions (
                revision_id INTEGER PRIMARY KEY, message_id INTEGER NOT NULL,
                observed_at TEXT NOT NULL, event TEXT NOT NULL,
                source TEXT NOT NULL, payload TEXT)""")
            db.execute("""CREATE INDEX IF NOT EXISTS idx_revisions_message
                ON message_revisions(message_id,revision_id)""")
            db.execute("""CREATE TABLE IF NOT EXISTS legacy_message_attachments (
                message_id INTEGER PRIMARY KEY, attachments TEXT NOT NULL)""")
            db.execute("""CREATE TABLE IF NOT EXISTS channel_authors (
                channel_id TEXT NOT NULL, author_id TEXT NOT NULL,
                PRIMARY KEY (channel_id, author_id)) WITHOUT ROWID""")
            if migrate:
                db.execute("""CREATE TABLE channel_authors_compact (
                    channel_id TEXT NOT NULL, author_id TEXT NOT NULL,
                    PRIMARY KEY (channel_id,author_id)) WITHOUT ROWID""")
                db.execute('INSERT INTO channel_authors_compact SELECT * FROM channel_authors')
                db.execute('DROP TABLE channel_authors')
                db.execute('ALTER TABLE channel_authors_compact RENAME TO channel_authors')
            db.execute("""CREATE TABLE IF NOT EXISTS scrape_cursors (
                channel_id TEXT PRIMARY KEY, guild_id TEXT,
                newest_message_id TEXT NOT NULL, oldest_message_id TEXT,
                history_complete INTEGER NOT NULL DEFAULT 0,
                pending_after_message_id TEXT, pending_before_message_id TEXT)""")
            cursor_cols = {row[1] for row in db.execute("PRAGMA table_info(scrape_cursors)")}
            for column in ("pending_after_message_id", "pending_before_message_id"):
                if column not in cursor_cols:
                    db.execute(f"ALTER TABLE scrape_cursors ADD COLUMN {column} TEXT")
            db.execute("CREATE INDEX IF NOT EXISTS idx_cursor_guild ON scrape_cursors(guild_id)")

            db.execute("""CREATE TABLE IF NOT EXISTS name_values (
                id INTEGER PRIMARY KEY, value TEXT UNIQUE)""")
            db.execute("""CREATE TABLE IF NOT EXISTS message_history (
                rowid INTEGER PRIMARY KEY, timestamp TEXT,
                timestamp_override INTEGER NOT NULL DEFAULT 0,
                guild_name_id INTEGER, channel_name_id INTEGER, author_name_id INTEGER)""")

            if migrate_history:
                db.execute('DROP VIEW IF EXISTS message_records')
                db.execute('DROP TRIGGER IF EXISTS messages_history_delete')
                db.execute('ALTER TABLE message_history RENAME TO message_history_full')
                db.execute("""INSERT OR IGNORE INTO legacy_message_attachments(message_id,attachments)
                    SELECT m.id,h.attachments FROM message_history_full h
                    JOIN messages m ON m.rowid=h.rowid WHERE h.attachments IS NOT NULL""")
                db.execute("""CREATE TABLE message_history (
                    rowid INTEGER PRIMARY KEY, timestamp TEXT,
                    timestamp_override INTEGER NOT NULL DEFAULT 0,
                    guild_name_id INTEGER, channel_name_id INTEGER, author_name_id INTEGER)""")
                restored = _historical_timestamp_sql('m.id')
                db.execute(f"""INSERT INTO message_history
                    SELECT h.rowid,
                           CASE WHEN h.timestamp IS {restored} THEN NULL ELSE h.timestamp END,
                           h.timestamp IS NOT {restored},
                           h.guild_name_id,h.channel_name_id,h.author_name_id
                    FROM message_history_full h JOIN messages m ON m.rowid=h.rowid""")
                if db.execute("SELECT COUNT(*) FROM message_history_full").fetchone()[0] != db.execute(
                        "SELECT COUNT(*) FROM message_history").fetchone()[0]:
                    raise RuntimeError('Historical row count mismatch; migration rolled back')
                if db.execute(f"""SELECT 1 FROM message_history_full h
                    JOIN message_history compact ON compact.rowid=h.rowid
                    JOIN messages m ON m.rowid=h.rowid
                    WHERE h.timestamp IS NOT CASE WHEN compact.timestamp_override
                        THEN compact.timestamp ELSE {restored} END
                       OR h.guild_name_id IS NOT compact.guild_name_id
                       OR h.channel_name_id IS NOT compact.channel_name_id
                       OR h.author_name_id IS NOT compact.author_name_id
                    LIMIT 1""").fetchone():
                    raise RuntimeError('Historical values did not round-trip; migration rolled back')
                db.execute('DROP TABLE message_history_full')

            if migrate:
                _migrate(db, legacy)
            else:
                db.execute("""CREATE TABLE IF NOT EXISTS messages (
                    rowid INTEGER PRIMARY KEY, id INTEGER NOT NULL UNIQUE,
                    channel_id INTEGER, guild_id INTEGER, author_id INTEGER,
                    content TEXT, image_urls TEXT)""")
            # Dense rowids shrink FTS posting lists. Public Discord IDs still
            # order results correctly after out-of-order history backfills.
            db.execute("CREATE INDEX IF NOT EXISTS idx_ch_id ON messages(channel_id,id)")
            db.execute("CREATE INDEX IF NOT EXISTS idx_gd_id ON messages(guild_id,id) WHERE guild_id IS NOT NULL")
            db.execute("CREATE INDEX IF NOT EXISTS idx_au_id ON messages(author_id,id)")
            db.execute("CREATE INDEX IF NOT EXISTS idx_authors_name ON authors(name COLLATE NOCASE)")
            db.execute("CREATE INDEX IF NOT EXISTS idx_profiles_username ON profiles(username COLLATE NOCASE)")

            db.execute("""CREATE TABLE IF NOT EXISTS stats_counts (
                kind TEXT NOT NULL, key TEXT NOT NULL, count INTEGER NOT NULL,
                PRIMARY KEY (kind, key)) WITHOUT ROWID""")
            if migrate or not has_stats:
                db.execute("DELETE FROM stats_counts")
                db.execute("""INSERT INTO stats_counts
                    SELECT 'total','',COUNT(*) FROM messages HAVING COUNT(*)>0""")
                for kind, expression in (
                        ("guild", "COALESCE(CAST(guild_id AS TEXT),'')"),
                        ("channel", "COALESCE(CAST(channel_id AS TEXT),'')"),
                        ("author", "COALESCE(CAST(author_id AS TEXT),'')"),
                        ("day", "date(((id >> 22)+1420070400000)/1000.0,'unixepoch')"),
                        ("hour", "strftime('%H',((id >> 22)+1420070400000)/1000.0,'unixepoch')")):
                    db.execute(f"""INSERT INTO stats_counts
                        SELECT ?,{expression},COUNT(*) FROM messages
                        GROUP BY {expression}""", (kind,))
            db.execute("""CREATE TRIGGER IF NOT EXISTS messages_stats_insert
                AFTER INSERT ON messages BEGIN
                INSERT INTO stats_counts(kind,key,count) VALUES
                    ('total','',1),
                    ('guild',COALESCE(CAST(new.guild_id AS TEXT),''),1),
                    ('channel',COALESCE(CAST(new.channel_id AS TEXT),''),1),
                    ('author',COALESCE(CAST(new.author_id AS TEXT),''),1),
                    ('day',date(((new.id >> 22)+1420070400000)/1000.0,'unixepoch'),1),
                    ('hour',strftime('%H',((new.id >> 22)+1420070400000)/1000.0,'unixepoch'),1)
                ON CONFLICT(kind,key) DO UPDATE SET count=count+1;
                END""")
            db.execute("""CREATE TRIGGER IF NOT EXISTS messages_stats_delete
                AFTER DELETE ON messages BEGIN
                INSERT INTO stats_counts(kind,key,count) VALUES
                    ('total','',-1),
                    ('guild',COALESCE(CAST(old.guild_id AS TEXT),''),-1),
                    ('channel',COALESCE(CAST(old.channel_id AS TEXT),''),-1),
                    ('author',COALESCE(CAST(old.author_id AS TEXT),''),-1),
                    ('day',date(((old.id >> 22)+1420070400000)/1000.0,'unixepoch'),-1),
                    ('hour',strftime('%H',((old.id >> 22)+1420070400000)/1000.0,'unixepoch'),-1)
                ON CONFLICT(kind,key) DO UPDATE SET count=count-1;
                END""")
            db.execute("""CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING
                fts5(content, content='messages', content_rowid='rowid',
                     tokenize='trigram', detail='none', columnsize=0)""")
            db.execute("""CREATE TRIGGER IF NOT EXISTS messages_fts_insert
                AFTER INSERT ON messages BEGIN
                INSERT INTO messages_fts(rowid,content) VALUES (new.rowid,new.content);
                END""")
            db.execute("""CREATE TRIGGER IF NOT EXISTS messages_fts_delete
                AFTER DELETE ON messages BEGIN
                INSERT INTO messages_fts(messages_fts,rowid,content)
                VALUES ('delete',old.rowid,old.content);
                END""")
            db.execute("""CREATE TRIGGER IF NOT EXISTS messages_fts_update
                AFTER UPDATE OF content ON messages BEGIN
                INSERT INTO messages_fts(messages_fts,rowid,content)
                VALUES ('delete',old.rowid,old.content);
                INSERT INTO messages_fts(rowid,content) VALUES (new.rowid,new.content);
                END""")
            db.execute("""CREATE TRIGGER IF NOT EXISTS messages_history_delete
                AFTER DELETE ON messages BEGIN
                DELETE FROM message_history WHERE rowid=old.rowid;
                END""")
            db.execute("""CREATE TRIGGER IF NOT EXISTS messages_payload_delete
                AFTER DELETE ON messages BEGIN
                DELETE FROM message_payloads WHERE message_id=old.id;
                DELETE FROM message_revisions WHERE message_id=old.id;
                DELETE FROM legacy_message_attachments WHERE message_id=old.id;
                END""")
            if migrate or not has_fts:
                db.execute("INSERT INTO messages_fts(messages_fts) VALUES ('rebuild')")
                db.execute("INSERT INTO messages_fts(messages_fts) VALUES ('optimize')")
            db.execute("DROP VIEW IF EXISTS message_records")
            historical_timestamp = _historical_timestamp_sql('m.id')
            db.execute(f"""CREATE VIEW message_records AS
                SELECT m.id, CAST(m.channel_id AS TEXT) channel_id,
                       CAST(m.guild_id AS TEXT) guild_id,
                       CAST(m.author_id AS TEXT) author_id, m.content,
                       CASE WHEN h.rowid IS NOT NULL AND h.timestamp_override
                           THEN h.timestamp
                           WHEN h.rowid IS NOT NULL THEN {historical_timestamp}
                           ELSE
                           strftime('%Y-%m-%dT%H:%M:%fZ',
                           ((m.id >> 22)+1420070400000)/1000.0,'unixepoch') END timestamp,
                       m.image_urls,
                       CASE WHEN h.rowid IS NOT NULL THEN cn.value ELSE c.name END channel_name,
                       CASE WHEN h.rowid IS NOT NULL THEN gn.value ELSE g.name END guild_name,
                       CASE WHEN h.rowid IS NOT NULL THEN an.value ELSE a.name END author_name
                FROM messages m
                LEFT JOIN message_history h ON h.rowid=m.rowid
                LEFT JOIN name_values gn ON gn.id=h.guild_name_id
                LEFT JOIN name_values cn ON cn.id=h.channel_name_id
                LEFT JOIN name_values an ON an.id=h.author_name_id
                LEFT JOIN channels c ON c.id=CAST(m.channel_id AS TEXT)
                LEFT JOIN guilds g ON g.id=CAST(m.guild_id AS TEXT)
                LEFT JOIN authors a ON a.id=CAST(m.author_id AS TEXT)""")
            db.execute("CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT)")
            db.execute(f"PRAGMA user_version={COLLECTION_VERSION}")
            db.commit()
        except Exception:
            db.rollback()
            raise
        if migrate or migrate_history:
            db.execute("PRAGMA wal_checkpoint(TRUNCATE)")
            db.execute("VACUUM")
            db.execute("PRAGMA wal_checkpoint(TRUNCATE)")
            if db.execute("PRAGMA integrity_check").fetchone()[0] != "ok":
                raise RuntimeError("Migrated database failed integrity check; backup retained")
            db.execute("INSERT INTO messages_fts(messages_fts,rank) VALUES ('integrity-check',1)")
            db.commit()
            db.execute("PRAGMA wal_checkpoint(TRUNCATE)")
            # Keep a checked recovery copy until its owner confirms the upgrade.
            try:
                backup = _pack_backup(backup)
            except (OSError, RuntimeError) as exc:
                print(f"Could not compress recovery backup; uncompressed copy retained: {exc}")
    return {"migrated": migrate or migrate_history, "backup": backup,
            "credentials_migrated": credentials_migrated}


def _message_json(message):
    return json.dumps(message, ensure_ascii=False, sort_keys=True,
                      separators=(',', ':'), allow_nan=False)


async def save_message_payloads(db, msgs, cid, cname, gid, gname, *, source='collection'):
    """Save current content and every newly observed payload inside a transaction.

    The caller commits metadata and cursors with this write. A return value only
    counts previously unseen IDs, keeping collection limits independent of edits.
    """
    if not msgs:
        return 0
    incoming = {int(message['id']): message for message in msgs}
    placeholders = ','.join('?' for _ in incoming)
    async with db.execute(f"""SELECT m.id,m.rowid,m.channel_id,m.guild_id,m.author_id,
        m.content,m.image_urls,p.payload,p.deleted_at
        FROM messages m LEFT JOIN message_payloads p ON p.message_id=m.id
        WHERE m.id IN ({placeholders})""", tuple(incoming)) as cursor:
        previous = {row[0]: row for row in await cursor.fetchall()}
    now = datetime.now(timezone.utc).isoformat()
    rows, revisions, payloads = [], [], []
    for ident, message in incoming.items():
        author = message['author']
        if message.get('channel_id') is not None and str(message['channel_id']) != str(cid):
            raise ValueError('Received message belongs to a different channel')
        old = previous.get(ident)
        if old and (str(old[2]) != str(cid) or str(old[4]) != str(author['id'])):
            raise ValueError('Received message has different immutable archive identifiers')
        payload = _message_json(message)
        rows.append((ident, int(cid), int(gid) if gid is not None else None,
                     int(author['id']), message.get('content') or '',
                     '\n'.join(image_urls(message.get('attachments'))) or None))
        if old and old[7] is None:
            # Preserve the last selective version before replacing its content.
            legacy = {'id': str(ident), 'channel_id': str(old[2]),
                      'guild_id': str(old[3]) if old[3] is not None else None,
                      'author': {'id': str(old[4])}, 'content': old[5],
                      'archived_image_urls': old[6].split('\n') if old[6] else []}
            async with db.execute('SELECT attachments FROM legacy_message_attachments WHERE message_id=?',
                                  (ident,)) as cursor:
                attachments = await cursor.fetchone()
            if attachments:
                legacy['archived_attachments'] = attachments[0]
            revisions.append((ident, now, 'message', 'legacy_archive', _message_json(legacy)))
        if not old or old[7] != payload or old[8] is not None:
            revisions.append((ident, now, 'message', source, payload))
        payloads.append((ident, payload, now, now))
    await db.executemany("""INSERT INTO messages
        (id,channel_id,guild_id,author_id,content,image_urls) VALUES (?,?,?,?,?,?)
        ON CONFLICT(id) DO UPDATE SET content=excluded.content,image_urls=excluded.image_urls
        WHERE content IS NOT excluded.content OR image_urls IS NOT excluded.image_urls""", rows)
    await db.executemany("""INSERT INTO message_payloads(message_id,payload,first_seen_at,last_seen_at)
        VALUES (?,?,?,?) ON CONFLICT(message_id) DO UPDATE SET
        payload=excluded.payload,last_seen_at=excluded.last_seen_at,deleted_at=NULL,complete=1""", payloads)
    await db.executemany("""INSERT INTO message_revisions(message_id,observed_at,event,source,payload)
        VALUES (?,?,?,?,?)""", revisions)
    return len(incoming) - len(previous)


async def mark_messages_deleted(db, ids, *, source='confirmed_delete'):
    """Record confirmed upstream absence; retain searchable archived contents."""
    now = datetime.now(timezone.utc).isoformat()
    count = 0
    for ident in dict.fromkeys(int(value) for value in ids):
        async with db.execute('''SELECT m.content,m.channel_id,m.guild_id,m.author_id,m.image_urls,
            p.deleted_at,p.payload FROM messages m
            LEFT JOIN message_payloads p ON p.message_id=m.id WHERE m.id=?''', (ident,)) as cursor:
            old = await cursor.fetchone()
        if old is None or old[5] is not None:
            continue
        if old[6] is None:
            legacy = _message_json({'id': str(ident), 'channel_id': str(old[1]),
                                    'guild_id': str(old[2]) if old[2] is not None else None,
                                    'author': {'id': str(old[3])}, 'content': old[0],
                                    'archived_image_urls': old[4].split('\n') if old[4] else []})
            await db.execute('''INSERT INTO message_payloads
                (message_id,payload,first_seen_at,last_seen_at,deleted_at,complete) VALUES (?,?,?,?,?,0)''',
                (ident, legacy, now, now, now))
            await db.execute('''INSERT INTO message_revisions
                (message_id,observed_at,event,source,payload) VALUES (?,?,'message','legacy_archive',?)''',
                (ident, now, legacy))
        else:
            await db.execute('UPDATE message_payloads SET deleted_at=? WHERE message_id=?', (now, ident))
        await db.execute('''INSERT INTO message_revisions
            (message_id,observed_at,event,source,payload) VALUES (?,?,'deleted',?,NULL)''',
            (ident, now, source))
        count += 1
    return count


def read_message_archive(path, message_id, *, after_revision=0, limit=100):
    """Read a bounded page of exact saved responses and observation history."""
    limit = max(1, min(100, int(limit)))
    ident = int(message_id)
    with closing(sqlite3.connect(Path(path).resolve().as_uri() + '?mode=ro', uri=True)) as db:
        row = db.execute('''SELECT m.id,p.payload,p.first_seen_at,p.last_seen_at,p.deleted_at,p.complete
            FROM messages m LEFT JOIN message_payloads p ON p.message_id=m.id WHERE m.id=?''',
            (ident,)).fetchone()
        if row is None:
            return None
        legacy = db.execute('SELECT attachments FROM legacy_message_attachments WHERE message_id=?',
                            (ident,)).fetchone()
        revisions = db.execute('''SELECT revision_id,observed_at,event,source,payload
            FROM message_revisions WHERE message_id=? AND revision_id>?
            ORDER BY revision_id LIMIT ?''', (ident, int(after_revision), limit + 1)).fetchall()
        return {'id': str(row[0]), 'payload': json.loads(row[1]) if row[1] else None,
                'complete': bool(row[5]) if row[1] else False,
                'first_seen_at': row[2], 'last_seen_at': row[3], 'deleted_at': row[4],
                'legacy_attachments': legacy[0] if legacy else None,
                'revisions': [{'revision_id': revision[0], 'observed_at': revision[1],
                               'event': revision[2], 'source': revision[3],
                               'payload': json.loads(revision[4]) if revision[4] else None}
                              for revision in revisions[:limit]],
                'has_more': len(revisions) > limit,
                'next_revision': revisions[limit - 1][0] if len(revisions) > limit else None}
