'use strict';

// ── State ────────────────────────────────────────────────────
const S = {
  guilds:        [],
  guild:         null,
  queue:         [],
  scraping:      false,
  activeJobId:   null,
  searchPage:    1,
  filterChans:   [],
  liveChannels:  new Set(),
  liveES:        null,
  // filter resolution maps: display text → id
  guildMap:      new Map(),
  channelMap:    new Map(),
  userMap:       new Map(),
};

// Chart.js instances — destroyed before re-render
const _charts = {};

// ── Boot ─────────────────────────────────────────────────────
document.addEventListener('DOMContentLoaded', () => {
  initNav();
  initSettings();
  initQueue();
  initSearch();
  initModal();
  initExportModal();
  initLive();
  initStats();
  boot();
});

async function boot() {
  const v = await api('/api/token/validate');
  if (!v.valid) {
    openSettings();
    setTokenStatus('No token — paste yours in settings.', 'fail');
  } else {
    loadGuilds();
  }
}

// ── Nav tabs ─────────────────────────────────────────────────
function initNav() {
  document.querySelectorAll('.tab').forEach(t =>
    t.addEventListener('click', () => switchView(t.dataset.view))
  );
}

function switchView(name) {
  document.querySelectorAll('.tab').forEach(t =>
    t.classList.toggle('active', t.dataset.view === name)
  );
  document.querySelectorAll('.view').forEach(v =>
    v.classList.toggle('active', v.id === `view-${name}`)
  );
  if (name === 'search') loadSearchFilters();
  if (name === 'live')   connectLiveSSE();
  if (name === 'stats')  loadStats();
  if (name === 'dms')    loadDms();
}

// ── Settings panel ───────────────────────────────────────────
function initSettings() {
  const btn     = $('menuBtn');
  const overlay = $('overlay');
  const panel   = $('settingsPanel');

  btn.addEventListener('click',     () => panel.classList.contains('open') ? closeSettings() : openSettings());
  overlay.addEventListener('click', closeSettings);
  $('closeSettings').addEventListener('click', closeSettings);

  // Token show/hide
  const inp = $('tokenInput');
  const tog = $('tokenToggle');
  tog.addEventListener('click', () => {
    const hide = inp.type === 'text';
    inp.type = hide ? 'password' : 'text';
    tog.textContent = hide ? 'show' : 'hide';
  });

  inp.addEventListener('keydown', e => { if (e.key === 'Enter') $('saveToken').click(); });

  $('saveToken').addEventListener('click', async () => {
    const token = inp.value.trim();
    if (!token) return;
    setTokenStatus('verifying…', '');
    await api('/api/settings', { method: 'POST', body: { token } });
    const v = await api('/api/token/validate');
    if (v.valid) {
      setTokenStatus(`✓ connected as ${v.username}`, 'ok');
      inp.value = '';
      setTimeout(() => { closeSettings(); loadGuilds(); }, 900);
    } else {
      setTokenStatus('✗ invalid token', 'fail');
    }
  });

  $('clearDb').addEventListener('click', async () => {
    if (!confirm('Delete all scraped data? This cannot be undone.')) return;
    await api('/api/messages', { method: 'DELETE' });
    loadDbStats();
  });
}

function openSettings() {
  $('menuBtn').classList.add('open');
  $('overlay').classList.add('visible');
  $('settingsPanel').classList.add('open');
  loadDbStats();
}
function closeSettings() {
  $('menuBtn').classList.remove('open');
  $('overlay').classList.remove('visible');
  $('settingsPanel').classList.remove('open');
}
function setTokenStatus(msg, cls) {
  const el = $('tokenStatus');
  el.textContent = msg;
  el.className = 'token-status' + (cls ? ` ${cls}` : '');
}
async function loadDbStats() {
  try {
    const f = await api('/api/search/filters');
    $('dbStats').textContent =
      `${n(f.total_messages)} messages · ${f.guilds.length} servers · ${f.channels.length} channels`;
  } catch { $('dbStats').textContent = '—'; }
}

// ── Guilds ───────────────────────────────────────────────────
async function loadGuilds() {
  const list = $('serverList');
  list.innerHTML = spinnerHTML();
  try {
    S.guilds = await api('/api/guilds');
    renderGuilds();
  } catch {
    list.innerHTML = '<div class="error-msg">Failed to load servers</div>';
  }
}

function renderGuilds() {
  const list = $('serverList');
  list.innerHTML = '';
  S.guilds.forEach(g => {
    const el = ce('div', 'server-item');
    el.dataset.id = g.id;

    const icon = ce('div', 'server-icon');
    if (g.icon) {
      const img = new Image();
      img.src = g.icon;
      img.alt = g.name;
      img.onerror = () => { img.remove(); icon.textContent = initial(g.name); };
      icon.appendChild(img);
    } else {
      icon.textContent = initial(g.name);
    }

    const name = ce('div', 'server-name');
    name.textContent = g.name;
    name.title = g.name;

    el.append(icon, name);
    el.addEventListener('click', () => selectGuild(g));
    list.appendChild(el);
  });
}

