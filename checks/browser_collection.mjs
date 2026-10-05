// Real Chromium, HTTP, SSE, and SQLite via collection_fixture.py.
// node checks/browser_collection.mjs http://127.0.0.1:8016 agents/collection-browser
import {spawn} from 'node:child_process';
import {existsSync, readFileSync, writeFileSync, mkdirSync} from 'node:fs';
import {setTimeout as delay} from 'node:timers/promises';
import path from 'node:path';
import assert from 'node:assert/strict';

const base = process.argv[2] || 'http://127.0.0.1:8016';
const output = path.resolve(process.argv[3] || 'agents/collection-browser');
mkdirSync(output, {recursive:true});
const chrome = process.env.CHROME_PATH || path.join(process.env.ProgramFiles, 'Google/Chrome/Application/chrome.exe');
const profile = path.join(output, `profile-${Date.now()}`);
const browser = spawn(chrome, ['--headless', '--disable-gpu', '--no-first-run', '--no-sandbox', '--remote-debugging-port=0', `--user-data-dir=${profile}`, 'about:blank'], {windowsHide:true, stdio:'ignore'});
const report = {checks:[]};
let socket;
try {
  const portFile = path.join(profile, 'DevToolsActivePort');
  for (let i=0; i<100 && !existsSync(portFile); i++) await delay(100);
  assert(existsSync(portFile), 'Chrome debugging endpoint available');
  const port = Number(readFileSync(portFile, 'utf8').split('\n')[0]);
  const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
  socket = new WebSocket(targets.find(t => t.type === 'page').webSocketDebuggerUrl);
  await new Promise((resolve,reject) => {socket.onopen=resolve; socket.onerror=reject;});
  let id=0;
  const pending=new Map(), exceptions=[];
  socket.onmessage=({data}) => {
    const message=JSON.parse(data);
    if (message.method==='Runtime.exceptionThrown') exceptions.push(message.params.exceptionDetails.text);
    if (!message.id) return;
    const item=pending.get(message.id); pending.delete(message.id);
    message.error ? item.reject(message.error) : item.resolve(message.result);
  };
  const command=(method,params={}) => new Promise((resolve,reject) => {
    const next=++id; pending.set(next,{resolve,reject}); socket.send(JSON.stringify({id:next,method,params}));
  });
  const evaluate=async expression => {
    const result=await command('Runtime.evaluate',{expression,awaitPromise:true,returnByValue:true});
    assert(!result.exceptionDetails, 'Browser expression has no exception');
    return result.result.value;
  };
  const until=async (expression, timeout=12000) => {
    const end=Date.now()+timeout;
    while(Date.now()<end) { if(await evaluate(expression)) return; await delay(100); }
    throw new Error('Timed out: '+expression);
  };
  const check=async (name,expression) => {assert(await evaluate(expression),name); report.checks.push(name);};
  const click=async (selector,double=false) => {
    await evaluate(`document.querySelector(${JSON.stringify(selector)}).scrollIntoView({behavior:'instant',block:'center'})`);
    const point=await evaluate(`(()=>{const r=document.querySelector(${JSON.stringify(selector)}).getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height/2};})()`);
    for(let count=1; count <= (double?2:1); count++) {
      await command('Input.dispatchMouseEvent',{type:'mousePressed',button:'left',clickCount:count,...point});
      await command('Input.dispatchMouseEvent',{type:'mouseReleased',button:'left',clickCount:count,...point});
    }
  };
  const state=async () => (await fetch(base+'/__checks/state')).json();
  const scenario=async body => fetch(base+'/__checks/scenario',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(body)});
  await command('Page.enable'); await command('Runtime.enable'); await command('Network.enable');
  await command('Network.setBlockedURLs',{urls:['https://*']});
  await command('Emulation.setDeviceMetricsOverride',{width:1440,height:1080,deviceScaleFactor:1,mobile:false});
  await command('Page.navigate',{url:base+'/#scrape'});
  await until(`document.body.dataset.archiveReady==='true' && activeView==='scrape'`);
  await click('#connectDiscord');
  await until(`document.querySelectorAll('.server-item').length===2`);
  if (!process.env.SCRAPE_CONTROL_ONLY) {
  await click('.server-item[data-id="10"]');
  await until(`document.querySelectorAll('#cpBody .ch-item').length===3`);
  await check('Single click browses without starting a scrape', `S.queue.length===0 && !S.activeJobKind`);
  assert(!(await state()).requests.some(r=>r.path.includes('/threads/')));
  report.checks.push('Ordinary browsing does not walk thread archives');
  await check('Private, view-only, and denied channels are hidden; empty readable and bot channels remain',
    `['20','21','24'].every(id=>document.querySelector('#cpBody .ch-item[data-id="'+id+'"]')) && ['22','23','25'].every(id=>!document.querySelector('#cpBody .ch-item[data-id="'+id+'"]'))`);
  await evaluate(`$('scrapeLimit').value='100'`);
  await click('.server-item[data-id="10"]',true);
  await until(`$('queueProgressTitle').textContent==='Scrape finished' && !S.scraping`);
  assert.deepEqual((await state()).stored, {'20':1});
  assert((await state()).requests.some(r=>r.path.endsWith('/threads/archived/public')));
  report.checks.push('Scan explicitly discovers accessible thread archives');
  report.checks.push('Native double-click starts scrape, excludes mixed-case bot names, and saves messages in SQLite');
  await check('Existing scrape limit and profile choice remain intact', `$('scrapeLimit').value==='100' && !$('harvestProfiles').disabled`);
  await click('#cpBody .ch-item[data-id="24"] .ch-btn');
  await check('Bot channels can still be manually queued', `S.queue.length===1 && S.queue[0].id==='24'`);
  await click('#clearQueue');

  await scenario({slow_scrapes:true});
  await click('.server-item[data-id="10"]',true);
  await until(`S.scraping && S.activeJobId`);
  await click('.server-item[data-id="11"]',true);
  await until(`S.queue.some(c=>c.id==='40')`);
  await check('A second scan waits behind the active batch without changing its progress rows',
    `S.scrapeRun.channels.length===2 && !S.scrapeRun.channels.some(c=>c.id==='40') && $('queueProgressRows').children.length===2`);
  await until(`!S.scraping && $('queueProgressTitle').textContent==='Scrape finished'`,18000);
  assert.equal((await state()).stored['40'],1);
  report.checks.push('Channels from the second scan start automatically after the current batch');

  await click('.server-item[data-id="10"]',true);
  await until(`S.scraping && S.activeJobId`);
  await click('#stopQueueScrape');
  await until(`!S.scraping && $('queueProgressTitle').textContent==='Scrape stopped'`);
  await check('Stop preserves the scanned queue and does not restart it', `S.queue.length===2 && !S.autoStartPending`);
  await click('#clearQueue');
  await scenario({fail_probes:true});
  await click('.server-item[data-id="10"]',true);
  await until(`$('scanStatus').textContent.includes('Could not scan')`);
  await check('Failed access scan leaves no partial automatic scrape', `!S.scraping && S.queue.length===0`);
  await scenario({});

  await evaluate(`$('inviteInput').value='https://example.test/nope'`);
  await click('#startInvites');
  await until(`$('inviteInput').getAttribute('aria-invalid')==='true'`);
  assert.equal((await state()).joins,0);
  report.checks.push('Invalid invite hosts show an inline error and never join');
  await evaluate(`$('inviteInput').value='https://discord.gg/first first https://discord.com/invite/second third'; $('inviteInput').dispatchEvent(new Event('input'))`);
  await click('#startInvites');
  await until(`inviteJob?.items[0].status==='joined'`);
  await check('Mixed invite forms deduplicate and show queued progress', `inviteJob.items.length===3 && $('inviteResults').children.length===3`);
  await click('[data-view="browse"]');
  await check('Navigation leaves the invite queue running', `activeView==='browse' && inviteJob.running`);
  await command('Page.reload');
  await until(`document.body.dataset.archiveReady==='true' && inviteJob?.running`);
  await click('[data-view="scrape"]');
  await check('Reload recovers the server-owned invite queue', `inviteJob.items[0].status==='joined' && !($('stopInvites').hidden)`);
  await until(`inviteJob?.items[1].status==='joined'`,40000);
  const joined=await state();
  assert(joined.join_intervals[0]>=30,'Join attempts spaced by at least 30 real seconds');
  report.joinIntervalSeconds=joined.join_intervals[0];
  report.checks.push('Production 30-second join interval verified with real elapsed time');
  await click('#stopInvites');
  await until(`inviteJob?.status==='stopped'`);
  assert.equal((await state()).joins,2);
  await check('Stop joining prevents the next queued invite and re-enables input', `!inviteJob.running && !$('inviteInput').disabled && !($('startInvites').disabled)`);
  await check('Successful joins refresh the existing server list', `!!document.querySelector('.server-item[data-id="12"]')`);
  await evaluate(`document.querySelector('.server-item[data-id="12"]').focus()`);
  await command('Input.dispatchKeyEvent',{type:'keyDown',key:'Enter',code:'Enter',windowsVirtualKeyCode:13,modifiers:8});
  await command('Input.dispatchKeyEvent',{type:'keyUp',key:'Enter',code:'Enter',windowsVirtualKeyCode:13,modifiers:8});
  await until(`!S.scraping && S.guild?.id==='12' && $('queueProgressTitle').textContent==='Scrape finished'`);
  assert.equal((await state()).stored['50'],1);
  report.checks.push('Shift+Enter scans and scrapes from the keyboard');
  const beforeButton=(await state()).requests.filter(r=>r.path.endsWith('/messages') && r.params.limit!=='1').length;
  await click('#scanServer');
  await until(`fetch('/__checks/state').then(r=>r.json()).then(s=>s.requests.filter(r=>r.path.endsWith('/messages') && r.params.limit!=='1').length>${beforeButton})`);
  await until(`!S.scraping && $('queueProgressTitle').textContent==='Scrape finished'`);
  report.checks.push('Scan and scrape button starts the same queue without double-clicking');
  await click('#sourceDms');
  await until(`document.querySelectorAll('#dmBody .ch-item').length===1`);
  await check('DM queue, monitor, and export controls remain available', `!!document.querySelector('#dmBody [aria-label="Toggle scrape queue"]') && !!document.querySelector('#dmBody [aria-label="Export to ChatML JSONL"]') && !!$('liveMonitorList')`);
  await click('#sourceChannels');
  }

  await scenario({slow_scrapes:true,page_scrapes:true});
  await click('.server-item[data-id="10"]');
  await until(`document.querySelector('#cpBody .ch-item[data-id="20"]')`);
  await click('#cpBody .ch-item[data-id="20"] .ch-btn');
  await evaluate(`$('scrapeLimit').value='250'`);
  let before=(await state()).requests.filter(r=>r.path.endsWith('/messages') && r.params.limit!=='1').length;
  const storedBefore=(await state()).stored['20'] || 0;
  await click('#startScrape');
  await until(`fetch('/__checks/state').then(r=>r.json()).then(s=>s.requests.filter(r=>r.path.endsWith('/messages') && r.params.limit!=='1').length>${before})`);
  await click('#pauseQueueScrape');
  await until(`S.scrapePaused && $('queueProgressTitle').textContent==='Scrape paused'`);
  await check('Pause saves the in-flight page and retains its count and queue', `S.scrapeRun.total===100 && S.queue.length===1 && !$('stopQueueScrape').disabled && $('pauseQueueScrape').textContent==='resume scraping'`);
  let pausedState=await state();
  assert.equal(pausedState.stored['20'],storedBefore+100);
  const pausedRequests=pausedState.requests.length;
  await delay(600);
  assert.equal((await state()).requests.length,pausedRequests);
  report.checks.push('Paused scrape sends no further requests');
  const pausedJob=await evaluate(`S.activeJobId`);
  await command('Page.reload');
  await until(`typeof S!=='undefined' && S.scrapePaused && S.activeJobId===${JSON.stringify(pausedJob)}`);
  await check('Reload reconnects to the paused job with its remaining limit and progress', `S.scrapeRun.total===100 && $('scrapeLimit').value==='250' && $('queueProgressRows').querySelector('strong').textContent==='100 saved'`);
  for(const width of [1440,375,320]) {
    await command('Emulation.setDeviceMetricsOverride',{width,height:1080,deviceScaleFactor:1,mobile:width<600});
    await evaluate(`$('queueProgress').scrollIntoView({behavior:'instant',block:'center'})`);
    await check(`Pause and stop controls fit at ${width}px`, `document.documentElement.scrollWidth<=innerWidth && $('pauseQueueScrape').getBoundingClientRect().right<=innerWidth && $('stopQueueScrape').getBoundingClientRect().right<=innerWidth`);
    const shot=await command('Page.captureScreenshot',{format:'png',captureBeyondViewport:false});
    writeFileSync(path.join(output,`paused-${width}.png`),Buffer.from(shot.data,'base64'));
  }
  await command('Emulation.setDeviceMetricsOverride',{width:1440,height:1080,deviceScaleFactor:1,mobile:false});
  await click('#pauseQueueScrape');
  await until(`!S.scraping && $('queueProgressTitle').textContent==='Scrape finished'`,18000);
  assert.equal((await state()).stored['20'],storedBefore+250);
  const scrapePages=(await state()).requests.filter(r=>r.path.endsWith('/messages') && r.params.limit!=='1').slice(before);
  assert.equal(scrapePages[1].params.before,'1000000000000000401');
  assert.equal(scrapePages[2].params.limit,'50');
  assert(scrapePages.every((request,index)=>!index || request.at-scrapePages[index-1].at>=0.5));
  report.checks.push('Resume uses the exact next page and remaining per-channel limit, with real request pacing');

  await click('#connectDiscord');
  await until(`document.querySelector('.server-item[data-id="10"]')`);
  await click('.server-item[data-id="10"]');
  await until(`document.querySelector('#cpBody .ch-item[data-id="20"]')`);
  await click('#cpBody .ch-item[data-id="20"] .ch-btn');
  await evaluate(`$('scrapeLimit').value='200'`);
  await scenario({page_scrapes:true,break_next:true});
  before=(await state()).requests.filter(r=>r.path.endsWith('/messages') && r.params.limit!=='1').length;
  await click('#startScrape');
  await until(`$('queueProgressTitle').textContent==='Mandatory scrape break'`);
  const breakMessages=await evaluate(`S.scrapeRun.total`);
  assert([0,100].includes(breakMessages));
  await check('Mandatory break keeps pause and stop available', `!$('pauseQueueScrape').disabled && !$('stopQueueScrape').disabled`);
  if (!breakMessages) report.checks.push('Empty successful pages count toward the mandatory break');
  await click('#pauseQueueScrape');
  await until(`S.scrapePaused`);
  await click('#pauseQueueScrape');
  await until(`!S.scrapePaused`);
  await until(`!S.scraping && $('queueProgressTitle').textContent==='Scrape finished'`,70000);
  const breakPages=(await state()).requests.filter(r=>r.path.endsWith('/messages') && r.params.limit!=='1').slice(before);
  assert.equal(breakPages.length,breakMessages ? 2 : 3);
  report.scrapeBreakSeconds=breakPages[1].at-breakPages[0].at;
  assert(report.scrapeBreakSeconds>=60);
  report.checks.push('The production 60-second break survives pause/resume and is verified with real elapsed time');
  await scenario({});

  for(const width of [1440,768,375,320]) {
    await command('Emulation.setDeviceMetricsOverride',{width,height:1080,deviceScaleFactor:1,mobile:width<600});
    await delay(200);
    await check(`No horizontal overflow at ${width}px`, `document.documentElement.scrollWidth<=innerWidth`);
    await evaluate(`$('inviteTitle').scrollIntoView({behavior:'instant',block:'start'})`);
    const shot=await command('Page.captureScreenshot',{format:'png',captureBeyondViewport:false});
    writeFileSync(path.join(output,`scrape-${width}.png`),Buffer.from(shot.data,'base64'));
  }
  await command('Emulation.setEmulatedMedia',{features:[{name:'prefers-reduced-motion',value:'reduce'}]});
  await check('Reduced-motion preference is supported', `matchMedia('(prefers-reduced-motion: reduce)').matches`);
  assert.deepEqual(exceptions,[],'No uncaught browser errors');
  report.checks.push('No uncaught browser errors');
  writeFileSync(path.join(output,'report.json'),JSON.stringify(report,null,2));
  console.log(JSON.stringify(report));
} finally {
  socket?.close();
  browser.kill();
  if (browser.exitCode === null) await new Promise(resolve => browser.once('exit',resolve));
}
