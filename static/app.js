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
  await loadSearchFilters();
  const params = new URLSearchParams(location.search);
  for (const [key, id] of Object.entries(searchFields)) $(id).value = params.get(key) || '';
  populateChannelFilter(resolveFilter(S.guildMap, $('fGuild').value));
  const view = location.hash.slice(1) || 'browse';
  if (params.has('search') || [...Object.keys(searchFields)].some(key => params.has(key))) {
    await doSearch(Math.max(1, Number(params.get('page')) || 1), false);
  }
  switchView(view);
}

function initNav() {
  document.querySelectorAll('.tab').forEach(t =>
    t.addEventListener('click', () => switchView(t.dataset.view))
  );
  window.addEventListener('hashchange', () => switchView(location.hash.slice(1), false));
  $('connectDiscord').addEventListener('click', connectDiscord);
}

function switchView(name, updateUrl = true) {
  if (!['browse', 'dms', 'live', 'stats'].includes(name)) name = 'browse';
  document.querySelectorAll('.tab').forEach(t => {
    t.classList.toggle('active', t.dataset.view === name);
    t.setAttribute('aria-pressed', String(t.dataset.view === name));
  });
  document.querySelectorAll('.view').forEach(v => v.classList.toggle('active', v.id === `view-${name}`));
  $('homeOverview').hidden = name !== 'browse';
  if (updateUrl) history.replaceState(null, '', `${location.pathname}${location.search}#${name}`);
  if (name === 'live') connectLiveSSE();
  if (name === 'stats') loadStats();
  if (name === 'dms') loadDms();
}

async function connectDiscord() {
  $('connectDiscord').disabled = true;
  try {
    const v = await api('/api/token/validate');
    if (!v.valid) { openSettings(); setTokenStatus('Paste your Discord token to connect.', 'fail'); return; }
    $('connectionStatus').textContent = `Connected as ${v.username}`;
    await loadGuilds();
  } catch (error) {
    $('connectionStatus').textContent = error.message;
  } finally { $('connectDiscord').disabled = false; }
}

// ── Settings panel ───────────────────────────────────────────
function initSettings() {
  const btn     = $('menuBtn');
  const overlay = $('overlay');
  const panel   = $('settingsPanel');

  btn.addEventListener('click',     () => panel.classList.contains('open') ? closeSettings() : openSettings());
  overlay.addEventListener('click', closeSettings);
  $('closeSettings').addEventListener('click', closeSettings);
  panel.addEventListener('keydown', e => {
    if (e.key === 'Escape') { e.preventDefault(); closeSettings(); }
    if (e.key === 'Tab') {
      const items = [...panel.querySelectorAll('button, input')].filter(el => !el.disabled);
      const first = items[0], last = items.at(-1);
      if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last.focus(); }
      else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
    }
  });

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
    try {
      await api('/api/messages', { method: 'DELETE' });
      searchController?.abort(); ++searchRequest;
      $('searchResults').replaceChildren(); $('resultsStatus').textContent = 'Archive cleared';
      $('pagination').hidden = true;
      await Promise.all([loadDbStats(), loadSearchFilters()]);
    } catch (error) { $('dbStats').textContent = `Could not clear archive: ${error.message}`; }
  });
}

