"""Synthetic correctness checks. Never opens the personal archive."""
from datetime import datetime, timezone
from contextlib import closing
from pathlib import Path
import sqlite3
import tempfile
import unittest
from unittest.mock import patch

from fastapi.testclient import TestClient
from search_app import Archive, create_app, EPOCH_MS
from storage import init_database


class DisplayTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.directory = tempfile.TemporaryDirectory()
        cls.path = Path(cls.directory.name) / "searchcord.db"
        init_database(str(cls.path))
        cls.base = (int(datetime(2026, 9, 1, tzinfo=timezone.utc).timestamp()*1000)-EPOCH_MS) << 22
        with closing(sqlite3.connect(cls.path)) as db, db:
            db.executemany("INSERT INTO guilds VALUES (?,?)", [('10','Server Alpha'),('20','Server Beta')])
            db.executemany("INSERT INTO channels VALUES (?,?,?)", [('30','10','general'),('40','20','general')])
            db.executemany("INSERT INTO authors(id,name) VALUES (?,?)", [('50','Alex'),('60','Alina')])
            db.executemany("INSERT INTO profiles(user_id,avatar_hash) VALUES (?,?)",
                           [('50','a_abc123'),('60','../invalid')])
            # Backfilled older messages have newer internal rowids. Public IDs
            # must determine order and cursor boundaries on both search paths.
            for i in list(range(40,80)) + list(range(40)):
                text = 'hello common' if i % 3 else 'different text'
                if i == 2: text = 'rare phrase <script>alert(1)</script>'
                if i == 4: text = 'literal 100%_value and wow!'
                if i == 6: text = 'literal 100xxvalue'
                if i == 8: text = 'MiXeD Héllo unicode'
                if i == 10: text = 'x' * 4000
                db.execute('INSERT INTO messages(id,channel_id,guild_id,author_id,content,image_urls) VALUES (?,?,?,?,?,?)',
                           (cls.base+i*(86400000 << 22),30 if i%2 else 40,10 if i%2 else 20,50 if i%2 else 60,text,
                            'https://cdn.discordapp.com/attachments/test.png\njavascript:alert(1)' if i == 2 else ''))
            db.execute("INSERT INTO name_values(id,value) VALUES (1,'Historical name')")
            rowid = db.execute('SELECT rowid FROM messages WHERE id=?',[cls.base+2*(86400000 << 22)]).fetchone()[0]
            db.execute('INSERT INTO message_history(rowid,author_name_id,timestamp_override,timestamp) VALUES (?,1,1,?)',(rowid,'2026-09-03T00:00:00+00:00'))
        cls.client = TestClient(create_app(cls.path, True))

    @classmethod
    def tearDownClass(cls):
        cls.client.close()
        cls.directory.cleanup()

    def get(self, **params):
        response = self.client.get('/api/search', params=params)
        self.assertEqual(response.status_code,200,response.text)
        return response.json()

    def test_summary_and_readonly_surface(self):
        self.assertEqual(self.client.get('/privacy.html').status_code,200)
        self.assertEqual(self.client.get('/bauhaus.css').status_code,200)
        self.assertEqual(self.client.get('/api/summary').json(),{'messages':80,'servers':2})
        for path in ['/api/settings','/api/token','/api/scrape','/api/export','/app.js','/../app.py']:
            self.assertEqual(self.client.get(path).status_code,404,path)
        self.assertEqual(self.client.post('/api/search').status_code,405)
        self.assertEqual(self.client.get('/api/search').headers['cache-control'],'no-store')
        with Archive(self.path,True).connect() as db:
            with self.assertRaises(sqlite3.OperationalError): db.execute('DELETE FROM messages')

    def test_cursor_pages_cover_archive_without_duplicates(self):
        ids=[]; cursor=None
        while True:
            result=self.get(**({'before':cursor} if cursor else {}),limit=7)
            ids.extend(row['id'] for row in result['messages'])
            if not result['has_more']: break
            cursor=result['next_cursor']
        self.assertEqual(len(ids),80)
        self.assertEqual(len(set(ids)),80)
        self.assertEqual(ids,sorted(ids,key=int,reverse=True))

    def test_adaptive_query_matches_reference_all_pages(self):
        with patch('search_app.WINDOW',8), closing(sqlite3.connect(self.path)) as db:
            for query in ['hello','rare phrase','100%_value','wow!','MiXeD','Héllo','not present','he','h','%','%%%']:
                expected=[str(r[0]) for r in db.execute("SELECT id FROM messages WHERE content LIKE ? ESCAPE '!' ORDER BY id DESC",['%'+query.replace('!','!!').replace('%','!%').replace('_','!_')+'%'])]
                actual=[]; cursor=None
                while True:
                    result=self.get(q=query,limit=5,**({'before':cursor} if cursor else {}))
                    actual.extend(row['id'] for row in result['messages'])
                    if not result['has_more']: break
                    cursor=result['next_cursor']
                self.assertEqual(actual,expected,query)

    def test_filters_and_inclusive_dates(self):
        result=self.get(guild_id='20',channel_id='40',author_id='60',date_from='2026-09-03',date_to='2026-09-03')
        self.assertEqual(len(result['messages']),1)
        self.assertEqual(result['messages'][0]['author_name'],'Historical name')
        self.assertEqual(result['messages'][0]['timestamp'],'2026-09-03T00:00:00+00:00')
        self.assertEqual(len(result['messages'][0]['attachments']),1)
        self.assertIsNone(result['messages'][0]['avatar_url'])
        self.assertEqual(self.get(author_id='50')['messages'][0]['avatar_url'],
                         'https://cdn.discordapp.com/avatars/50/a_abc123.png?size=64')
        self.assertEqual(self.get(guild_id='10',channel_id='40')['messages'],[])

    def test_validation(self):
        for params in [dict(guild_id='name'),dict(author_id='9'*30),dict(date_from='2026-99-01'),dict(date_from='2026-09-04',date_to='2026-09-01')]:
            self.assertEqual(self.client.get('/api/search',params=params).status_code,400,params)
        for params in [dict(limit=101),dict(before=-1),dict(q='a'*201)]:
            self.assertEqual(self.client.get('/api/search',params=params).status_code,422)

    def test_suggestions_scoping_and_minimum(self):
        self.assertEqual(self.client.get('/api/suggestions/author?q=A').json(),[])
        self.assertEqual(len(self.client.get('/api/suggestions/author?q=Al').json()),2)
        self.assertEqual(len(self.client.get('/api/suggestions/channel?guild_id=10').json()),1)
        self.assertEqual(self.client.get('/api/suggestions/author?q=Al%25').json(),[])
        self.assertEqual(self.client.get('/api/suggestions/settings').status_code,422)

    def test_missing_old_and_wal_archives(self):
        missing = TestClient(create_app(self.path.parent/'missing.db',True))
        self.assertEqual(missing.get('/api/summary').status_code,503)
        self.assertFalse((self.path.parent/'missing.db').exists())
        old = self.path.parent/'old.db'
        sqlite3.connect(old).close()
        self.assertEqual(TestClient(create_app(old,True)).get('/api/summary').status_code,503)
        wal = Path(str(old)+'-wal'); wal.write_bytes(b'pending')
        self.assertIn('WAL',TestClient(create_app(old,True)).get('/api/summary').json()['detail'])


if __name__ == '__main__':
    unittest.main()
