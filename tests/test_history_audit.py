"""Synthetic audit fixtures are created in temporary repositories only."""
import os
import sqlite3
import subprocess
import tempfile
import unittest
from contextlib import closing
from pathlib import Path

from benchmarks.audit_history import audit


class HistoryAuditTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.previous = Path.cwd()
        os.chdir(self.temp.name)
        subprocess.run(['git', 'init', '-q'], check=True, capture_output=True)

    def tearDown(self):
        os.chdir(self.previous)
        self.temp.cleanup()

    def blob(self, data):
        return subprocess.check_output(['git', 'hash-object', '-w', '--stdin'], input=data).decode().strip()

    def test_unreachable_data_and_known_credentials_are_found_without_content(self):
        secret = 'synthetic-credential-' + 'x' * 24
        database = Path('private.sqlite')
        with closing(sqlite3.connect(database)) as db:
            db.execute('CREATE TABLE settings(key TEXT, value TEXT)')
            db.execute('INSERT INTO settings VALUES (?, ?)', ('token', secret))
            db.commit()
        known = self.blob(('a saved value: ' + secret).encode())
        exported = self.blob(b'{"messages":[{"role":"user","content":"synthetic fixture"}]}')
        lines = self.blob(b'{"author_id":"1","content":"synthetic one"}\n{"author_id":"2","content":"synthetic two"}')
        database_blob = self.blob(b'SQLite format 3\x00synthetic header')
        self.blob(b'Ordinary source code without private records')
        report = audit([database])
        self.assertEqual({row['object'] for row in report['findings']}, {known, exported, lines, database_blob})
        self.assertNotIn(secret, str(report))
        self.assertNotIn('synthetic fixture', str(report))

    def test_clean_repository_and_private_history_paths(self):
        self.blob(b'const count = 1;')
        self.assertEqual(audit()['findings'], [])
        Path('data').mkdir()
        Path('data/archive.txt').write_text('synthetic placeholder')
        subprocess.run(['git', 'add', 'data/archive.txt'], check=True, capture_output=True)
        subprocess.run(['git', '-c', 'user.name=Audit test', '-c', 'user.email=test@users.noreply.github.com',
                        'commit', '-qm', 'synthetic fixture'], check=True, capture_output=True)
        findings = audit()['findings']
        self.assertEqual(len(findings), 1)
        self.assertIn('private-data path', findings[0]['reasons'])
