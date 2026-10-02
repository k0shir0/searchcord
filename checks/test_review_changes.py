"""Regressions for bounded collector search and standalone packaging."""
import hashlib
import importlib.util
import os
import sqlite3
import tempfile
import unittest
from contextlib import closing
from pathlib import Path
from unittest.mock import patch

from fastapi.testclient import TestClient

import app
import profile_store
from search_app import create_app
from storage import init_database


class ReviewTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.path = Path(self.temp.name) / 'archive.db'
        init_database(str(self.path))
        with closing(sqlite3.connect(self.path)) as db, db:
            db.execute("INSERT INTO authors(id,name) VALUES ('50','Example')")
            db.execute("INSERT INTO guilds VALUES ('10','🌿 Garden')")
            for i in range(120):
                db.execute('INSERT INTO messages(id,guild_id,author_id,content,image_urls) VALUES (?,?,?,?,?)',
                           (1000000000000000000+i,10 if i%2 else 20,50,
                            'literal %_ marker' if i==0 else 'hello',
                            'javascript:bad\nhttps://cdn.discordapp.com/attachments/image.png'))
        self.patch = patch.object(app,'DB_PATH',str(self.path)); self.patch.start()
        self.client = TestClient(app.app)

    def tearDown(self):
        self.client.close(); self.patch.stop(); self.temp.cleanup()

    def test_cursor_compatibility_validation_and_attachment_positions(self):
        legacy = self.client.get('/api/search',params={'q':'hello','limit':7}).json()
        first = self.client.get('/api/search',params={'q':'hello','limit':7,'cursor':True}).json()
        self.assertEqual([m['id'] for m in legacy['messages']],[m['id'] for m in first['messages']])
        self.assertEqual(legacy['total'],119); self.assertIsNone(first['total'])
        self.assertTrue(first['messages'][0]['attachments'][0].endswith('/1'))
        ids=[]; before=None
        while True:
            result = self.client.get('/api/search',params={'cursor':True,'limit':7,**({'before':before} if before else {})}).json()
            ids.extend(m['id'] for m in result['messages'])
            if not result['has_more']: break
            before=result['next_cursor']
        self.assertEqual(len(ids),120); self.assertEqual(len(set(ids)),120)
        literal=self.client.get('/api/search',params={'cursor':True,'q':'%_'}).json()
        self.assertEqual(len(literal['messages']),1)
        for params,status in [({'cursor':True,'before':-1},422),({'cursor':True,'q':'x'*201},422),({'cursor':True,'author_id':'name'},400)]:
            self.assertEqual(self.client.get('/api/search',params=params).status_code,status)

    def test_profile_aggregation_preserves_unknown_guilds_and_limits_work(self):
        with patch.object(profile_store,'search_label',wraps=profile_store.search_label) as normalize, closing(sqlite3.connect(self.path)) as db:
            result=profile_store.profile_servers(db,'50',limit=1)
            self.assertTrue(result['has_more'])
            self.assertEqual(result['servers'][0]['messages'],60)
            self.assertEqual(normalize.call_count,3)
            self.assertEqual(profile_store.profile_servers(db,'50','Garden')['servers'][0]['id'],'10')
            self.assertEqual(profile_store.profile_servers(db,'50',offset=1)['servers'][0]['name'],'Unknown server')

    def test_explicit_database_environment_is_readonly_and_has_no_admin_routes(self):
        before=hashlib.sha256(self.path.read_bytes()).digest()
        with patch.dict(os.environ,{'SEARCHCORD_DB':str(self.path)}), TestClient(create_app(immutable=True)) as client:
            self.assertEqual(client.get('/api/summary').json()['messages'],120)
            for route in ['/archive.db','/data/searchcord.db','/api/settings','/app.js']:
                self.assertEqual(client.get(route).status_code,404)
        self.assertEqual(before,hashlib.sha256(self.path.read_bytes()).digest())

    def test_package_is_allowlisted_and_never_overwrites(self):
        source=Path(__file__).resolve().parents[1]/'deploy/search/package.py'
        spec=importlib.util.spec_from_file_location('search_package',source)
        package=importlib.util.module_from_spec(spec); spec.loader.exec_module(package)
        target=package.build(Path(self.temp.name)/'bundle')
        self.assertTrue((target/'static/search/index.html').is_file())
        self.assertFalse((target/'app.py').exists())
        self.assertEqual(list((target/'data').iterdir()),[])
        self.assertEqual(len(list(target.rglob('*.db'))),0)
        with self.assertRaises(FileExistsError): package.build(target)
