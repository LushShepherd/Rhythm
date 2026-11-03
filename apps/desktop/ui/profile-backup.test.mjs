import test from 'node:test';
import assert from 'node:assert/strict';
import { BackgroundPreferences, loadBackgrounds } from './backgrounds.mjs';
import { captureProfile, restoreProfile, validateProfile, PROFILE_KEYS } from './profile-backup.mjs';
import { configureAdaptive, createAdaptiveState, recordRhythm, recordReceptivity } from './adaptive.mjs';
import { configureReminderDelay, createReminderDelayState, recordReminderResponse } from './reminder-delay.mjs';

// The production verifier uses Image.decode in WebView. Pure storage tests keep
// their byte fixtures independent of browser APIs; signature/base64 validation
// still runs before this injected final decoder.
const options = { verifyImage: async () => {} };
const png = Uint8Array.from(atob('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII='), c => c.charCodeAt(0));
function storage(initial = {}) {
  const values = new Map(Object.entries(initial));
  return {
    values,
    getItem: key => values.get(key) ?? null,
    setItem: (key, value) => { values.set(key, value); },
    removeItem: key => { values.delete(key); },
  };
}
function repository(initial = null) {
  let value = initial;
  return {
    read: async () => structuredClone(value),
    write: async next => { value = structuredClone(next); },
  };
}
async function profileState() {
  let adaptive = configureAdaptive(createAdaptiveState(), { enabled: true, context: 'creative' });
  let reminder = configureReminderDelay(createReminderDelayState(), { enabled: true, baseMs: 600_000 });
  for (let i = 0; i < 7; i++) {
    adaptive = recordRhythm(adaptive, { kind: 'focus', actualMs: 1_200_000, plannedMs: 1_500_000, manual: true, hour: 11 });
    adaptive = recordReceptivity(adaptive, { response: 'snooze', hour: 19 });
    reminder = recordReminderResponse(reminder, { kind: 'break', response: 'snooze', postponements: 2 });
  }
  const preferences = {
    'rhythm.language': 'zh-Hant', 'rhythm.theme': 'system', 'rhythm.entered': 'true',
    'rhythm.adaptive.v1': JSON.stringify(adaptive, null, 2),
    'rhythm.reminder-delay.v1': JSON.stringify(reminder, null, 1),
  };
  const backgrounds = await loadBackgrounds({ repository: repository() });
  const draft = backgrounds.createDraft();
  draft.setImage('focus', new Blob([png, Uint8Array.from({ length: 256 }, (_, i) => i)], { type: 'image/png' }), '慢慢來 🌿.png');
  draft.setImage('break', new Blob([png], { type: 'image/png' }), '留著的背景.png');
  draft.choose('break', 'dusk');
  await backgrounds.save(draft);
  return { storage: storage(preferences), backgrounds, ...options };
}

test('complete profile round-trip preserves exact learned bytes, Unicode names and both custom images', async () => {
  const source = await profileState();
  source.storage.setItem('unrelated.secret', 'never included');
  const snapshot = await captureProfile(source);
  assert.deepEqual(Object.keys(snapshot.preferences).sort(), [...PROFILE_KEYS].sort());
  assert.equal(snapshot.backgrounds.break.choice, 'dusk');
  assert.equal(snapshot.backgrounds.break.custom.name, '留著的背景.png');
  const target = { storage: storage({ 'unrelated.secret': 'keep on target' }), backgrounds: await loadBackgrounds({ repository: repository() }), ...options };
  await restoreProfile(snapshot, target);
  assert.deepEqual(await captureProfile(target), snapshot);
  assert.equal(target.storage.getItem('unrelated.secret'), 'keep on target');
  assert.equal(target.storage.getItem('rhythm.adaptive.v1'), source.storage.getItem('rhythm.adaptive.v1'));
  assert.deepEqual(new Uint8Array(await target.backgrounds.value.focus.custom.blob.arrayBuffer()), new Uint8Array(await source.backgrounds.value.focus.custom.blob.arrayBuffer()));
  // Applying a retained pending restore again cannot add observations or images.
  await restoreProfile(snapshot, target);
  assert.deepEqual(await captureProfile(target), snapshot);
});

