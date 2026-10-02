// Offline collector review with real Chrome, HTTP and a synthetic SQLite archive.
// node checks/browser_review.mjs http://127.0.0.1:8026 agents/review-evidence/collector
import {spawn} from 'node:child_process';
import {existsSync, readFileSync, writeFileSync, mkdirSync} from 'node:fs';
import {setTimeout as delay} from 'node:timers/promises';
import path from 'node:path';
import assert from 'node:assert/strict';

const base = process.argv[2] || 'http://127.0.0.1:8026';
const output = path.resolve(process.argv[3] || 'agents/review-evidence/collector');
mkdirSync(output, {recursive: true});
const profile = path.join(output, `chrome-${Date.now()}`);
const chrome = process.env.CHROME_PATH || path.join(process.env.ProgramFiles, 'Google/Chrome/Application/chrome.exe');
const browser = spawn(chrome, ['--headless', '--disable-gpu', '--disable-background-networking', '--no-first-run', '--no-sandbox', '--remote-debugging-port=0', `--user-data-dir=${profile}`, 'about:blank'], {windowsHide: true, stdio: 'ignore'});
const report = {checks: [], errors: [], externalRequestsBlocked: true};
let socket;
try {
  const portFile = path.join(profile, 'DevToolsActivePort');
  for (let i = 0; i < 100 && !existsSync(portFile); i++) await delay(100);
  assert(existsSync(portFile), 'Chrome started');
  const port = Number(readFileSync(portFile, 'utf8').split('\n')[0]);
  const pages = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
  socket = new WebSocket(pages.find(p => p.type === 'page').webSocketDebuggerUrl);
  await new Promise((resolve, reject) => {socket.onopen = resolve; socket.onerror = reject;});
  const pending = new Map(); let sequence = 0;
  const requests = [];
  const command = (method, params = {}) => new Promise((resolve, reject) => {const id = ++sequence; pending.set(id, {resolve, reject}); socket.send(JSON.stringify({id, method, params}));});
  socket.onmessage = ({data}) => {
    const message = JSON.parse(data);
    if (message.method === 'Runtime.exceptionThrown') report.errors.push(message.params.exceptionDetails.text);
    if (message.method === 'Network.requestWillBeSent') requests.push(message.params.request.url);
    if (message.method === 'Fetch.requestPaused') {
      const {requestId, request} = message.params;
      const local = new URL(request.url).origin === new URL(base).origin;
      command(local ? 'Fetch.continueRequest' : 'Fetch.failRequest', local ? {requestId} : {requestId, errorReason: 'BlockedByClient'}).catch(() => {});
    }
    if (!message.id) return;
    const item = pending.get(message.id); pending.delete(message.id);
    message.error ? item.reject(message.error) : item.resolve(message.result);
  };
  const evaluate = async expression => {const result = await command('Runtime.evaluate', {expression, returnByValue: true, awaitPromise: true}); assert(!result.exceptionDetails, JSON.stringify(result.exceptionDetails)); return result.result.value;};
  const until = async expression => {for (let i = 0; i < 150; i++) {if (await evaluate(expression)) return; await delay(100);} throw Error('Timed out: ' + expression);};
  const check = async (name, expression) => {assert(await evaluate(expression), name); report.checks.push(name);};
  const settled = `!document.querySelector('#searchResults').hasAttribute('aria-busy') && !document.querySelector('#resultsSection').hidden`;
  const click = async selector => {await evaluate(`document.querySelector(${JSON.stringify(selector)}).click()`);};
  const screenshot = async name => {const result = await command('Page.captureScreenshot', {format: 'png'}); writeFileSync(path.join(output, name + '.png'), Buffer.from(result.data, 'base64'));};
  await command('Page.enable'); await command('Runtime.enable'); await command('Network.enable');
  await command('Fetch.enable', {patterns: [{urlPattern: '*'}]});
  await command('Emulation.setDeviceMetricsOverride', {width: 1440, height: 1000, deviceScaleFactor: 1, mobile: false});
  await command('Page.navigate', {url: base});
  await until(`document.body?.dataset.archiveReady === 'true'`);
  await check('Archive opens without a token', `document.querySelector('#homeMessages').textContent.replace(/[^0-9]/g, '') === '1081'`);
  await evaluate(`document.querySelector('#searchInput').value='hello';document.querySelector('#queryForm').requestSubmit()`);
  await until(settled);
  await check('Browse loads 50 results through the cursor API', `document.querySelectorAll('#searchResults .result-row').length === 50 && new URLSearchParams(location.search).get('cursor') === 'true'`);
  await evaluate(`window.firstIds = [...document.querySelectorAll('#searchResults .result-copy')].map(e => e.textContent)`);
  await click('#pagination button:last-child'); await until(settled);
  await check('Next page has no duplicate messages', `document.querySelector('.page-current').textContent === 'page 2' && [...document.querySelectorAll('#searchResults .result-copy')].every(e => !window.firstIds.includes(e.textContent))`);
  const secondIds = await evaluate(`[...document.querySelectorAll('#searchResults .result-copy')].map(e => e.textContent)`);
  await command('Page.reload'); await until(`document.body?.dataset.archiveReady === 'true' && ${settled}`);
  await check('Reload preserves the page number and its results', `document.querySelector('.page-current').textContent === 'page 2' && JSON.stringify([...document.querySelectorAll('#searchResults .result-copy')].map(e => e.textContent)) === ${JSON.stringify(JSON.stringify(secondIds))}`);
  await click('#pagination button:first-child'); await until(settled);
  await check('Previous after reload returns the first page', `document.querySelector('.page-current').textContent === 'page 1' && !new URLSearchParams(location.search).has('before')`);
  await click('#pagination button:last-child'); await until(settled);
  await click('#pagination button:last-child'); await until(settled);
  await command('Page.reload'); await until(`document.body?.dataset.archiveReady === 'true' && ${settled}`);
  await check('A deep link offers an honest first-page fallback', `document.querySelector('.page-current').textContent === 'page 3' && document.querySelector('#pagination button:first-child').textContent === 'first page'`);
  await click('#pagination button:first-child'); await until(settled);
  await check('First-page fallback restores the start of the results', `document.querySelector('.page-current').textContent === 'page 1' && !new URLSearchParams(location.search).has('before')`);
  await command('Page.navigate', {url: base + '/?search=1&q=hello&page=2#browse'});
  await until(`document.body?.dataset.archiveReady === 'true' && ${settled}`);
  await check('Existing counted page links remain functional', `document.querySelector('.page-current').textContent === 'page 2' && document.querySelector('#resultsStatus').textContent.includes('of 1,081') && !new URLSearchParams(location.search).has('cursor')`);
  await evaluate(`document.querySelector('#searchInput').value='hello';document.querySelector('#fUser').value='60';document.querySelector('#queryForm').requestSubmit()`); await until(settled);
  await check('New filters reset pagination and isolate the author', `document.querySelectorAll('#searchResults .result-row').length === 1 && document.querySelector('.message-author').dataset.profileId === '60' && !new URLSearchParams(location.search).has('before')`);
  await evaluate(`document.querySelector('#fUser').value='50';document.querySelector('#queryForm').requestSubmit()`); await until(settled);
  await click('[data-profile-id="50"]'); await until(`document.querySelector('.profile-dialog').open && document.querySelectorAll('.profile-server').length === 30`);
  await check('Saved profile and first 30 server counts render locally', `document.querySelector('.profile-name').textContent === 'Alice Example' && document.querySelector('.profile-server strong').textContent === 'Garden Club'`);
  await click('.profile-more'); await until(`document.querySelectorAll('.profile-server').length === 45`);
  await check('Profile pagination retains all 45 observed servers', `document.querySelector('.profile-more').hidden`);
  await evaluate(`document.querySelector('.profile-server-search input').value='garden';document.querySelector('.profile-server-search input').dispatchEvent(new Event('input'))`);
  await until(`document.querySelectorAll('.profile-server').length === 1`);
  await check('Profile server search still works', `document.querySelector('.profile-server strong').textContent === 'Garden Club'`);
  await evaluate(`document.querySelector('.profile-dialog').close()`);
  await click('[data-view="stats"]'); await until(`document.querySelector('#view-stats').classList.contains('active')`);
  await check('Stats remain available', `document.querySelector('[data-view="stats"]').getAttribute('aria-pressed') === 'true'`);
  await click('[data-view="scrape"]');
  await check('Existing collection controls remain present', `!!document.querySelector('#connectDiscord') && !!document.querySelector('#backfillProfiles') && !!document.querySelector('[data-source="dms"]')`);
  await click('[data-view="browse"]');
  for (const width of [1440, 768, 375, 320]) {
    await command('Emulation.setDeviceMetricsOverride', {width, height: 1000, deviceScaleFactor: 1, mobile: width < 500});
    await delay(150);
    await check(`Collector fits ${width}px`, `document.documentElement.scrollWidth <= innerWidth`);
    await screenshot('collector-' + width);
  }
  const forbidden = requests.filter(url => /\/api\/(?:token\/validate|guilds|scrape\/|invites\/|profiles\/[^/]+\/fetch|profiles\/backfill)/.test(url));
  assert.deepEqual(forbidden, [], 'no credential-dependent features invoked'); report.checks.push('No credential-dependent features invoked');
  assert.deepEqual(report.errors, [], 'no browser exceptions'); report.checks.push('No browser exceptions');
} catch (error) {report.failure = String(error); throw error;}
finally {writeFileSync(path.join(output, 'report.json'), JSON.stringify(report, null, 2)); console.log(JSON.stringify(report, null, 2)); socket?.close(); browser.kill();}