function openSettings() {
  $('menuBtn').classList.add('open');
  $('overlay').classList.add('visible');
  $('settingsPanel').classList.add('open');
  $('settingsPanel').inert = false;
  $('menuBtn').setAttribute('aria-expanded', 'true');
  $('tokenInput').focus();
  loadDbStats();
}
function closeSettings() {
  $('menuBtn').classList.remove('open');
  $('overlay').classList.remove('visible');
  $('settingsPanel').classList.remove('open');
  $('settingsPanel').inert = true;
  $('menuBtn').setAttribute('aria-expanded', 'false');
  $('menuBtn').focus();
}
function setTokenStatus(msg, cls) {
  const el = $('tokenStatus');
  el.textContent = msg;
  el.className = 'token-status' + (cls ? ` ${cls}` : '');
}
async function loadDbStats() {
  try {
    const f = await api('/api/search/filters?include_users=false');
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
    const el = ce('button', 'server-item');
    el.type = 'button';
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

    // DM clear button (trash icon)
    const clearBtn = ce('button', 'ch-btn ch-clear-btn');
    clearBtn.setAttribute('aria-label', 'Delete your messages in this DM');
    clearBtn.title = 'Delete your messages in this DM';
    clearBtn.textContent = '🗑';

    el.append(hash, name, btn, exportBtn, clearBtn);

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
    clearBtn.addEventListener('click', e => {
      e.stopPropagation();
      handleDmClearClick(dm, el, clearBtn);
    });

    body.appendChild(el);
  });
}

// DM Clear state
let dmClearPending = null; // { dm, el, clearBtn, confirmed: boolean }

function handleDmClearClick(dm, el, clearBtn) {
  if (dmClearPending && dmClearPending.dm.id === dm.id && dmClearPending.confirmed) {
    // Already confirmed and running — do nothing (progress modal is open)
    return;
  }

  if (dmClearPending && dmClearPending.dm.id === dm.id && !dmClearPending.confirmed) {
    // Second click = confirm
    dmClearPending.confirmed = true;
    clearBtn.textContent = '⏳';
    clearBtn.disabled = true;
    clearBtn.title = 'Starting…';
    startDmClear(dm);
    return;
  }

  // First click on this DM (or different DM) — show inline confirm
  if (dmClearPending) {
    // Reset previous pending DM's button
    dmClearPending.clearBtn.textContent = '🗑';
    dmClearPending.clearBtn.disabled = false;
    dmClearPending.clearBtn.title = 'Delete your messages in this DM';
  }

  dmClearPending = { dm, el, clearBtn, confirmed: false };
  clearBtn.textContent = '✕';
  clearBtn.title = 'Click again to confirm deletion';
  clearBtn.classList.add('pending-confirm');

  // Auto-cancel after 5 seconds
  setTimeout(() => {
    if (dmClearPending && dmClearPending.dm.id === dm.id && !dmClearPending.confirmed) {
      clearBtn.textContent = '🗑';
      clearBtn.disabled = false;
      clearBtn.title = 'Delete your messages in this DM';
      clearBtn.classList.remove('pending-confirm');
      dmClearPending = null;
    }
  }, 5000);
}

async function startDmClear(dm) {
  const modal = $('modalBg');
  const modalTitle = modal.querySelector('.modal-title');
  modalTitle.textContent = 'deleting your messages';
  modal.classList.add('visible');
  $('modalFoot').classList.remove('visible');
  $('stopScrapeBtn').disabled = false;
  $('moLog').innerHTML = '';
  $('moBar').style.width = '0%';
  $('moStats').textContent = `Deleting your messages in ${esc(dm.name)}…`;
  $('moChannel').textContent = 'Starting…';

  $('scrapePill').classList.add('visible');

  let resp;
  try {
    resp = await api('/api/dm-clear/start', { method: 'POST', body: { channel_id: dm.id } });
  } catch (e) {
    finishDmClear(`Failed to start: ${e.message}`);
    return;
  }

  const { job_id } = resp;
  S.activeJobId = job_id;

  const es = new EventSource(`/api/dm-clear/progress/${job_id}`);

  es.onmessage = e => {
    const ev = JSON.parse(e.data);
    const log = $('moLog');

    switch (ev.type) {
      case 'progress':
        $('moChannel').textContent = `Deleted ${n(ev.deleted)} messages…`;
        $('moStats').textContent = `${n(ev.deleted)} deleted · ${n(ev.scanned)} scanned`;
        break;

      case 'warning':
        logLine(log, `⚠ ${ev.message}`, 'err');
        break;

      case 'complete':
        es.close();
        $('moBar').style.width = '100%';
        $('moChannel').textContent = 'Done.';
        $('moStats').textContent = `Deleted ${n(ev.deleted)} messages`;
        logLine(log, `✓ Complete — ${n(ev.deleted)} messages deleted`, 'ok');
        finishDmClear();
        break;

      case 'cancelled':
        es.close();
        $('moChannel').textContent = 'Stopped.';
        $('moStats').textContent = `Deleted ${n(ev.deleted)} messages before stopping`;
        logLine(log, `⏹ Stopped — ${n(ev.deleted)} messages deleted`, 'ok');
        finishDmClear();
        break;

      case 'error':
        es.close();
        logLine(log, `✗ ${ev.message}`, 'err');
        finishDmClear(ev.message);
        break;
    }
  };

  es.onerror = () => {
    es.close();
    finishDmClear('Connection lost');
  };
}

