'use strict';

let statsData;
let statsRequest = 0;
let statsController;
const contributorState = {offset: 0, more: true, busy: false, request: 0, peak: 1, controller: null};
const chartPalette = ['#8abde8', '#8dd8b0', '#c2b5ec', '#7c9fb8', '#68ab97', '#a39ac4'];

function initStats() {
  $('refreshStats').addEventListener('click', () => loadStats(true));
  $('statsRange').addEventListener('change', () => loadStats(false));
  $('timelineType').addEventListener('change', () => { if (statsData) renderStatsCharts(statsData); });
  $('serverChartType').addEventListener('change', () => { if (statsData) renderStatsCharts(statsData); });
  $('moreContributors').addEventListener('click', () => loadContributors(false));
  $('leaderboard').addEventListener('scroll', () => {
    const list = $('leaderboard');
    if (list.scrollHeight - list.scrollTop - list.clientHeight < 80) loadContributors(false);
  }, {passive: true});
  let timer;
  $('contributorSearch').addEventListener('input', () => {
    clearTimeout(timer);
    contributorState.controller?.abort();
    ++contributorState.request;
    contributorState.busy = false;
    contributorState.more = false;
    timer = setTimeout(() => loadContributors(true), 220);
  });
}

async function loadStats(resetContributors = true) {
  statsController?.abort();
  statsController = new AbortController();
  const request = ++statsRequest;
  $('statsStatus').textContent = 'Loading archive statistics…';
  $('view-stats').setAttribute('aria-busy', 'true');
  if (resetContributors) loadContributors(true);
  try {
    const data = await api(`/api/stats?days=${encodeURIComponent($('statsRange').value)}`, {signal: statsController.signal});
    if (request !== statsRequest) return;
    statsData = data;
    for (const [id, key] of [['sTotal','total_messages'], ['sServers','total_servers'], ['sChannels','total_channels'], ['sUsers','total_users']]) {
      $(id).textContent = Intl.NumberFormat(undefined, {notation:'compact', maximumFractionDigits:1}).format(data[key]);
      $(id).title = n(data[key]);
    }
    $('sDbSize').textContent = fmtBytes(data.db_size_bytes);
    renderStatsCharts(data);
    $('statsStatus').textContent = typeof Chart === 'undefined' ? 'Charts could not load. The data tables below remain available.' : '';
  } catch (error) {
    if (error.name !== 'AbortError' && request === statsRequest) $('statsStatus').textContent = `Statistics unavailable: ${error.message}. Use refresh to retry.`;
  } finally {
    if (request === statsRequest) $('view-stats').removeAttribute('aria-busy');
  }
}

async function loadContributors(reset = false) {
  if (!reset && (contributorState.busy || !contributorState.more)) return;
  if (reset) {
    contributorState.controller?.abort();
    contributorState.offset = 0; contributorState.more = true;
    $('leaderboard').replaceChildren(); $('leaderboard').scrollTop = 0;
  }
  const request = ++contributorState.request;
  contributorState.busy = true;
  contributorState.controller = new AbortController();
  $('moreContributors').disabled = true;
  $('contributorStatus').textContent = 'Loading contributors…';
  const params = new URLSearchParams({offset: contributorState.offset, limit: 50, q: $('contributorSearch').value.trim()});
  try {
    const data = await api(`/api/stats/contributors?${params}`, {signal: contributorState.controller.signal});
    if (request !== contributorState.request) return;
    if (reset) contributorState.peak = data.users[0]?.count || 1;
    data.users.forEach((user, index) => {
      const row = ce('button', 'lb-item'); row.type = 'button';
      row.title = `Search messages by ${user.author_name || user.author_id}`;
      row.innerHTML = `<span class="lb-rank">${n(contributorState.offset + index + 1)}</span><span class="lb-info"><span class="lb-name">${esc(user.author_name || 'Unknown author')}</span><span class="lb-uid">${esc(user.author_id)}</span></span><span class="lb-bar-wrap" aria-hidden="true"><span class="lb-bar" style="display:block;width:${Math.min(100, 100 * user.count / contributorState.peak)}%"></span></span><span class="lb-count">${n(user.count)}</span>`;
      row.addEventListener('click', () => statsDrilldown({author_id: user.author_id}));
      $('leaderboard').appendChild(row);
    });
    contributorState.offset += data.users.length;
    contributorState.more = data.has_more;
    $('contributorStatus').textContent = contributorState.offset ? `${n(contributorState.offset)} contributors loaded${data.has_more ? ' · scroll for more' : ' · all shown'}` : 'No matching contributors';
    $('moreContributors').hidden = !data.has_more;
  } catch (error) {
    if (error.name !== 'AbortError' && request === contributorState.request) {
      $('contributorStatus').textContent = 'Could not load contributors. Try again.';
      $('moreContributors').hidden = false;
    }
  } finally {
    if (request === contributorState.request) { contributorState.busy = false; $('moreContributors').disabled = false; }
  }
}

async function statsDrilldown(filters) {
  for (const id of Object.values(searchFields)) { $(id).value = ''; $(id).setCustomValidity(''); $(id).removeAttribute('aria-invalid'); }
  for (const [key, value] of Object.entries(filters)) $(searchFields[key]).value = value;
  populateChannelFilter(filters.guild_id || '');
  setFilterTrayExpanded(false);
  await doSearch(1, true);
}

