// Reminder timing is independent of focus/recovery duration and never uses sensors.
// The first version pools observations by personal state. Context/time fields are
// accepted for a future contextual model, but are not stored or used to split it.
const MINUTE = 60_000;
const DAY = 86_400_000;
const KEY = 'rhythm.reminder-delay.v1';
const KINDS = ['focus', 'break'];
const RESPONSES = ['accept', 'snooze', 'automatic', 'ignored'];
const CONTEXTS = ['general', 'work', 'study', 'creative'];
const clone = value => structuredClone(value);
const object = value => value !== null && typeof value === 'object' && !Array.isArray(value);
const validBase = value => Number.isInteger(value) && value >= MINUTE && value <= 60 * MINUTE;
const maximumAdjustment = baseMs => Math.min(9 * MINUTE, Math.floor(baseMs / 2), 60 * MINUTE - baseMs);

export function createReminderDelayState() {
  return { schema: 1, baseMs: 9 * MINUTE, enabled: false, kinds: {} };
}

export function validateReminderDelayState(value) {
  if (!object(value) || value.schema !== 1 || typeof value.enabled !== 'boolean'
    || !validBase(value.baseMs) || !object(value.kinds)
    || Object.keys(value).some(key => !['schema', 'baseMs', 'enabled', 'kinds'].includes(key))
    || Object.keys(value.kinds).some(kind => !KINDS.includes(kind))) {
    throw new Error('Saved reminder delay preferences could not be read.');
  }
  for (const sample of Object.values(value.kinds)) {
    if (!object(sample) || !Number.isSafeInteger(sample.samples) || sample.samples < 1
      || !Number.isInteger(sample.adjustmentMs) || sample.adjustmentMs < 0
      || sample.adjustmentMs > maximumAdjustment(value.baseMs)
      || !Array.isArray(sample.pending) || sample.pending.length > 4
      || sample.pending.length !== sample.samples % 5
      || sample.pending.some(signal => ![-1, 0, 1].includes(signal))
      || Object.keys(sample).some(key => !['samples', 'adjustmentMs', 'pending'].includes(key))) {
      throw new Error('Saved reminder delay observations could not be read.');
    }
  }
  return clone(value);
}

/** A changed manual duration starts fresh so an old offset cannot undo the edit.
 * Disabling learning preserves observations but uses the exact manual duration. */
export function configureReminderDelay(state, { baseMs, enabled } = {}) {
  const next = validateReminderDelayState(state);
  if (baseMs !== undefined) {
    if (!validBase(baseMs)) throw new Error('Choose a reminder delay between 1 and 60 minutes.');
    if (baseMs !== next.baseMs) next.kinds = {};
    next.baseMs = baseMs;
  }
  if (enabled !== undefined) {
    if (typeof enabled !== 'boolean') throw new Error('Reminder learning must be on or off.');
    next.enabled = enabled;
  }
  return next;
}

export function resetReminderDelay(state) {
  const next = validateReminderDelayState(state);
  next.kinds = {};
  return next;
}

function validateObservation(observation) {
  if (!KINDS.includes(observation.kind)
    || (observation.responseMs != null && (!Number.isFinite(observation.responseMs)
      || observation.responseMs < 0 || observation.responseMs > DAY))
    || !Number.isInteger(observation.postponements ?? 0)
    || (observation.postponements ?? 0) < 0 || (observation.postponements ?? 0) > 1_000
    || (observation.context !== undefined && !CONTEXTS.includes(observation.context))
    || (observation.at !== undefined && !Number.isFinite(observation.at))
    || (observation.hour !== undefined && (!Number.isInteger(observation.hour)
      || observation.hour < 0 || observation.hour > 23))) {
    throw new Error('The reminder response could not be recorded.');
  }
}

/** responseMs is visible response time for this invitation, not time spent away.
 * postponements counts EARLIER postponements in this same reminder chain.
 *
 * Repeated explicit postponements and consistently late responses after a delay
 * can lengthen it. Prompt acceptance can return a learned extension toward the
 * manual baseline; it does not justify guessing an earlier-than-requested time.
 * Five new explicit choices permit at most one minute of change, using a median
 * so a single unusual response cannot set the next delay. */
export function recordReminderResponse(state, observation = {}) {
  const next = validateReminderDelayState(state);
  if (!next.enabled) return next;
  if (!RESPONSES.includes(observation.response)) throw new Error('Unknown reminder response.');
  // Silence and the automatic default are never evidence of timing preference.
  if (['ignored', 'automatic'].includes(observation.response)) return next;
  validateObservation(observation);
  const earlier = observation.postponements ?? 0;
  const latency = observation.responseMs;
  let signal = 0;
  if (observation.response === 'snooze' && earlier >= 1) signal = 1;
  else if (earlier >= 1 && latency != null && latency >= 2 * MINUTE) signal = 1;
  else if (observation.response === 'accept' && earlier <= 1 && latency != null && latency <= 30_000) signal = -1;
  const sample = next.kinds[observation.kind] ??= { samples: 0, adjustmentMs: 0, pending: [] };
  sample.samples++;
  sample.pending.push(signal);
  if (sample.pending.length === 5) {
    const median = [...sample.pending].sort((a, b) => a - b)[2];
    sample.adjustmentMs = Math.max(0, Math.min(maximumAdjustment(next.baseMs), sample.adjustmentMs + median * MINUTE));
    sample.pending = [];
  }
  return next;
}

export function recommendReminderDelay(state, { kind = 'focus' } = {}) {
  const current = validateReminderDelayState(state);
  if (!KINDS.includes(kind)) throw new Error('Choose Focus or Break for this reminder.');
  const sample = current.kinds[kind];
  const adjustmentMs = current.enabled ? sample?.adjustmentMs ?? 0 : 0;
  const samples = sample?.samples ?? 0;
  return { durationMs: current.baseMs + adjustmentMs, baseMs: current.baseMs, adjustmentMs, samples,
    reason: !current.enabled ? 'disabled' : samples < 5 ? 'insufficient_samples'
      : adjustmentMs > 0 ? 'learned_longer' : 'manual_delay' };
}

/** Only a bounded local summary is persisted: no raw notes, timestamps, sensor
 * data, or judgment about productivity. Failed reads/writes leave old bytes alone. */
export class ReminderDelayStore {
  constructor({ storage, key = KEY } = {}) { this.storage = storage; this.key = key; }

  load() {
    let raw;
    try { raw = (this.storage ?? globalThis.localStorage).getItem(this.key); }
    catch { throw new Error('Reminder preferences could not be read on this device. Learning stays off.'); }
    if (raw === null) return createReminderDelayState();
    try { return validateReminderDelayState(JSON.parse(raw)); }
    catch { throw new Error('Saved reminder preferences could not be read. Existing data has been kept; learning stays off.'); }
  }

  save(value) {
    const next = validateReminderDelayState(value);
    try { (this.storage ?? globalThis.localStorage).setItem(this.key, JSON.stringify(next)); }
    catch { throw new Error('Reminder preferences could not be saved on this device. Please try again.'); }
    return next;
  }
}
