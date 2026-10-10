"""Build a compact, lossless search-only snapshot without modifying its source.

python search_snapshot.py SOURCE.db OUTPUT.db
"""
import argparse
from contextlib import closing
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import time
import zlib

from snapshot_codec import (VERSION, BLOCK_ROWS, FIELDS, METADATA, SCHEMA,
                            canonical, digest_row, short_terms, install_reader)


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
            if db.execute('PRAGMA source.user_version').fetchone()[0] not in (9, 10):
                raise ValueError('Export needs a schema v9/v10 collection archive')
            expected = db.execute('SELECT COUNT(*) FROM source.messages').fetchone()[0]
            if db.execute("SELECT 1 FROM source.sqlite_master WHERE name='message_payloads' AND type='table'").fetchone():
                db.execute("""INSERT INTO message_attachments
                    SELECT message_id,json_extract(payload,'$.attachments') FROM source.message_payloads
                    WHERE json_array_length(json_extract(payload,'$.attachments'))>0""")
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
    parser.add_argument('--with-media', action='store_true', help='Copy saved referenced media to a new sibling media directory')
    args = parser.parse_args()
    media_destination = args.destination.resolve().parent/'media'
    if args.with_media and (media_destination.exists() or media_destination.is_symlink()):
        parser.error('Media export needs a new destination directory without an existing media cache')
    if args.report:
        if args.report.resolve() in (args.source.resolve(), args.destination.resolve()):
            parser.error('The report must be separate from both archive files')
        if args.report.exists():
            parser.error('Refusing to replace an existing report')
    report = build_snapshot(args.source, args.destination, block_rows=args.block_rows,
                           progress=lambda value: print(json.dumps(value), flush=True))
    if args.with_media:
        report['media'] = export_media(args.source,args.destination)
    if args.report:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        with args.report.open('x', encoding='utf-8') as output:
            output.write(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report, indent=2))


def export_media(source, destination):
    """Copy references from the verified snapshot, never unrelated cached files."""
    from media_store import copy_referenced_media
    from profile_store import image_url

    def references():
        with closing(sqlite3.connect(Path(destination).resolve().as_uri()+'?mode=ro&immutable=1',uri=True)) as db:
            install_reader(db)
            for row in db.execute('SELECT image_urls FROM message_records'):
                yield from (row[0] or '').splitlines()
            if db.execute("SELECT 1 FROM sqlite_master WHERE name='message_attachments'").fetchone():
                for row in db.execute('SELECT payload FROM message_attachments'):
                    for attachment in json.loads(row[0]):
                        if isinstance(attachment,dict):
                            yield attachment.get('url')
            for uid,avatar,banner in db.execute('SELECT user_id,avatar_hash,banner_hash FROM profiles'):
                yield image_url(uid,avatar)
                yield image_url(uid,banner,'banners',1024)
            for uid,payload in db.execute('SELECT user_id,payload FROM profile_details'):
                details = json.loads(payload)
                user = details.get('user') or {}
                profile = details.get('user_profile') or {}
                yield image_url(uid,user.get('avatar'))
                yield image_url(uid,profile.get('banner') or user.get('banner'),'banners',1024)
    return copy_referenced_media(source,destination,references())


if __name__ == '__main__':
    main()
