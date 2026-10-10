'use strict';
const $ = selector => document.querySelector(selector);
const form = $('#searchForm');
const searchInput = $('#searchInput');
const filters = [...document.querySelectorAll('[data-param]')];
const dateFields = [$('#dateFrom'), $('#dateTo')];
const number = new Intl.NumberFormat();
const dateFormat = new Intl.DateTimeFormat(undefined, {dateStyle:'medium', timeStyle:'short'});
let request, generation = 0, current = null, nextCursor = null, page = 1;
let cursors = [null];
let activeProfile = null, profileRequest = 0;
const suggestionRequests = new Map();
const suggestionTimers = new Map();
const profileView = new SearchcordProfile($('#profileHost'), {
  onServer: (uid, guild) => {
    const params=new URLSearchParams({profile:uid,author_id:uid,guild_id:guild});
    if($('#profileMessageQuery').value.trim())params.set('q',$('#profileMessageQuery').value.trim());
    cursors=[null];run(params);
  }
});

function tray(open) {
  $('.search-surface').classList.toggle('is-expanded', open);
  $('#filterTray').inert = !open;
  searchInput.setAttribute('aria-expanded', String(open));
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
function messageAvatar(authorId, avatarUrl) {
  let hash = 0;
  for (const char of String(authorId)) hash = (hash * 31 + char.charCodeAt(0)) >>> 0;
  const avatar = element('div', `message-avatar avatar-${'abcdef'[hash % 6]}`);
  avatar.setAttribute('aria-hidden', 'true');
  const fallback = () => { avatar.innerHTML = '<span class="shape-one"></span><span class="shape-two"></span><span class="shape-three"></span><span class="shape-four"></span>'; };
  fallback();
  if (avatarUrl) {
    const img = new Image(); img.alt = ''; img.loading = 'lazy'; img.referrerPolicy = 'no-referrer';
    img.onload = () => avatar.replaceChildren(img);
    img.onerror = fallback; avatar.appendChild(img); img.src = avatarUrl;
  }
  return avatar;
}

function render(messages, query) {
  const fragment = document.createDocumentFragment();
  for (const message of messages) {
    const row = element('article', 'result-row');
    row.dataset.id = message.id;
    const body = element('div', 'message-body');
    const heading = element('div', 'message-heading');
    const author = element('button', 'message-author profile-author', message.author_name || message.author_id);
    author.type='button';author.addEventListener('click',()=>openProfile(message.author_id));
    const timestamp = new Date(message.timestamp);
    const time = element('time', 'message-time', Number.isNaN(timestamp.valueOf()) ? 'Unknown date' : dateFormat.format(timestamp));
    if (!Number.isNaN(timestamp.valueOf())) time.dateTime = message.timestamp;
    const location = element('span', 'message-location', `${message.guild_name || 'Direct messages'} · #${message.channel_name || 'unknown channel'}`);
    const id = element('span', 'message-id', `id: ${message.author_id}`);
    heading.append(author, time, location, id);
    const text = element('p', 'result-copy');
    putText(text, message.content || '(no text content)', query);
    body.append(heading, text);
    if (message.attachment_files?.length || message.attachments?.length) {
      const attachments = element('div','rc-attachments');
      const files = message.attachment_files?.length ? message.attachment_files :
        message.attachments.map((url,index)=>({url,filename:`image ${index+1}`}));
      files.forEach(({url,filename}) => {
        const link = element('a','rc-att',filename);
        link.href = url; link.target = '_blank'; link.rel = 'noopener noreferrer';
        attachments.append(link);
      });
      body.append(attachments);
    }
    row.append(messageAvatar(message.author_id, message.avatar_url), body);
    fragment.append(row);
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
  url.set('limit', '50');
  if (cursor) url.set('before', cursor);
  try {
    const data = await json(`/api/search?${url}`, request.signal);
    if (version !== generation) return;
    if (data.scan) { params.set('scan','true'); url.set('scan','true'); }
    current = new URLSearchParams(params);
    nextCursor = data.next_cursor;
    page = targetPage;
    render(data.messages, params.get('q') || '');
    $('#resultStatus').textContent = data.partial ? 'More history remains. Continue searching older messages.' :
      (data.messages.length ? 'Results loaded.' : 'No messages match these filters.');
    $('#emptyState').hidden = data.partial || data.messages.length > 0;
    $('#nextPage').textContent = data.partial ? 'Search older' : 'Next';
    $('#pageNumber').textContent = `Page ${page}`;
    $('#previousPage').disabled = page === 1;
    $('#nextPage').disabled = !data.has_more;
    $('#pagination').hidden = page === 1 && !data.has_more;
    if (push) {
      const address = new URLSearchParams(url);
      address.delete('limit');
      address.set('search','1');
      history.pushState({cursors, page}, '', `?${address}`);
    }
    if (scroll) {
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
async function submitSearch(scroll = false) {
  const profileVersion=++profileRequest;
  request?.abort(); generation++;
  $('#resultBody').setAttribute('aria-busy','false');
  try {
    const raw=searchInput.value.trim();
    if(raw.startsWith('@')) {
      const id=raw.match(/^@(?:.* · )?([0-9]+)$/)?.[1];
      if(id){await openProfile(id);return;}
      const name=raw.slice(1).trim();
      if(!name)throw new Error('Type a username or user ID after @.');
      const suggestions=await json(`/api/suggestions/author?q=${encodeURIComponent(name)}`);
      if(profileVersion!==profileRequest)return;
      const exact=suggestions.filter(row=>[row.username,row.name].some(value=>value?.toLocaleLowerCase()===name.toLocaleLowerCase()));
      const match=exact.length===1?exact[0]:(suggestions.length===1?suggestions[0]:null);
      if(!match)throw new Error('Choose an author suggestion to open their profile.');
      await openProfile(match.id);return;
    }
    closeProfile(false);
    const params = parameters();
    cursors = [null];
    run(params, {scroll});
  } catch (exception) { error(exception.message); }
}
form.addEventListener('submit', event => { event.preventDefault(); submitSearch(true); });
searchInput.addEventListener('focus', () => tray(true));
searchInput.addEventListener('click', () => tray(true));
document.addEventListener('pointerdown', event => {if (!form.contains(event.target)) tray(false);});
document.addEventListener('focusin', event => {if (!form.contains(event.target)) tray(false);});
document.addEventListener('keydown', event => {
  if (event.key === 'Escape' && !$('#filterTray').inert) {
    searchInput.focus(); tray(false);
  }
  if (event.key === '/' && !event.ctrlKey && !event.metaKey && !event.altKey && !event.target.matches('input,textarea,[contenteditable]')) {
    event.preventDefault(); focusSearch();
  }
});
for (const input of [searchInput, ...filters, ...dateFields]) input.addEventListener('input', () => {
  input.removeAttribute('aria-invalid'); error();
});
$('#clearFilters').addEventListener('click', () => {
  for (const input of [...filters,...dateFields]) { input.value = ''; input.removeAttribute('aria-invalid'); }
  for (const controller of suggestionRequests.values()) controller.abort();
  for (const timer of suggestionTimers.values()) clearTimeout(timer);
  document.querySelectorAll('datalist').forEach(list => list.replaceChildren());
  $('#suggestionStatus').textContent = '';
  error(); submitSearch();
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
          const name = row.name.normalize('NFKC').replace(/^[^\p{L}\p{N}]+/u, '') || row.name;
          option.value = `${input.id === 'channel' ? '#' : ''}${name} · ${row.id}`;
          option.label = row.name;
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
  clearTimeout(suggestionTimers.get($('#channel')));
});
for (const input of [...filters, ...dateFields]) input.addEventListener('change', () => submitSearch());
$('#nextPage').addEventListener('click', () => {
  if (!nextCursor || !current) return;
  cursors[page] = nextCursor;
  run(current, {cursor:nextCursor,targetPage:page+1});
});
$('#previousPage').addEventListener('click', () => {
  if (page > 1 && current) run(current, {cursor:cursors[page-2],targetPage:page-1});
});
function focusSearch() {
  if(activeProfile)setProfileSearch(true);
  else searchInput.focus();
}
function setProfileSearch(open) {
  $('#profileMessageForm').hidden=!open;
  $('#profileSearchToggle').setAttribute('aria-expanded',String(open));
  if(open)$('#profileMessageQuery').focus();
}
$('#editSearch').addEventListener('click', focusSearch);
function restore() {
  const url = new URLSearchParams(location.search);
  searchInput.value = url.get('q') || '';
  filters.forEach(input => {input.value = url.get(input.dataset.param) || '';});
  dateFields.forEach(input => {input.value = url.get(input.name) || '';});
  error(); tray(false);
  if(url.has('profile')) {
    cursors=history.state?.cursors || [url.get('before')];page=history.state?.page||1;
    openProfile(url.get('profile'),{push:false,params:url,cursor:url.get('before'),targetPage:page,scroll:false});return;
  }
  closeProfile(false);
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
async function openProfile(uid,{push=true,params=null,cursor=null,targetPage=1,scroll=true}={}) {
  profileRequest++; activeProfile=uid;
  request?.abort();generation++;
  document.body.classList.add('has-profile');$('#profileSection').hidden=false;$('#profileSearchToggle').hidden=false;
  $('#profileMessageQuery').value=params?.get('q')||'';
  setProfileSearch(false);
  searchInput.value=`@${uid}`;tray(false);
  if(!params)cursors=[null];
  const query=new URLSearchParams(params||{});query.delete('before');query.delete('search');
  query.set('profile',uid);query.set('author_id',uid);
  const profileLoading=profileView.open(uid);
  const resultsLoading=run(query,{push,cursor,targetPage,scroll:false});
  if(scroll){
    $('#closeProfile').focus({preventScroll:true});
    $('#profileSection').scrollIntoView({behavior:matchMedia('(prefers-reduced-motion: reduce)').matches?'instant':'smooth',block:'start'});
  }
  await Promise.all([profileLoading,resultsLoading]);
}
function closeProfile(reset=true) {
  activeProfile=null;profileView.close();$('#profileSection').hidden=true;$('#profileSearchToggle').hidden=true;$('#profileMessageForm').hidden=true;
  document.body.classList.remove('has-profile');
  if(reset){profileRequest++;request?.abort();generation++;searchInput.value='';$('#results').hidden=true;history.pushState(null,'',location.pathname);searchInput.focus();}
}
$('#closeProfile').addEventListener('click',()=>closeProfile());
$('#profileSearchToggle').addEventListener('click',()=>{
  setProfileSearch($('#profileMessageForm').hidden);
});
document.addEventListener('pointerdown',event=>{
  if(!$('#profileMessageForm').contains(event.target)&&!$('#profileSearchToggle').contains(event.target))setProfileSearch(false);
});
$('#profileMessageForm').addEventListener('keydown',event=>{
  if(event.key==='Escape'){event.preventDefault();setProfileSearch(false);$('#profileSearchToggle').focus();}
});
$('#profileMessageForm').addEventListener('submit',event=>{
  event.preventDefault();if(!activeProfile)return;
  const params=new URLSearchParams({profile:activeProfile,author_id:activeProfile});
  if($('#profileMessageQuery').value.trim())params.set('q',$('#profileMessageQuery').value.trim());
  setProfileSearch(false);$('#profileSearchToggle').focus();
  cursors=[null];run(params,{scroll:false});
});
let mentionTimer, mentionRequest;
searchInput.addEventListener('input',()=>{
  profileRequest++;clearTimeout(mentionTimer);mentionRequest?.abort();$('#profileOptions').replaceChildren();
  const value=searchInput.value.trim();if(!value.startsWith('@')||value.includes(' · '))return;
  mentionTimer=setTimeout(async()=>{
    mentionRequest=new AbortController();
    try{
      const rows=await json(`/api/suggestions/author?q=${encodeURIComponent(value.slice(1))}`,mentionRequest.signal);
      if(searchInput.value.trim()!==value)return;
      $('#profileOptions').replaceChildren(...rows.map(row=>{const option=document.createElement('option');option.value=`@${row.username||row.name} · ${row.id}`;option.label=row.name;return option;}));
    }catch(exception){if(exception.name!=='AbortError')error('Author suggestions unavailable. You can still enter @ followed by a user ID.');}
  },180);
});
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
