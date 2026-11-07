"""Disposable archive checks; no personal DBs, credentials or Discord calls."""
import asyncio
from contextlib import closing, contextmanager
import gzip
import json
import os
from pathlib import Path
import sqlite3
import tempfile
import unittest
from unittest.mock import patch

import aiosqlite

import credential_store as credentials
import storage


@contextmanager
def connect(path):
    with closing(sqlite3.connect(path)) as db, db:
        yield db


class ArchiveStorageTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.path = self.root / 'archive' / 'synthetic.db'
        self.vault = self.root / 'credentials' / 'synthetic.vault'
        self.environment = patch.dict(os.environ, {'SEARCHCORD_CREDENTIAL_FILE': str(self.vault),
            'SEARCHCORD_CREDENTIAL_KEY_FILE': str(self.vault.with_suffix('.key'))})
        self.environment.start()
        self.addCleanup(self.environment.stop)
        self.addCleanup(self.temp.cleanup)

    def message(self, content='first', attachment='https://cdn.discordapp.com/attachments/20/30/file.zip'):
        return {'id': '1450000000000000001', 'channel_id': '20',
                'timestamp': '2026-01-01T00:00:00+00:00', 'edited_timestamp': None,
                'content': content, 'author': {'id': '30', 'username': 'fixture'},
                'attachments': [{'id': '40', 'filename': 'file.zip', 'url': attachment,
                                 'content_type': 'application/zip', 'size': 500}],
                'embeds': [{'description': 'kept'}], 'poll': {'question': {'text': 'kept'}}}

    def test_native_credentials_migrate_out_of_sqlite_and_backup(self):
        storage.init_database(str(self.path))
        token = 'synthetic-collector-credential-do-not-ship'
        with connect(self.path) as db:
            db.execute("INSERT INTO settings VALUES ('token',?)", (token,))
        result = storage.init_database(str(self.path))
        self.assertTrue(result['credentials_migrated'])
        self.assertEqual(credentials.load_credentials(self.path)['tokens'][0]['token'], token)
        self.assertNotIn(token.encode(), self.vault.read_bytes())
        with connect(self.path) as db:
            self.assertNotIn(token, repr(db.execute('SELECT * FROM settings').fetchall()))
            # Pretend an old plaintext record is still present: recovery copies
            # must sanitize and rebuild their own pages regardless of source.
            db.execute("INSERT INTO settings VALUES ('token',?)", (token,))
            db.commit()
            db.execute('BEGIN IMMEDIATE')
            backup = storage._backup(self.path)
            db.rollback()
        self.assertNotIn(token.encode(), Path(backup).read_bytes())
        with connect(backup) as db:
            self.assertEqual(db.execute('SELECT COUNT(*) FROM settings').fetchone()[0], 0)
            self.assertEqual(db.execute('PRAGMA integrity_check').fetchone()[0], 'ok')
        # The source was not scrubbed or erased as a side effect of backup.
        with connect(self.path) as db:
            self.assertEqual(db.execute("SELECT value FROM settings WHERE key='token'").fetchone()[0], token)
        credentials.clear_credentials(self.path)
        self.assertEqual(credentials.load_credentials(self.path)['tokens'], [])

    def test_portable_vault_fails_closed_when_key_is_missing(self):
        with patch.object(credentials, '_USE_DPAPI', False):
            tokens = [{'id': 'one', 'label': 'Fixture', 'token': 'synthetic-portable-token'}]
            credentials.save_credentials(self.path, tokens, 'one')
            self.assertNotIn(b'synthetic-portable-token', self.vault.read_bytes())
            self.assertEqual(credentials.load_credentials(self.path)['tokens'], tokens)
            self.vault.with_suffix('.key').unlink()
            before = self.vault.read_bytes()
            with self.assertRaises(credentials.CredentialError):
                credentials.save_credentials(self.path, tokens, 'one')
            self.assertEqual(before, self.vault.read_bytes())
            credentials.clear_credentials(self.path)

    def test_vault_cannot_be_captured_with_archive_directory(self):
        with patch.dict(os.environ, {'SEARCHCORD_CREDENTIAL_FILE': str(self.path.parent / 'secret.vault')}):
            with self.assertRaises(credentials.CredentialError):
                credentials.load_credentials(self.path)

    def test_exact_payload_edits_deletions_fts_and_counts(self):
        storage.init_database(str(self.path))
        async def run():
            async with aiosqlite.connect(self.path) as db:
                async def save(message):
                    await db.execute('BEGIN')
                    result = await storage.save_message_payloads(db, [message], '20', 'Channel', '10', 'Guild')
                    await db.commit()
                    return result
                self.assertEqual(await save(self.message('first searchable')), 1)
                self.assertEqual(await save(self.message('first searchable')), 0)
                with connect(self.path) as verify:
                    rowid = verify.execute('SELECT rowid FROM messages').fetchone()[0]
                edited = self.message('edited searchable')
                edited['edited_timestamp'] = '2026-01-02T00:00:00+00:00'
                self.assertEqual(await save(edited), 0)
                await db.execute('BEGIN')
                self.assertEqual(await storage.mark_messages_deleted(db, [edited['id']], source='confirmed_delete'), 1)
                self.assertEqual(await storage.mark_messages_deleted(db, [edited['id']]), 0)
                await db.commit()
                deleted = storage.read_message_archive(self.path, edited['id'])
                self.assertIsNotNone(deleted['deleted_at'])
                self.assertEqual([r['event'] for r in deleted['revisions']], ['message', 'message', 'deleted'])
                self.assertEqual(deleted['payload'], edited)
                self.assertEqual(await save(edited), 0)
                restored = storage.read_message_archive(self.path, edited['id'])
                self.assertIsNone(restored['deleted_at'])
                self.assertEqual(len(restored['revisions']), 4)
                self.assertTrue(restored['complete'])
                with connect(self.path) as verify:
                    self.assertEqual(verify.execute('SELECT rowid FROM messages').fetchone()[0], rowid)
                    self.assertEqual(verify.execute("SELECT count FROM stats_counts WHERE kind='total'").fetchone()[0], 1)
                    self.assertEqual(verify.execute("SELECT COUNT(*) FROM messages_fts WHERE content LIKE '%edited%'").fetchone()[0], 1)
                    self.assertEqual(verify.execute("SELECT COUNT(*) FROM messages_fts WHERE content LIKE '%first%'").fetchone()[0], 0)
                    verify.execute("INSERT INTO messages_fts(messages_fts,rank) VALUES ('integrity-check',1)")
                    self.assertEqual(verify.execute('PRAGMA integrity_check').fetchone()[0], 'ok')
                page = storage.read_message_archive(self.path, edited['id'], limit=2)
                self.assertTrue(page['has_more'])
                self.assertEqual(len(page['revisions']), 2)
                next_page = storage.read_message_archive(self.path, edited['id'], after_revision=page['next_revision'], limit=2)
                self.assertFalse(next_page['has_more'])
                await db.execute('DELETE FROM messages')
                await db.commit()
                async with db.execute('SELECT COUNT(*) FROM message_revisions') as cur:
                    self.assertEqual((await cur.fetchone())[0], 0)
        asyncio.run(run())

    def test_legacy_attachment_migration_preserves_all_original_values(self):
        self.path.parent.mkdir(parents=True)
        attachments = json.dumps(['https://cdn.discordapp.com/file.png', 'https://cdn.discordapp.com/file.zip'])
        with connect(self.path) as db:
            db.execute('''CREATE TABLE messages(id TEXT PRIMARY KEY,channel_id TEXT,guild_id TEXT,
                author_id TEXT,content TEXT,timestamp TEXT,attachments TEXT,channel_name TEXT,
                guild_name TEXT,author_name TEXT)''')
            db.execute('INSERT INTO messages VALUES (?,?,?,?,?,?,?,?,?,?)',
                (self.message()['id'], '20', '10', '30', 'legacy exact',
                 '2026-01-01T00:00:00+00:00', attachments, 'Channel', 'Guild', 'Author'))
        result = storage.init_database(str(self.path))
        self.assertTrue(result['migrated'])
        with connect(self.path) as db:
            self.assertEqual(db.execute('PRAGMA user_version').fetchone()[0], 10)
            self.assertEqual(db.execute('SELECT content,timestamp,channel_name,guild_name,author_name FROM message_records').fetchone(),
                ('legacy exact', '2026-01-01T00:00:00+00:00', 'Channel', 'Guild', 'Author'))
            self.assertEqual(db.execute('SELECT attachments FROM legacy_message_attachments').fetchone()[0], attachments)
            self.assertEqual(db.execute('PRAGMA integrity_check').fetchone()[0], 'ok')
        with gzip.open(result['backup'], 'rb') as backup:
            self.assertGreater(len(backup.read()), 0)
        before = storage.read_message_archive(self.path, self.message()['id'])
        self.assertFalse(before['complete'])
        self.assertIsNone(before['payload'])
        async def observe():
            async with aiosqlite.connect(self.path) as db:
                await db.execute('BEGIN')
                await storage.save_message_payloads(db, [self.message('updated')], '20', 'Channel', '10', 'Guild')
                await db.commit()
        asyncio.run(observe())
        after = storage.read_message_archive(self.path, self.message()['id'])
        self.assertEqual(after['revisions'][0]['source'], 'legacy_archive')
        self.assertEqual(after['revisions'][0]['payload']['content'], 'legacy exact')
        self.assertEqual(after['revisions'][0]['payload']['archived_attachments'], attachments)
        self.assertEqual(after['payload'], self.message('updated'))

    def test_old_history_attachments_survive_timestamp_compression(self):
        storage.init_database(str(self.path))
        attachments = json.dumps(['https://cdn.discordapp.com/file.zip'])
        with connect(self.path) as db:
            db.execute('ALTER TABLE message_history ADD COLUMN attachments TEXT')
            db.execute('INSERT INTO messages(id,channel_id,guild_id,author_id,content) VALUES (?,?,?,?,?)',
                       (int(self.message()['id']), 20, 10, 30, 'unchanged'))
            rowid = db.execute('SELECT rowid FROM messages').fetchone()[0]
            db.execute('''INSERT INTO message_history(rowid,timestamp,timestamp_override,attachments)
                VALUES (?,?,1,?)''', (rowid, '2020-01-01T00:00:00+00:00', attachments))
            db.execute('PRAGMA user_version=8')
        result = storage.init_database(str(self.path))
        self.assertTrue(result['migrated'])
        with connect(self.path) as db:
            self.assertEqual(db.execute('SELECT timestamp FROM message_records').fetchone()[0],
                             '2020-01-01T00:00:00+00:00')
            self.assertEqual(db.execute('SELECT attachments FROM legacy_message_attachments').fetchone()[0], attachments)
            self.assertEqual(db.execute('SELECT rowid FROM messages').fetchone()[0], rowid)

    def test_invalid_message_rolls_back_payload_and_search_together(self):
        storage.init_database(str(self.path))
        async def run():
            async with aiosqlite.connect(self.path) as db:
                await db.execute('BEGIN')
                await storage.save_message_payloads(db, [self.message()], '20', 'Channel', '10', 'Guild')
                await db.commit()
                edited = self.message('should never commit')
                edited['channel_id'] = 'wrong-channel'
                await db.execute('BEGIN')
                with self.assertRaises(ValueError):
                    await storage.save_message_payloads(db, [edited], '20', 'Channel', '10', 'Guild')
                await db.rollback()
        asyncio.run(run())
        current = storage.read_message_archive(self.path, self.message()['id'])
        self.assertEqual(current['payload'], self.message())
        self.assertEqual(len(current['revisions']), 1)
        with connect(self.path) as db:
            self.assertEqual(db.execute('SELECT content FROM messages').fetchone()[0], 'first')


if __name__ == '__main__':
    unittest.main()
