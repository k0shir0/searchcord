"""Authoritative packed archive schema, encoding and bounded decoding.

Shared by archive reading and snapshot export; no web or collector startup.
"""
from bisect import bisect_left
from functools import lru_cache
import json
import sqlite3
import struct
import zlib

VERSION = 101
BLOCK_ROWS = 128
FIELDS = ('id', 'channel_id', 'guild_id', 'author_id', 'content', 'timestamp',
          'image_urls', 'channel_name', 'guild_name', 'author_name')
BODY_FIELDS = FIELDS[4:]
METADATA = ('guilds', 'channels', 'authors', 'profiles', 'profile_details', 'stats_counts')

SCHEMA = """
CREATE TABLE snapshot_info(key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;
CREATE TABLE message_attachments(message_id INTEGER PRIMARY KEY,payload TEXT NOT NULL);
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
