// Optional, device-local behavioral learning. These two models never share
// observations: a reminder response is not evidence about a person's need for rest.
const MINUTE = 60_000;
const DAY = 24 * 60 * MINUTE;
const KEY = 'rhythm.adaptive.v1';
const KINDS = ['focus', 'break'];
export const ADAPTIVE_CONTEXTS = ['general', 'work', 'study', 'creative'];
const RESPONSES = ['accept', 'snooze', 'dismiss', 'ignored'];
const PERIODS = ['morning', 'afternoon', 'evening', 'night'];
const clone = value => structuredClone(value);
const clamp = (value, min, max) => Math.min(max, Math.max(min, value));
const validMs = value => Number.isInteger(value) && value >= 1 && value <= DAY;

export function createAdaptiveState() {
  return {
    schema: 1, enabled: false, context: 'general',
    bounds: {
      focus: { minMs: 10 * MINUTE, maxMs: 60 * MINUTE },
      break: { minMs: 3 * MINUTE, maxMs: 15 * MINUTE },
    },
    rhythm: {}, receptivity: {},
  };
}

function periodFor({ at = Date.now(), hour } = {}) {
  const localHour = hour ?? new Date(at).getHours();
  if (!Number.isFinite(at) || !Number.isInteger(localHour) || localHour < 0 || localHour > 23) {
    throw new Error('The learning observation has an invalid time.');
  }
  return localHour >= 5 && localHour < 12 ? 'morning'
    : localHour >= 12 && localHour < 18 ? 'afternoon'
      : localHour >= 18 && localHour < 23 ? 'evening' : 'night';
}

function bucketKey(state, observation) {
  const context = observation.context ?? state.context;
  if (!ADAPTIVE_CONTEXTS.includes(context)) throw new Error('Choose a supported activity context.');
  return `${context}:${periodFor(observation)}`;
}

function assertBounds(bounds) {
  for (const kind of KINDS) {
    const value = bounds?.[kind];
    if (!validMs(value?.minMs) || !validMs(value?.maxMs) || value.minMs > value.maxMs) {
      throw new Error('Learning boundaries must be ordered durations between 1 millisecond and 24 hours.');
    }
  }
}

function object(value) { return Boolean(value) && typeof value === 'object' && !Array.isArray(value); }
function validKey(key) {
  const [context, period, extra] = key.split(':');
  return !extra && ADAPTIVE_CONTEXTS.includes(context) && PERIODS.includes(period);
}

/** Reject incompatible or corrupt saved data instead of silently opting a person in. */
export function validateAdaptiveState(value) {
  if (!object(value) || value.schema !== 1 || typeof value.enabled !== 'boolean'
    || !ADAPTIVE_CONTEXTS.includes(value.context) || !object(value.rhythm) || !object(value.receptivity)) {
    throw new Error('Saved learning preferences could not be read. Learning stays off until they are reset.');
  }
  assertBounds(value.bounds);
  if (Object.keys(value.rhythm).length > 16 || Object.keys(value.receptivity).length > 16) {
    throw new Error('Saved learning history is invalid.');
  }
  for (const [key, bucket] of Object.entries(value.rhythm)) {
    if (!validKey(key) || !object(bucket) || Object.keys(bucket).some(kind => !KINDS.includes(kind))) {
      throw new Error('Saved rhythm history is invalid.');
    }
    for (const sample of Object.values(bucket)) {
      if (!object(sample) || !Number.isSafeInteger(sample.samples) || sample.samples < 1
        || !Number.isInteger(sample.adjustmentMs) || Math.abs(sample.adjustmentMs) > DAY
        || !Array.isArray(sample.pending) || sample.pending.length > 4
        || sample.pending.length !== sample.samples % 5
        || sample.pending.some(delta => !Number.isInteger(delta) || Math.abs(delta) > DAY)) {
        throw new Error('Saved rhythm observations are invalid.');
      }
    }
  }
  for (const [key, sample] of Object.entries(value.receptivity)) {
    if (!validKey(key) || !object(sample) || !Number.isSafeInteger(sample.samples) || sample.samples < 1
      || !Array.isArray(sample.recent) || sample.recent.length !== Math.min(sample.samples, 20)
      || sample.recent.some(response => !RESPONSES.includes(response))) {
      throw new Error('Saved reminder observations are invalid.');
    }
  }
  return clone(value);
}

/** Partial bounds are supported; callers save the returned value after success. */
export function configureAdaptive(state, patch = {}) {
  const next = clone(state);
  if (patch.enabled !== undefined) {
    if (typeof patch.enabled !== 'boolean') throw new Error('Learning must be on or off.');
    next.enabled = patch.enabled;
  }
  if (patch.context !== undefined) {
    if (!ADAPTIVE_CONTEXTS.includes(patch.context)) throw new Error('Choose a supported activity context.');
    next.context = patch.context;
  }
  if (patch.bounds !== undefined) {
    for (const kind of KINDS) next.bounds[kind] = { ...next.bounds[kind], ...patch.bounds[kind] };
    assertBounds(next.bounds);
  }
  return next;
}

/** Forget history without changing consent or boundaries. Manual duration edits
 * should reset the rhythm model so old offsets cannot undo the person's choice. */
export function resetLearning(state, model = 'all') {
  if (!['all', 'rhythm', 'receptivity'].includes(model)) throw new Error('Unknown learning model.');
  const next = clone(state);
  if (model !== 'receptivity') next.rhythm = {};
  if (model !== 'rhythm') next.receptivity = {};
  return next;
}