async function selectGuild(guild) {
  S.guild = guild;
  document.querySelectorAll('.server-item').forEach(el =>
    el.classList.toggle('active', el.dataset.id === guild.id)
  );

  $('emptyState').style.display = 'none';
  const pane = $('channelsPane');
  pane.classList.add('visible');
  $('cpHead').textContent = guild.name;
  $('cpBody').innerHTML = spinnerHTML();

  try {
    const channels = await api(`/api/guilds/${guild.id}/channels`);
    renderChannels(channels, guild);
  } catch {
    $('cpBody').innerHTML = '<div class="error-msg">Failed to load channels</div>';
  }
}

// ── Direct Messages / Group DMs ───────────────────────────────
async function loadDms() {
  const body = $('dmBody');
  body.innerHTML = spinnerHTML();

  try {
    const dms = await api('/api/dms');
    renderDmList(dms, body);
  } catch {
    body.innerHTML = '<div class="error-msg">Failed to load DMs</div>';
  }
}

function renderDmList(dms, body) {
  body.innerHTML = '';

  if (!dms.length) {
    body.innerHTML = '<div class="placeholder-msg">No DMs or group chats found.</div>';
    return;
  }

  dms.forEach(dm => {
    const el = ce('div', 'ch-item');
    el.dataset.id = dm.id;
    if (S.queue.some(q => q.id === dm.id)) el.classList.add('queued');

    const hash = ce('div', 'ch-hash');
    hash.textContent = dm.type === 3 ? '👥' : '@';

    const name = ce('div', 'ch-name');
    name.textContent = dm.name;

    const btn = ce('button', 'ch-btn');
    btn.setAttribute('aria-label', 'Toggle scrape queue');
    btn.textContent = el.classList.contains('queued') ? '✓' : '+';

    const exportBtn = ce('button', 'ch-btn');
    exportBtn.setAttribute('aria-label', 'Export to ChatML JSONL');
    exportBtn.title = 'Export to ChatML JSONL';
    exportBtn.textContent = '⇩';

    el.append(hash, name, btn, exportBtn);

    // Whole row toggles the scrape queue, matching how server channel rows
    // behave. guild_id/guild_name are synthetic since DMs have no guild.
    el.addEventListener('click', e => {
      e.stopPropagation();
      const guild = { id: null, name: dm.type === 3 ? 'Group DM' : 'Direct Message' };
      toggleQueue(dm, guild, el, btn);
    });
    exportBtn.addEventListener('click', e => {
      e.stopPropagation();
      openExportModal(dm);
    });

    body.appendChild(el);
  });
}

// ── Channels ─────────────────────────────────────────────────
function renderChannels(channels, guild) {
  const body = $('cpBody');
  body.innerHTML = '';

  const categories = {};
  const uncategorised = [];

  channels.forEach(c => {
    if (c.category_id) {
      if (!categories[c.category]) categories[c.category] = [];
      categories[c.category].push(c);
    } else {
      uncategorised.push(c);
    }
  });

  const makeRow = ch => {
    const el = ce('div', 'ch-item');
    el.dataset.id = ch.id;
    if (S.queue.some(q => q.id === ch.id)) el.classList.add('queued');
    if (S.liveChannels.has(ch.id)) el.classList.add('live');

    const hash = ce('div', 'ch-hash');
    hash.textContent = ch.type === 5 ? '📣' : '#';

    const name = ce('div', 'ch-name');
    name.textContent = ch.name;

    // live pulsing dot (only visible when .live)
    const dot = ce('div', 'ch-live-dot');

    // scrape queue toggle
    const btn = ce('button', 'ch-btn');
    btn.setAttribute('aria-label', 'Toggle scrape queue');
    btn.textContent = el.classList.contains('queued') ? '✓' : '+';

    // live monitor toggle
    const liveBtn = ce('button', 'ch-btn ch-live-btn');
    liveBtn.setAttribute('aria-label', 'Toggle live monitor');
    liveBtn.textContent = '⚡';
    liveBtn.title = S.liveChannels.has(ch.id) ? 'Stop live monitor' : 'Start live monitor';

    el.append(hash, name, dot, btn, liveBtn);

    el.addEventListener('click', e => {
      e.stopPropagation();
      toggleQueue(ch, guild, el, btn);
    });
    liveBtn.addEventListener('click', e => {
      e.stopPropagation();
      toggleLiveChannel(ch, guild, el, liveBtn);
    });

    return el;
  };

  uncategorised.forEach(c => body.appendChild(makeRow(c)));

  Object.entries(categories).forEach(([cat, chs]) => {
    const lbl = ce('div', 'category-label');
    lbl.textContent = cat;
    body.appendChild(lbl);
    chs.forEach(c => body.appendChild(makeRow(c)));
  });
}

// ── Scrape queue ─────────────────────────────────────────────
function toggleQueue(ch, guild, rowEl, btnEl) {
  const idx = S.queue.findIndex(q => q.id === ch.id);
  if (idx > -1) {
    S.queue.splice(idx, 1);
    rowEl.classList.remove('queued');
    btnEl.textContent = '+';
  } else {
    S.queue.push({ id: ch.id, name: ch.name, guild_id: guild.id, guild_name: guild.name });
    rowEl.classList.add('queued');
    btnEl.textContent = '✓';
  }
  renderQueue();
}

