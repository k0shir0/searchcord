'use strict';

// ── State ────────────────────────────────────────────────────
const S = {
  guilds:        [],
  guild:         null,
  queue:         [],
  scraping:      false,
  activeJobId:   null,
  activeJobKind: null,
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
  initCounterSizing();
  initDialogFocus();
  boot();
});

function initCounterSizing() {
  const fit = () => document.querySelectorAll('.index-number').forEach(number => {
    if (!number.clientWidth) return;
    number.style.fontSize = '';
    const available = number.parentElement.clientWidth - 2 * parseFloat(getComputedStyle(number.parentElement).paddingLeft);
    if (number.scrollWidth > available) number.style.fontSize = `${parseFloat(getComputedStyle(number).fontSize) * available / number.scrollWidth}px`;
  });
  const observer = new ResizeObserver(() => requestAnimationFrame(fit));
  document.querySelectorAll('.index-stat').forEach(panel => observer.observe(panel));
  new MutationObserver(fit).observe($('homeMessages'), {childList:true});
  new MutationObserver(fit).observe($('homeServers'), {childList:true});
}

function initDialogFocus() {
  for (const id of ['modalBg', 'exportModalBg']) {
    const overlay = $(id), dialog = overlay.querySelector('.modal');
    dialog.setAttribute('role', 'dialog'); dialog.setAttribute('aria-modal', 'true'); dialog.tabIndex = -1;
    dialog.setAttribute('aria-label', id === 'modalBg' ? 'Collection progress' : 'Export conversation');
    let previousFocus;
    new MutationObserver(() => {
      if (overlay.classList.contains('visible')) {
        previousFocus = document.activeElement;
        (dialog.querySelector('button:not(:disabled)') || dialog).focus();
      } else if (previousFocus?.isConnected) previousFocus.focus();
    }).observe(overlay, {attributes:true, attributeFilter:['class']});
    dialog.addEventListener('keydown', event => {
      if (event.key === 'Escape') { overlay.classList.remove('visible'); return; }
      if (event.key !== 'Tab') return;
      const targets = [...dialog.querySelectorAll('button,input,textarea')].filter(el => !el.disabled && el.getClientRects().length);
      if (!targets.length) { event.preventDefault(); dialog.focus(); return; }
      if (event.shiftKey && document.activeElement === targets[0]) { event.preventDefault(); targets.at(-1).focus(); }
      else if (!event.shiftKey && document.activeElement === targets.at(-1)) { event.preventDefault(); targets[0].focus(); }
    });
  }
}

async function boot() {
  await loadSearchFilters();
  const params = new URLSearchParams(location.search);
  for (const [key, id] of Object.entries(searchFields)) $(id).value = params.get(key) || '';
  populateChannelFilter(resolveFilter(S.guildMap, $('fGuild').value));
  const view = location.hash.slice(1) || 'browse';
  if (params.has('search') || [...Object.keys(searchFields)].some(key => params.has(key))) {
    await doSearch(Math.max(1, Number(params.get('page')) || 1), false);
  }
  await switchView(view, true, false);
}

function initNav() {
  document.querySelectorAll('.tab').forEach(t =>
    t.addEventListener('click', () => switchView(t.dataset.view))
  );
  window.addEventListener('hashchange', () => switchView(location.hash.slice(1), false));
  $('connectDiscord').addEventListener('click', connectDiscord);
  $('collectionSearch').addEventListener('input', filterCollection);
  document.querySelectorAll('[data-source]').forEach(button => {
    button.addEventListener('click', () => switchSource(button.dataset.source));
    button.addEventListener('keydown', event => {
      if (!['ArrowLeft','ArrowRight','Home','End'].includes(event.key)) return;
      event.preventDefault();
      const source = event.key === 'Home' ? 'channels' : event.key === 'End' ? 'dms' : button.dataset.source === 'channels' ? 'dms' : 'channels';
      switchSource(source); document.querySelector(`[data-source="${source}"]`).focus();
    });
  });
  $('showScrapeProgress').addEventListener('click', () => $('modalBg').classList.add('visible'));
}