function finishDmClear(errMsg) {
  $('modalFoot').classList.add('visible');
  $('stopScrapeBtn').disabled = true;
  $('scrapePill').classList.remove('visible');
  S.activeJobId = null;

  // Reset the DM clear button
  if (dmClearPending) {
    dmClearPending.clearBtn.textContent = '🗑';
    dmClearPending.clearBtn.disabled = false;
    dmClearPending.clearBtn.title = 'Delete your messages in this DM';
    dmClearPending.clearBtn.classList.remove('pending-confirm');
    dmClearPending = null;
  }

  if (errMsg) logLine($('moLog'), `Error: ${errMsg}`, 'err');
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
    resp = await api('/api/scrape/start', { method: 'POST', body: {
      channels: S.queue, limit, harvest_profiles: $('harvestProfiles').checked,
    } });
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

      case 'profiles':
        logLine(log, `✓ #${ev.channel}: ${n(ev.saved)} new profiles`, 'ok');
        break;

      case 'profile_progress':
        $('moStats').textContent = `${n(ev.saved)} new profiles saved`;
        break;

      case 'profile_warning':
        logLine(log, `⚠ ${ev.message}`, 'err');
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
  loadSearchFilters();
}

function initModal() {
  $('dismissModal').addEventListener('click', () => $('modalBg').classList.remove('visible'));
  $('stopScrapeBtn').addEventListener('click', async () => {
    if (!S.activeJobId) return;
    $('stopScrapeBtn').disabled = true;
    $('moChannel').textContent = 'Stopping…';
    try {
      // The same modal is reused for both scrape and DM clear jobs.
      // Check if it's a DM clear job by seeing if the job exists in delete_jobs (can't from here).
      // We'll just try both - one will 404, the other will succeed.
      await api(`/api/scrape/${S.activeJobId}/stop`, { method: 'POST' });
    } catch {
      try {
        await api(`/api/dm-clear/${S.activeJobId}/stop`, { method: 'POST' });
      } catch { /* SSE will still fire cancelled event */ }
    }
  });
}

function logLine(container, text, cls = '') {
  const d = ce('div', cls ? `log-${cls}` : '');
  d.textContent = text;
  container.appendChild(d);
  container.scrollTop = container.scrollHeight;
}

// ── Search ────────────────────────────────────────────────────
const searchFields = {q: 'searchInput', guild_id: 'fGuild', channel_id: 'fChannel', author_id: 'fUser', date_from: 'fDateFrom', date_to: 'fDateTo'};
let searchController;
let searchRequest = 0;
let submittedSearch = new URLSearchParams();

function setFilterTrayExpanded(expanded) {
  $('queryBlock').classList.toggle('is-expanded', expanded);
  $('searchInput').setAttribute('aria-expanded', String(expanded));
  $('filterTray').inert = !expanded;
}

