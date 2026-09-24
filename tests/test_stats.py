import tempfile
import unittest
from contextlib import closing
from pathlib import Path
from unittest.mock import patch

import httpx
import sqlite3

import app
from storage import init_database


class StatsTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.path = str(Path(self.temp.name) / 'archive.db')
        init_database(self.path)
        self.patch = patch.object(app, 'DB_PATH', self.path)
        self.patch.start()
        self.client = httpx.AsyncClient(transport=httpx.ASGITransport(app=app.app), base_url='http://test')
        # Populate cached counts with deterministic tied ranks and sparse dates.
        with closing(sqlite3.connect(self.path)) as db:
            db.executemany('INSERT INTO authors(id,name) VALUES (?,?)', [(str(i), f'Author {i:03}') for i in range(123)])
            db.executemany('INSERT INTO stats_counts(kind,key,count) VALUES (?,?,?)', [('author', str(i), 10) for i in range(123)])
            db.executemany('INSERT INTO stats_counts(kind,key,count) VALUES (?,?,?)', [('total','',15),('day','2026-08-01',3),('day','2026-09-01',5),('day','2026-09-23',7)])
            db.commit()

    async def asyncTearDown(self):
        await self.client.aclose()
        self.patch.stop()
        self.temp.cleanup()

    async def test_contributor_pages_are_bounded_complete_and_stable(self):
        found = []
        for offset in (0,50,100):
            response = await self.client.get(f'/api/stats/contributors?offset={offset}&limit=50')
            self.assertEqual(response.status_code,200)
            data = response.json()
            self.assertLessEqual(len(data['users']),50)
            self.assertEqual(data['has_more'],offset < 100)
            found.extend(row['author_id'] for row in data['users'])
        self.assertEqual(len(found),123)
        self.assertEqual(len(set(found)),123)
        self.assertEqual(found,sorted(found))

    async def test_contributor_filter_escapes_wildcards_and_supports_ids(self):
        self.assertEqual((await self.client.get('/api/stats/contributors?q=%25')).json()['users'],[])
        by_name = (await self.client.get('/api/stats/contributors?q=Author%20022')).json()['users']
        self.assertEqual([row['author_id'] for row in by_name],['22'])
        self.assertTrue(any(row['author_id']=='121' for row in (await self.client.get('/api/stats/contributors?q=121')).json()['users']))

    async def test_stats_aggregate_calendar_and_window(self):
        data = (await self.client.get('/api/stats?days=7')).json()
        self.assertEqual(data['range_end'],'2026-09-23')
        self.assertEqual(data['range_start'],'2026-09-17')
        self.assertEqual(sum(row['count'] for row in data['messages_by_day']),7)
        self.assertEqual(sum(row['count'] for row in data['messages_by_weekday']),15)
        self.assertEqual(data['messages_by_month'],[{'month':'2026-08','count':3},{'month':'2026-09','count':12}])
        data = (await self.client.get('/api/stats?days=0')).json()
        self.assertEqual(data['range_start'],'2026-08-01')
        self.assertEqual(len(data['top_users']),50)

    async def test_invalid_bounds_rejected(self):
        for query in ('limit=101','limit=0','offset=-1','q='+'x'*101):
            self.assertEqual((await self.client.get('/api/stats/contributors?'+query)).status_code,422)
        self.assertEqual((await self.client.get('/api/stats?days=-1')).status_code,422)

    async def test_empty_archive(self):
        with closing(sqlite3.connect(self.path)) as db:
            db.execute('DELETE FROM stats_counts'); db.commit()
        data = (await self.client.get('/api/stats?days=0')).json()
        self.assertIsNone(data['range_start'])
        self.assertEqual(data['messages_by_month'],[])
        self.assertEqual(sum(row['count'] for row in data['messages_by_weekday']),0)
        self.assertEqual((await self.client.get('/api/stats/contributors')).json()['has_more'],False)