function renderStatsCharts(data) {
  const servers = data.messages_by_server;
  drawStatsChart('chartByServer', $('serverChartType').value, servers.map(row => row.guild_name || 'Direct messages'), servers.map(row => row.count), {
    horizontal: true, actions: servers.map(row => row.guild_id ? () => statsDrilldown({guild_id: row.guild_id}) : null),
  });
  const channels = data.messages_by_channel;
  drawStatsChart('chartByChannel', 'bar', channels.map(row => `#${row.channel_name || row.channel_id}`), channels.map(row => row.count), {
    horizontal: true, actions: channels.map(row => () => statsDrilldown({channel_id: row.channel_id})),
  });
  const daily = new Map(data.messages_by_day.map(row => [row.date, row.count]));
  const dates = [];
  if (data.range_start && data.range_end) {
    for (let time = Date.parse(data.range_start + 'T00:00:00Z'); time <= Date.parse(data.range_end + 'T00:00:00Z'); time += 86400000) dates.push(new Date(time).toISOString().slice(0,10));
  }
  drawStatsChart('chartOverTime', $('timelineType').value, dates, dates.map(date => daily.get(date) || 0), {
    actions: dates.map(date => () => statsDrilldown({date_from:date, date_to:date})),
  });
  $('statsRangeLabel').textContent = data.range_end ? `${data.range_start} to ${data.range_end} · UTC · window ends at the latest archived message` : 'No messages indexed yet';
  const hours = new Map(data.messages_by_hour.map(row => [row.hour, row.count]));
  drawStatsChart('chartByHour', 'bar', Array.from({length:24}, (_,i) => `${String(i).padStart(2,'0')}:00`), Array.from({length:24},(_,i) => hours.get(i) || 0));
  drawStatsChart('chartByWeekday', 'bar', ['Monday','Tuesday','Wednesday','Thursday','Friday','Saturday','Sunday'], data.messages_by_weekday.map(row => row.count));
  const months = data.messages_by_month;
  drawStatsChart('chartByMonth', 'bar', months.map(row => row.month), months.map(row => row.count), {
    actions: months.map(row => () => {
      const [year, month] = row.month.split('-').map(Number);
      statsDrilldown({date_from: `${row.month}-01`, date_to:new Date(Date.UTC(year, month, 0)).toISOString().slice(0,10)});
    }),
  });
}

function drawStatsChart(id, type, labels, counts, {horizontal = false, actions = []} = {}) {
  const container = $(id + 'Data');
  const table = ce('table');
  table.innerHTML = '<thead><tr><th scope="col">Group</th><th scope="col">Messages</th></tr></thead>';
  const body = ce('tbody');
  labels.forEach((label, index) => {
    const row = ce('tr'), name = ce('td'), value = ce('td');
    if (actions[index]) {
      const button = ce('button','data-link'); button.type = 'button'; button.textContent = label;
      button.addEventListener('click', actions[index]); name.appendChild(button);
    } else name.textContent = label;
    value.textContent = n(counts[index]); row.append(name,value); body.appendChild(row);
  });
  table.appendChild(body); container.replaceChildren(table);
  if (!labels.length) container.textContent = 'No archived messages in this group.';
  if (typeof Chart === 'undefined') { container.parentElement.open = true; return; }
  if (_charts[id]) _charts[id].destroy();
  const ring = type === 'doughnut';
  const axis = {border:{display:false},grid:{color:'#253a4e'},ticks:{color:'#aab9c8',font:{family:'Arial',size:11},maxTicksLimit:8},beginAtZero:true};
  const valueAxis = {...axis, ticks:{...axis.ticks,callback:value => Intl.NumberFormat(undefined,{notation:'compact'}).format(value)}};
  const labelAxis = {...axis, grid:{display:false},ticks:{...axis.ticks,callback:function(value){return truncate(String(this.getLabelForValue(value)),horizontal ? 18 : 12);}}};
  const options = {
    responsive:true, maintainAspectRatio:false, resizeDelay:60,
    animation: matchMedia('(prefers-reduced-motion: reduce)').matches ? false : {duration:280,easing:'easeOutQuart'},
    indexAxis: horizontal && !ring ? 'y' : 'x',
    interaction: {mode: ring ? 'nearest' : 'index', axis:horizontal && !ring ? 'y' : 'x',intersect: false},
    plugins:{legend:{display:ring,position:'bottom',labels:{color:'#aab9c8',boxWidth:12,padding:16}},tooltip:{backgroundColor:'#0e1624',titleColor:'#edf3f8',bodyColor:'#c1d3e2',borderColor:'#63778e',borderWidth:1,callbacks:{label:context => `${n(counts[context.dataIndex])} messages`}}},
    onClick: (_event, hits) => { if (hits.length) actions[hits[0].index]?.(); },
    onHover: (_event, hits, chart) => { chart.canvas.style.cursor = hits.length && actions[hits[0].index] ? 'pointer' : 'default'; },
  };
  if (!ring) options.scales = horizontal ? {x:valueAxis,y:labelAxis} : {x:labelAxis,y:valueAxis};
  _charts[id] = new Chart($(id), {type,data:{labels,datasets:[{
    label:'Messages',data:counts,
    backgroundColor:ring ? labels.map((_,i)=>chartPalette[i % chartPalette.length]) : type === 'line' ? '#8abde815' : '#8abde845',
    borderColor:ring ? '#0e1624' : id === 'chartByWeekday' ? '#8dd8b0' : '#8abde8',
    hoverBackgroundColor:'#c2b5ec',borderWidth:ring ? 3 : 1.5,borderRadius:0,borderSkipped:false,
    pointRadius:counts.length > 100 ? 0 : 2,pointHoverRadius:5,tension:.22,fill:type === 'line',
  }]},options});
}
