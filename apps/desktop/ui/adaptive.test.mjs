import test from 'node:test';
import assert from 'node:assert/strict';
import { AdaptiveStore, configureAdaptive, createAdaptiveState, recordReceptivity,
  recordRhythm, recommendReceptivity, recommendRhythm, resetLearning, validateAdaptiveState } from './adaptive.mjs';

const minute = 60_000;
const context = { hour: 10, context: 'work' };
const enabled = () => configureAdaptive(createAdaptiveState(), { enabled: true });
const focus = (state, count, actual = 40, planned = 25, overrides = {}) => {
  for (let index = 0; index < count; index++) state = recordRhythm(state, {
    ...context, kind: 'focus', actualMs: actual * minute, plannedMs: planned * minute, manual: true, ...overrides,
  });
  return state;
};
const suggestion = (state, extras = {}) => recommendRhythm(state, {
  ...context, kind: 'focus', baseMs: 25 * minute, ...extras,
});

test('learning starts off, records nothing without consent, and respects ordinary settings', () => {
  const state = createAdaptiveState();
  assert.equal(state.enabled, false);
  assert.deepEqual(focus(state, 20), state);
  assert.deepEqual(recordReceptivity(state, { ...context, response: 'ignored' }), state);
  assert.equal(suggestion(state, { baseMs: 80 * minute }).durationMs, 80 * minute);
});

test('five intentional observations permit one gradual step and do not mutate prior state', () => {
  const original = enabled();
  const four = focus(original, 4);
  assert.equal(suggestion(four).durationMs, 25 * minute);
  assert.equal(suggestion(four).reason, 'insufficient_samples');
  const five = focus(four, 1);
  assert.equal(suggestion(five).durationMs, 26 * minute);
  assert.equal(suggestion(five).reason, 'learned_longer');
  assert.equal(suggestion(focus(five, 4)).durationMs, 26 * minute);
  assert.equal(suggestion(focus(five, 5)).durationMs, 27 * minute);
  assert.deepEqual(original.rhythm, {});
});

test('early transitions shorten suggestions, but outliers and automatic deadlines do not', () => {
  assert.equal(suggestion(focus(enabled(), 5, 15)).durationMs, 24 * minute);
  const typical = focus(enabled(), 4, 25);
  assert.equal(suggestion(focus(typical, 1, 120)).durationMs, 25 * minute);
  assert.deepEqual(focus(enabled(), 5, 40, 25, { manual: false }).rhythm, {});
  assert.deepEqual(focus(enabled(), 5, 0.5).rhythm, {});
});

test('activity, time of day, and recovery have independent rhythm observations', () => {
  let state = focus(enabled(), 5);
  for (let index = 0; index < 5; index++) state = recordRhythm(state, {
    ...context, kind: 'break', manual: true, actualMs: 10 * minute, plannedMs: 5 * minute,
  });
  assert.equal(suggestion(state).durationMs, 26 * minute);
  assert.equal(suggestion(state, { context: 'study' }).durationMs, 25 * minute);
  assert.equal(suggestion(state, { hour: 15 }).durationMs, 25 * minute);
  assert.equal(suggestion(state, { kind: 'break', baseMs: 5 * minute }).durationMs, 6 * minute);
});

test('user boundaries constrain learning and a manual reset overrides learned duration', () => {
  let state = focus(enabled(), 30);
  state = configureAdaptive(state, { bounds: { focus: { maxMs: 27 * minute } } });
  assert.equal(suggestion(state).durationMs, 27 * minute);
  assert.equal(suggestion(state).reason, 'boundary');
  state = recordReceptivity(state, { ...context, response: 'snooze' });
  const reset = resetLearning(state, 'rhythm');
  assert.equal(suggestion(reset).durationMs, 25 * minute);
  assert.deepEqual(reset.receptivity, state.receptivity);
  assert.equal(reset.enabled, true);
  assert.equal(reset.bounds.focus.maxMs, 27 * minute);
  assert.throws(() => configureAdaptive(state, { bounds: { focus: { maxMs: minute } } }), /boundaries/);
});

test('ignored and dismissed notifications never change focus or recovery needs', () => {
  const state = focus(enabled(), 5, 15);
  let ignored = state;
  for (let index = 0; index < 20; index++) ignored = recordReceptivity(ignored, { ...context, response: 'ignored' });
  assert.deepEqual(ignored.rhythm, state.rhythm);
  assert.deepEqual(suggestion(ignored), suggestion(state));
  assert.deepEqual(suggestion(ignored, { kind: 'break', baseMs: 5 * minute }),
    suggestion(state, { kind: 'break', baseMs: 5 * minute }));
  assert.equal(recommendReceptivity(ignored, context).style, 'quiet');
  assert.equal(recommendReceptivity(ignored, context).delayMs, 0);
});

test('reminder learning responds to explicit snoozes while retaining reminders and bounded history', () => {
  let state = enabled();
  for (let index = 0; index < 5; index++) state = recordReceptivity(state, { ...context, response: 'snooze' });
  assert.equal(recommendReceptivity(state, context).delayMs, 2 * minute);
  assert.equal(recommendReceptivity(state, { ...context, hour: 22 }).delayMs, 0);
  for (let index = 0; index < 25; index++) state = recordReceptivity(state, { ...context, response: 'accept' });
  assert.equal(state.receptivity['work:morning'].recent.length, 20);
  assert.equal(recommendReceptivity(state, context).style, 'gentle');
  assert.equal(recommendReceptivity(state, context).delayMs, 0);
  assert.deepEqual(state.rhythm, {});
});

test('turning learning off stops collection and applying suggestions without deleting consent preferences', () => {
  const state = configureAdaptive(focus(enabled(), 5), { enabled: false });
  assert.deepEqual(focus(state, 5), state);
  assert.equal(suggestion(state).durationMs, 25 * minute);
  assert.equal(recommendReceptivity(state, context).reason, 'disabled');
  const cleared = resetLearning(state);
  assert.deepEqual(cleared.rhythm, {});
  assert.deepEqual(cleared.receptivity, {});
  assert.deepEqual(cleared.bounds, state.bounds);
});

test('storage round trips validated history and preserves unreadable bytes for recovery', () => {
  let raw = null;
  const storage = { getItem: () => raw, setItem: (_key, value) => { raw = value; } };
  const store = new AdaptiveStore({ storage });
  assert.deepEqual(store.load(), createAdaptiveState());
  const state = focus(enabled(), 7);
  store.save(state);
  assert.deepEqual(store.load(), state);
  raw = '{broken';
  assert.throws(() => store.load(), /Learning stays off/);
  assert.equal(raw, '{broken');
  raw = JSON.stringify({ ...state, enabled: 'yes' });
  assert.throws(() => store.load(), /Learning stays off/);
});

test('denied storage and invalid histories fail explicitly without claiming a save', () => {
  const store = new AdaptiveStore({ storage: {
    getItem: () => { throw new Error('Denied'); }, setItem: () => { throw new Error('Quota'); },
  } });
  assert.throws(() => store.load(), /Learning stays off/);
  assert.throws(() => store.save(enabled()), /could not be saved/);
  const state = focus(enabled(), 2);
  state.rhythm['work:morning'].focus.pending.push(0);
  assert.throws(() => validateAdaptiveState(state), /observations/);
  assert.throws(() => focus(enabled(), 1, 2000), /invalid/);
  assert.throws(() => recordReceptivity(enabled(), { ...context, response: 'approved' }), /invalid/);
});