let activeView = 'browse';
let navigationVersion = 0;
let viewTransition;
async function switchView(name, updateUrl = true, animate = true) {
  const source = name === 'dms' ? 'dms' : null;
  if (name === 'dms' || name === 'live') name = 'scrape';
  if (!['browse', 'scrape', 'stats'].includes(name)) name = 'browse';
  const version = ++navigationVersion;
  const changed = activeView !== name;
  const apply = () => {
    if (version !== navigationVersion) return;
    activeView = name;
    document.querySelectorAll('.tab').forEach(t => {
      t.classList.toggle('active', t.dataset.view === name);
      t.setAttribute('aria-pressed', String(t.dataset.view === name));
    });
    document.querySelectorAll('.view').forEach(v => v.classList.toggle('active', v.id === `view-${name}`));
    $('homeOverview').hidden = name !== 'browse';
    if (updateUrl) history.replaceState(null, '', `${location.pathname}${location.search}#${name}`);
    if (name === 'stats') loadStats();
    if (name === 'scrape') {
      refreshLiveStatus();
      if (source) switchSource(source);
    }
  };
  viewTransition?.skipTransition();
  if (changed && animate && !matchMedia('(prefers-reduced-motion: reduce)').matches) {
    if (document.startViewTransition) {
      viewTransition = document.startViewTransition(apply);
      // Superseded navigation rejects ready even when finished resolves.
      viewTransition.ready.catch(() => {});
      await viewTransition.finished.catch(() => {});
    } else {
      const stage = $('pageStage');
      stage.getAnimations().forEach(animation => animation.cancel());
      await stage.animate([{opacity:1},{opacity:0}], {duration:100,fill:'forwards'}).finished.catch(() => {});
      if (version !== navigationVersion) return;
      apply();
      stage.getAnimations().forEach(animation => animation.cancel());
      if (version === navigationVersion) stage.animate([{opacity:0,transform:'translateY(8px)'},{opacity:1,transform:'none'}], {duration:220,easing:'ease-out'});
    }
  } else apply();
}

function switchSource(source) {
  document.querySelectorAll('[data-source]').forEach(button => {
    const selected = button.dataset.source === source;
    button.setAttribute('aria-selected', String(selected));
    button.tabIndex = selected ? 0 : -1;
    $('source-' + button.dataset.source).hidden = !selected;
  });
  if (source === 'dms') loadDms();
  filterCollection();
}

function filterCollection() {
  const query = $('collectionSearch').value.trim().toLocaleLowerCase();
  document.querySelectorAll('#cpBody .ch-item, #dmBody .ch-item').forEach(row => {
    row.hidden = !row.querySelector('.ch-name').textContent.toLocaleLowerCase().includes(query);
  });
}

