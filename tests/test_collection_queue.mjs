// Native Node regression checks: node --test tests/test_collection_queue.mjs
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {runInNewContext} from 'node:vm';
import test from 'node:test';

const create = runInNewContext(readFileSync(new URL('../static/collection-queue.js', import.meta.url), 'utf8') + '\ncreateCollectionQueue;');
const guild = {id: '10', name: 'Fixture'};
const channel = id => ({id, name: 'channel-' + id});
const source = id => ({...channel(id), guild_id: guild.id, guild_name: guild.name});
const ids = items => Array.from(items, item => item.id);
const deferred = () => {
  let resolve, reject;
  const promise = new Promise((yes, no) => {resolve = yes; reject = no;});
  return {promise, resolve, reject};
};
async function fixture() {
  const starts = [];
  const queue = create({startBatch: async items => {starts.push(items);}, changed() {}});
  await queue.recover(async () => null);
  return {queue, starts};
}

test('manual/scanned acceptance deduplicates stable IDs, preserves names and allows manual bots', async () => {
  const {queue, starts} = await fixture();
  queue.toggle({...source('24'), name: 'RoBOT-commands'});
  queue.toggle({...source(20), name: 'manual name'});
  const result = await queue.scan(guild, async () => [channel('20'), channel('20'), channel('21'), {id: '24', name: 'RoBOT-commands'}]);
  assert.equal(result.added, 1);
  assert.equal(result.skipped, 1);
  assert.equal(starts.length, 1);
  assert.deepEqual(ids(starts[0]), ['24', '20', '21']);
  assert.equal(starts[0][1].name, 'manual name');
  assert.equal(starts[0][0].guild_name, guild.name);
});

test('later scans cannot mutate the active batch and successful completion removes only that batch', async () => {
  const {queue, starts} = await fixture();
  await queue.scan(guild, async () => [channel('20')]);
  const active = queue.view().active;
  assert.throws(() => {active[0].name = 'changed';}, TypeError);
  assert.throws(() => {queue.view().sources.push(source('99'));}, {name: 'TypeError'});
  await queue.scan({id: '11', name: 'Second'}, async () => [channel('40')]);
  queue.remove('20'); queue.toggle(source('50'));
  assert.deepEqual(ids(active), ['20']);
  assert.deepEqual(ids(queue.view().pending), ['40']);
  await queue.finish(true);
  assert.equal(starts.length, 2);
  assert.deepEqual(ids(starts[1]), ['40']);
  assert.deepEqual(ids(queue.view().sources), ['40']);
  await queue.finish(true);
  assert.equal(queue.view().sources.length, 0);
});

for (const action of ['stop', 'clear']) {
  test(`scan completion after ${action} accepts sources without automatically starting`, async () => {
    const {queue, starts} = await fixture();
    queue.toggle(source('manual'));
    const load = deferred();
    const scan = queue.scan(guild, () => load.promise);
    queue[action]();
    load.resolve([channel('20')]);
    assert.equal((await scan).state, 'suspended');
    assert.deepEqual(ids(queue.view().sources), action === 'clear' ? ['20'] : ['manual', '20']);
    assert.equal(starts.length, 0);
    await queue.start();
    assert.equal(starts.length, 1);
  });
  test(`${action} during an active batch prevents a late scan from restarting after completion`, async () => {
    const {queue, starts} = await fixture();
    await queue.scan(guild, async () => [channel('20')]);
    const load = deferred();
    const scan = queue.scan({id: '11', name: 'Second'}, () => load.promise);
    queue[action]();
    await queue.finish(true);
    load.resolve([channel('40')]);
    assert.equal((await scan).state, 'suspended');
    assert.equal(starts.length, 1);
    assert.deepEqual(ids(queue.view().sources), ['40']);
  });
}

test('account changes reject old results and block starts while the settings request is in flight', async () => {
  const {queue, starts} = await fixture();
  const load = deferred(), settings = deferred();
  queue.toggle(source('manual'));
  const scan = queue.scan(guild, () => load.promise);
  const change = queue.changeAccount(() => settings.promise);
  assert.equal(await queue.start(), false);
  assert.equal((await queue.scan(guild, async () => [channel('wrong-account')])).state, 'ignored');
  load.resolve([channel('old-account')]);
  assert.equal((await scan).state, 'stale');
  settings.resolve(); await change;
  assert.equal(starts.length, 0);
  assert.equal(queue.view().sources.length, 0);
  await queue.scan(guild, async () => [channel('new-account')]);
  assert.deepEqual(ids(starts[0]), ['new-account']);
});