test('missing preferences remain missing and replace target values with implicit defaults', async () => {
  const source = { storage: storage(), backgrounds: await loadBackgrounds({ repository: repository() }), ...options };
  const snapshot = await captureProfile(source);
  assert.deepEqual(snapshot.preferences, {});
  const target = await profileState();
  await restoreProfile(snapshot, target);
  assert.deepEqual([...target.storage.values], []);
  assert.deepEqual(await captureProfile(target), snapshot);
});

test('invalid, future, and unrelated preferences are rejected before any writes', async () => {
  const source = await profileState(), snapshot = await captureProfile(source);
  const mutations = [
    item => { item.schema = 2; },
    item => { item.extra = true; },
    item => { item.preferences['credentials'] = 'not portable'; },
    item => { item.preferences['rhythm.language'] = 'unknown'; },
    item => { item.preferences['rhythm.theme'] = 'unknown'; },
    item => { item.preferences['rhythm.entered'] = true; },
    item => { item.preferences['rhythm.adaptive.v1'] = '{bad'; },
    item => { item.preferences['rhythm.adaptive.v1'] = JSON.stringify({ ...createAdaptiveState(), schema: 2 }); },
    item => { item.preferences['rhythm.reminder-delay.v1'] = JSON.stringify({ ...createReminderDelayState(), baseMs: 0 }); },
    item => { item.preferences['rhythm.adaptive.v1'] = ' '.repeat(65_537); },
  ];
  const target = { storage: storage(), backgrounds: await loadBackgrounds({ repository: repository() }), ...options };
  const before = await captureProfile(target);
  for (const mutate of mutations) {
    const bad = structuredClone(snapshot); mutate(bad);
    await assert.rejects(restoreProfile(bad, target));
    assert.deepEqual(await captureProfile(target), before);
  }
});

test('invalid image type, noncanonical base64 and undecodable content never alter the target', async () => {
  const snapshot = await captureProfile(await profileState());
  const mutations = [
    item => { item.backgrounds.schema = 2; },
    item => { item.backgrounds.focus.custom.mime = 'image/svg+xml'; },
    item => { item.backgrounds.focus.custom.mime = 'image/jpeg'; },
    item => { item.backgrounds.focus.custom.base64 += '\n'; },
    item => { item.backgrounds.focus.custom.base64 = 'AA=='; },
    item => { item.backgrounds.focus.custom.base64 = 'AB=='; },
    item => { item.backgrounds.focus.custom.name = 'x'.repeat(181); },
    item => { item.backgrounds.focus.custom.extra = true; },
    item => { item.backgrounds.focus.custom = null; },
  ];
  const target = { storage: storage(), backgrounds: await loadBackgrounds({ repository: repository() }), ...options };
  const before = await captureProfile(target);
  for (const mutate of mutations) {
    const bad = structuredClone(snapshot); mutate(bad);
    await assert.rejects(restoreProfile(bad, target));
    assert.deepEqual(await captureProfile(target), before);
  }
  await assert.rejects(restoreProfile(snapshot, { ...target, verifyImage: async () => { throw new Error('Decoder rejected corrupt image'); } }), /Decoder rejected/);
  assert.deepEqual(await captureProfile(target), before);
});

test('unreadable background or preference storage cannot produce a misleading complete backup', async () => {
  const backgrounds = await loadBackgrounds({ repository: { read: async () => { throw new Error('blocked'); }, write: async () => {} } });
  await assert.rejects(captureProfile({ backgrounds, storage: storage(), ...options }), /could not be read/);
  const good = await loadBackgrounds({ repository: repository() });
  await assert.rejects(captureProfile({ backgrounds: good, storage: { getItem: () => { throw new Error('read blocked'); } }, ...options }), /read blocked/);
  await assert.rejects(captureProfile({ backgrounds: good, storage: storage({ 'rhythm.adaptive.v1': 'broken existing model' }), ...options }), /unreadable learning/);
});