function initSearch() {
  $('queryForm').addEventListener('submit', e => { e.preventDefault(); doSearch(1, true); });
  $('filterForm').addEventListener('submit', e => { e.preventDefault(); doSearch(1, true); });
  $('searchInput').addEventListener('focus', () => setFilterTrayExpanded(true));
  $('searchInput').addEventListener('click', () => setFilterTrayExpanded(true));
  $('queryBlock').addEventListener('keydown', e => {
    if (e.key === 'Escape') { $('searchInput').focus(); setFilterTrayExpanded(false); }
  });
  document.addEventListener('pointerdown', e => {
    if (!$('queryBlock').contains(e.target)) setFilterTrayExpanded(false);
  });
  $('queryBlock').addEventListener('focusout', e => {
    if (e.relatedTarget && !$('queryBlock').contains(e.relatedTarget)) setFilterTrayExpanded(false);
  });
  $('filterForm').addEventListener('reset', () => {
    queueMicrotask(() => {
      document.querySelectorAll('.filter-form input').forEach(el => { el.setCustomValidity(''); el.removeAttribute('aria-invalid'); });
      populateChannelFilter('');
      doSearch(1);
    });
  });
  document.querySelectorAll('.filter-form input').forEach(input => {
    input.addEventListener('input', () => { input.setCustomValidity(''); input.removeAttribute('aria-invalid'); });
    input.addEventListener('change', () => {
      if (input.id === 'fGuild') {
        $('fChannel').value = '';
        populateChannelFilter(resolveFilter(S.guildMap, input.value));
      }
      doSearch(1);
    });
  });
  let authorTimer;
  let authorRequest = 0;
  $('fUser').addEventListener('input', e => {
    clearTimeout(authorTimer);
    const query = e.target.value.trim();
    const request = ++authorRequest;
    if (query.length < 2 || S.userMap.has(query) || /^\d{1,20}$/.test(query)) return;
    authorTimer = setTimeout(async () => {
      try {
        const users = await api(`/api/search/authors?q=${encodeURIComponent(query)}`);
        if (request !== authorRequest || $('fUser').value.trim() !== query) return;
        const dl = $('dl-users');
        dl.replaceChildren();
        S.userMap.clear();
        users.forEach(u => {
          const display = `${u.author_name} · ${u.author_id}`;
          S.userMap.set(display, u.author_id);
          const option = document.createElement('option');
          option.value = display;
          dl.appendChild(option);
        });
      } catch { /* Raw IDs remain usable when suggestions are unavailable. */ }
    }, 150);
  });
}

async function loadSearchFilters() {
  try {
    const f = await api('/api/search/filters?include_users=false');
    S.filterChans = f.channels;
    S.guildMap.clear();
    $('dl-guilds').replaceChildren();
    f.guilds.forEach(g => {
      const display = `${g.guild_name} · ${g.guild_id}`;
      S.guildMap.set(display, g.guild_id);
      const option = document.createElement('option');
      option.value = display;
      $('dl-guilds').appendChild(option);
    });
    const compact = value => Intl.NumberFormat(undefined, {notation: 'compact', maximumFractionDigits: 1}).format(value);
    $('homeMessages').textContent = compact(f.total_messages);
    $('homeMessages').title = n(f.total_messages);
    $('homeServers').textContent = compact(f.guilds.length);
    $('homeServers').title = n(f.guilds.length);
    populateChannelFilter(resolveFilter(S.guildMap, $('fGuild').value));
    $('connectionStatus').textContent = 'Local archive ready';
  } catch {
    $('connectionStatus').textContent = 'Archive unavailable. Check the server, then reload.';
    $('homeMessages').textContent = '—'; $('homeServers').textContent = '—';
  }
}

function populateChannelFilter(guildId) {
  const dl = $('dl-channels');
  dl.replaceChildren();
  S.channelMap.clear();
  const list = guildId ? S.filterChans.filter(c => c.guild_id === guildId) : S.filterChans;
  list.forEach(c => {
    const display = `#${c.channel_name} · ${c.channel_id}`;
    S.channelMap.set(display, c.channel_id);
    const option = document.createElement('option');
    option.value = display;
    dl.appendChild(option);
  });
}

function resolveFilter(map, rawValue) {
  const value = rawValue.trim();
  return map.get(value) || (/^\d{1,20}$/.test(value) ? value : '');
}