/** Record an intentional Focus/Break transition, not an automatic deadline or
 * notification dismissal. Pass the actual time spent in the personal state,
 * including voluntary extensions. Long breaks are excluded by the caller.
 * Five new observations permit at most one minute of change. A median prevents
 * a single unusual period from determining the next suggestion. */
export function recordRhythm(state, observation) {
  if (!state.enabled || observation.manual !== true) return clone(state);
  const { kind, actualMs, plannedMs } = observation;
  if (!KINDS.includes(kind) || !validMs(actualMs) || !validMs(plannedMs)) {
    throw new Error('The focus or recovery observation is invalid.');
  }
  // Accidental rapid switches do not express a useful duration preference.
  if (actualMs < MINUTE) return clone(state);
  const key = bucketKey(state, observation);
  const next = clone(state);
  const bucket = next.rhythm[key] ??= {};
  const sample = bucket[kind] ??= { samples: 0, adjustmentMs: 0, pending: [] };
  sample.samples++;
  sample.pending.push(actualMs - plannedMs);
  if (sample.pending.length === 5) {
    const median = [...sample.pending].sort((a, b) => a - b)[2];
    // Small variations around a deadline are normal, not a preference change.
    const step = Math.abs(median) < 30_000 ? 0 : clamp(median, -MINUTE, MINUTE);
    sample.adjustmentMs = clamp(sample.adjustmentMs + step, -DAY, DAY);
    sample.pending = [];
  }
  return next;
}

/** Only short recovery suggestions use kind:'break'. A long recovery keeps its
 * separately configured duration. baseMs is always the person's current setting. */
export function recommendRhythm(state, { kind, baseMs, ...observation }) {
  if (!KINDS.includes(kind) || !validMs(baseMs)) throw new Error('The requested rhythm is invalid.');
  const key = bucketKey(state, observation);
  const sample = state.rhythm[key]?.[kind];
  if (!state.enabled) return { durationMs: baseMs, reason: 'disabled', samples: 0, adjustmentMs: 0 };
  const adjustmentMs = sample?.adjustmentMs ?? 0;
  const bounds = state.bounds[kind];
  const durationMs = clamp(baseMs + adjustmentMs, bounds.minMs, bounds.maxMs);
  const reason = durationMs !== baseMs + adjustmentMs ? 'boundary'
    : (sample?.samples ?? 0) < 5 ? 'insufficient_samples'
      : adjustmentMs < 0 ? 'learned_shorter' : adjustmentMs > 0 ? 'learned_longer' : 'within_usual_rhythm';
  return { durationMs, reason, samples: sample?.samples ?? 0, adjustmentMs: durationMs - baseMs };
}

/** This function never reads or writes rhythm durations. Ignored reminders can
 * influence presentation only; reminders remain available and rest is not waived. */
export function recordReceptivity(state, observation) {
  if (!state.enabled) return clone(state);
  if (!RESPONSES.includes(observation.response)) throw new Error('The reminder response is invalid.');
  const key = bucketKey(state, observation);
  const next = clone(state);
  const sample = next.receptivity[key] ??= { samples: 0, recent: [] };
  sample.samples++;
  sample.recent = [...sample.recent, observation.response].slice(-20);
  return next;
}

/** A bounded, optional delay applies to reminder delivery only, never deadlines.
 * Ignoring a reminder does not delay it: only explicit snoozes teach a delay. */
export function recommendReceptivity(state, observation = {}) {
  const key = bucketKey(state, observation);
  const sample = state.receptivity[key];
  if (!state.enabled) return { style: 'gentle', delayMs: 0, reason: 'disabled', samples: 0 };
  if (!sample || sample.samples < 5) {
    return { style: 'gentle', delayMs: 0, reason: 'insufficient_samples', samples: sample?.samples ?? 0 };
  }
  const responses = sample.recent;
  const count = response => responses.filter(value => value === response).length;
  const quieter = (count('snooze') + count('dismiss') + count('ignored')) / responses.length >= 0.6;
  const delayMs = count('snooze') / responses.length >= 0.6 ? 2 * MINUTE : 0;
  return { style: quieter ? 'quiet' : 'gentle', delayMs,
    reason: delayMs ? 'frequent_snoozes' : quieter ? 'quieter_reminders' : 'receptive', samples: sample.samples };
}

/** Synchronous storage keeps saves explicit and reviewable by the UI. Read errors
 * leave saved bytes untouched. Save errors must be shown before claiming success. */
export class AdaptiveStore {
  constructor({ storage, key = KEY } = {}) {
    this.storage = storage;
    this.key = key;
  }
  load() {
    let raw;
    try { raw = (this.storage ?? globalThis.localStorage).getItem(this.key); }
    catch { throw new Error('Learning preferences could not be read on this device. Learning stays off.'); }
    if (raw === null) return createAdaptiveState();
    try { return validateAdaptiveState(JSON.parse(raw)); }
    catch { throw new Error('Saved learning preferences could not be read. Learning stays off until they are reset.'); }
  }
  save(value) {
    const next = validateAdaptiveState(value);
    try { (this.storage ?? globalThis.localStorage).setItem(this.key, JSON.stringify(next)); }
    catch { throw new Error('Learning preferences could not be saved on this device. Please try again.'); }
    return next;
  }
}