test('failed preference write rolls back both stores and leaves the durable restore retryable', async () => {
  const snapshot = await captureProfile(await profileState());
  const target = { storage: storage({ 'rhythm.theme': 'light', 'rhythm.adaptive.v1': 'keep old bytes even if unreadable' }), backgrounds: await loadBackgrounds({ repository: repository() }), ...options };
  const previousValues = [...target.storage.values];
  const previousBackgrounds = await target.backgrounds.exportSnapshot(options);
  const originalWrite = target.storage.setItem;
  let failOnce = true;
  target.storage.setItem = (key, value) => {
    if (failOnce && key === 'rhythm.theme' && value === 'system') { failOnce = false; throw new Error('Quota exceeded'); }
    originalWrite(key, value);
  };
  await assert.rejects(restoreProfile(snapshot, target), /Previous preferences were kept/);
  assert.deepEqual([...target.storage.values].sort(), previousValues.sort());
  assert.deepEqual(await target.backgrounds.exportSnapshot(options), previousBackgrounds);
  await restoreProfile(snapshot, target);
  assert.deepEqual(await captureProfile(target), snapshot);
});

test('failed background transaction keeps preferences intact and a retry completes', async () => {
  const snapshot = await captureProfile(await profileState());
  const repo = repository(), originalWrite = repo.write;
  let failOnce = true;
  repo.write = async value => { if (failOnce) { failOnce = false; throw new Error('Image quota'); } await originalWrite(value); };
  const target = { storage: storage({ 'rhythm.theme': 'light' }), backgrounds: await loadBackgrounds({ repository: repo }), ...options };
  await assert.rejects(restoreProfile(snapshot, target), /Previous preferences were kept/);
  assert.deepEqual([...target.storage.values], [['rhythm.theme', 'light']]);
  assert.equal(target.backgrounds.value.focus.choice, 'forest');
  await restoreProfile(snapshot, target);
  assert.deepEqual(await captureProfile(target), snapshot);
});

test('a rollback failure is reported honestly and the same pending profile can recover later', async () => {
  const snapshot = await captureProfile(await profileState());
  const target = { storage: storage({ 'rhythm.theme': 'light' }), backgrounds: await loadBackgrounds({ repository: repository() }), ...options };
  const originalWrite = target.storage.setItem;
  target.storage.setItem = () => { throw new Error('Storage unavailable'); };
  await assert.rejects(restoreProfile(snapshot, target), /safety backup is preserved/);
  target.storage.setItem = originalWrite;
  await restoreProfile(snapshot, target);
  assert.deepEqual(await captureProfile(target), snapshot);
});

test('capture waits for a pending committed background save', async () => {
  const repo = repository();
  let release;
  const commit = new Promise(resolve => { release = resolve; });
  repo.write = () => commit;
  const backgrounds = new BackgroundPreferences(repo);
  const draft = backgrounds.createDraft(); draft.choose('focus', 'dusk');
  const save = backgrounds.save(draft);
  let captured = false;
  const capture = captureProfile({ backgrounds, storage: storage(), ...options }).then(value => { captured = true; return value; });
  await Promise.resolve();
  assert.equal(captured, false);
  release(); await save;
  assert.equal((await capture).backgrounds.focus.choice, 'dusk');
});

test('multi-megabyte image round-trip preserves all bytes without regex or argument overflow', async () => {
  const bytes = new Uint8Array(2 * 1024 * 1024); bytes.set(png);
  for (let i = png.length; i < bytes.length; i++) bytes[i] = i % 256;
  const source = { storage: storage(), backgrounds: await loadBackgrounds({ repository: repository() }), ...options };
  const draft = source.backgrounds.createDraft(); draft.setImage('focus', new Blob([bytes], { type: 'image/png' }), 'large.png');
  await source.backgrounds.save(draft);
  const snapshot = await captureProfile(source);
  const target = { storage: storage(), backgrounds: await loadBackgrounds({ repository: repository() }), ...options };
  await restoreProfile(snapshot, target);
  assert.deepEqual(new Uint8Array(await target.backgrounds.value.focus.custom.blob.arrayBuffer()), bytes);
  assert.equal(target.backgrounds.value.focus.custom.blob.size, bytes.length);
});
