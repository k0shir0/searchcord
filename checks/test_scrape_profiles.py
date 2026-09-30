"""Synthetic message ingestion must save profiles before the next Discord page."""
import asyncio
from contextlib import closing
from pathlib import Path
import sqlite3
import tempfile
import time
import unittest
from unittest.mock import patch

import httpx
from fastapi.testclient import TestClient
import app


CHANNEL = {'id':'20','name':'fixture','guild_id':'10','guild_name':'Fixture'}


class ScrapeProfilesTest(unittest.TestCase):
    def test_new_profiles_are_saved_before_next_page_and_deduplicated(self):
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/'searchcord.db'
            with patch.object(app,'DB_PATH',str(path)), TestClient(app.app) as client:
                with closing(sqlite3.connect(path)) as db, db:
                    db.execute("INSERT INTO settings VALUES ('token','synthetic-token')")
                calls=[]
                first_page=[{'id':str(1000000000000000200-i),
                    'author':{'id':'50','username':'fixture'},'content':'synthetic'} for i in range(100)]
                first_page[0]['webhook_id']='70'
                first_page[0]['author']['id']='70'
                def request(method,url,**kwargs):
                    calls.append(url.split('/v10')[-1])
                    if '/users/' in url:
                        self.assertEqual(len(calls),2)
                        return httpx.Response(200,json={'user':{'id':'50','username':'fixture'},
                                                       'user_profile':{'bio':'saved immediately'}})
                    if 'before' in kwargs['params']:
                        with closing(sqlite3.connect(path)) as db:
                            self.assertEqual(db.execute('SELECT user_id FROM profile_details').fetchall(),[('50',)])
                            self.assertEqual(db.execute('SELECT COUNT(*) FROM messages').fetchone()[0],100)
                        return httpx.Response(200,json=[])
                    return httpx.Response(200,json=first_page)
                with patch.object(app.http_client,'request',side_effect=request),patch.object(app,'JOB_RETENTION_SECONDS',0):
                    response=client.post('/api/scrape/start',json={'channels':[CHANNEL],'harvest_profiles':True})
                self.assertEqual(response.status_code,200)
                self.assertEqual(calls,['/channels/20/messages','/users/50/profile','/channels/20/messages'])

    def test_unchecked_profiles_make_no_profile_requests(self):
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/'searchcord.db'
            with patch.object(app,'DB_PATH',str(path)), TestClient(app.app) as client:
                with closing(sqlite3.connect(path)) as db, db:
                    db.execute("INSERT INTO settings VALUES ('token','synthetic-token')")
                page=[{'id':'1000000000000000100','author':{'id':'50','username':'fixture'},'content':'synthetic'}]
                with patch.object(app.http_client,'request',return_value=httpx.Response(200,json=page)) as request, \
                        patch.object(app,'JOB_RETENTION_SECONDS',0):
                    client.post('/api/scrape/start',json={'channels':[CHANNEL]})
                request.assert_called_once()
                with closing(sqlite3.connect(path)) as db:
                    self.assertEqual(db.execute('SELECT COUNT(*) FROM profile_details').fetchone()[0],0)

    def test_success_is_returned_before_discord_cooldown(self):
        async def scenario():
            response=httpx.Response(200,json={'user':{'id':'50'}},headers={
                'X-RateLimit-Remaining':'0','X-RateLimit-Reset-After':'60'})
            class Client:
                async def request(self,*args,**kwargs):return response
            with patch.object(app,'http_client',Client()),patch.object(app,'last_discord_request',0), \
                    patch.object(app,'discord_ready_at',0),patch.object(app,'discord_request_lock',asyncio.Lock()):
                started=time.monotonic()
                self.assertIs(await asyncio.wait_for(app.discord('GET','/users/50/profile','synthetic'),.5),response)
                self.assertLess(time.monotonic()-started,.5)
                self.assertGreater(app.discord_ready_at,time.monotonic()+59)
        asyncio.run(scenario())


if __name__=='__main__':unittest.main()
