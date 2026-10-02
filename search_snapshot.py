"""Build a compact, lossless search-only snapshot without modifying its source.

python search_snapshot.py SOURCE.db OUTPUT.db
"""
import argparse
from bisect import bisect_left
from contextlib import closing
from functools import lru_cache
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import struct
import time
import zlib

VERSION = 101
BLOCK_ROWS = 128
FIELDS = ('id', 'channel_id', 'guild_id', 'author_id', 'content', 'timestamp',
          'image_urls', 'channel_name', 'guild_name', 'author_name')
BODY_FIELDS = FIELDS[4:]
METADATA = ('guilds', 'channels', 'authors', 'profiles', 'profile_details', 'stats_counts')

SCHEMA = """
CREATE TABLE snapshot_info(key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;
CREATE TABLE ref_values(ref INTEGER PRIMARY KEY, value TEXT NOT NULL UNIQUE);
CREATE TABLE record_names(id INTEGER PRIMARY KEY, value TEXT NOT NULL UNIQUE);
CREATE TABLE message_sources(ref INTEGER PRIMARY KEY, channel_id TEXT, guild_id TEXT,
    UNIQUE(channel_id,guild_id));
CREATE INDEX idx_sources_guild ON message_sources(guild_id);
CREATE TABLE message_index(rowid INTEGER PRIMARY KEY, source_id INTEGER, author_id INTEGER);
CREATE TABLE message_blocks(block INTEGER PRIMARY KEY, first_id INTEGER NOT NULL UNIQUE,
    payload BLOB NOT NULL);
CREATE VIRTUAL TABLE messages_fts USING fts5(content, content='',
    tokenize='trigram', detail='none', columnsize=0);
CREATE VIRTUAL TABLE message_short_fts USING fts5(tokens, content='',
    tokenize='ascii', detail='none', columnsize=0);
CREATE VIEW message_records AS
    SELECT record_id(m.rowid) id, s.channel_id, s.guild_id, a.value author_id,
        record_content(m.rowid) content, record_timestamp(m.rowid) timestamp,
        record_image_urls(m.rowid) image_urls, record_channel_name(m.rowid) channel_name,
        record_guild_name(m.rowid) guild_name, record_author_name(m.rowid) author_name,
        m.rowid storage_rowid
    FROM message_index m
    LEFT JOIN message_sources s ON s.ref=m.source_id
    LEFT JOIN ref_values a ON a.ref=m.author_id;
"""


def canonical(row):
    return json.dumps(list(row), ensure_ascii=False, separators=(',', ':')).encode('utf-8')


def digest_row(digest, row):
    data = canonical(row)
    digest.update(struct.pack('>Q', len(data)))
    digest.update(data)


@lru_cache(maxsize=64)
def decoded_block(payload):
    # Key by the compressed bytes, so reuse is safe across connections and
    # archives. Keep only 64 blocks; never cache the entire message corpus.
    return json.loads(zlib.decompress(payload))