async function doSearch(page, shouldScroll = false, reuseSubmitted = false) {
  searchController?.abort();
  const request = ++searchRequest;
  $('searchResults').removeAttribute('aria-busy');
  const params = reuseSubmitted ? new URLSearchParams(submittedSearch) : new URLSearchParams();
  $('pagination').hidden = true;
  if (!reuseSubmitted) {
    for (const [key, id] of Object.entries(searchFields)) {
      const input = $(id);
      input.setCustomValidity(''); input.removeAttribute('aria-invalid');
      let value = input.value.trim();
      const map = {guild_id: S.guildMap, channel_id: S.channelMap, author_id: S.userMap}[key];
      if (map && value) {
        value = resolveFilter(map, value);
        if (!value) {
          input.setCustomValidity('Choose an indexed suggestion or enter a Discord ID.');
          input.setAttribute('aria-invalid', 'true');
          setFilterTrayExpanded(true);
          $('resultsSection').hidden = false;
          $('resultsStatus').textContent = 'Choose a suggestion or enter an ID for each filter.';
          $('searchResults').replaceChildren();
          input.focus();
          return;
        }
      }
      if (value) params.set(key, value);
    }
    if ($('fDateFrom').value && $('fDateTo').value && $('fDateFrom').value > $('fDateTo').value) {
      setFilterTrayExpanded(true);
      $('fDateTo').setCustomValidity('To date must be on or after the from date.');
      $('fDateTo').setAttribute('aria-invalid', 'true');
      $('fDateTo').reportValidity();
      $('resultsSection').hidden = false;
      $('resultsStatus').textContent = 'To date must be on or after the from date.';
      $('searchResults').replaceChildren();
      return;
    }
  }
  params.set('page', page); params.set('limit', 50);
  submittedSearch = new URLSearchParams(params);
  S.searchPage = page;
  switchView('browse');
  $('resultsSection').hidden = false;
  $('resultsStatus').textContent = 'Searching…';
  $('searchResults').setAttribute('aria-busy', 'true');
  $('searchResults').innerHTML = spinnerHTML();
  const url = new URLSearchParams(params); url.delete('limit'); url.set('search', '1');
  history.replaceState(null, '', `${location.pathname}?${url}#browse`);
  searchController = new AbortController();
  try {
    const data = await api(`/api/search?${params}`, {signal: searchController.signal});
    if (request !== searchRequest) return;
    renderResults(data, params.get('q') || '');
    if (shouldScroll) $('resultsSection').scrollIntoView({behavior: matchMedia('(prefers-reduced-motion: reduce)').matches ? 'instant' : 'smooth', block: 'start'});
  } catch (error) {
    if (error.name === 'AbortError' || request !== searchRequest) return;
    $('resultsStatus').textContent = `Search failed: ${error.message}`;
    $('searchResults').innerHTML = '<p class="no-results">Search is unavailable. Submit again to retry.</p>';
  } finally {
    if (request === searchRequest) $('searchResults').removeAttribute('aria-busy');
  }
}

