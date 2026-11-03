import test from 'node:test';
import assert from 'node:assert/strict';
import { captureNoteTimezone, validateNoteTimezone } from './note-provenance.mjs';
import { createBrowserBridge } from './browser.mjs';

test('source offset follows the creation instant across DST and international date boundaries', () => {
  const early = Date.parse('2026-11-01T08:30:00Z');
  const late = Date.parse('2026-11-01T09:30:00Z');
  assert.deepEqual(captureNoteTimezone(early, 'America/Los_Angeles'), {
    source_timezone: 'America/Los_Angeles', source_utc_offset_minutes: -420,
  });
  assert.equal(captureNoteTimezone(late, 'America/Los_Angeles').source_utc_offset_minutes, -480);
  assert.equal(captureNoteTimezone(early, 'Asia/Shanghai').source_utc_offset_minutes, 480);
  assert.equal(captureNoteTimezone(early, 'Pacific/Kiritimati').source_utc_offset_minutes, 840);
  assert.equal(captureNoteTimezone(early, 'Asia/Kathmandu').source_utc_offset_minutes, 345);
});

test('unknown original zones stay unknown and malformed partial provenance is rejected', () => {
  assert.deepEqual(validateNoteTimezone({}), {});
  assert.deepEqual(validateNoteTimezone({ source_timezone: null, source_utc_offset_minutes: null }), {});
  for (const value of [
    { source_timezone: 'UTC' }, { source_utc_offset_minutes: 0 },
    { source_timezone: 'imaginary/zone', source_utc_offset_minutes: 0 },
    { source_timezone: 'UTC', source_utc_offset_minutes: 1440 },
    { source_timezone: 'UTC', source_utc_offset_minutes: 1.5 },
  ]) assert.throws(() => validateNoteTimezone(value));
});

test('travel while writing and reloading cannot rewrite a saved note’s original time zone', async () => {
  const values = new Map();
  const storage = { getItem: key => values.get(key) ?? null, setItem: (key, value) => values.set(key, value) };
  let now = Date.parse('2026-11-01T08:30:00Z'), zone = 'America/Los_Angeles', nextId = 0;
  const options = { storage, now: () => now, sourceTimezone: () => zone,
    makeId: () => `provenance-${++nextId}`, setInterval: () => 1, clearInterval() {} };
  const bridge = createBrowserBridge(options);
  const invoke = (op, data) => bridge.core.invoke('execute', { request: { op, data } });
  await bridge.core.invoke('snapshot');
  const original = (await invoke('switch_to_focus', { duration_ms: 60_000 })).view.period_note;
  assert.equal(original.source_utc_offset_minutes, -420);
  zone = 'Asia/Shanghai'; now += 30_000;
  const saved = (await invoke('save_period_note', { id: original.id, version: original.version, body: 'Words while travelling' })).view.period_note;
  assert.equal(saved.source_timezone, 'America/Los_Angeles');
  assert.equal(saved.created_at_utc_ms, original.created_at_utc_ms);
  const switched = (await invoke('switch_to_break', { duration_ms: 60_000 })).view;
  assert.equal(switched.archived_notes[0].source_timezone, 'America/Los_Angeles');
  assert.equal(switched.period_note.source_timezone, 'Asia/Shanghai');
  const reopened = await createBrowserBridge(options).core.invoke('snapshot');
  assert.equal(reopened.archived_notes[0].source_utc_offset_minutes, -420);
  assert.equal(reopened.period_note.source_utc_offset_minutes, 480);
});