def install_reader(db):
    block_rows = int(db.execute("SELECT value FROM snapshot_info WHERE key='block_rows'").fetchone()[0])
    if not 16 <= block_rows <= 512:
        raise sqlite3.DatabaseError('Invalid snapshot block size')

    @lru_cache(maxsize=16)
    def block(number):
        payload = db.execute('SELECT payload FROM message_blocks WHERE block=?', (number,)).fetchone()
        if payload is None:
            raise sqlite3.DatabaseError('Missing message block')
        return decoded_block(payload[0])

    def field(rowid, index):
        value = block((rowid - 1) // block_rows + 1)[(rowid - 1) % block_rows][index]
        return read_name(value) if index >= 4 and value is not None else value

    @lru_cache(maxsize=256)
    def read_name(key):
        return db.execute('SELECT value FROM record_names WHERE id=?', (key,)).fetchone()[0]

    for index, name in enumerate(('id',) + BODY_FIELDS):
        db.create_function('record_' + name, 1, lambda rowid, i=index: field(rowid, i), deterministic=True)
    # One ID per compressed block is indexed. Binary search within that block
    # replaces the duplicate public-ID index that otherwise costs bytes per row.
    def before_id(value):
        row = db.execute('SELECT block FROM message_blocks WHERE first_id<? ORDER BY first_id DESC LIMIT 1', (value,)).fetchone()
        if row is None:
            return 0
        ids = [r[0] for r in block(row[0])]
        return (row[0]-1)*block_rows + bisect_left(ids, value)
    db.create_function('row_before_id', 1, before_id, deterministic=True)
    return block


ASCII_FOLD = str.maketrans('ABCDEFGHIJKLMNOPQRSTUVWXYZ', 'abcdefghijklmnopqrstuvwxyz')


@lru_cache(maxsize=4096)
def char_code(char):
    return f'{ord(char):06x}'


def short_token(value):
    return ('u' if len(value) == 1 else 'p') + ''.join(char_code(c) for c in value)


def short_terms(contents):
    # Index short text once per block, rather than repeating a second posting
    # list for every message. ASCII folding matches SQLite LIKE exactly.
    terms = set()
    for content in contents:
        text = (content or '').split('\0', 1)[0].translate(ASCII_FOLD)
        terms.update('u' + char_code(c) for c in set(text))
        terms.update('p' + char_code(a) + char_code(b) for a, b in zip(text, text[1:]))
    return ' '.join(sorted(terms))


def trigram_query(q):
    # SQLite LIKE stops at NUL. Grams beyond that point cannot be required.
    value = q.split('\0', 1)[0]
    if len(value) < 3:
        return None
    # Conjunctions of short tokens work with detail=none. Literal verification
    # is still necessary: this index stores neither positions nor original text.
    steps = range(0, len(value) - 2, max(1, (len(value) - 2) // 8))
    grams = dict.fromkeys(value[i:i+3] for i in steps)
    return ' AND '.join('"' + gram.replace('"', '""') + '"' for gram in grams)


def search_ids(db, q, filters, lower, upper, before, take, literal):
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


def profile_servers(db, uid, q, offset, limit, normalize):
    ref = db.execute('SELECT ref FROM ref_values WHERE value=?', (uid,)).fetchone()
    if ref is None:
        return {'servers': [], 'has_more': False, 'offset': offset}
    db.create_function('search_label', 1, normalize, deterministic=True)
    value = normalize(q).replace('%', '!%').replace('_', '!_')
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
    return {'servers': [dict(r) for r in rows[:limit]], 'has_more': len(rows)>limit, 'offset': offset}


def page_usage(db, schema='main'):
    try:
        return {row[0]: row[1] for row in db.execute(
            'SELECT name,SUM(pgsize) FROM dbstat(?) GROUP BY name', (schema,))}
    except sqlite3.OperationalError as exc:
        # Stock Python SQLite builds may omit this diagnostic extension.
        # Total page counts and complete export verification still work.
        if str(exc) == 'no such table: dbstat':
            return None
        raise


def build_snapshot(source, destination, *, block_rows=BLOCK_ROWS, progress=None):
    source, destination = Path(source).resolve(), Path(destination).resolve()
    if not source.is_file():
        raise ValueError('Source archive does not exist')
    if source == destination or destination.exists():
        raise ValueError('Refusing to replace an existing file or the source archive')
    if not 16 <= block_rows <= 512:
        raise ValueError('Block size must be between 16 and 512')
    destination.parent.mkdir(parents=True, exist_ok=True)
    temporary = destination.with_name(destination.name + '.building')
    # Exclusive creation protects another exporter or a leftover partial build.
    temporary.touch(exist_ok=False)
    started = time.monotonic()
    source_stamp = (source.stat().st_size, source.stat().st_mtime_ns)
    count, source_digest = 0, hashlib.sha256()
    try:
        with closing(sqlite3.connect(temporary, uri=True)) as db:
            db.row_factory = sqlite3.Row
            db.execute('PRAGMA main.journal_mode=DELETE')
            db.execute('PRAGMA main.synchronous=OFF')
            db.executescript(SCHEMA)
            # SQLite pins a consistent read transaction, including pending WAL
            # rows. mode=ro never migrates or changes the source database.
            db.execute('ATTACH DATABASE ? AS source', (source.as_uri() + '?mode=ro',))
            db.execute('BEGIN')
            if db.execute('PRAGMA source.user_version').fetchone()[0] != 9:
                raise ValueError('Export needs a schema v9 collection archive')
            expected = db.execute('SELECT COUNT(*) FROM source.messages').fetchone()[0]
            source_bytes = db.execute('PRAGMA source.page_count').fetchone()[0] * db.execute('PRAGMA source.page_size').fetchone()[0]
            source_pages = page_usage(db, 'source')
            for table in METADATA:
                sql = db.execute("SELECT sql FROM source.sqlite_master WHERE type='table' AND name=?", (table,)).fetchone()
                if sql is None:
                    if table == 'profile_details':
                        db.execute('CREATE TABLE profile_details(user_id TEXT PRIMARY KEY,payload TEXT NOT NULL,fetched_at TEXT NOT NULL)')
                        continue
                    raise ValueError('Missing required archive metadata table')
                db.execute(sql[0])
                db.execute(f'INSERT INTO {table} SELECT * FROM source.{table}')
            db.execute("""INSERT INTO ref_values(value)
                SELECT DISTINCT CAST(author_id AS TEXT) FROM source.messages WHERE author_id IS NOT NULL""")
            db.execute("""INSERT INTO message_sources(channel_id,guild_id)
                SELECT DISTINCT CAST(channel_id AS TEXT), CAST(guild_id AS TEXT) FROM source.messages""")
            db.execute("""INSERT INTO record_names(value)
                SELECT value FROM source.name_values WHERE value IS NOT NULL
                UNION SELECT name FROM source.guilds WHERE name IS NOT NULL
                UNION SELECT name FROM source.channels WHERE name IS NOT NULL
                UNION SELECT name FROM source.authors WHERE name IS NOT NULL""")
            fields = ','.join('r.' + column for column in FIELDS)
            rows = db.execute(f"""SELECT {fields}, s.ref,a.ref,nc.id,ng.id,na.id FROM source.message_records r
                LEFT JOIN message_sources s ON s.channel_id IS r.channel_id AND s.guild_id IS r.guild_id
                LEFT JOIN ref_values a ON a.value=r.author_id
                LEFT JOIN record_names nc ON nc.value=r.channel_name
                LEFT JOIN record_names ng ON ng.value=r.guild_name
                LEFT JOIN record_names na ON na.value=r.author_name ORDER BY r.id""")
            while batch := rows.fetchmany(block_rows):
                indices, texts, body = [], [], []
                for row in batch:
                    count += 1
                    values = tuple(row[:len(FIELDS)])
                    digest_row(source_digest, values)
                    indices.append((count, *row[len(FIELDS):len(FIELDS)+2]))
                    texts.append((count, values[4]))
                    body.append((values[0], *values[4:7], *row[len(FIELDS)+2:]))
                db.executemany('INSERT INTO message_index VALUES (?,?,?)', indices)
                db.executemany('INSERT INTO messages_fts(rowid,content) VALUES (?,?)', texts)
                number = (count-1)//block_rows + 1
                db.execute('INSERT INTO message_short_fts(rowid,tokens) VALUES (?,?)',
                           (number, short_terms([r[1] for r in body])))
                db.execute('INSERT INTO message_blocks VALUES (?,?,?)',
                           (number, body[0][0], zlib.compress(canonical(body), 9)))
                if progress and count % (block_rows * 1024) == 0:
                    progress({'phase':'pack','messages':count,'total':expected})
            if count != expected:
                raise RuntimeError('Message count did not round-trip')
            db.execute("INSERT INTO messages_fts(messages_fts) VALUES ('optimize')")
            db.execute("INSERT INTO message_short_fts(message_short_fts) VALUES ('optimize')")
            db.execute('CREATE INDEX idx_snapshot_source ON message_index(source_id)')
            db.execute('CREATE INDEX idx_snapshot_author ON message_index(author_id)')
            db.execute('CREATE INDEX idx_authors_name ON authors(name COLLATE NOCASE)')
            db.execute('CREATE INDEX idx_profiles_username ON profiles(username COLLATE NOCASE)')
            info = {'format':'searchcord-search-snapshot','block_rows':str(block_rows),
                    'messages':str(count),'records_sha256':source_digest.hexdigest()}
            db.executemany('INSERT INTO snapshot_info VALUES (?,?)', info.items())
            db.commit()
            db.execute('DETACH DATABASE source')
            db.execute('ANALYZE')
            db.execute('VACUUM')
            if db.execute('PRAGMA integrity_check').fetchone()[0] != 'ok':
                raise RuntimeError('Snapshot failed SQLite integrity check')
            db.execute("INSERT INTO messages_fts(messages_fts) VALUES ('integrity-check')")
            db.execute("INSERT INTO message_short_fts(message_short_fts) VALUES ('integrity-check')")
            db.commit()
            install_reader(db)
            restored = hashlib.sha256()
            for row in db.execute('SELECT ' + ','.join(FIELDS) + ' FROM message_records ORDER BY storage_rowid'):
                digest_row(restored, row)
            if restored.digest() != source_digest.digest():
                raise RuntimeError('Message fields did not round-trip')
            if db.execute("SELECT 1 FROM sqlite_master WHERE type='table' AND name='settings'").fetchone():
                raise RuntimeError('Snapshot unexpectedly contains settings')
            db.execute(f'PRAGMA user_version={VERSION}')
            db.commit()
            pages = page_usage(db)
        # Publish only a completely verified file, without overwriting a racer.
        os.link(temporary, destination)
        temporary.unlink()
        size = destination.stat().st_size
        return {'messages':count,'source_bytes':source_bytes,'snapshot_bytes':size,
                'source_bytes_per_message':round(source_bytes/count,2) if count else 0,
                'snapshot_bytes_per_message':round(size/count,2) if count else 0,
                'saved_percent':round((1-size/source_bytes)*100,2),
                'records_sha256':source_digest.hexdigest(),'fields_verified':list(FIELDS),
                'integrity':'ok','source_pages':source_pages,'snapshot_pages':pages,
                'source_main_file_unchanged':source_stamp==(source.stat().st_size,source.stat().st_mtime_ns),
                'build_seconds':round(time.monotonic()-started,2)}
    except BaseException:
        temporary.unlink(missing_ok=True)
        Path(str(temporary) + '-journal').unlink(missing_ok=True)
        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source', type=Path)
    parser.add_argument('destination', type=Path)
    parser.add_argument('--block-rows', type=int, default=BLOCK_ROWS)
    parser.add_argument('--report', type=Path, help='Save aggregate measurements without message data')
    args = parser.parse_args()
    if args.report:
        if args.report.resolve() in (args.source.resolve(), args.destination.resolve()):
            parser.error('The report must be separate from both archive files')
        if args.report.exists():
            parser.error('Refusing to replace an existing report')
    report = build_snapshot(args.source, args.destination, block_rows=args.block_rows,
                           progress=lambda value: print(json.dumps(value), flush=True))
    if args.report:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        with args.report.open('x', encoding='utf-8') as output:
            output.write(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report, indent=2))


if __name__ == '__main__':
    main()