function initQueue() {
  $('clearQueue').addEventListener('click', () => {
    S.queue = [];
    renderQueue();
    document.querySelectorAll('.ch-item.queued').forEach(el => {
      el.classList.remove('queued');
      const b = el.querySelector('.ch-btn');
      if (b) b.textContent = '+';
    });
  });
  $('startScrape').addEventListener('click', startScraping);
}

function renderQueue() {
  const bar   = $('queueBar');
  const chips = $('queueChips');
  $('queueCount').textContent = S.queue.length;

  if (S.queue.length === 0) {
    bar.classList.remove('visible');
    return;
  }
  bar.classList.add('visible');

  chips.innerHTML = '';
  S.queue.forEach(ch => {
    const chip = ce('div', 'qb-chip');
    chip.innerHTML =
      `<span>#</span><span>${esc(ch.name)}</span>` +
      `<span class="qb-chip-server">${esc(ch.guild_name)}</span>` +
      `<button class="qb-chip-x" data-id="${ch.id}">&#x2715;</button>`;
    chip.querySelector('.qb-chip-x').addEventListener('click', () => {
      S.queue = S.queue.filter(q => q.id !== ch.id);
      renderQueue();
      const row = document.querySelector(`.ch-item[data-id="${ch.id}"]`);
      if (row) { row.classList.remove('queued'); const b = row.querySelector('.ch-btn'); if (b) b.textContent = '+'; }
    });
    chips.appendChild(chip);
  });
}

// ── Scraping ─────────────────────────────────────────────────
async function startScraping() {
  if (!S.queue.length || S.scraping) return;
  S.scraping = true;

  const limitVal = parseInt($('scrapeLimit').value, 10);
  const limit = (!isNaN(limitVal) && limitVal > 0) ? limitVal : 0;

  // Show modal
  const modal = $('modalBg');
  modal.classList.add('visible');
  $('modalFoot').classList.remove('visible');
  $('stopScrapeBtn').disabled = false;
  $('moLog').innerHTML = '';
  $('moBar').style.width = '0%';
  $('moStats').textContent = limit ? `Limit: ${n(limit)} msgs per channel` : '';
  $('moChannel').textContent = 'Initializing…';

  $('scrapePill').classList.add('visible');

  let resp;
  try {
    resp = await api('/api/scrape/start', { method: 'POST', body: { channels: S.queue, limit } });
  } catch {
    finishScrape('Failed to start job');
    return;
  }

  const { job_id } = resp;
  S.activeJobId = job_id;

  const es = new EventSource(`/api/scrape/progress/${job_id}`);
  const total = S.queue.length;
  let done = 0;
  let totalMsgs = 0;

  es.onmessage = e => {
    const ev = JSON.parse(e.data);
    const log = $('moLog');

    switch (ev.type) {
      case 'channel_start':
        $('moChannel').textContent = `Scraping #${ev.channel} in ${ev.guild}…`;
        logLine(log, `→ #${ev.channel}  (${ev.guild})`);
        break;

      case 'progress':
        totalMsgs = ev.total_messages;
        $('moStats').textContent =
          `${n(ev.messages)} msgs from #${ev.channel}  ·  ${n(totalMsgs)} total`;
        $('scrapeStatus').textContent = `${n(totalMsgs)} msgs`;
        break;

      case 'channel_complete':
        done++;
        $('moBar').style.width = `${(done / total) * 100}%`;
        logLine(log, `✓ #${ev.channel}: ${n(ev.messages)} msgs`, 'ok');
        break;

      case 'channel_error':
        done++;
        logLine(log, `✗ #${ev.channel}: ${ev.message}`, 'err');
        break;

      case 'complete':
        es.close();
        $('moBar').style.width = '100%';
        $('moChannel').textContent = 'Done.';
        $('moStats').textContent =
          `${n(ev.total_messages)} messages scraped across ${ev.channels} channels`;
        logLine(log, `✓ Complete — ${n(ev.total_messages)} total`, 'ok');
        finishScrape();
        break;

      case 'cancelled':
        es.close();
        $('moChannel').textContent = 'Stopped.';
        $('moStats').textContent = `Scraped ${n(ev.total_messages)} messages before stopping`;
        logLine(log, `⏹ Stopped — ${n(ev.total_messages)} msgs saved`, 'ok');
        finishScrape();
        break;

      case 'error':
        es.close();
        logLine(log, `✗ ${ev.message}`, 'err');
        finishScrape(ev.message);
        break;
    }
  };

  es.onerror = () => {
    es.close();
    finishScrape('Connection lost');
  };
}

function finishScrape(errMsg) {
  S.scraping = false;
  S.activeJobId = null;
  $('scrapePill').classList.remove('visible');
  $('stopScrapeBtn').disabled = true;
  $('modalFoot').classList.add('visible');
  if (errMsg) logLine($('moLog'), `Error: ${errMsg}`, 'err');
  S.queue = [];
  renderQueue();
}

