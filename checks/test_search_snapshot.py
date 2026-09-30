"""Compare compressed snapshots against the existing search and profile contract."""
from contextlib import closing
import json
import io
from pathlib import Path
import sqlite3
import tempfile
import unittest
from unittest.mock import patch

from fastapi.testclient import TestClient
from checks import test_search_display
from search_app import Archive, create_app
from search_snapshot import build_snapshot, FIELDS, METADATA, VERSION, main as snapshot_main
from storage import init_database


class SnapshotTests(test_search_display.DisplayTests):
    @classmethod
    def setUpClass(cls):
        super().setUpClass()
        cls.client.close()
        cls.snapshot = cls.path.parent / 'snapshot.db'
        cls.report = build_snapshot(cls.path, cls.snapshot, block_rows=16)
        cls.client = TestClient(create_app(cls.snapshot, True))

    def test_all_fields_and_profile_metadata_round_trip(self):
        with closing(sqlite3.connect(self.path)) as source, Archive(self.snapshot, True).connect() as output:
            columns = ','.join(FIELDS)
            self.assertEqual(source.execute(f'SELECT {columns} FROM message_records ORDER BY id').fetchall(),
                             [tuple(r) for r in output.execute(f'SELECT {columns} FROM message_records ORDER BY id')])
            for table in METADATA:
                self.assertEqual(source.execute(f'SELECT * FROM {table} ORDER BY 1,2').fetchall(),
                                 [tuple(r) for r in output.execute(f'SELECT * FROM {table} ORDER BY 1,2')], table)
            names = {r[0] for r in output.execute('SELECT name FROM sqlite_master')}
            self.assertNotIn('settings',names);self.assertNotIn('scrape_cursors',names)
            self.assertEqual(output.execute('PRAGMA user_version').fetchone()[0],VERSION)
            with self.assertRaises(sqlite3.OperationalError):output.execute('DELETE FROM message_index')
        self.assertTrue(self.report['source_main_file_unchanged'])
        self.assertEqual(self.report['integrity'],'ok')
        self.assertEqual(sum(self.report['source_pages'].values()),self.report['source_bytes'])

    def test_empty_archive_and_existing_output_are_safe(self):
        with tempfile.TemporaryDirectory() as directory:
            source=Path(directory)/'source.db';target=Path(directory)/'target.db'
            init_database(str(source))
            report=build_snapshot(source,target)
            self.assertEqual(report['messages'],0)
            self.assertEqual(Archive(target,True).search()['messages'],[])
            before=target.read_bytes()
            with self.assertRaises(ValueError):build_snapshot(source,target)
            with self.assertRaises(ValueError):build_snapshot(source,source)
            self.assertEqual(before,target.read_bytes())
            with closing(sqlite3.connect(source)) as db:
                self.assertEqual(db.execute('PRAGMA user_version').fetchone()[0],9)

    def test_export_includes_pending_wal_and_preserves_live_source(self):
        with tempfile.TemporaryDirectory() as directory:
            source=Path(directory)/'source.db';target=Path(directory)/'target.db'
            init_database(str(source))
            with closing(sqlite3.connect(source)) as writer:
                writer.execute("INSERT INTO settings VALUES ('token','synthetic-credential')")
                writer.execute("INSERT INTO messages(id,content) VALUES (1000000000000000000,'pending WAL text')")
                writer.commit()
                self.assertGreater(Path(str(source)+'-wal').stat().st_size,0)
                stamps=[(p.stat().st_size,p.stat().st_mtime_ns) for p in (source,Path(str(source)+'-wal'))]
                report=build_snapshot(source,target)
                self.assertEqual(report['messages'],1)
                self.assertEqual(Archive(target,True).search(q='WAL')['messages'][0]['content'],'pending WAL text')
                self.assertEqual(stamps,[(p.stat().st_size,p.stat().st_mtime_ns) for p in (source,Path(str(source)+'-wal'))])
                self.assertEqual(writer.execute("SELECT value FROM settings WHERE key='token'").fetchone()[0],'synthetic-credential')

    def test_cli_report_cannot_replace_an_archive(self):
        with tempfile.TemporaryDirectory() as directory:
            source, target = Path(directory)/'source.db', Path(directory)/'target.db'
            init_database(str(source))
            original = source.read_bytes()
            for report in (source, target):
                args = ['search_snapshot.py', str(source), str(target), '--report', str(report)]
                with patch('sys.argv', args), patch('sys.stderr', io.StringIO()):
                    with self.assertRaises(SystemExit) as error:
                        snapshot_main()
                self.assertEqual(error.exception.code, 2)
                self.assertEqual(original, source.read_bytes())
                self.assertFalse(target.exists())

    def test_unicode_punctuation_and_nul_queries_match_sqlite_like(self):
        with tempfile.TemporaryDirectory() as directory:
            source=Path(directory)/'source.db';target=Path(directory)/'target.db'
            init_database(str(source))
            contents=['abc def','abc XXX def','abc"xyz','a%_value','FOO_bar',
                'İstanbul Straße','istanbul STRASSE','Héllo héLLo','HELLO',
                '夏天好天气','🧊ice 🔒privacy','x\0abc','abc\0def','ends abc','',None]
            with closing(sqlite3.connect(source)) as db, db:
                db.executemany('INSERT INTO messages(id,content) VALUES (?,?)',
                    [(1000000000000000000+i,value) for i,value in enumerate(contents)])
            build_snapshot(source,target,block_rows=16)
            with closing(sqlite3.connect(source)) as db:
                for query in ['abc def','abc','abc"','a%_','FOO_','İst','ist','Straße','strasse',
                              'Héllo','hello','夏天好','🧊ic','🔒pr','abc\0def','x\0','%','_','']:
                    escaped=query.replace('!','!!').replace('%','!%').replace('_','!_')
                    expected=[str(r[0]) for r in db.execute("SELECT id FROM messages WHERE content LIKE ? ESCAPE '!' ORDER BY id DESC",('%'+escaped+'%',))] if query else [str(r[0]) for r in db.execute('SELECT id FROM messages ORDER BY id DESC')]
                    result=Archive(target,True).search(q=query,limit=100)
                    self.assertEqual([r['id'] for r in result['messages']],expected,repr(query))


if __name__=='__main__':unittest.main()