test('an old scan cannot remove the new account scan for the same server', async () => {
  const {queue} = await fixture();
  const oldLoad = deferred(), newLoad = deferred();
  const oldScan = queue.scan(guild, () => oldLoad.promise);
  await queue.changeAccount(async () => {});
  const newScan = queue.scan(guild, () => newLoad.promise);
  oldLoad.resolve([channel('old')]); await oldScan;
  assert.deepEqual(ids(queue.view().scanning.map(id => ({id}))), ['10']);
  newLoad.resolve([channel('new')]); await newScan;
  assert.deepEqual(ids(queue.view().sources), ['new']);
});

test('overlapping account changes only release starts when the newest one finishes', async () => {
  const {queue, starts} = await fixture();
  const first = deferred(), second = deferred();
  const one = queue.changeAccount(() => first.promise);
  const two = queue.changeAccount(() => second.promise);
  first.resolve(); await one;
  assert.equal((await queue.scan(guild, async () => [channel('wrong')])).state, 'ignored');
  second.resolve(); await two;
  await queue.scan(guild, async () => [channel('right')]);
  assert.deepEqual(ids(starts[0]), ['right']);
});

test('account changes retain the server-owned active batch and discard only pending work', async () => {
  const {queue, starts} = await fixture();
  await queue.scan(guild, async () => [channel('20')]);
  await queue.scan({id: '11', name: 'Second'}, async () => [channel('40')]);
  await queue.changeAccount(async () => {});
  assert.deepEqual(ids(queue.view().active), ['20']);
  assert.deepEqual(ids(queue.view().sources), ['20']);
  await queue.finish(true);
  assert.equal(starts.length, 1);
});

test('removing all saved accounts clears stale sources while retaining progress until server cancellation', async () => {
  const {queue, starts} = await fixture();
  await queue.scan(guild, async () => [channel('20')]);
  await queue.changeAccount(async () => {}, {removed: true});
  assert.deepEqual(ids(queue.view().active), ['20']);
  assert.equal(queue.view().sources.length, 0);
  await queue.finish(false);
  assert.equal(queue.view().sources.length, 0);
  assert.equal(starts.length, 1);
});

test('failed token removal retains the active batch for a stopped/error retry', async () => {
  const {queue} = await fixture();
  await queue.scan(guild, async () => [channel('20')]);
  await assert.rejects(queue.changeAccount(async () => {throw new Error('removal failure');}, {removed: true}), /removal failure/);
  await queue.finish(false);
  assert.deepEqual(ids(queue.view().sources), ['20']);
});

test('failed token removal preserves idle manually queued sources', async () => {
  const {queue, starts} = await fixture();
  queue.toggle(source('20'));
  await assert.rejects(queue.changeAccount(async () => {throw new Error('busy');}, {removed: true}), /busy/);
  assert.deepEqual(ids(queue.view().sources), ['20']);
  assert.equal(starts.length, 0);
});

test('account mutation waits for an already-reserved start to bind its original saved account', async () => {
  const request = deferred(), events = [];
  const queue = create({startBatch: async () => {events.push('start'); await request.promise; events.push('bound');}, changed() {}});
  await queue.recover(async () => null);
  queue.toggle(source('20'));
  const start = queue.start();
  const change = queue.changeAccount(async () => {events.push('account');});
  await Promise.resolve();
  assert.deepEqual(events, ['start']);
  request.resolve(); await Promise.all([start, change]);
  assert.deepEqual(events, ['start', 'bound', 'account']);
});

test('rapid account mutations cannot complete out of order or release starts over an older request', async () => {
  const {queue, starts} = await fixture();
  const first = deferred(), second = deferred(), events = [];
  const one = queue.changeAccount(async () => {events.push('first-start'); await first.promise; events.push('first-end');});
  const two = queue.changeAccount(async () => {events.push('second-start'); await second.promise; events.push('second-end');});
  second.resolve();
  await Promise.resolve(); await Promise.resolve();
  assert.deepEqual(events, ['first-start']);
  assert.equal((await queue.scan(guild, async () => [channel('wrong')])).state, 'ignored');
  first.resolve(); await Promise.all([one, two]);
  assert.deepEqual(events, ['first-start', 'first-end', 'second-start', 'second-end']);
  await queue.scan(guild, async () => [channel('right')]);
  assert.deepEqual(ids(starts[0]), ['right']);
});

test('a failed account mutation does not strand a newer account change or release it early', async () => {
  const {queue} = await fixture();
  const next = deferred();
  const one = queue.changeAccount(async () => {throw new Error('settings failure');});
  const two = queue.changeAccount(() => next.promise);
  await assert.rejects(one, /settings failure/);
  assert.equal((await queue.scan(guild, async () => [channel('wrong')])).state, 'ignored');
  next.resolve(); await two;
  await queue.scan(guild, async () => [channel('right')]);
  assert.deepEqual(ids(queue.view().active), ['right']);
});