function initModal() {
  $('dismissModal').addEventListener('click', () => $('modalBg').classList.remove('visible'));
  $('stopScrapeBtn').addEventListener('click', async () => {
    if (!S.activeJobId) return;
    $('stopScrapeBtn').disabled = true;
    $('moChannel').textContent = 'Stopping…';
    try {
      await api(`/api/scrape/${S.activeJobId}/stop`, { method: 'POST' });
    } catch { /* SSE will still fire cancelled event */ }
  });
}

function logLine(container, text, cls = '') {
  const d = ce('div', cls ? `log-${cls}` : '');
  d.textContent = text;
  container.appendChild(d);
  container.scrollTop = container.scrollHeight;
}

// ── Search ────────────────────────────────────────────────────
function initSearch() {
  $('searchBtn').addEventListener('click', () => doSearch(1));
  $('searchInput').addEventListener('keydown', e => { if (e.key === 'Enter') doSearch(1); });

  // text inputs fire on 'input'; date inputs fire on 'change'
  ['fGuild', 'fChannel', 'fUser'].forEach(id =>
    $(id).addEventListener('input', () => doSearch(1))
  );
  ['fDateFrom', 'fDateTo'].forEach(id =>
    $(id).addEventListener('change', () => doSearch(1))
  );

  // When guild filter changes, refresh channel datalist
  $('fGuild').addEventListener('input', e => {
    const gid = S.guildMap.get(e.target.value.trim()) || '';
    populateChannelFilter(gid);
  });
}

async function loadSearchFilters() {
  try {
    const f = await api('/api/search/filters');
    S.filterChans = f.channels;
    S.guildMap.clear();
    S.channelMap.clear();
    S.userMap.clear();

    // Guilds datalist
    const gdl = $('dl-guilds');
    gdl.innerHTML = '';
    f.guilds.forEach(g => {
      S.guildMap.set(g.guild_name, g.guild_id);
      S.guildMap.set(g.guild_id,   g.guild_id);
      const o = document.createElement('option');
      o.value = g.guild_name;
      gdl.appendChild(o);
    });

    // Users datalist — searchable by name OR raw ID
    const udl = $('dl-users');
    udl.innerHTML = '';
    f.users.forEach(u => {
      const display = `${u.author_name}  ·  ${u.author_id}`;
      S.userMap.set(display,       u.author_id);
      S.userMap.set(u.author_name, u.author_id);
      S.userMap.set(u.author_id,   u.author_id);
      const o = document.createElement('option');
      o.value = display;
      udl.appendChild(o);
    });

    // Rebuild channel list (respect current guild filter if any)
    const gid = S.guildMap.get($('fGuild').value.trim()) || '';
    populateChannelFilter(gid);
  } catch {}
}

function populateChannelFilter(guildId) {
  const dl = $('dl-channels');
  dl.innerHTML = '';
  S.channelMap.clear();
  const list = guildId
    ? S.filterChans.filter(c => c.guild_id === guildId)
    : S.filterChans;
  list.forEach(c => {
    const display = `#${c.channel_name}`;
    S.channelMap.set(display,     c.channel_id);
    S.channelMap.set(c.channel_id, c.channel_id);
    const o = document.createElement('option');
    o.value = display;
    dl.appendChild(o);
  });
}

// Resolve a free-text filter value to the ID the API expects
function resolveFilter(map, rawValue) {
  const v = rawValue.trim();
  if (!v) return '';
  return map.get(v) || (/^\d{17,20}$/.test(v) ? v : '');
}

async function doSearch(page) {
  S.searchPage = page;
  const q          = $('searchInput').value.trim();
  const guild_id   = resolveFilter(S.guildMap,   $('fGuild').value);
  const channel_id = resolveFilter(S.channelMap, $('fChannel').value);
  const author_id  = resolveFilter(S.userMap,    $('fUser').value);
  const date_from  = $('fDateFrom').value;
  const date_to    = $('fDateTo').value;

  const p = new URLSearchParams({ page, limit: 50 });
  if (q)          p.set('q',          q);
  if (guild_id)   p.set('guild_id',   guild_id);
  if (channel_id) p.set('channel_id', channel_id);
  if (author_id)  p.set('author_id',  author_id);
  if (date_from)  p.set('date_from',  date_from + 'T00:00:00');
  if (date_to)    p.set('date_to',    date_to   + 'T23:59:59');

  const results = $('searchResults');
  results.innerHTML = spinnerHTML();

  try {
    const data = await api(`/api/search?${p}`);
    renderResults(data, q);
  } catch {
    results.innerHTML = '<div class="no-results">Search failed</div>';
  }
}

