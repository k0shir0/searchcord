"""Fault injection and restart tests assert the owner interface, never job internals."""
import asyncio
from contextlib import closing
import json
import sqlite3
from unittest.mock import AsyncMock, patch

import httpx
import app
import durable_scrape
from collection_state import load_jobs, persist_job
from tests.durable_fixture import ScrapeCase, CHANNEL, message


class DurableScrapeTest(ScrapeCase):
    async def test_checkpoint_failure_rolls_back_page_cursor_flags_and_projection(self):
        entered,release=asyncio.Event(),asyncio.Event()
        original=durable_scrape.save_job
        attempts=0
        async def save(db,job_id,kind,state):
            nonlocal attempts
            if (state.get('last_event') or {}).get('type')=='progress':
                attempts+=1
                if attempts==1:
                    entered.set()
                    await release.wait()
                    raise sqlite3.OperationalError('database busy')
            await original(db,job_id,kind,state)
        self.replies=[httpx.Response(200,json=[message(41),message(40)])]
        with patch.object(durable_scrape,'save_job',save):
            job_id=await self.start(limit=3)
            await asyncio.wait_for(entered.wait(),2)
            before=self.owner.snapshot(job_id)
            self.assertEqual(before['total_messages'],0)
            self.assertEqual(self.query('SELECT COUNT(*) FROM messages'),[(0,)])
            self.assertEqual(self.query('SELECT COUNT(*) FROM scrape_cursors'),[(0,)])
            persisted=(await load_jobs(self.path,'scrape'))[0][2]
            self.assertEqual(persisted['total_messages'],0)
            pause=asyncio.create_task(self.owner.pause(job_id))
            release.set()
            await pause
            await self.until(lambda:self.owner.snapshot(job_id)['paused'])
        after=self.owner.snapshot(job_id)
        self.assertEqual(after['total_messages'],2)
        self.assertEqual(after['rows'][0]['messages'],2)
        self.assertEqual(self.query('SELECT newest_message_id,oldest_message_id,history_complete FROM scrape_cursors'),
                         [(message(41)['id'],message(40)['id'],1)])
        self.assertEqual(attempts,2)
        self.assertEqual(len(self.requests),1)
        saved=(await load_jobs(self.path,'scrape'))[0][2]
        self.assertEqual(saved['rows'],after['rows'])

    async def test_stop_joins_inflight_commit_before_archive_clear(self):
        entered,release=asyncio.Event(),asyncio.Event()
        original=durable_scrape.save_job
        async def save(db,job_id,kind,state):
            if (state.get('last_event') or {}).get('type')=='progress':
                entered.set()
                await release.wait()
            await original(db,job_id,kind,state)
        self.replies=[httpx.Response(200,json=[message(41),message(40)])]
        with patch.object(durable_scrape,'save_job',save):
            job_id=await self.start()
            await asyncio.wait_for(entered.wait(),2)
            stop=asyncio.create_task(self.owner.stop(job_id))
            await asyncio.sleep(.02)
            self.assertFalse(stop.done())
            self.assertEqual((await self.client.delete('/api/messages?confirm=true')).status_code,409)
            self.assertEqual((await self.client.delete('/api/settings/tokens?confirm=true')).status_code,409)
            release.set()
            await asyncio.wait_for(stop,2)
        self.assertEqual(self.owner.snapshot(job_id)['total_messages'],2)
        self.assertEqual(self.query('SELECT COUNT(*) FROM messages'),[(2,)])
        self.assertFalse(self.owner.has_work)
        self.assertEqual((await self.client.delete('/api/messages?confirm=true')).status_code,200)
        await asyncio.sleep(.05)
        self.assertEqual(self.query("SELECT COUNT(*) FROM collection_jobs WHERE kind='scrape'"),[(0,)])

    async def test_shutdown_joins_commit_and_recovers_committed_rows_budget_and_queue(self):
        entered,release=asyncio.Event(),asyncio.Event()
        original=durable_scrape.save_job
        async def save(db,job_id,kind,state):
            if (state.get('last_event') or {}).get('type')=='progress':
                entered.set()
                await release.wait()
            await original(db,job_id,kind,state)
        self.replies=[httpx.Response(200,json=[message(i) for i in range(200,100,-1)])]
        with patch.object(durable_scrape,'save_job',save):
            job_id=await self.start(limit=101,channels=[CHANNEL,{**CHANNEL,'id':'21'}])
            await asyncio.wait_for(entered.wait(),2)
            shutdown=asyncio.create_task(self.owner.suspend())
            await asyncio.sleep(.02)
            self.assertFalse(shutdown.done())
            release.set()
            await asyncio.wait_for(shutdown,2)
        self.owner.reset()
        await self.owner.recover()
        snapshot=self.owner.snapshot(job_id)
        self.assertTrue(snapshot['paused'])
        self.assertEqual(snapshot['total_messages'],100)
        self.assertEqual(snapshot['rows'][0]['messages'],100)
        self.replies=[httpx.Response(200,json=[message(100)])]
        await self.owner.resume(job_id)
        snapshot=await self.finished(job_id)
        self.assertEqual(snapshot['total_messages'],101)
        self.assertEqual(snapshot['completed'],2)
        self.assertEqual(self.requests[1][1],{'limit':'1','before':message(101)['id']})
        self.assertEqual(self.requests[2][0],'/api/v10/channels/21/messages')

    async def test_restart_restores_pending_profiles_before_remaining_page_budget(self):
        self.hold=True
        self.replies=[httpx.Response(200,json=[message(i) for i in range(200,100,-1)])]
        job_id=await self.start(limit=101,profiles=True)
        await self.pending.wait()
        await self.owner.pause(job_id)
        self.release.set()
        await self.until(lambda:self.owner.snapshot(job_id)['paused'])
        await self.owner.suspend()
        self.owner.reset()
        await self.owner.recover()
        saved=(await load_jobs(self.path,'scrape'))[0][2]
        self.assertEqual(saved['channel_state']['pending_profiles'],['30'])
        self.assertEqual((await self.client.delete('/api/messages?confirm=true')).status_code,409)
        self.assertEqual((await self.client.post('/api/scrape/start',json={'channels':[CHANNEL]})).status_code,409)
        self.replies=[httpx.Response(200,json={'user':{'id':'30'}}),httpx.Response(200,json=[message(100)])]
        await self.owner.resume(job_id)
        await self.finished(job_id)
        self.assertEqual([path for path,_ in self.requests],['/api/v10/channels/20/messages',
            '/api/v10/users/30/profile','/api/v10/channels/20/messages'])
        self.assertEqual(self.requests[-1][1],{'limit':'1','before':message(101)['id']})

    async def test_old_checkpoint_representation_requires_original_account_and_keeps_budget(self):
        # Literal legacy state from the pre-refactor job format, not a new serializer.
        old={"running":True,"cancelled":False,"pause_requested":False,"paused":False,
             "channels":[CHANNEL],"limit":3,"harvest_profiles":False,"index":1,
             "total_messages":2,"completed":0,"rows":[{"type":"progress","channel":"fixture",
                 "messages":2,"total_messages":2}],"last_event":{"type":"progress","channel":"fixture",
                 "messages":2,"total_messages":2},"next_index":0,"failed_channels":0,
             "credential_id":"account","profile_auth_failed":None,"channel_state":{
                 "stage":"history","messages":2,"profiles":0,"pending_profiles":[],
                 "anchor":None,"before":message(40)['id'],"newest":None,"oldest":None,
                 "history_complete":False,"resuming_pending":False}}
        await self.seed([message(41),message(40)])
        await persist_job(self.path,'old-scrape','scrape',old)
        await self.owner.recover()
        self.assertTrue(self.owner.snapshot('old-scrape')['paused'])
        self.selected.return_value=('different','synthetic-other')
        self.assertEqual((await self.client.post('/api/scrape/old-scrape/resume')).status_code,409)
        self.assertEqual(len(self.requests),0)
        self.selected.return_value=('account','synthetic')
        self.replies=[httpx.Response(200,json=[message(39)])]
        await self.owner.resume('old-scrape')
        self.assertEqual((await self.finished('old-scrape'))['total_messages'],3)
        self.assertEqual(self.requests[0][1],{'limit':'1','before':message(40)['id']})

    async def test_account_race_between_resume_and_worker_remains_paused(self):
        self.hold=True
        job_id=await self.start()
        await self.pending.wait()
        await self.owner.suspend()
        self.owner.reset()
        await self.owner.recover()
        self.selected.side_effect=[('account','synthetic'),('different','synthetic-other')]
        await self.owner.resume(job_id)
        await self.until(lambda:self.owner.snapshot(job_id)['paused'])
        self.assertEqual(len(self.requests),1)
        saved=(await load_jobs(self.path,'scrape'))[0][2]
        self.assertTrue(saved['running'])
        self.assertTrue(saved['paused'])
        self.assertEqual(saved['credential_id'],'account')

    async def test_simultaneous_start_reserves_channels_before_checkpoint_write(self):
        self.hold=True
        responses=await asyncio.gather(*(self.client.post('/api/scrape/start',json={'channels':[CHANNEL]}) for _ in range(2)))
        self.assertEqual(sorted(response.status_code for response in responses),[200,409])

    async def test_shutdown_before_worker_first_turn_keeps_recoverable_checkpoint(self):
        job_id=await self.owner.start([CHANNEL])
        await self.owner.suspend()
        self.owner.reset()
        await self.owner.recover()
        self.assertTrue(self.owner.snapshot(job_id)['paused'])
        self.assertEqual(self.owner.snapshot(job_id)['total_messages'],0)
        self.assertEqual(len(self.requests),0)

    async def test_resume_cannot_launch_worker_after_shutdown_barrier(self):
        self.hold=True
        job_id=await self.start()
        await self.pending.wait()
        await self.owner.pause(job_id)
        entered,release=asyncio.Event(),asyncio.Event()
        async def credentials():
            entered.set()
            await release.wait()
            return 'account','synthetic'
        with patch.object(app,'_selected_credentials',credentials):
            resume=asyncio.create_task(self.client.post(f'/api/scrape/{job_id}/resume'))
            await entered.wait()
            shutdown=asyncio.create_task(self.owner.suspend())
            await asyncio.sleep(.01)
            release.set()
            self.assertEqual((await resume).status_code,409)
            await shutdown
        self.assertTrue(self.owner.snapshot(job_id)['paused'])
        self.assertEqual(len(self.requests),1)
