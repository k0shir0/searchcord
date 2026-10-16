"""Shared pacing retains errors, empty responses, concurrency and restart budgets."""
import asyncio
import time
from types import SimpleNamespace
from unittest.mock import patch
import httpx
import app
from collection_state import load_jobs
from tests.durable_fixture import ScrapeCase, CHANNEL

INTERVAL_BOUNDS = (app.SCRAPE_INTERVAL_MIN_SECONDS, app.SCRAPE_INTERVAL_MAX_SECONDS)


class ScrapePacingTest(ScrapeCase):
    async def test_failed_network_request_persists_the_last_attempt_deadline(self):
        ids=await self.blocked_jobs()
        self.replies=[httpx.ReadTimeout('fixture')]
        with self.assertRaises(httpx.ReadTimeout):
            await app.discord('GET','/channels/20/messages','synthetic',scrape_job_id=ids[0])
        saved=(await load_jobs(self.path,'pacing'))[0][2]
        self.assertGreater(saved['last_scrape_request'],time.time()-1)
        self.assertEqual(saved['scrape_confirmed_requests'],0)

    async def blocked_jobs(self):
        async def fetch(*args):
            await asyncio.Event().wait()
        self.patches.enter_context(patch.object(app,'_fetch_messages',fetch))
        ids=[await self.owner.start([CHANNEL]),await self.owner.start([{**CHANNEL,'id':'21'}])]
        await self.until(lambda:all(self.owner.snapshot(job_id)['index']==1 for job_id in ids))
        return ids

    async def test_random_intervals_retries_and_100_success_break_are_shared(self):
        ids=await self.blocked_jobs()
        now=[time.monotonic()+1]
        sent=[]
        waits=[]
        replies=[httpx.Response(429,json={'retry_after':0}),httpx.Response(503)]
        class Client:
            async def request(self,*args,**kwargs):
                sent.append(now[0])
                return replies.pop(0) if replies else httpx.Response(200,json=[])
        async def sleep(seconds):
            waits.append(seconds)
            now[0]+=seconds
        self.owner._clock=lambda:now[0]
        self.owner._sleep=sleep
        async with self.owner.subscribe(ids[0]) as (_,qa), self.owner.subscribe(ids[1]) as (_,qb):
            with patch.object(app,'http_client',Client()), \
                    patch.object(app,'time',SimpleNamespace(monotonic=lambda:now[0],time=time.time)), \
                    patch.object(app,'SCRAPE_INTERVAL_MIN_SECONDS',INTERVAL_BOUNDS[0]), \
                    patch.object(app,'SCRAPE_INTERVAL_MAX_SECONDS',INTERVAL_BOUNDS[1]), \
                    patch.object(app.random,'uniform',side_effect=lambda low,high:low if high>2 or len(sent)%2 else high) as draw:
                for index in range(102):
                    await app.discord('GET','/channels/20/messages','synthetic',scrape_job_id=ids[index%2])
            self.assertEqual(draw.call_count,104)
            interval_draws=[call for call in draw.call_args_list if call.args==(.5,1.5)]
            self.assertEqual(len(interval_draws),103)
            self.assertEqual(draw.call_args_list[-2].args,(60,90))
            intervals=[end-start for start,end in zip(sent[:-2],sent[1:-1])]
            self.assertTrue(all(.499<=seconds<=1.501 for seconds in intervals))
            self.assertTrue(any(seconds<.501 for seconds in intervals))
            self.assertTrue(any(seconds>1.499 for seconds in intervals))
            self.assertAlmostEqual(sent[-1]-sent[-2],60)
            self.assertEqual(app.scrape_confirmed_requests,1)
            events=[]
            for queue in (qa,qb):
                while not queue.empty(): events.append(queue.get_nowait())
            self.assertTrue(any(event['type']=='scrape_break' for event in events))
        saved=(await load_jobs(self.path,'pacing'))[0][2]
        self.assertEqual(saved['scrape_confirmed_requests'],1)

    async def test_actual_random_interval_bounds_and_slow_response_are_shared(self):
        ids=await self.blocked_jobs()
        now=[time.monotonic()+1]
        sent,finished=[],[]
        class Client:
            async def request(self,*args,**kwargs):
                sent.append(now[0])
                now[0]+=.6
                finished.append(now[0])
                return httpx.Response(200,json=[])
        async def sleep(seconds): now[0]+=seconds
        self.owner._clock=lambda:now[0]
        self.owner._sleep=sleep
        with patch.object(app,'http_client',Client()), \
                patch.object(app,'time',SimpleNamespace(monotonic=lambda:now[0],time=time.time)), \
                patch.object(app,'SCRAPE_INTERVAL_MIN_SECONDS',INTERVAL_BOUNDS[0]), \
                patch.object(app,'SCRAPE_INTERVAL_MAX_SECONDS',INTERVAL_BOUNDS[1]), \
                patch.object(app.random,'uniform',return_value=.75) as draw:
            await asyncio.gather(*(app.discord('GET','/channels/20/messages','synthetic',scrape_job_id=ids[index%2])
                                   for index in range(4)))
        self.assertTrue(all(call.args==(.5,1.5) for call in draw.call_args_list))
        self.assertTrue(all(.749 <= start-end <= .751 for end,start in zip(finished,sent[1:])))
        self.assertEqual(app.scrape_confirmed_requests,4)

    async def test_counter_cooldowns_and_break_deadline_survive_restart_and_job_changes(self):
        ids=await self.blocked_jobs()
        app.scrape_confirmed_requests=99
        self.replies=[httpx.Response(200,json=[],headers={
            'X-RateLimit-Remaining':'0','X-RateLimit-Reset-After':'30'})]
        with patch.object(app.random,'uniform',side_effect=lambda low,high:high) as draw:
            await app.discord('GET','/channels/20/messages','synthetic',scrape_job_id=ids[0])
        self.assertEqual(draw.call_args_list[-1].args,(60,90))
        saved=(await load_jobs(self.path,'pacing'))[0][2]
        self.assertGreater(saved['scrape_break_until'],time.time()+88)
        self.assertEqual(app.scrape_confirmed_requests,0)
        await self.owner.suspend()
        self.owner.reset()
        await app._recover_collection_jobs()
        self.assertGreater(app.scrape_break_until,app.time.monotonic()+88)
        self.assertGreater(app.discord_ready_at,app.time.monotonic()+28)
        self.assertEqual(app.scrape_confirmed_requests,0)
        for job_id in ids: await self.owner.stop(job_id)
        job_id=await self.owner.start([{**CHANNEL,'id':'22'}])
        self.assertGreater(self.owner.snapshot(job_id)['break_seconds'],88)

    async def test_nonzero_counter_survives_restart(self):
        ids=await self.blocked_jobs()
        self.replies=[httpx.Response(200,json=[]),httpx.Response(503)]
        for job_id in ids:
            await app.discord('GET','/channels/20/messages','synthetic',scrape_job_id=job_id)
        await self.owner.suspend()
        self.owner.reset()
        await app._recover_collection_jobs()
        self.assertEqual(app.scrape_confirmed_requests,1)