function renderResults(data, query) {
  const results = $('searchResults');
  const pag     = $('pagination');

  if (!data.messages.length) {
    results.innerHTML = '<div class="no-results">No results found</div>';
    pag.classList.remove('visible');
    return;
  }

  results.innerHTML = `<div class="result-count">${n(data.total)} result${data.total !== 1 ? 's' : ''}</div>`;

  data.messages.forEach(msg => {
    const card = ce('div', 'result-card');
    const ts   = new Date(msg.timestamp).toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' });
    const body = msg.content || '';
    const hi   = query ? highlight(body, query) : esc(body);

    let attsHtml = '';
    if (msg.attachments?.length) {
      const links = msg.attachments.map(url => {
        const isImg = /\.(png|jpe?g|gif|webp|avif)(\?|$)/i.test(url);
        return `<a href="${esc(url)}" target="_blank" rel="noopener" class="rc-att">${isImg ? '🖼 image' : '📎 file'}</a>`;
      }).join('');
      attsHtml = `<div class="rc-attachments">${links}</div>`;
    }

    card.innerHTML = `
      <div class="rc-meta">
        <span class="rc-author">${esc(msg.author_name)}</span>
        <span class="rc-loc"><span class="srv">${esc(msg.guild_name)}</span> / #${esc(msg.channel_name)}</span>
        <span class="rc-ts">${ts}</span>
      </div>
      <div class="rc-meta" style="margin-bottom:0;margin-top:-3px">
        <span class="rc-uid">uid: ${esc(msg.author_id)}</span>
      </div>
      <div class="rc-body ${!body ? 'empty' : ''}" style="margin-top:6px">${hi || '(no text content)'}</div>
      ${attsHtml}
    `;
    results.appendChild(card);
  });

  if (data.pages > 1) {
    renderPagination(data.page, data.pages);
    pag.classList.add('visible');
  } else {
    pag.classList.remove('visible');
  }
}

function renderPagination(cur, total) {
  const pag = $('pagination');
  pag.innerHTML = '';

  const btn = (label, page, disabled = false) => {
    const b = ce('button', `pg-btn${page === cur ? ' active' : ''}`);
    b.innerHTML = label;
    b.disabled  = disabled;
    if (!disabled && page !== cur) b.addEventListener('click', () => doSearch(page));
    return b;
  };

  pag.appendChild(btn('&#8592;', cur - 1, cur === 1));

  let pages;
  if (total <= 7) {
    pages = Array.from({ length: total }, (_, i) => i + 1);
  } else {
    pages = [1];
    if (cur > 3)        pages.push('…');
    for (let i = Math.max(2, cur - 1); i <= Math.min(total - 1, cur + 1); i++) pages.push(i);
    if (cur < total - 2) pages.push('…');
    pages.push(total);
  }

  pages.forEach(p => {
    if (p === '…') {
      const s = ce('span', 'pg-ellipsis');
      s.textContent = '…';
      pag.appendChild(s);
    } else {
      pag.appendChild(btn(p, p));
    }
  });

  pag.appendChild(btn('&#8594;', cur + 1, cur === total));
}

// ── Stats / data visualisation ───────────────────────────────

function initStats() {
  $('refreshStats').addEventListener('click', loadStats);
}

async function loadStats() {
  // Show spinner in each chart box while loading
  ['chartByServer', 'chartOverTime', 'chartByHour'].forEach(id => {
    const canvas = $(id);
    if (canvas) canvas.style.opacity = '0.3';
  });

  try {
    const d = await api('/api/stats');
    renderStats(d);
  } catch {
    // silent fail — DB might be empty
  } finally {
    ['chartByServer', 'chartOverTime', 'chartByHour'].forEach(id => {
      const canvas = $(id);
      if (canvas) canvas.style.opacity = '1';
    });
  }
}

