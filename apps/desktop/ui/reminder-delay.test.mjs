import test from 'node:test';
import assert from 'node:assert/strict';
import { createReminderDelayState, configureReminderDelay, recordReminderResponse,
  recommendReminderDelay, resetReminderDelay, ReminderDelayStore, validateReminderDelayState } from './reminder-delay.mjs';

const MINUTE = 60_000;
const learning = () => configureReminderDelay(createReminderDelayState(), { enabled: true });
const repeatedDelay = { kind: 'focus', response: 'snooze', responseMs: 2_000, postponements: 1 };
function repeat(state, count, response = repeatedDelay) {
  for (let index = 0; index < count; index++) state = recordReminderResponse(state, response);
  return state;
}

test('default is nine minutes, independent learning starts off, and no consent means no observations', () => {
  const state = createReminderDelayState();
  assert.equal(state.enabled, false);
  assert.equal(recommendReminderDelay(state).durationMs, 9 * MINUTE);
  assert.deepEqual(repeat(state, 30), state);
});

test('one unusual interaction cannot set timing; five consistent choices allow one gradual step', () => {
  const state = repeat(learning(), 4);
  assert.equal(recommendReminderDelay(state).durationMs, 9 * MINUTE);
  const learned = repeat(state, 1);
  assert.equal(recommendReminderDelay(learned).durationMs, 10 * MINUTE);
  const mixed = repeat(learning(), 1);
  const neutral = repeat(mixed, 4, { ...repeatedDelay, postponements: 0 });
  assert.equal(recommendReminderDelay(neutral).durationMs, 9 * MINUTE);
});

test('response latency after postponement informs recurring patterns without raw event retention', () => {
  const state = repeat(learning(), 5, { kind: 'break', response: 'accept', responseMs: 130_000, postponements: 1,
    context: 'study', hour: 8, at: 1_800_000_000_000 });
  assert.equal(recommendReminderDelay(state, { kind: 'break' }).durationMs, 10 * MINUTE);
  assert.deepEqual(Object.keys(state.kinds.break).sort(), ['adjustmentMs', 'pending', 'samples']);
  assert.equal(recommendReminderDelay(state, { kind: 'focus' }).durationMs, 9 * MINUTE);
});

test('prompt acceptance gradually returns extensions toward the manual baseline without guessing shorter needs', () => {
  let state = repeat(learning(), 10);
  assert.equal(recommendReminderDelay(state).durationMs, 11 * MINUTE);
  const accepted = { kind: 'focus', response: 'accept', responseMs: 4_000, postponements: 1 };
  state = repeat(state, 5, accepted);
  assert.equal(recommendReminderDelay(state).durationMs, 10 * MINUTE);
  state = repeat(state, 30, accepted);
  assert.equal(recommendReminderDelay(state).durationMs, 9 * MINUTE);
  const afterMany = repeat(repeat(learning(), 5), 5, { ...accepted, postponements: 3 });
  assert.equal(recommendReminderDelay(afterMany).durationMs, 10 * MINUTE);
});

test('ignored and automatic choices neither train nor change a learned delay', () => {
  const state = repeat(learning(), 6);
  for (const response of ['automatic', 'ignored']) {
    assert.deepEqual(repeat(state, 100, { ...repeatedDelay, response }), state);
  }
});

test('manual editing overrides learned offsets, disabling applies the exact manual duration, reset preserves preferences', () => {
  const learned = repeat(learning(), 10);
  const disabled = configureReminderDelay(learned, { enabled: false });
  assert.equal(recommendReminderDelay(disabled).durationMs, 9 * MINUTE);
  assert.deepEqual(disabled.kinds, learned.kinds);
  assert.equal(recommendReminderDelay(configureReminderDelay(disabled, { enabled: true })).durationMs, 11 * MINUTE);
  const manual = configureReminderDelay(learned, { baseMs: 15 * MINUTE });
  assert.deepEqual(manual.kinds, {});
  assert.equal(recommendReminderDelay(manual).durationMs, 15 * MINUTE);
  assert.deepEqual(resetReminderDelay(learned), { ...learning(), kinds: {} });
  assert.deepEqual(configureReminderDelay(learned, { baseMs: learned.baseMs }).kinds, learned.kinds);
});

test('learned changes remain bounded around the manual preference and at the maximum allowed delay', () => {
  const extended = repeat(learning(), 100);
  assert.equal(recommendReminderDelay(extended).durationMs, 13.5 * MINUTE);
  const high = repeat(configureReminderDelay(learning(), { baseMs: 59 * MINUTE }), 100);
  assert.equal(recommendReminderDelay(high).durationMs, 60 * MINUTE);
  assert.throws(() => configureReminderDelay(learning(), { baseMs: 61 * MINUTE }), /between/);
  assert.throws(() => configureReminderDelay(learning(), { baseMs: 0 }), /between/);
});

test('contexts remain pooled in this first version while Focus and Break learn independently', () => {
  let state = repeat(learning(), 3, { ...repeatedDelay, context: 'work', hour: 8 });
  state = repeat(state, 2, { ...repeatedDelay, context: 'creative', hour: 22 });
  assert.equal(recommendReminderDelay(state, { kind: 'focus', context: 'study', hour: 12 }).durationMs, 10 * MINUTE);
  assert.equal(recommendReminderDelay(state, { kind: 'break' }).durationMs, 9 * MINUTE);
});

test('local storage round trips bounded summaries and preserves corrupt or unwritable data', () => {
  const values = new Map();
  const storage = { getItem: key => values.get(key) ?? null, setItem: (key, value) => values.set(key, value) };
  const store = new ReminderDelayStore({ storage, key: 'test-delay' });
  assert.deepEqual(store.load(), createReminderDelayState());
  const learned = repeat(learning(), 12);
  store.save(learned);
  assert.deepEqual(store.load(), learned);
  const raw = values.get('test-delay');
  const denied = new ReminderDelayStore({ storage: { ...storage, setItem() { throw new Error('Denied'); } }, key: 'test-delay' });
  assert.throws(() => denied.save(createReminderDelayState()), /could not be saved/);
  assert.equal(values.get('test-delay'), raw);
  values.set('test-delay', '{bad data');
  assert.throws(() => store.load(), /Existing data has been kept/);
  assert.equal(values.get('test-delay'), '{bad data');
});

test('invalid histories and observations are rejected without mutating the caller', () => {
  const state = learning();
  assert.throws(() => recordReminderResponse(state, { ...repeatedDelay, responseMs: -1 }), /could not be recorded/);
  assert.throws(() => recordReminderResponse(state, { ...repeatedDelay, postponements: -1 }), /could not be recorded/);
  assert.throws(() => recordReminderResponse(state, { ...repeatedDelay, hour: 24 }), /could not be recorded/);
  assert.throws(() => validateReminderDelayState({ ...state, enabled: 'yes' }), /could not be read/);
  assert.throws(() => validateReminderDelayState({ ...state, kinds: { focus: { samples: 6, adjustmentMs: 0, pending: [] } } }), /observations/);
  assert.deepEqual(state, learning());
  assert.deepEqual(repeat(state, 5, { kind: 'focus', response: 'accept' }).kinds.focus.adjustmentMs, 0);
});