function renderResults(data, query) {
  const results = $('searchResults');
  results.replaceChildren();
  $('resultsStatus').textContent = data.total ? `${n((data.page - 1) * data.limit + 1)}–${n(Math.min(data.page * data.limit, data.total))} of ${n(data.total)} messages` : 'No messages match these filters';
  if (!data.messages.length) results.innerHTML = '<p class="no-results">No results found. Try another query or clear the filters.</p>';
  data.messages.forEach(msg => {
    const row = ce('article', 'result-row');
    let hash = 0;
    for (const char of String(msg.author_id)) hash = (hash * 31 + char.charCodeAt(0)) >>> 0;
    const avatar = ce('div', `message-avatar avatar-${'abcdef'[hash % 6]}`);
    avatar.setAttribute('aria-hidden', 'true');
    const fallback = () => { avatar.innerHTML = '<span class="shape-one"></span><span class="shape-two"></span><span class="shape-three"></span><span class="shape-four"></span>'; };
    fallback();
    if (msg.avatar_url) {
      const img = new Image(); img.alt = ''; img.loading = 'lazy'; img.referrerPolicy = 'no-referrer';
      img.onload = () => avatar.replaceChildren(img);
      img.onerror = fallback; img.src = msg.avatar_url;
    }
    const content = ce('div');
    const text = ce('p', 'result-copy');
    text.innerHTML = msg.content ? (query ? highlight(msg.content, query) : esc(msg.content)) : '(no text content)';
    content.appendChild(text);
    if (msg.attachments?.length) {
      const attachments = ce('div', 'rc-attachments');
      msg.attachments.forEach((url, i) => {
        const link = ce('a', 'rc-att'); link.href = url; link.target = '_blank'; link.rel = 'noopener noreferrer'; link.textContent = `image ${i + 1}`;
        attachments.appendChild(link);
      });
      content.appendChild(attachments);
    }
    const meta = ce('div', 'result-meta');
    meta.innerHTML = `<strong>${esc(msg.author_name)}</strong><span>${esc(msg.guild_name || 'Direct messages')} · #${esc(msg.channel_name)}</span><span>id: ${esc(msg.author_id)}</span>`;
    const time = ce('time'); time.dateTime = msg.timestamp;
    time.textContent = new Date(msg.timestamp).toLocaleString(undefined, {dateStyle: 'medium', timeStyle: 'short'});
    meta.appendChild(time);
    row.append(avatar, content, meta); results.appendChild(row);
  });
  renderPagination(data.page, data.pages);
}

function renderPagination(page, pages) {
  const pagination = $('pagination');
  pagination.replaceChildren(); pagination.hidden = pages <= 1;
  for (const [label, target, disabled] of [['previous', page - 1, page === 1], ['next', page + 1, page >= pages]]) {
    const button = ce('button', 'page-control'); button.type = 'button'; button.textContent = label; button.disabled = disabled;
    button.addEventListener('click', () => doSearch(target, true, true));
    if (label === 'next') {
      const readout = ce('div', 'page-readout');
      readout.innerHTML = `<span class="page-current">page ${n(page)}</span><span class="page-rule" aria-hidden="true"></span><span>${n(pages)} pages</span>`;
      pagination.appendChild(readout);
    }
    pagination.appendChild(button);
  }
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
    const [d] = await Promise.all([api('/api/stats'), loadChartLibrary()]);
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

let chartLibraryPromise;
function loadChartLibrary() {
  if (typeof Chart !== 'undefined') return Promise.resolve();
  if (!chartLibraryPromise) chartLibraryPromise = new Promise(resolve => {
    const script = document.createElement('script');
    script.src = 'https://cdn.jsdelivr.net/npm/chart.js@4.4.0/dist/chart.umd.min.js';
    script.integrity = 'sha256-DiMmxoaAcr7BWSdgxnKQQ8ruopYKK0bO5qIZKqxqv/A=';
    script.crossOrigin = 'anonymous'; script.referrerPolicy = 'no-referrer';
    script.onload = () => resolve();
    script.onerror = () => { chartLibraryPromise = null; script.remove(); resolve(); };
    document.head.appendChild(script);
  });
  return chartLibraryPromise;
}

function renderChart(id, type, data, extraOpts = {}) {
  if (typeof Chart === 'undefined') {
    $(id).hidden = true;
    const box = $(id).parentElement;
    if (!box.querySelector('.chart-error')) {
      const note = ce('p', 'chart-error');
      note.textContent = 'Chart library unavailable. Reconnect and refresh to load charts.';
      box.appendChild(note);
    }
    return;
  }
  $(id).hidden = false;
  $(id).parentElement.querySelector('.chart-error')?.remove();
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
    const links = ev.attachments.map(url =>
      `<a href="${esc(url)}" target="_blank" rel="noopener" class="rc-att">🖼 image</a>`
    ).join('');
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
  const init = { method: opts.method || 'GET', headers: {}, signal: opts.signal };
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
