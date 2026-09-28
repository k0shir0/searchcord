'use strict';
const $ = selector => document.querySelector(selector);
const form = $('#searchForm');
const searchInput = $('#searchInput');
const filters = [...document.querySelectorAll('[data-param]')];
const dateFields = [$('#dateFrom'), $('#dateTo')];
const number = new Intl.NumberFormat();
const dateFormat = new Intl.DateTimeFormat(undefined, {dateStyle:'medium', timeStyle:'short', timeZone:'UTC'});
let request, generation = 0, current = null, nextCursor = null, page = 1;
let cursors = [null];
const suggestionRequests = new Map();
const suggestionTimers = new Map();

function tray(open) {
  $('#filterTray').hidden = !open;
  $('#filterToggle').setAttribute('aria-expanded', String(open));
  searchInput.setAttribute('aria-expanded', String(open));
}
function filterCount() {
  const count = [...filters, ...dateFields].filter(input => input.value.trim()).length;
  $('#filterCount').textContent = count ? `(${count})` : '';
}
function error(message = '') {
  $('#searchError').textContent = message;
  $('#searchError').hidden = !message;
}
async function json(url, signal) {
  const response = await fetch(url, {signal, cache:'no-store'});
  const data = await response.json();
  if (!response.ok) throw new Error(typeof data.detail === 'string' ? data.detail : 'Check your search and filters, then try again.');
  return data;
}
function filterId(input) {
  const value = input.value.trim();
  if (!value) return '';
  if (/^\d+$/.test(value)) return value;
  const match = value.match(/ · (\d+)$/);
  if (match) return match[1];
  input.setAttribute('aria-invalid', 'true');
  input.setAttribute('aria-describedby', 'searchError');
  tray(true);
  input.focus();
  throw new Error(`Choose a ${input.dataset.kind} suggestion or enter its Discord ID.`);
}
function parameters() {
  const params = new URLSearchParams();
  const q = searchInput.value.trim();
  if (q && q.length < 3) {
    searchInput.setAttribute('aria-invalid', 'true');
    searchInput.focus();
    throw new Error('Use at least 3 characters, or leave search empty to browse.');
  }
  if (q) params.set('q', q);
  for (const input of filters) {
    const id = filterId(input);
    if (id) params.set(input.dataset.param, id);
  }
  for (const input of dateFields) if (input.value) params.set(input.name, input.value);
  if (dateFields[0].value && dateFields[1].value && dateFields[0].value > dateFields[1].value) {
    tray(true);
    dateFields[1].setAttribute('aria-invalid','true');
    dateFields[1].setAttribute('aria-describedby','searchError');
    dateFields[1].focus();
    throw new Error('From date must be on or before the to date.');
  }
  return params;
}
function putText(parent, text, query) {
  // Text nodes keep archived HTML inert. Highlight only the literal search phrase.
  if (!query) { parent.textContent = text; return; }
  const escaped = query.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const regex = new RegExp(escaped, 'gi');
  let last = 0;
  for (const match of text.matchAll(regex)) {
    parent.append(document.createTextNode(text.slice(last, match.index)));
    const mark = document.createElement('mark');
    mark.textContent = match[0]; parent.append(mark);
    last = match.index + match[0].length;
  }
  parent.append(document.createTextNode(text.slice(last)));
}
function element(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}
function render(messages, query) {
  const fragment = document.createDocumentFragment();
  for (const message of messages) {
    const item = element('li', 'message');
    item.dataset.id = message.id;
    const shape = Number(BigInt(message.author_id || '0') % 4n);
    const avatar = element('span', `avatar a${shape}`, ['◇','○','△','□'][shape]);
    avatar.setAttribute('aria-hidden','true');
    const body = element('div');
    const meta = element('div','message-meta');
    meta.append(element('strong', '', message.author_name || 'Unknown author'));
    const timestamp = new Date(message.timestamp);
    const time = element('time', '', Number.isNaN(timestamp.valueOf()) ? 'Unknown date' : dateFormat.format(timestamp) + ' UTC');
    if (!Number.isNaN(timestamp.valueOf())) time.dateTime = timestamp.toISOString();
    meta.append(time);
    const source = element('p','message-source', `${message.guild_name || 'Direct messages'} · #${message.channel_name || 'unknown channel'}`);
    const content = element('p','message-text');
    content.id = `text-${message.id}`;
    const fullText = message.content || '';
    putText(content, fullText.length > 2000 ? fullText.slice(0,2000) + '…' : fullText, query);
    body.append(meta, source, content);
    if (fullText.length > 2000) {
      const expand = element('button','expand-message','Show full message');
      expand.type = 'button';
      expand.setAttribute('aria-expanded','false');
      expand.setAttribute('aria-controls',content.id);
      expand.addEventListener('click', () => {
        const open = expand.getAttribute('aria-expanded') === 'false';
        content.replaceChildren();
        putText(content, open ? fullText : fullText.slice(0,2000) + '…', query);
        expand.setAttribute('aria-expanded',String(open));
        expand.textContent = open ? 'Show less' : 'Show full message';
      });
      body.append(expand);
    }
    if (message.attachments.length) {
      const attachments = element('div','attachments');
      message.attachments.forEach((url, index) => {
        const link = element('a','',`Image ${index+1} ↗`);
        link.href = url; link.target = '_blank'; link.rel = 'noopener noreferrer';
        link.title = 'Open saved Discord image link (may have expired)';
        attachments.append(link);
      });
      body.append(attachments);
    }
    item.append(avatar, body); fragment.append(item);
  }
  $('#messages').replaceChildren(fragment);
  $('#emptyState').hidden = messages.length > 0;
}
async function run(params, {push=true, cursor=null, targetPage=1, scroll=true} = {}) {
  request?.abort();
  request = new AbortController();
  const version = ++generation;
  error();
  $('#results').hidden = false;
  $('#resultBody').setAttribute('aria-busy','true');
  $('#resultStatus').textContent = 'Searching…';
  $('#pagination').hidden = true;
  const url = new URLSearchParams(params);
  if (cursor) url.set('before', cursor);
  try {
    const data = await json(`/api/search?${url}`, request.signal);
    if (version !== generation) return;
    current = new URLSearchParams(params);
    nextCursor = data.next_cursor;
    page = targetPage;
    render(data.messages, params.get('q') || '');
    $('#resultStatus').textContent = `${data.messages.length} ${data.messages.length === 1 ? 'message' : 'messages'}${data.has_more ? ' · more available' : ''} · ${number.format(data.elapsed_ms)} ms`;
    $('#pageNumber').textContent = `Page ${page}`;
    $('#previousPage').disabled = page === 1;
    $('#nextPage').disabled = !data.has_more;
    $('#pagination').hidden = !data.messages.length && page === 1;
    if (push) {
      const address = new URLSearchParams(url);
      address.set('search','1');
      history.pushState({cursors, page}, '', `?${address}`);
    }
    if (scroll) {
      $('#resultsTitle').focus({preventScroll:true});
      $('#results').scrollIntoView({behavior:matchMedia('(prefers-reduced-motion: reduce)').matches ? 'instant' : 'smooth', block:'start'});
    }
  } catch (exception) {
    if (exception.name === 'AbortError' || version !== generation) return;
    error(exception.message || 'Could not connect. Check the display server and try again.');
    $('#messages').replaceChildren();
    $('#emptyState').hidden = true;
    $('#resultStatus').textContent = 'Search failed. Try again using the search above.';
  } finally {
    if (version === generation) $('#resultBody').setAttribute('aria-busy','false');
  }
}
form.addEventListener('submit', event => {
  event.preventDefault();
  try {
    const params = parameters();
    cursors = [null];
    tray(false);
    run(params);
  } catch (exception) { error(exception.message); }
});
searchInput.addEventListener('focus', () => tray(true));
$('#filterToggle').addEventListener('click', () => tray($('#filterTray').hidden));
document.addEventListener('pointerdown', event => {if (!form.contains(event.target)) tray(false);});
document.addEventListener('focusin', event => {if (!form.contains(event.target)) tray(false);});
document.addEventListener('keydown', event => {
  if (event.key === 'Escape' && !$('#filterTray').hidden) {
    searchInput.focus(); tray(false);
  }
  if (event.key === '/' && !event.ctrlKey && !event.metaKey && !event.altKey && !event.target.matches('input,textarea,[contenteditable]')) {
    event.preventDefault(); searchInput.focus();
  }
});
for (const input of [searchInput, ...filters, ...dateFields]) input.addEventListener('input', () => {
  input.removeAttribute('aria-invalid'); error(); filterCount();
});
$('#clearFilters').addEventListener('click', () => {
  for (const input of [...filters,...dateFields]) { input.value = ''; input.removeAttribute('aria-invalid'); }
  for (const controller of suggestionRequests.values()) controller.abort();
  for (const timer of suggestionTimers.values()) clearTimeout(timer);
  document.querySelectorAll('datalist').forEach(list => list.replaceChildren());
  $('#suggestionStatus').textContent = '';
  filterCount(); error();
});
for (const input of filters) {
  const suggest = () => {
    clearTimeout(suggestionTimers.get(input));
    suggestionRequests.get(input)?.abort();
    input.list.replaceChildren();
    const value = input.value.trim();
    if (/ · \d+$/.test(value) || /^\d+$/.test(value)) return;
    suggestionTimers.set(input, setTimeout(async () => {
      const controller = new AbortController();
      suggestionRequests.set(input, controller);
      const params = new URLSearchParams({q:value});
      if (input.id === 'channel') {
        const guild = $('#guild').value.trim().match(/(?:^| · )(\d+)$/);
        if (guild) params.set('guild_id',guild[1]);
      }
      try {
        const rows = await json(`/api/suggestions/${input.dataset.kind}?${params}`,controller.signal);
        if (input.value.trim() !== value) return;
        input.list.replaceChildren(...rows.map(row => {
          const option = document.createElement('option');
          option.value = `${row.name} · ${row.id}`;
          return option;
        }));
        $('#suggestionStatus').textContent = '';
      } catch (exception) {
        if (exception.name !== 'AbortError') $('#suggestionStatus').textContent = 'Suggestions unavailable. You can still enter an ID.';
      }
    },180));
  };
  input.addEventListener('focus',suggest);
  input.addEventListener('input',suggest);
}
$('#guild').addEventListener('input', () => {
  $('#channel').value = ''; $('#channelOptions').replaceChildren();
  suggestionRequests.get($('#channel'))?.abort();
  clearTimeout(suggestionTimers.get($('#channel'))); filterCount();
});
$('#nextPage').addEventListener('click', () => {
  if (!nextCursor || !current) return;
  cursors[page] = nextCursor;
  run(current, {cursor:nextCursor,targetPage:page+1});
});
$('#previousPage').addEventListener('click', () => {
  if (page > 1 && current) run(current, {cursor:cursors[page-2],targetPage:page-1});
});
$('#editSearch').addEventListener('click', () => searchInput.focus());
function restore() {
  const url = new URLSearchParams(location.search);
  searchInput.value = url.get('q') || '';
  filters.forEach(input => {input.value = url.get(input.dataset.param) || '';});
  dateFields.forEach(input => {input.value = url.get(input.name) || '';});
  filterCount(); error(); tray(false);
  if (!url.has('search') && !url.has('q')) {
    request?.abort(); generation++;
    $('#results').hidden = true;
    return;
  }
  const cursor = url.get('before');
  cursors = history.state?.cursors || [null];
  page = history.state?.page || 1;
  // A directly opened deep link has no earlier cursor history. Its first page
  // is the linked position; subsequent next/previous navigation remains valid.
  if (!history.state?.cursors) cursors = [cursor];
  url.delete('search'); url.delete('before');
  run(url, {push:false,cursor,targetPage:page,scroll:false});
}
window.addEventListener('popstate', restore);
json('/api/summary').then(data => {
  $('#messageTotal').textContent = number.format(data.messages);
  $('#serverTotal').textContent = number.format(data.servers);
  document.body.dataset.archiveReady = 'true';
}).catch(exception => {
  $('#messageTotal').textContent = 'Unavailable'; $('#serverTotal').textContent = 'Unavailable';
  $('#archiveError').textContent = exception.message;
  $('#archiveError').hidden = false;
});
restore();