function renderStats(d) {
  // ── Summary cards
  $('sTotal').textContent   = n(d.total_messages);
  $('sServers').textContent = n(d.total_servers);
  $('sChannels').textContent= n(d.total_channels);
  $('sUsers').textContent   = n(d.total_users);
  $('sDbSize').textContent  = fmtBytes(d.db_size_bytes);

  // ── Leaderboard
  const lb = $('leaderboard');
  lb.innerHTML = '';
  const medals = ['🥇', '🥈', '🥉'];
  const maxCount = d.top_users[0]?.count || 1;

  if (!d.top_users.length) {
    lb.innerHTML = '<div class="lb-empty">No data yet</div>';
  } else {
    d.top_users.forEach((u, i) => {
      const pct = Math.round((u.count / maxCount) * 100);
      const item = ce('div', 'lb-item');
      item.innerHTML =
        `<span class="lb-rank">${medals[i] || (i + 1)}</span>` +
        `<div class="lb-info">` +
          `<div class="lb-name">${esc(u.author_name)}</div>` +
          `<div class="lb-uid">${esc(u.author_id)}</div>` +
        `</div>` +
        `<div class="lb-bar-wrap"><div class="lb-bar" style="width:${pct}%"></div></div>` +
        `<div class="lb-count">${n(u.count)}</div>`;
      lb.appendChild(item);
    });
  }

  // ── Chart: messages by server (horizontal bar)
  renderChart('chartByServer', 'bar', {
    labels: d.messages_by_server.map(s => truncate(s.guild_name, 18)),
    datasets: [{
      data:            d.messages_by_server.map(s => s.count),
      backgroundColor: 'rgba(0,229,160,0.7)',
      hoverBackgroundColor: '#00e5a0',
      borderRadius:    4,
      borderSkipped:   false,
    }],
  }, {
    indexAxis: 'y',
    scales: {
      x: { grid: { color: '#1e1e1e' }, ticks: { color: '#666', font: { size: 10 } }, border: { color: '#232323' } },
      y: { grid: { display: false },   ticks: { color: '#aaa', font: { size: 10 } }, border: { color: '#232323' } },
    },
  });

  // ── Chart: messages over time (line)
  const dayLabels  = d.messages_by_day.map(r => r.date);
  const dayCounts  = d.messages_by_day.map(r => r.count);

  renderChart('chartOverTime', 'line', {
    labels: dayLabels,
    datasets: [{
      data:         dayCounts,
      borderColor:  '#00e5a0',
      borderWidth:  1.5,
      pointRadius:  2,
      pointHoverRadius: 4,
      pointBackgroundColor: '#00e5a0',
      fill:         true,
      backgroundColor: (ctx) => {
        const chart = ctx.chart;
        const { ctx: c, chartArea } = chart;
        if (!chartArea) return 'transparent';
        const grad = c.createLinearGradient(0, chartArea.top, 0, chartArea.bottom);
        grad.addColorStop(0, 'rgba(0,229,160,0.22)');
        grad.addColorStop(1, 'rgba(0,229,160,0)');
        return grad;
      },
      tension: 0.35,
    }],
  }, {
    scales: {
      x: { grid: { color: '#1e1e1e' }, ticks: { color: '#666', font: { size: 10 }, maxTicksLimit: 8 }, border: { color: '#232323' } },
      y: { grid: { color: '#1e1e1e' }, ticks: { color: '#666', font: { size: 10 } }, border: { color: '#232323' }, beginAtZero: true },
    },
  });

  // ── Chart: activity by hour (bar)
  const hourBuckets = Array.from({ length: 24 }, (_, i) => {
    const found = d.messages_by_hour.find(h => h.hour === i);
    return found ? found.count : 0;
  });
  const peakHour = hourBuckets.indexOf(Math.max(...hourBuckets));

  renderChart('chartByHour', 'bar', {
    labels: Array.from({ length: 24 }, (_, i) => `${String(i).padStart(2, '0')}h`),
    datasets: [{
      data: hourBuckets,
      backgroundColor: hourBuckets.map((_, i) =>
        i === peakHour ? '#00e5a0' : 'rgba(0,229,160,0.45)'
      ),
      hoverBackgroundColor: '#00e5a0',
      borderRadius: 3,
      borderSkipped: false,
    }],
  }, {
    scales: {
      x: { grid: { display: false }, ticks: { color: '#666', font: { size: 9 } }, border: { color: '#232323' } },
      y: { grid: { color: '#1e1e1e' }, ticks: { color: '#666', font: { size: 10 } }, border: { color: '#232323' }, beginAtZero: true },
    },
  });
}

function renderChart(id, type, data, extraOpts = {}) {
  if (_charts[id]) { _charts[id].destroy(); delete _charts[id]; }
  const canvas = $(id);
  if (!canvas) return;

  const base = {
    responsive: true,
    maintainAspectRatio: false,
    animation: { duration: 400 },
    plugins: {
      legend: { display: false },
      tooltip: {
        backgroundColor: '#161616',
        borderColor: '#2e2e2e',
        borderWidth: 1,
        titleColor: '#efefef',
        bodyColor: '#aaaaaa',
        padding: 8,
        cornerRadius: 6,
      },
    },
  };

  _charts[id] = new Chart(canvas, {
    type,
    data,
    options: deepMerge(base, extraOpts),
  });
}

// Simple deep merge for chart options (two levels)
function deepMerge(base, extra) {
  const out = { ...base };
  for (const k of Object.keys(extra)) {
    if (extra[k] && typeof extra[k] === 'object' && !Array.isArray(extra[k]) && typeof base[k] === 'object') {
      out[k] = deepMerge(base[k], extra[k]);
    } else {
      out[k] = extra[k];
    }
  }
  return out;
}

function fmtBytes(b) {
  if (!b) return '0 B';
  const u = ['B', 'KB', 'MB', 'GB'];
  let i = 0;
  while (b >= 1024 && i < u.length - 1) { b /= 1024; i++; }
  return `${b.toFixed(i > 0 ? 1 : 0)} ${u[i]}`;
}

function truncate(str, len) {
  return str.length > len ? str.slice(0, len - 1) + '…' : str;
}

// ── Live monitoring ───────────────────────────────────────────

function initLive() {
  $('stopAllLive').addEventListener('click', () => stopLiveChannels([]));
  $('clearFeed').addEventListener('click', () => {
    $('liveFeed').innerHTML = '<div class="live-feed-empty">Feed cleared.</div>';
  });
}

async function toggleLiveChannel(ch, guild, rowEl, btnEl) {
  if (S.liveChannels.has(ch.id)) {
    await stopLiveChannels([ch.id]);
  } else {
    await startLiveChannels([{ id: ch.id, name: ch.name, guild_id: guild.id, guild_name: guild.name }]);
    // Auto-switch to live tab
    switchView('live');
    connectLiveSSE();
  }
}

