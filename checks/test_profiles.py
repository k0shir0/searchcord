import asyncio
from contextlib import closing
import json
from pathlib import Path
import sqlite3
import tempfile
import time
import unittest
from unittest.mock import patch

import aiosqlite
from fastapi import FastAPI
from fastapi.testclient import TestClient
import httpx

from storage import init_database
from profile_api import profile_router, fetch_profile
from profile_store import read_profile, profile_servers, search_label


class ProfilesTest(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.path=Path(self.temp.name)/'archive.db';init_database(str(self.path))
        with closing(sqlite3.connect(self.path)) as db, db:
            db.executemany('INSERT INTO authors(id,name) VALUES (?,?)',[('50','Alice'),('60','Bob')])
            db.executemany('INSERT INTO guilds VALUES (?,?)',[('10','🌿 Garden'),('20','Workshop')])
            for i,guild in enumerate([10,10,20]):
                db.execute('INSERT INTO messages(id,author_id,guild_id,channel_id,content) VALUES (?,?,?,?,?)',
                           (1000000000000000000+i,50,guild,30,'hello fixture'))
        self.status=200;self.calls=[];self.presence=None;self.replies={};self.hold_uid=None
        self.pending_request=False;self.request_cancelled=False
        async def token():return 'synthetic-test-token'
        async def discord(method,path,token,**kwargs):
            self.calls.append(path);uid=path.split('/')[2]
            if uid==self.hold_uid:
                self.pending_request=True
                try:await asyncio.Event().wait()
                finally:self.request_cancelled=True
            replies=self.replies.get(uid, [])
            if replies:
                reply=replies.pop(0)
                if isinstance(reply, Exception):raise reply
                return reply
            payload={'user':{'id':uid,'username':'alice','global_name':'Alice','avatar':'abc123'},
                  'user_profile':{'bio':'<script>inert</script>','pronouns':'she/her','banner':'banner123'},
                  'connected_accounts':[{'type':'github','name':'alice','verified':True}], 'badges':[{'id':'example','description':'Example badge'}]}
            if self.presence is not None:payload['presence']=self.presence
            return httpx.Response(self.status,json=payload)
        router,self.shutdown=profile_router(str(self.path),token,discord,asyncio.Lock())
        app=FastAPI();app.include_router(router);self.client=TestClient(app);self.client.__enter__()

    def tearDown(self):
        self.client.portal.call(self.shutdown);self.client.__exit__(None,None,None);self.temp.cleanup()

    def test_basic_profile_and_servers(self):
        data=self.client.get('/api/profiles/50').json();self.assertFalse(data['extended']);self.assertEqual(data['message_count'],3)
        data=self.client.get('/api/profiles/50/servers').json();self.assertEqual([s['messages'] for s in data['servers']],[2,1])
        self.assertEqual(self.client.get('/api/profiles/50/servers?q=garden').json()['servers'][0]['id'],'10')
        self.assertEqual(search_label('📢┃Ｇｅｎｅｒａｌ-chat'),'general chat')
        self.assertEqual(self.client.get('/api/profiles/50/servers?limit=1').json()['has_more'],True)
        self.assertEqual(len(self.client.get('/api/profiles/50/servers?limit=1&offset=1').json()['servers']),1)
        self.assertEqual(self.client.get('/api/profiles/nope').status_code,400)
        self.assertEqual(self.client.get('/api/profiles/99').status_code,404)

    def test_fetch_preserves_extended_fields_and_refuses_errors(self):
        self.presence={'status':'online','activities':[{'type':4,'state':'Testing custom status'}]}
        r=self.client.post('/api/profiles/50/fetch');self.assertEqual(r.status_code,200,r.text)
        data=r.json();self.assertTrue(data['extended']);self.assertEqual(data['pronouns'],'she/her')
        self.assertEqual(data['online_status'],'online');self.assertEqual(data['custom_status'],'Testing custom status')
        self.assertIn('/avatars/50/abc123.png',data['avatar_url']);self.assertIn('/banners/50/banner123.png',data['banner_url'])
        self.assertEqual(data['connections'][0]['name'],'alice');self.assertEqual(data['bio'],'<script>inert</script>')
        self.status=403;self.assertEqual(self.client.post('/api/profiles/50/fetch').status_code,403)
        self.assertEqual(self.client.get('/api/profiles/50').json()['bio'],data['bio'])
        self.assertEqual(self.client.post('/api/profiles/99/fetch').status_code,404)

    def test_missing_presence_is_not_inferred(self):
        self.assertEqual(self.client.post('/api/profiles/50/fetch').status_code,200)
        data=self.client.get('/api/profiles/50').json()
        self.assertIsNone(data['online_status']);self.assertIsNone(data['custom_status'])

    def test_backfill_resumes_skipping_saved_profiles(self):
        self.assertEqual(self.client.post('/api/profiles/50/fetch').status_code,200)
        self.assertEqual(self.client.post('/api/profile-backfill').status_code,200)
        for _ in range(100):
            state=self.client.get('/api/profile-backfill').json()
            if not state['running']:break
            time.sleep(.02)
        self.assertFalse(state['running']);self.assertEqual(state['saved'],1)
        self.assertEqual(self.calls.count('/users/50/profile'),1)
        self.assertTrue(self.client.get('/api/profiles/60').json()['extended'])

    def test_backfill_stops_on_authorization_failure(self):
        self.status=401;self.client.post('/api/profile-backfill')
        for _ in range(100):
            state=self.client.get('/api/profile-backfill').json()
            if not state['running']:break
            time.sleep(.02)
        self.assertFalse(state['running']);self.assertEqual(state['saved'],0);self.assertEqual(state['failed'],1)

    def wait_for(self, predicate):
        for _ in range(200):
            state=self.client.get('/api/profile-backfill').json()
            if predicate(state):return state
            time.sleep(.01)
        self.fail(f'Backfill did not reach expected state: {state}')

    def test_backfill_retries_rate_limit_network_and_invalid_json_at_same_user(self):
        self.replies['50']=[httpx.Response(429,json={'retry_after':0}),
            httpx.ReadTimeout('synthetic timeout'),httpx.Response(503),
            httpx.Response(200,text='broken JSON')]
        with patch('profile_api.RETRY_BASE_SECONDS',.001),patch('profile_api.RETRY_MAX_SECONDS',.005):
            self.client.post('/api/profile-backfill')
            state=self.wait_for(lambda s:not s['running'])
        self.assertEqual(state['saved'],2);self.assertEqual(state['failed'],0)
        self.assertEqual(state['retries'],4)
        self.assertEqual(self.calls,['/users/50/profile']*5+['/users/60/profile'])

    def test_unavailable_profile_does_not_stop_other_users(self):
        for code in (403,404):
            self.replies['50']=[httpx.Response(code)]
            self.client.post('/api/profile-backfill')
            state=self.wait_for(lambda s:not s['running'])
            self.assertEqual(state['failed'],1)
            self.assertTrue(self.client.get('/api/profiles/60').json()['extended'])
        self.assertEqual(self.calls.count('/users/60/profile'),1)

    def test_stop_interrupts_retry_and_restart_skips_finished_users(self):
        self.replies['60']=[httpx.Response(429,json={'retry_after':60})]
        self.client.post('/api/profile-backfill')
        state=self.wait_for(lambda s:s['wait_seconds']>=60)
        self.assertEqual(state['saved'],1)
        started=time.monotonic()
        stopped=self.client.post('/api/profile-backfill/stop').json()
        self.assertFalse(stopped['running']);self.assertLess(time.monotonic()-started,1)
        self.assertTrue(self.client.get('/api/profiles/50').json()['extended'])
        self.client.post('/api/profile-backfill')
        self.assertEqual(self.wait_for(lambda s:not s['running'])['saved'],1)
        self.assertEqual(self.calls.count('/users/50/profile'),1)

    def test_backfill_rolls_back_and_retries_database_busy(self):
        import profile_api
        original=profile_api.save_profile
        attempts=0
        async def save(*args):
            nonlocal attempts
            attempts+=1
            if attempts==1:raise sqlite3.OperationalError('database is locked')
            return await original(*args)
        with patch('profile_api.save_profile',side_effect=save),patch('profile_api.RETRY_BASE_SECONDS',.001):
            self.client.post('/api/profile-backfill')
            state=self.wait_for(lambda s:not s['running'])
        self.assertEqual(state['saved'],2);self.assertEqual(state['failed'],0)
        self.assertEqual(self.calls,['/users/50/profile','/users/50/profile','/users/60/profile'])

    def test_stop_cancels_an_active_profile_request(self):
        self.hold_uid='50'
        self.client.post('/api/profile-backfill')
        self.wait_for(lambda s:self.pending_request)
        stopped=self.client.post('/api/profile-backfill/stop').json()
        self.assertFalse(stopped['running']);self.assertTrue(self.request_cancelled)
        self.assertFalse(self.client.get('/api/profiles/50').json()['extended'])


if __name__=='__main__':unittest.main()
