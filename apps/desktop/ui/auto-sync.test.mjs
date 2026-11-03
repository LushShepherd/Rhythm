import test from 'node:test';
import assert from 'node:assert/strict';
import { AutomaticSyncController, readSyncStatus, validSyncPassphrase } from './auto-sync.mjs';
import { autoSyncCopy } from './auto-sync-copy.mjs';

const disconnected = () => ({ state: 'disconnected', connected: false, last_success_at_utc_ms: null, error: null });
const ready = () => ({ state: 'ready', connected: true, last_success_at_utc_ms: 1793600000000, error: null });
function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

test('sync passphrases count Unicode characters and are bounded without normalization', () => {
  assert.equal(validSyncPassphrase('a long private phrase'), true);
  assert.equal(validSyncPassphrase('森'.repeat(12)), true);
  assert.equal(validSyncPassphrase('🌲'.repeat(6)), false);
  assert.equal(validSyncPassphrase('🌲'.repeat(12)), true);
  assert.equal(validSyncPassphrase('x'.repeat(1024)), true);
  assert.equal(validSyncPassphrase('x'.repeat(1025)), false);
  assert.equal(validSyncPassphrase('🌲'.repeat(256)), true);
  assert.equal(validSyncPassphrase('🌲'.repeat(257)), false);
  assert.equal(validSyncPassphrase('森'.repeat(342)), false);
  assert.equal(validSyncPassphrase('valid phrase' + '\ud800'), false);
  assert.equal(validSyncPassphrase(null), false);
});

test('native status is allowlisted and never carries tokens or keys into the view', () => {
  const status = readSyncStatus({ ...ready(), access_token: 'not-for-ui', key: 'not-for-ui', reconnect_required: true });
  assert.deepEqual(Object.keys(status).sort(), ['connected', 'error', 'last_success_at_utc_ms', 'reconnect_required', 'state']);
  assert.equal(status.reconnect_required, true);
  for (const value of [null, { ...ready(), state: 'all_devices_synced' }, { ...ready(), connected: 'true' },
    { ...ready(), last_success_at_utc_ms: Infinity }, { ...ready(), error: {} }]) {
    assert.throws(() => readSyncStatus(value), /autoSyncUnavailable/);
  }
});

test('a late status read cannot undo a newer native event', async () => {
  const read = deferred();
  const controller = new AutomaticSyncController({ invoke: () => read.promise });
  const pending = controller.refresh();
  controller.receive(disconnected());
  read.resolve(ready());
  assert.equal(await pending, false);
  assert.equal(controller.status.connected, false);
});

test('disconnect invalidates an older in-flight status read', async () => {
  const read = deferred();
  const controller = new AutomaticSyncController({ invoke: command => {
    if (command === 'sync_status') return read.promise;
    assert.equal(command, 'disconnect_sync');
    return Promise.resolve(disconnected());
  } });
  controller.receive(ready());
  const pending = controller.refresh();
  assert.equal(await controller.disconnect(), true);
  read.resolve(ready());
  assert.equal(await pending, false);
  assert.equal(controller.status.connected, false);
});

test('overlapping refreshes share one native call and reject stale errors', async () => {
  const read = deferred(); let calls = 0;
  const controller = new AutomaticSyncController({ invoke: () => { calls++; return read.promise; } });
  const first = controller.refresh(), second = controller.refresh();
  assert.equal(first, second);
  await Promise.resolve();
  controller.receive(ready());
  read.reject(new Error('old connection failure'));
  assert.equal(await first, false);
  assert.equal(calls, 1);
  assert.equal(controller.error, null);
});

test('sign-in serializes connection operations and retains no passphrase in controller state', async () => {
  const connection = deferred(); const calls = [];
  const phrase = 'private words remain private';
  const controller = new AutomaticSyncController({ invoke: (command, args) => {
    calls.push({ command, args }); return connection.promise;
  } });
  controller.receive(disconnected());
  const pending = controller.connect(phrase);
  assert.equal(controller.action, 'connect_sync');
  assert.equal(await controller.connect(phrase), false);
  assert.equal(await controller.retry(), false);
  assert.equal(await controller.refresh(), false);
  assert.equal(calls.length, 1);
  assert.deepEqual(calls[0], { command: 'connect_sync', args: { passphrase: phrase } });
  assert.equal(JSON.stringify(controller).includes(phrase), false);
  connection.resolve(ready());
  assert.equal(await pending, true);
  assert.equal(controller.action, null);
  assert.equal(controller.status.connected, true);
  assert.equal(JSON.stringify(controller).includes(phrase), false);
});

test('invalid passphrase never starts sign-in and errors leave retry possible', async () => {
  let calls = 0;
  const controller = new AutomaticSyncController({ invoke: async () => { calls++; throw new Error('Connection unavailable'); } });
  assert.equal(await controller.connect('too short'), false);
  assert.equal(calls, 0);
  assert.equal(controller.error, 'autoSyncPhraseInvalid');
  assert.equal(await controller.connect('long enough phrase'), false);
  assert.equal(controller.error, 'Connection unavailable');
  assert.equal(controller.action, null);
  controller.receive(disconnected());
  assert.equal(controller.error, null);
  controller.invoke = async () => ready();
  assert.equal(await controller.connect('long enough phrase'), true);
});

test('malformed or failed status does not become a false successful check', async () => {
  const controller = new AutomaticSyncController({ invoke: async () => ({ ...ready(), last_success_at_utc_ms: 'now' }) });
  assert.equal(await controller.refresh(), false);
  assert.equal(controller.status, null);
  assert.equal(controller.error, 'autoSyncUnavailable');
  controller.invoke = async () => ready();
  assert.equal(await controller.refresh(), true);
  assert.equal(controller.error, null);
});

test('a synchronous native error does not leave status polling permanently locked', async () => {
  const controller = new AutomaticSyncController({ invoke: () => { throw new Error('Bridge unavailable'); } });
  assert.equal(await controller.refresh(), false);
  assert.equal(controller.reading, null);
  controller.invoke = async () => disconnected();
  assert.equal(await controller.refresh(), true);
});

test('all interface languages cover the same sync controls and recovery instructions', () => {
  const keys = Object.keys(autoSyncCopy.en).sort();
  for (const language of ['zh-Hans', 'zh-Hant']) {
    assert.deepEqual(Object.keys(autoSyncCopy[language]).sort(), keys);
    for (const key of keys) assert.equal(typeof autoSyncCopy[language][key], 'string');
  }
});