async function startLiveChannels(channels) {
  try {
    await api('/api/live/start', { method: 'POST', body: { channels } });
    channels.forEach(ch => S.liveChannels.add(ch.id));
    updateLiveBadge();
    // Update any visible channel rows
    channels.forEach(ch => {
      const row = document.querySelector(`.ch-item[data-id="${ch.id}"]`);
      if (row) {
        row.classList.add('live');
        const lb = row.querySelector('.ch-live-btn');
        if (lb) lb.title = 'Stop live monitor';
      }
    });
  } catch (e) { console.error('live start failed', e); }
}

async function stopLiveChannels(channelIds) {
  try {
    await api('/api/live/stop', { method: 'POST', body: { channel_ids: channelIds } });
    const stopped = channelIds.length ? channelIds : [...S.liveChannels];
    stopped.forEach(cid => {
      S.liveChannels.delete(cid);
      const row = document.querySelector(`.ch-item[data-id="${cid}"]`);
      if (row) {
        row.classList.remove('live');
        const lb = row.querySelector('.ch-live-btn');
        if (lb) lb.title = 'Start live monitor';
      }
    });
    updateLiveBadge();
    renderMonitorList();
  } catch (e) { console.error('live stop failed', e); }
}

function connectLiveSSE() {
  if (S.liveES) return; // already connected
  const es = new EventSource('/api/live/events');
  S.liveES = es;

  es.onmessage = e => {
    const ev = JSON.parse(e.data);
    if (ev.type === 'ping') return;

    if (ev.type === 'monitor_start') {
      S.liveChannels.add(ev.channel_id);
      updateLiveBadge();
      renderMonitorList();
      const row = document.querySelector(`.ch-item[data-id="${ev.channel_id}"]`);
      if (row) row.classList.add('live');
    }
    if (ev.type === 'monitor_stop') {
      S.liveChannels.delete(ev.channel_id);
      updateLiveBadge();
      renderMonitorList();
      const row = document.querySelector(`.ch-item[data-id="${ev.channel_id}"]`);
      if (row) row.classList.remove('live');
    }
    if (ev.type === 'message') {
      appendLiveMessage(ev);
    }
  };

  es.onerror = () => {
    S.liveES = null;
    es.close();
    // Reconnect after 5s if there are still monitored channels
    if (S.liveChannels.size > 0) {
      setTimeout(connectLiveSSE, 5000);
    }
  };
}

function renderMonitorList() {
  const list = $('liveMonitorList');
  if (S.liveChannels.size === 0) {
    list.innerHTML = '<div class="live-empty-msg">No channels monitored.<br>Go to Browse → ⚡ to start.</div>';
    return;
  }
  list.innerHTML = '';
  // Build from current live_monitors state (we track in S.liveChannels + guild data from SSE)
  // Re-fetch status for display names
  api('/api/live/status').then(data => {
    list.innerHTML = '';
    data.channels.forEach(ch => {
      const item = ce('div', 'live-monitor-item');
      item.dataset.cid = ch.channel_id;
      item.innerHTML =
        `<div class="lm-dot"></div>` +
        `<div class="lm-info"><div class="lm-channel">#${esc(ch.channel_name)}</div>` +
        `<div class="lm-guild">${esc(ch.guild_name)}</div></div>` +
        `<button class="lm-stop" title="Stop monitoring" data-cid="${ch.channel_id}">✕</button>`;
      item.querySelector('.lm-stop').addEventListener('click', () =>
        stopLiveChannels([ch.channel_id])
      );
      list.appendChild(item);
    });
    if (!data.channels.length) {
      list.innerHTML = '<div class="live-empty-msg">No channels monitored.<br>Go to Browse → ⚡ to start.</div>';
    }
  }).catch(() => {});
}

function appendLiveMessage(ev) {
  const feed = $('liveFeed');
  // Remove empty placeholder
  const empty = feed.querySelector('.live-feed-empty');
  if (empty) empty.remove();

  const ts = new Date(ev.timestamp).toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit', second: '2-digit' });
  const card = ce('div', 'live-msg');

  let attsHtml = '';
  if (ev.attachments?.length) {
    const links = ev.attachments.map(url => {
      const isImg = /\.(png|jpe?g|gif|webp|avif)(\?|$)/i.test(url);
      return `<a href="${esc(url)}" target="_blank" rel="noopener" class="rc-att">${isImg ? '🖼 image' : '📎 file'}</a>`;
    }).join('');
    attsHtml = `<div class="live-msg-atts">${links}</div>`;
  }

  card.innerHTML =
    `<div class="live-msg-meta">` +
    `<span class="live-msg-author">${esc(ev.author)}</span>` +
    `<span class="live-msg-loc"><span class="srv">${esc(ev.guild)}</span> / #${esc(ev.channel)}</span>` +
    `<span class="live-msg-ts">${ts}</span>` +
    `</div>` +
    `<div class="live-msg-body ${!ev.content ? 'empty' : ''}">${esc(ev.content) || '(no text content)'}</div>` +
    attsHtml;

  feed.appendChild(card);

  // Keep scroll anchored to bottom if user hasn't scrolled up
  const atBottom = feed.scrollHeight - feed.scrollTop - feed.clientHeight < 80;
  if (atBottom) feed.scrollTop = feed.scrollHeight;

  // Cap feed at 200 cards to avoid memory bloat
  while (feed.children.length > 200) feed.removeChild(feed.firstChild);
}