async function refreshLiveStatus() {
  try {
    const data = await api('/api/live/status');
    S.liveChannels = new Set(data.channels.map(channel => channel.channel_id));
    updateLiveBadge(); renderMonitorList();
    if (S.liveChannels.size) connectLiveSSE();
  } catch (error) { $('liveMonitorList').textContent = `Could not load monitors: ${error.message}`; }
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
    $('saveToken').disabled = true;
    try {
      await api('/api/settings', { method: 'POST', body: { token } });
      const v = await api('/api/token/validate');
      if (v.valid) {
        setTokenStatus(`connected as ${v.username}`, 'ok');
        $('connectionStatus').textContent = `Connected as ${v.username}`;
        inp.value = '';
        setTimeout(() => { closeSettings(); loadGuilds(); }, 600);
      } else setTokenStatus('Invalid token. Check it and try again.', 'fail');
    } catch (error) { setTokenStatus(`Connection failed: ${error.message}`, 'fail'); }
    finally { $('saveToken').disabled = false; }
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
    if (S.guild.id !== guild.id) return;
    renderChannels(channels, guild);
  } catch {
    if (S.guild.id !== guild.id) return;
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
    hash.textContent = dm.type === 3 ? 'group' : '@';

    const name = ce('div', 'ch-name');
    name.textContent = dm.name;

    const btn = ce('button', 'ch-btn');
    btn.setAttribute('aria-label', 'Toggle scrape queue');
    btn.textContent = el.classList.contains('queued') ? 'queued' : 'queue';

    const exportBtn = ce('button', 'ch-btn');
    exportBtn.setAttribute('aria-label', 'Export to ChatML JSONL');
    exportBtn.title = 'Export to ChatML JSONL';
    exportBtn.textContent = 'export';

    // DM deletion has a separate confirmation step.
    const clearBtn = ce('button', 'ch-btn ch-clear-btn');
    clearBtn.setAttribute('aria-label', 'Delete your messages in this DM');
    clearBtn.title = 'Delete your messages in this DM';
    clearBtn.textContent = 'delete';

    el.append(hash, name, btn, exportBtn, clearBtn);

    // DMs share the channel queue but have no guild ID.
    btn.addEventListener('click', e => {
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
  filterCollection();
  renderQueue();
}

// DM Clear state
let dmClearPending = null; // { dm, el, clearBtn, confirmed: boolean }

function handleDmClearClick(dm, el, clearBtn) {
  if (S.activeJobKind) { $('modalBg').classList.add('visible'); return; }
  if (dmClearPending && dmClearPending.dm.id === dm.id && dmClearPending.confirmed) {
    // Already confirmed and running — do nothing (progress modal is open)
    return;
  }

  if (dmClearPending && dmClearPending.dm.id === dm.id && !dmClearPending.confirmed) {
    // Second click = confirm
    dmClearPending.confirmed = true;
    clearBtn.textContent = 'starting';
    clearBtn.disabled = true;
    clearBtn.title = 'Starting…';
    startDmClear(dm);
    return;
  }

  // First click on this DM (or different DM) — show inline confirm
  if (dmClearPending) {
    // Reset previous pending DM's button
    dmClearPending.clearBtn.textContent = 'delete';
    dmClearPending.clearBtn.disabled = false;
    dmClearPending.clearBtn.title = 'Delete your messages in this DM';
    dmClearPending.clearBtn.classList.remove('pending-confirm');
  }

  dmClearPending = { dm, el, clearBtn, confirmed: false };
  clearBtn.textContent = 'confirm';
  clearBtn.title = 'Click again to confirm deletion';
  clearBtn.classList.add('pending-confirm');

  // Auto-cancel after 5 seconds
  setTimeout(() => {
    if (dmClearPending && dmClearPending.dm.id === dm.id && !dmClearPending.confirmed) {
      clearBtn.textContent = 'delete';
      clearBtn.disabled = false;
      clearBtn.title = 'Delete your messages in this DM';
      clearBtn.classList.remove('pending-confirm');
      dmClearPending = null;
    }
  }, 5000);
}

async function startDmClear(dm) {
  S.activeJobKind = 'dm-clear';
  renderQueue();
  const modal = $('modalBg');
  const modalTitle = modal.querySelector('.modal-title');
  modalTitle.textContent = 'deleting your messages';
  modal.classList.add('visible');
  $('modalFoot').classList.remove('visible');
  $('stopScrapeBtn').disabled = false;
  $('moLog').innerHTML = '';
  $('moBar').style.width = '0%';
  $('moStats').textContent = `Deleting your messages in ${dm.name}…`;
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
        logLine(log, `Notice: ${ev.message}`, 'err');
        break;

      case 'complete':
        es.close();
        $('moBar').style.width = '100%';
        $('moChannel').textContent = 'Done.';
        $('moStats').textContent = `Deleted ${n(ev.deleted)} messages`;
        logLine(log, `Complete — ${n(ev.deleted)} messages deleted`, 'ok');
        finishDmClear();
        break;

      case 'cancelled':
        es.close();
        $('moChannel').textContent = 'Stopped.';
        $('moStats').textContent = `Deleted ${n(ev.deleted)} messages before stopping`;
        logLine(log, `Stopped — ${n(ev.deleted)} messages deleted`, 'ok');
        finishDmClear();
        break;

      case 'error':
        es.close();
        logLine(log, `${ev.message}`, 'err');
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
  S.activeJobKind = null;
  renderQueue();

  // Reset the DM clear button
  if (dmClearPending) {
    dmClearPending.clearBtn.textContent = 'delete';
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
    hash.textContent = '#';

    const name = ce('div', 'ch-name');
    name.textContent = ch.name;

    // live pulsing dot (only visible when .live)
    const dot = ce('div', 'ch-live-dot');

    // scrape queue toggle
    const btn = ce('button', 'ch-btn');
    btn.setAttribute('aria-label', 'Toggle scrape queue');
    btn.textContent = el.classList.contains('queued') ? 'queued' : 'queue';

    // live monitor toggle
    const liveBtn = ce('button', 'ch-btn ch-live-btn');
    liveBtn.setAttribute('aria-label', 'Toggle live monitor');
    liveBtn.textContent = S.liveChannels.has(ch.id) ? 'stop monitor' : 'monitor';
    liveBtn.title = S.liveChannels.has(ch.id) ? 'Stop live monitor' : 'Start live monitor';

    el.append(hash, name, dot, btn, liveBtn);

    btn.addEventListener('click', e => {
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
  filterCollection();
  renderQueue();
}

// ── Scrape queue ─────────────────────────────────────────────
function toggleQueue(ch, guild, rowEl, btnEl) {
  if (S.scraping) return;
  const idx = S.queue.findIndex(q => q.id === ch.id);
  if (idx > -1) {
    S.queue.splice(idx, 1);
    rowEl.classList.remove('queued');
    btnEl.textContent = 'queue';
  } else {
    S.queue.push({ id: ch.id, name: ch.name, guild_id: guild.id, guild_name: guild.name });
    rowEl.classList.add('queued');
    btnEl.textContent = 'queued';
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
      if (b) b.textContent = 'queue';
    });
  });
  $('startScrape').addEventListener('click', startScraping);
  renderQueue();
}

function renderQueue() {
  const bar   = $('queueBar');
  const chips = $('queueChips');
  $('queueCount').textContent = S.queue.length;
  document.querySelectorAll('.ch-item').forEach(row => {
    const queued = S.queue.some(ch => ch.id === row.dataset.id);
    row.classList.toggle('queued', queued);
    const button = row.querySelector('[aria-label="Toggle scrape queue"]');
    if (button) { button.textContent = queued ? 'queued' : 'queue'; button.setAttribute('aria-pressed', String(queued)); button.disabled = S.scraping; }
  });
  $('clearQueue').disabled = S.scraping;
  $('scrapeLimit').disabled = S.scraping;
  $('harvestProfiles').disabled = S.scraping;
  $('startScrape').disabled = !!S.activeJobKind || S.queue.length === 0;
  if (S.queue.length === 0) {
    bar.classList.remove('visible');
    chips.innerHTML = '<p class="queue-empty">Select channels or conversations to build a queue.</p>';
    return;
  }
  bar.classList.add('visible');

  chips.innerHTML = '';
  S.queue.forEach(ch => {
    const chip = ce('div', 'qb-chip');
    chip.innerHTML =
      `<span>#</span><span>${esc(ch.name)}</span>` +
      `<span class="qb-chip-server">${esc(ch.guild_name)}</span>` +
      `<button class="qb-chip-x" data-id="${ch.id}" ${S.scraping ? 'disabled' : ''}>remove</button>`;
    chip.querySelector('.qb-chip-x').addEventListener('click', () => {
      S.queue = S.queue.filter(q => q.id !== ch.id);
      renderQueue();
      const row = document.querySelector(`.ch-item[data-id="${ch.id}"]`);
      if (row) { row.classList.remove('queued'); const b = row.querySelector('.ch-btn'); if (b) b.textContent = 'queue'; }
    });
    chips.appendChild(chip);
  });
}

// ── Scraping ─────────────────────────────────────────────────
async function startScraping() {
  if (!S.queue.length || S.activeJobKind) return;
  S.scraping = true;
  S.activeJobKind = 'scrape';
  renderQueue();

  const limitVal = parseInt($('scrapeLimit').value, 10);
  const limit = (!isNaN(limitVal) && limitVal > 0) ? limitVal : 0;

  // Show modal
  const modal = $('modalBg');
  modal.querySelector('.modal-title').textContent = 'scraping in progress';
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
        logLine(log, `Saved #${ev.channel}: ${n(ev.messages)} msgs`, 'ok');
        break;

      case 'profiles':
        logLine(log, `Saved #${ev.channel}: ${n(ev.saved)} new profiles`, 'ok');
        break;

      case 'profile_progress':
        $('moStats').textContent = `${n(ev.saved)} new profiles saved`;
        break;

      case 'profile_warning':
        logLine(log, `Notice: ${ev.message}`, 'err');
        break;

      case 'channel_error':
        done++;
        logLine(log, `#${ev.channel}: ${ev.message}`, 'err');
        break;

      case 'complete':
        es.close();
        $('moBar').style.width = '100%';
        $('moChannel').textContent = 'Done.';
        $('moStats').textContent =
          `${n(ev.total_messages)} messages scraped across ${ev.channels} channels`;
        logLine(log, `Complete — ${n(ev.total_messages)} total`, 'ok');
        finishScrape();
        break;

      case 'cancelled':
        es.close();
        $('moChannel').textContent = 'Stopped.';
        $('moStats').textContent = `Scraped ${n(ev.total_messages)} messages before stopping`;
        logLine(log, `Stopped — ${n(ev.total_messages)} msgs saved`, 'ok');
        finishScrape();
        break;

      case 'error':
        es.close();
        logLine(log, `${ev.message}`, 'err');
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
  S.activeJobKind = null;
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
      await api(`/api/${S.activeJobKind}/${S.activeJobId}/stop`, { method: 'POST' });
    } catch (error) {
      $('stopScrapeBtn').disabled = false;
      $('moChannel').textContent = `Could not stop: ${error.message}. Try again.`;
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
    $('homeMessages').textContent = n(f.total_messages);
    $('homeMessages').title = n(f.total_messages);
    $('homeServers').textContent = n(f.guilds.length);
    $('homeServers').title = n(f.guilds.length);
    populateChannelFilter(resolveFilter(S.guildMap, $('fGuild').value));
    document.body.dataset.archiveReady = 'true';
    $('archiveStatus').hidden = true;
  } catch {
    $('archiveStatus').textContent = 'Archive unavailable. Check the server, then reload.';
    $('archiveStatus').hidden = false;
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
  $('resultsSection').hidden = false;
  $('resultsStatus').textContent = 'Searching…';
  $('searchResults').setAttribute('aria-busy', 'true');
  $('searchResults').innerHTML = spinnerHTML();
  await switchView('browse');
  if (request !== searchRequest) return;
  if (activeView !== 'browse') { $('searchResults').removeAttribute('aria-busy'); return; }
  const url = new URLSearchParams(params); url.delete('limit'); url.set('search', '1');
  history.replaceState(null, '', `${location.pathname}?${url}#browse`);
  searchController = new AbortController();
  try {
    const data = await api(`/api/search?${params}`, {signal: searchController.signal});
    if (request !== searchRequest) return;
    renderResults(data, params.get('q') || '');
    if (shouldScroll && activeView === 'browse') $('resultsSection').scrollIntoView({behavior: matchMedia('(prefers-reduced-motion: reduce)').matches ? 'instant' : 'smooth', block: 'start'});
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
  btnEl.disabled = true;
  if (S.liveChannels.has(ch.id)) {
    await stopLiveChannels([ch.id]);
  } else {
    await startLiveChannels([{ id: ch.id, name: ch.name, guild_id: guild.id, guild_name: guild.name }]);
  }
  btnEl.disabled = false;
}

async function startLiveChannels(channels) {
  try {
    await api('/api/live/start', { method: 'POST', body: { channels } });
    channels.forEach(ch => S.liveChannels.add(ch.id));
    updateLiveBadge();
    renderMonitorList();
    connectLiveSSE();
    // Update any visible channel rows
    channels.forEach(ch => {
      const row = document.querySelector(`.ch-item[data-id="${ch.id}"]`);
      if (row) {
        row.classList.add('live');
        const lb = row.querySelector('.ch-live-btn');
        if (lb) { lb.title = 'Stop live monitor'; lb.textContent = 'stop monitor'; }
      }
    });
  } catch (e) { $('connectionStatus').textContent = `Could not start monitor: ${e.message}`; }
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
        if (lb) { lb.title = 'Start live monitor'; lb.textContent = 'monitor'; }
      }
    });
    updateLiveBadge();
    renderMonitorList();
  } catch (e) { $('connectionStatus').textContent = `Could not stop monitor: ${e.message}`; }
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
    list.innerHTML = '<div class="live-empty-msg">No channels monitored. Use the monitor button beside a channel to start.</div>';
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
        `<button class="lm-stop" title="Stop monitoring" data-cid="${ch.channel_id}">stop</button>`;
      item.querySelector('.lm-stop').addEventListener('click', () =>
        stopLiveChannels([ch.channel_id])
      );
      list.appendChild(item);
    });
    if (!data.channels.length) {
      list.innerHTML = '<div class="live-empty-msg">No channels monitored. Use the monitor button beside a channel to start.</div>';
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
      `<a href="${esc(url)}" target="_blank" rel="noopener" class="rc-att">image</a>`
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
    const el = ce('button', 'persona-item');
    el.type = 'button';
    el.setAttribute('aria-pressed', 'false');
    el.dataset.id = p.author_id;
    el.innerHTML =
      `<span class="persona-name">${esc(p.author_name)}</span>` +
      `<span class="persona-uid">${esc(p.author_id)}</span>`;
    el.addEventListener('click', () => {
      document.querySelectorAll('.persona-item').forEach(x => { x.classList.remove('selected'); x.setAttribute('aria-pressed', 'false'); });
      el.classList.add('selected');
      el.setAttribute('aria-pressed', 'true');
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
