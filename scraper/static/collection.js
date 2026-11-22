'use strict';

// Collection automation shares the existing queue, limits, and progress panel.
let inviteJob = null;
let invitePoll = null;
let inviteJoined = 0;

function initCollectionAutomation() {
  $('inviteForm').addEventListener('submit', startInvites);
  $('inviteInput').addEventListener('input', () => $('inviteInput').removeAttribute('aria-invalid'));
  $('stopInvites').addEventListener('click', stopInvites);
  const resume = ce('button', 'btn-ghost'); resume.type = 'button'; resume.id = 'resumeInvites';
  resume.textContent = 'resume pending invites'; resume.hidden = true;
  $('stopInvites').after(resume);
  resume.addEventListener('click', async () => {
    resume.disabled = true;
    try {
      const {job} = await api(`/api/invites/${encodeURIComponent(inviteJob.id)}/resume`, {method: 'POST'});
      renderInvites(job); scheduleInvitePoll();
    } catch (error) { $('inviteStatus').textContent = `Could not resume: ${error.message}`; }
    finally { resume.disabled = false; }
  });
  $('scanServer').addEventListener('click', () => { if (S.guild) scanGuild(S.guild); });
  refreshInvites();
}

async function scanGuild(guild) {
  if (collectionQueue.view().scanning.includes(String(guild.id))) return;
  $('scanStatus').textContent = `Checking readable channels in ${guild.name}…`;
  const result = await collectionQueue.scan(guild, () => selectGuild(guild, true));
  switch (result.state) {
    case 'ignored': return;
    case 'stale':
      $('scanStatus').textContent = 'Selected account changed. Scan the server again with the new account.';
      return;
    case 'failed':
      $('scanStatus').textContent = `Could not scan ${guild.name}. Check the channel error and try again. Your queue is kept.`;
      return;
  }
  const summary = `${result.available} readable ${result.available === 1 ? 'channel' : 'channels'}; ${result.skipped} with “bot” skipped.`;
  const messages = {
    empty: 'Nothing to queue.',
    suspended: 'Queued; automatic start paused because collection was stopped, cleared or failed.',
    waiting: result.added ? 'Added to the queue; starts after the current job.' : 'Already queued in the current job.',
    starting: 'Starting the collection queue.',
  };
  $('scanStatus').textContent = `${guild.name}: ${summary} ${messages[result.state]}`;
}

async function startInvites(event) {
  event.preventDefault();
  if (inviteJob?.running || $('startInvites').disabled) return;
  $('startInvites').disabled = true;
  $('inviteInput').removeAttribute('aria-invalid');
  $('inviteStatus').textContent = 'Starting invite queue…';
  try {
    const data = await api('/api/invites/start', {method: 'POST', body: {invites: $('inviteInput').value}});
    inviteJoined = 0;
    renderInvites(data.job);
    scheduleInvitePoll();
  } catch (error) {
    $('inviteStatus').textContent = `Could not start: ${error.message}`;
    $('inviteInput').setAttribute('aria-invalid', 'true');
    $('startInvites').disabled = false;
    // A second tab may own a running queue. Recover its actual status.
    if (inviteJob?.running) scheduleInvitePoll();
  }
}

function scheduleInvitePoll() {
  clearTimeout(invitePoll);
  invitePoll = setTimeout(refreshInvites, 1000);
}

async function refreshInvites() {
  try {
    const {job} = await api('/api/invites/status');
    renderInvites(job);
    if (job?.running) scheduleInvitePoll();
  } catch (error) {
    if (inviteJob?.running) {
      $('inviteStatus').textContent = `Could not refresh progress: ${error.message}. Reconnecting; the server keeps the invite queue.`;
      scheduleInvitePoll();
    }
  }
}

function renderInvites(job) {
  inviteJob = job;
  $('startInvites').disabled = !!job?.running;
  $('inviteInput').disabled = !!job?.running;
  $('stopInvites').hidden = !job?.running;
  $('stopInvites').disabled = false;
  $('resumeInvites').hidden = !job?.paused;
  $('inviteResults').hidden = !job;
  if (!job) return;
  const joined = job.items.filter(item => item.status === 'joined').length;
  const wait = job.running && job.wait_seconds ? ` Next attempt in ${job.wait_seconds}s.` : '';
  $('inviteStatus').textContent = `${joined}/${job.items.length} joined. ${job.message}${wait}`;
  const list = $('inviteResults');
  list.replaceChildren();
  job.items.forEach(item => {
    const row = ce('li', 'invite-result');
    const name = ce('span'); name.textContent = item.guild_name ? `${item.guild_name} · ${item.code}` : item.code;
    const detail = ce('span', 'invite-detail'); detail.textContent = item.message;
    const status = ce('strong'); status.textContent = item.status;
    row.append(name, detail, status); list.appendChild(row);
  });
  if (joined > inviteJoined) { inviteJoined = joined; loadGuilds(); }
}

async function stopInvites() {
  if (!inviteJob?.running) return;
  $('stopInvites').disabled = true;
  try {
    const {job} = await api(`/api/invites/${encodeURIComponent(inviteJob.id)}/stop`, {method: 'POST'});
    clearTimeout(invitePoll);
    renderInvites(job);
  } catch (error) {
    $('inviteStatus').textContent = `Could not stop joining: ${error.message}. Try again.`;
    $('stopInvites').disabled = false;
    scheduleInvitePoll();
  }
}