function updateLiveBadge() {
  const badge = $('liveBadge');
  const count = S.liveChannels.size;
  if (count > 0) {
    badge.textContent = count;
    badge.classList.add('visible');
  } else {
    badge.classList.remove('visible');
    // Disconnect SSE when nothing is monitored
    if (S.liveES) {
      S.liveES.close();
      S.liveES = null;
    }
  }
}

// ── ChatML export ──────────────────────────────────────────────

let exportTarget = null; // { channelId, targetId, targetName }

function initExportModal() {
  $('closeExportModal').addEventListener('click', closeExportModal);
  $('runExportBtn').addEventListener('click', runExport);
}

function closeExportModal() {
  $('exportModalBg').classList.remove('visible');
  exportTarget = null;
}

async function openExportModal(dm) {
  exportTarget = { channelId: dm.id, targetId: null, targetName: null };
  $('exportOtherName').value = '';
  $('exportSystemPrompt').value = '';
  $('runExportBtn').disabled = true;
  $('exportParticipants').innerHTML = spinnerHTML();
  $('exportModalBg').classList.add('visible');

  try {
    const participants = await api(`/api/export/participants?channel_id=${encodeURIComponent(dm.id)}`);
    renderExportParticipants(participants);
  } catch {
    $('exportParticipants').innerHTML = '<div class="error-msg">Failed to load participants</div>';
  }
}

function renderExportParticipants(participants) {
  const box = $('exportParticipants');
  box.innerHTML = '';

  if (!participants.length) {
    box.innerHTML = '<div class="placeholder-msg">No scraped messages for this channel yet.</div>';
    return;
  }

  participants.forEach(p => {
    const el = ce('div', 'persona-item');
    el.dataset.id = p.author_id;
    el.innerHTML =
      `<span class="persona-name">${esc(p.author_name)}</span>` +
      `<span class="persona-uid">${esc(p.author_id)}</span>`;
    el.addEventListener('click', () => {
      document.querySelectorAll('.persona-item').forEach(x => x.classList.remove('selected'));
      el.classList.add('selected');
      exportTarget.targetId = p.author_id;
      exportTarget.targetName = p.author_name;
      $('runExportBtn').disabled = false;
    });
    box.appendChild(el);
  });
}

async function runExport() {
  if (!exportTarget?.targetId) return;
  const btn = $('runExportBtn');
  btn.disabled = true;
  btn.textContent = 'exporting…';

  const body = {
    channel_id:     exportTarget.channelId,
    target_id:      exportTarget.targetId,
    other_name:     $('exportOtherName').value.trim() || null,
    system_prompt:  $('exportSystemPrompt').value.trim() || null,
  };

  try {
    // Not using the shared api() helper here: that always parses the
    // response as JSON, but this endpoint returns a raw .jsonl file.
    const r = await fetch('/api/export/chatml', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    });
    if (!r.ok) {
      const err = await r.json().catch(() => ({}));
      throw new Error(err.detail || `HTTP ${r.status}`);
    }
    const blob = await r.blob();
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `chatml_${exportTarget.channelId}.jsonl`;
    document.body.appendChild(a);
    a.click();
    a.remove();
    URL.revokeObjectURL(url);
    closeExportModal();
  } catch (e) {
    alert(`Export failed: ${e.message}`);
  } finally {
    btn.disabled = false;
    btn.textContent = 'export .jsonl';
  }
}

// ── Utilities ────────────────────────────────────────────────
function $(id)           { return document.getElementById(id); }
function ce(tag, cls)    { const el = document.createElement(tag); if (cls) el.className = cls; return el; }
function esc(s)          { return String(s ?? '').replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;'); }
function n(x)            { return Number(x).toLocaleString(); }
function initial(name)   { return (name || '?').replace(/^The\s+/i,'').charAt(0).toUpperCase(); }
function spinnerHTML()   { return '<div class="spinner-wrap"><div class="spinner"></div></div>'; }

// Match on the raw text, then escape each piece — highlighting escaped HTML
// instead would let a query like "amp" or "lt" match inside an entity and
// tear it apart on screen.
function highlight(raw, query) {
  const safe = query.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const re = new RegExp(safe, 'gi');
  let out = '';
  let last = 0;
  for (const m of raw.matchAll(re)) {
    if (m[0] === '') break;  // guard against a zero-width match looping
    out += esc(raw.slice(last, m.index)) + `<mark>${esc(m[0])}</mark>`;
    last = m.index + m[0].length;
  }
  return out + esc(raw.slice(last));
}

async function api(url, opts = {}) {
  const init = { method: opts.method || 'GET', headers: {} };
  if (opts.body) {
    init.headers['Content-Type'] = 'application/json';
    init.body = JSON.stringify(opts.body);
  }
  const r = await fetch(url, init);
  if (!r.ok) {
    const err = await r.json().catch(() => ({}));
    throw new Error(err.detail || `HTTP ${r.status}`);
  }
  return r.json();
}