for (const scanFirst of [true, false]) {
  test(`batch completion and another scan resolving in ${scanFirst ? 'scan-first' : 'finish-first'} order starts once`, async () => {
    const {queue, starts} = await fixture();
    await queue.scan(guild, async () => [channel('20')]);
    const load = deferred();
    const scan = queue.scan({id: '11', name: 'Second'}, () => load.promise);
    if (scanFirst) { load.resolve([channel('40')]); await scan; await queue.finish(true); }
    else { await queue.finish(true); load.resolve([channel('40')]); await scan; }
    assert.equal(starts.length, 2);
    assert.deepEqual(ids(starts[1]), ['40']);
    await queue.start();
    assert.equal(starts.length, 2);
  });
}

test('multiple server scans accepted together reserve starts synchronously without losing sources', async () => {
  const {queue, starts} = await fixture();
  const first = deferred(), second = deferred();
  const one = queue.scan(guild, () => first.promise);
  const two = queue.scan({id: '11', name: 'Second'}, () => second.promise);
  assert.equal((await queue.scan(guild, async () => [])).state, 'ignored');
  first.resolve([channel('20')]); second.resolve([channel('40')]);
  await Promise.all([one, two]);
  assert.equal(starts.length, 1);
  await queue.finish(true);
  assert.equal(starts.length, 2);
  assert.deepEqual(ids(starts[1]), ['40']);
});

test('scrape failure preserves work and suspends scans still in flight, then manual retry succeeds', async () => {
  const {queue, starts} = await fixture();
  await queue.scan(guild, async () => [channel('20')]);
  await queue.scan({id: '11', name: 'Second'}, async () => [channel('40')]);
  const load = deferred();
  const scan = queue.scan({id: '12', name: 'Third'}, () => load.promise);
  await queue.finish(false);
  load.resolve([channel('50')]);
  assert.equal((await scan).state, 'suspended');
  assert.equal(starts.length, 1);
  assert.deepEqual(ids(queue.view().sources), ['20', '40', '50']);
  await queue.start();
  await queue.finish(true);
  assert.equal(starts.length, 2);
  assert.equal(queue.view().sources.length, 0);
});

test('a failed scan preserves manually queued work without starting a partial scrape', async () => {
  const {queue, starts} = await fixture();
  queue.toggle(source('manual'));
  assert.equal((await queue.scan(guild, async () => null)).state, 'failed');
  assert.equal(starts.length, 0);
  assert.deepEqual(ids(queue.view().sources), ['manual']);
});

test('DM cleanup blocks starts and releases accepted collection work on completion', async () => {
  const {queue, starts} = await fixture();
  assert.equal(queue.beginOtherJob(), true);
  await queue.scan(guild, async () => [channel('20')]);
  assert.equal(await queue.start(), false);
  assert.equal(starts.length, 0);
  await queue.finishOtherJob();
  assert.equal(starts.length, 1);
  assert.equal(queue.beginOtherJob(), false);
});

test('stop during DM cleanup suspends late scans when cleanup releases its reservation', async () => {
  const {queue, starts} = await fixture();
  queue.beginOtherJob();
  const load = deferred();
  const scan = queue.scan(guild, () => load.promise);
  queue.stop(); load.resolve([channel('20')]); await scan;
  await queue.finishOtherJob();
  assert.equal(starts.length, 0);
  assert.deepEqual(ids(queue.view().sources), ['20']);
});

test('server recovery blocks starts, reconstructs its active batch and keeps later pending sources', async () => {
  const starts = [], snapshot = deferred();
  const queue = create({startBatch: async items => {starts.push(items);}, changed() {}});
  const recovery = queue.recover(() => snapshot.promise);
  queue.toggle(source('manual'));
  await queue.scan(guild, async () => [channel('40')]);
  assert.equal(await queue.start(), false);
  assert.equal(queue.beginOtherJob(), false);
  snapshot.resolve({channels: [source('20')], paused: true, needs_account: true});
  await recovery;
  assert.equal(starts.length, 0);
  assert.deepEqual(ids(queue.view().active), ['20']);
  assert.deepEqual(ids(queue.view().pending), ['manual', '40']);
  assert.equal(await queue.start(), false);
  await queue.finish(true);
  assert.equal(starts.length, 1);
  assert.deepEqual(ids(starts[0]), ['manual', '40']);
});

test('recovery with no job releases a scan once, while failed recovery keeps starts blocked', async () => {
  const starts = [], snapshot = deferred();
  const queue = create({startBatch: async items => {starts.push(items);}, changed() {}});
  const recovery = queue.recover(() => snapshot.promise);
  await queue.scan(guild, async () => [channel('20')]);
  snapshot.resolve(null); await recovery;
  assert.equal(starts.length, 1);
  const failed = create({startBatch: async items => {starts.push(items);}, changed() {}});
  failed.toggle(source('40'));
  await assert.rejects(failed.recover(async () => {throw new Error('offline');}), /offline/);
  assert.equal(await failed.start(), false);
  assert.equal(starts.length, 1);
});
