'use strict';

// Browser-owned acceptance and continuation. Requests, progress and controls
// stay in the page adapters; the server owns execution and saved scrape state.
function createCollectionQueue({startBatch, changed}) {
  let sources = [], active = null, starting = null;
  let account = 0, suspension = 0, automatic = false;
  let recovering = true, changingAccount = false, otherJob = false;
  let accountChange = Promise.resolve();
  const scans = new Map();
  const copy = source => Object.freeze({...source, id: String(source.id)});
  const batch = items => Object.freeze(items.map(copy));

  function view() {
    const idle = !recovering && !changingAccount && !otherJob && !active;
    return Object.freeze({
      sources: Object.freeze([...sources]), active,
      pending: Object.freeze(sources.filter(source => !active?.some(item => item.id === source.id))),
      canEdit: !active && !changingAccount,
      canStart: idle && sources.length > 0,
      canRunOtherJob: idle,
      scanning: Object.freeze([...scans.keys()]),
    });
  }

  function suspend() { suspension++; automatic = false; }

  async function start() {
    if (!view().canStart) return false;
    // Reserve before crossing the asynchronous seam: two completions can never
    // request the same batch, and later additions cannot change this batch.
    const reserved = active = batch(sources);
    automatic = false;
    changed();
    const request = starting = Promise.resolve().then(() => startBatch(reserved));
    try { await request; }
    finally { if (starting === request) starting = null; }
    return true;
  }

  function continueQueue() {
    if (automatic) return start();
  }

  function add(items) {
    const ids = new Set(sources.map(source => source.id));
    let added = 0;
    items.forEach(source => {
      const item = copy(source);
      if (!ids.has(item.id)) { ids.add(item.id); sources.push(item); added++; }
    });
    return added;
  }

  return Object.freeze({
    view, start,
    toggle(source) {
      if (!view().canEdit) return;
      const id = String(source.id);
      if (sources.some(item => item.id === id)) sources = sources.filter(item => item.id !== id);
      else add([source]);
      changed();
    },
    remove(id) {
      if (!view().canEdit) return;
      sources = sources.filter(source => source.id !== String(id));
      changed();
    },
    clear() {
      suspend();
      sources = active ? [...active] : [];
      changed();
    },
    stop() { suspend(); changed(); },
    async changeAccount(change, {removed = false} = {}) {
      const generation = ++account;
      suspend();
      changingAccount = true;
      scans.clear();
      // A running server job keeps its original saved account. Its batch stays
      // visible/controllable; all browser-owned pending work is discarded.
      // Token removal keeps selected sources until deletion succeeds.
      if (!removed) sources = active ? [...active] : [];
      changed();
      const request = accountChange = accountChange.catch(() => {}).then(async () => {
        // Let an already-issued start bind its server job to the original
        // saved account before changing the server's selected credential.
        if (starting) await starting;
        const result = await change();
        if (removed) { sources = []; changed(); }
        return result;
      });
      try { return await request; }
      finally {
        if (generation === account) { changingAccount = false; changed(); continueQueue(); }
      }
    },
    async scan(guild, load) {
      const id = String(guild.id);
      if (scans.has(id) || changingAccount) return {state: 'ignored'};
      const ticket = {account, suspension};
      scans.set(id, ticket);
      changed();
      try {
        const channels = await load();
        if (ticket.account !== account) return {state: 'stale'};
        if (!channels) return {state: 'failed'};
        const available = channels.filter(channel => !channel.name.toLowerCase().includes('bot'));
        const added = add(available.map(channel => ({
          id: channel.id, name: channel.name, guild_id: guild.id, guild_name: guild.name,
        })));
        const result = {available: available.length, skipped: channels.length - available.length, added};
        let state;
        if (!available.length) state = 'empty';
        else if (ticket.suspension !== suspension) state = 'suspended';
        else {
          state = view().canStart ? 'starting' : 'waiting';
          automatic ||= added > 0 || state === 'starting';
        }
        changed();
        await continueQueue();
        return {...result, state};
      } finally {
        // An old account's completion must not erase its replacement scan.
        if (scans.get(id) === ticket) scans.delete(id);
        changed();
      }
    },
    finish(success) {
      if (!active) return;
      if (success) {
        const finished = new Set(active.map(source => source.id));
        sources = sources.filter(source => !finished.has(source.id));
      } else suspend();
      active = null;
      changed();
      return continueQueue();
    },
    async recover(load) {
      // A failed snapshot keeps starts blocked. Reload retries recovery.
      const job = await load();
      if (job) {
        active = batch(job.channels);
        const pending = sources;
        sources = [...active];
        add(pending);
      }
      recovering = false;
      changed();
      await continueQueue();
      return job;
    },
    beginOtherJob() {
      if (!view().canRunOtherJob) return false;
      otherJob = true;
      changed();
      return true;
    },
    finishOtherJob() {
      if (!otherJob) return;
      otherJob = false;
      changed();
      // DM cleanup historically releases pending collection even on error.
      return continueQueue();
    },
  });
}
