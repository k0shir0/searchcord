// Browser-only Discord substitutes. Every write and event stream is intercepted.
// Imported by browser_home.mjs after the real archive read checks.
export async function checkScrape({evaluate, check, click, until, key, shot}) {
  await evaluate(`(() => {
    const originalFetch = window.fetch.bind(window);
    window.mockCalls = []; window.mockStreams = []; window.mockMonitors = [];
    window.fetch = async (url, options = {}) => {
      const endpoint = String(url), method = options.method || 'GET';
      const body = options.body ? JSON.parse(options.body) : {};
      const response = (data, status=200) => new Response(JSON.stringify(data), {status, headers:{'Content-Type':'application/json'}});
      if (method !== 'GET') window.mockCalls.push({endpoint,method,body});
      if (endpoint === '/api/token/validate') return response({valid:true,username:'Test account'});
      if (endpoint === '/api/guilds') return response([{id:'100',name:'Design studio',icon:null}]);
      if (endpoint === '/api/guilds/100/channels') return response([{id:'101',name:'general',type:0},{id:'102',name:'projects',type:0}]);
      if (endpoint === '/api/dms') return response([{id:'201',name:'Test conversation',type:1},{id:'202',name:'Design group',type:3}]);
      if (endpoint === '/api/live/status') return response({channels:window.mockMonitors});
      if (endpoint === '/api/live/start') {
        window.mockMonitors = body.channels.map(ch => ({channel_id:ch.id,channel_name:ch.name,guild_name:ch.guild_name}));
        return response({ok:true});
      }
      if (endpoint === '/api/live/stop') {
        window.mockMonitors = body.channel_ids.length ? window.mockMonitors.filter(ch=>!body.channel_ids.includes(ch.channel_id)) : [];
        return response({ok:true});
      }
      if (endpoint === '/api/scrape/start') return response({job_id:'test-job'});
      if (endpoint === '/api/scrape/test-job/stop') return response({ok:true});
      if (endpoint === '/api/dm-clear/start') return response({job_id:'test-delete'});
      if (endpoint === '/api/dm-clear/test-delete/stop') return response({ok:true});
      if (endpoint.startsWith('/api/export/participants?')) return response([{author_id:'301',author_name:'Test sender'}]);
      if (endpoint === '/api/export/chatml') return response({test:true});
      if (method !== 'GET') throw new Error('Unexpected write blocked by test harness');
      if (!endpoint.startsWith('/api/search') && !endpoint.startsWith('/api/stats')) throw new Error('Unexpected read blocked by test harness');
      return originalFetch(url, options);
    };
    window.EventSource = class {
      constructor(url) { this.url=url; window.mockStreams.push(this); }
      close() { this.closed=true; }
      emit(data) { if(!this.closed) this.onmessage?.({data:JSON.stringify(data)}); }
    };
    // Downloads contain only a synthetic response and remain inside the browser.
    const originalClick = HTMLAnchorElement.prototype.click;
    HTMLAnchorElement.prototype.click = function() { if(this.download) window.mockDownload=true; else originalClick.call(this); };
  })()`);
  await evaluate(`switchView('scrape')`);
  await click('#sourceChannels');
  await click('#connectDiscord');
  await until(`document.querySelectorAll('.server-item').length===1`);
  await click('.server-item');
  await until(`document.querySelectorAll('#cpBody .ch-item').length===2`);
  await click('#cpBody .ch-btn');
  await check('channel queue button selects a source', `S.queue.length===1 && document.querySelector('#queueCount').textContent==='1' && !document.querySelector('#startScrape').disabled`);
  await click('#sourceDms');
  await until(`document.querySelectorAll('#dmBody .ch-item').length===2`);
  await evaluate(`document.querySelector('#collectionSearch').value='conversation'; document.querySelector('#collectionSearch').dispatchEvent(new Event('input'));`);
  await check('conversation name filter hides other sources', `[...document.querySelectorAll('#dmBody .ch-item')].filter(row=>!row.hidden).length===1`);
  await click('#dmBody .ch-btn');
  await check('channels and DMs share the collection queue', `S.queue.length===2 && S.queue[1].guild_id===null`);
  await evaluate(`document.querySelector('#scrapeLimit').value='100';document.querySelector('#harvestProfiles').checked=true;`);
  await evaluate(`document.querySelector('#queueBar').scrollIntoView({behavior:'instant',block:'center'});`);
  await shot('scrape-queued');
  await click('#startScrape');
  await until(`window.mockStreams.some(s=>s.url==='/api/scrape/progress/test-job')`);
  await check('collection options reach start endpoint', `(()=>{const call=window.mockCalls.find(c=>c.endpoint==='/api/scrape/start');return call.body.channels.length===2 && call.body.limit===100 && call.body.harvest_profiles===true && document.querySelector('#modalBg').classList.contains('visible');})()`);
  await evaluate(`window.mockStreams.find(s=>s.url==='/api/scrape/progress/test-job').emit({type:'progress',channel:'general',messages:10,total_messages:10});`);
  await check('progress stream updates collection UI', `document.querySelector('#moStats').textContent.includes('10 msgs')`);
  await click('#stopScrapeBtn');
  await check('stop requests the active job', `window.mockCalls.some(c=>c.endpoint==='/api/scrape/test-job/stop')`);
  await evaluate(`window.mockStreams.find(s=>s.url==='/api/scrape/progress/test-job').emit({type:'cancelled',total_messages:10});`);
  await check('stopped scrape clears queue and exits running state', `!S.scraping && S.queue.length===0 && document.querySelector('#startScrape').disabled && document.querySelector('#moChannel').textContent==='Stopped.'`);
  await check('finished run clears visible queue selections', `document.querySelectorAll('.ch-item.queued').length===0`);
  await click('#dismissModal');
  await click('#dmBody [aria-label="Export to ChatML JSONL"]');
  await until(`document.querySelector('.persona-item')`);
  await click('.persona-item');
  await check('export participant is keyboard-operable', `document.querySelector('.persona-item').tagName==='BUTTON' && !document.querySelector('#runExportBtn').disabled`);
  await click('#runExportBtn');
  await until(`!document.querySelector('#exportModalBg').classList.contains('visible')`);
  await check('export submits selected conversation and participant', `window.mockCalls.some(c=>c.endpoint==='/api/export/chatml' && c.body.channel_id==='201' && c.body.target_id==='301') && window.mockDownload`);
  await click('#dmBody .ch-clear-btn');
  await check('deletion requires confirmation without sending request', `!!document.querySelector('.pending-confirm') && !window.mockCalls.some(c=>c.endpoint.includes('dm-clear'))`);
  await click('#dmBody .ch-clear-btn');
  await until(`window.mockStreams.some(s=>s.url==='/api/dm-clear/progress/test-delete')`);
  await check('confirmed deletion owns the shared progress dialog', `S.activeJobKind==='dm-clear' && document.querySelector('#startScrape').disabled`);
  await click('#stopScrapeBtn');
  await check('deletion stop uses its own endpoint', `window.mockCalls.some(c=>c.endpoint==='/api/dm-clear/test-delete/stop')`);
  await evaluate(`window.mockStreams.find(s=>s.url==='/api/dm-clear/progress/test-delete').emit({type:'cancelled',deleted:0});`);
  await click('#dismissModal');
  await check('stopped deletion releases shared progress state', `S.activeJobKind===null && S.activeJobId===null`);
  await evaluate(`document.querySelector('#collectionSearch').value='';document.querySelector('#collectionSearch').dispatchEvent(new Event('input'));`);
  await click('#sourceChannels');
  await click('#cpBody .ch-live-btn');
  await until(`document.querySelector('.live-monitor-item')`);
  await check('monitor starts within the unified workspace', `activeView==='scrape' && S.liveChannels.has('101') && document.querySelector('#liveBadge').textContent==='1'`);
  await evaluate(`window.mockStreams.find(s=>s.url==='/api/live/events' && !s.closed).emit({type:'message',author:'Test sender',guild:'Design studio',channel:'general',timestamp:'2026-09-24T00:00:00Z',content:'Synthetic live preview'});`);
  await check('live stream renders in the scrape feed', `document.querySelectorAll('.live-msg').length===1`);
  await click('#clearFeed');
  await check('clear feed leaves monitor running', `document.querySelectorAll('.live-msg').length===0 && S.liveChannels.size===1`);
  await click('#stopAllLive');
  await until(`S.liveChannels.size===0`);
  await check('stop all closes the event stream', `!S.liveES && window.mockMonitors.length===0`);
  await click('#sourceChannels');
  await key('ArrowRight');
  await check('source tabs support keyboard navigation', `document.activeElement.id==='sourceDms' && document.querySelector('#sourceDms').getAttribute('aria-selected')==='true'`);
  await shot('scrape-tested');
}
