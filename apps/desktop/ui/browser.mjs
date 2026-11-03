// The browser preview uses its own local storage. The desktop app continues to
// use the durable Rust service; no preview data is uploaded or mixed into it.
import { mergeNotes, validateNotes } from './sync.mjs';
import { pageNoteHistory } from './note-history.mjs';
import { queryNoteHistory } from './history-query.mjs';
import { captureNoteTimezone, validateNoteTimezone } from './note-provenance.mjs';
const STORAGE_KEY = 'rhythm.browser.v1';
const MAX_PERIOD = 86_400_000;
const DEFAULT_DELAY = 9 * 60_000;
const MAX_DELAY = 60 * 60_000;
const defaults = { focus_ms: 1_500_000, short_rest_ms: 300_000, long_rest_ms: 1_800_000, focus_before_long_rest: 4 };
const clone = value => structuredClone(value);

function initialView() {
  return {
    revision: 0,
    checkpoint: { version: 1, settings: { ...defaults }, completed_focus: 0, completed_short_rests: 0, long_rest_due: false,
      state: { state: 'ready', data: { next: 'focus' } } },
    sessions: [], drafts: [], reflections: [], routines: [], invitations: [],
    routine_windows: [], selected_routine_window: null, routine_next: [],
    routine_error: null, routines_revision: 0,
    notes_revision: 0, period_note: null, archived_notes: [], pending_reminder: null,
  };
}

function validateSettings(settings) {
  for (const key of ['focus_ms', 'short_rest_ms', 'long_rest_ms']) {
    if (!Number.isInteger(settings?.[key]) || settings[key] < 1 || settings[key] > MAX_PERIOD) {
      throw new Error('Each period must be between 1 millisecond and 24 hours.');
    }
  }
  if (!Number.isInteger(settings.focus_before_long_rest) || settings.focus_before_long_rest < 1
    || settings.focus_before_long_rest > 1000) throw new Error('Choose 1 to 1,000 focus periods between long breaks.');
}

function reminderMatches(view, reminder) {
  const current = view.checkpoint.state, latest = view.sessions[0];
  if (!reminder || current.state !== 'ready' || latest?.id !== reminder.period_id || latest.outcome !== 'completed') return false;
  return reminder.kind === 'focus' ? latest.kind === 'focus' && ['short_rest', 'long_rest'].includes(current.data.next)
    : ['short_rest', 'long_rest'].includes(latest.kind) && current.data.next === 'focus';
}

function reminderFor(record) {
  return { period_id: record.id, kind: record.kind === 'focus' ? 'focus' : 'break', sequence: 1,
    due_at_utc_ms: record.detected_utc_ms ?? record.ended_utc_ms, available: true, postponements: 0 };
}

function validateSaved(value) {
  if (![1, 2, 3].includes(value?.schema) || !value.view || !Number.isInteger(value.view.revision)) throw new Error('Unsupported preview data.');
  const view = value.view;
  // Older checkpoints did not record recovery. Do not infer rest from focus counts.
  view.checkpoint.completed_short_rests ??= 0;
  if (!Number.isInteger(view.checkpoint.completed_short_rests) || view.checkpoint.completed_short_rests < 0) throw new Error('Invalid recovery count.');
  validateSettings(view.checkpoint?.settings);
  if (!Array.isArray(view.drafts) || !Array.isArray(view.sessions) || !Array.isArray(view.reflections)
    || !Number.isInteger(view.checkpoint.completed_focus) || view.checkpoint.completed_focus < 0
    || typeof view.checkpoint.long_rest_due !== 'boolean') throw new Error('Invalid preview data.');
  const state = view.checkpoint.state;
  if (!['ready', 'running'].includes(state?.state) || !state.data) throw new Error('Invalid preview state.');
  if (state.state === 'running') {
    const active = state.data;
    if (!['focus', 'short_rest', 'long_rest'].includes(active.kind) || typeof active.id !== 'string'
      || !Number.isFinite(active.started_utc_ms) || !Number.isInteger(active.planned_ms)
      || active.planned_ms < 1 || active.planned_ms > MAX_PERIOD) throw new Error('Invalid preview period.');
    validateSettings(active.settings_snapshot);
  }
  for (const draft of view.drafts) {
    validateNoteTimezone(draft);
    if (typeof draft.id !== 'string' || !Number.isInteger(draft.version) || draft.version < 1
      || typeof draft.content?.body !== 'string') throw new Error('Invalid preview note.');
  }
  if (value.schema >= 2) {
    if (!Number.isInteger(view.notes_revision) || view.notes_revision < 0 || !Array.isArray(view.archived_notes)) {
      throw new Error('Invalid period notes.');
    }
    const ids = new Set();
    for (const note of [...view.archived_notes, ...(view.period_note ? [view.period_note] : [])]) {
      validateNoteTimezone(note);
      if (typeof note?.id !== 'string' || !note.id || ids.has(note.id) || !['focus', 'break'].includes(note.kind)
        || typeof note.body !== 'string' || !Number.isInteger(note.version) || note.version < 1
        || !Number.isFinite(note.created_at_utc_ms) || !Number.isFinite(note.updated_at_utc_ms)
        || (note.archived_at_utc_ms !== null && !Number.isFinite(note.archived_at_utc_ms))) throw new Error('Invalid period note.');
      ids.add(note.id);
    }
    if (view.period_note?.archived_at_utc_ms != null || view.archived_notes.some(note => note.archived_at_utc_ms === null)) {
      throw new Error('Invalid period note scope.');
    }
  }
  if (value.schema < 3) {
    const latest = view.sessions[0];
    const candidate = latest?.outcome === 'completed' ? reminderFor(latest) : null;
    view.pending_reminder = reminderMatches(view, candidate) ? candidate : null;
  } else {
    const reminder = view.pending_reminder;
    if (reminder !== null && (typeof reminder !== 'object' || Array.isArray(reminder)
      || typeof reminder.period_id !== 'string' || !reminder.period_id || !['focus', 'break'].includes(reminder.kind)
      || !Number.isSafeInteger(reminder.sequence) || reminder.sequence < 1
      || !Number.isSafeInteger(reminder.due_at_utc_ms) || typeof reminder.available !== 'boolean'
      || !Number.isSafeInteger(reminder.postponements) || reminder.postponements < 0)) throw new Error('Invalid reminder state.');
    if (reminder && !reminderMatches(view, reminder)) view.pending_reminder = null;
  }
  return view;
}

/** Injectable clock/storage keep the browser adapter's persistence tests deterministic. */
export function createBrowserBridge(options = {}) {
  const now = options.now ?? Date.now;
  const schedule = options.setInterval ?? globalThis.setInterval;
  const unschedule = options.clearInterval ?? globalThis.clearInterval;
  const makeId = options.makeId ?? (() => globalThis.crypto.randomUUID());
  const listeners = new Map();
  let view = initialView();
  let timer = null;
  let disposed = false;
  let reportedError = '';
  let lastReminder = undefined;
  let loadedRaw = null;

  function emit(name, payload) {
    for (const listener of listeners.get(name) ?? []) listener({ payload: clone(payload) });
  }
  function publishReminder() {
    const encoded = JSON.stringify(view.pending_reminder);
    if (encoded !== lastReminder) {
      lastReminder = encoded;
      emit('rhythm:reminder', view.pending_reminder);
    }
  }
  function storage() { return options.storage ?? globalThis.localStorage; }
  function sourceTime(at) {
    const zone = typeof options.sourceTimezone === 'function' ? options.sourceTimezone() : options.sourceTimezone;
    return captureNoteTimezone(at, zone);
  }
  function emptyNote(kind, at) {
    return { id: makeId(), kind, body: '', version: 1, created_at_utc_ms: at,
      updated_at_utc_ms: at, archived_at_utc_ms: null, ...sourceTime(at) };
  }
  function changeNotePeriod(next, kind, at) {
    if (next.period_note) {
      next.period_note.archived_at_utc_ms = at;
      next.archived_notes.push(next.period_note);
    }
    next.period_note = kind === null ? null : emptyNote(kind, at);
    next.notes_revision++;
  }
  function migrateNotes(next) {
    const at = now();
    next.notes_revision = 1;
    next.period_note = null;
    next.archived_notes = next.drafts.filter(draft => draft.content.kind === 'thought'
      && !draft.record_id && !draft.content.session_id && draft.content.body.trim()).map(draft => ({
      id: makeId(), kind: 'break', body: draft.content.body, version: 1,
      created_at_utc_ms: draft.created_at_utc_ms ?? at, updated_at_utc_ms: draft.updated_at_utc_ms ?? at,
      archived_at_utc_ms: at,
    }));
    const state = next.checkpoint.state;
    if (state.state === 'running') next.period_note = emptyNote(state.data.kind === 'focus' ? 'focus' : 'break', at);
    else if (next.sessions.length || state.data.next !== 'focus') next.period_note = emptyNote('break', at);
  }
  function load() {
    let raw;
    try { raw = storage().getItem(STORAGE_KEY); }
    catch { throw new Error('Browser storage is unavailable. Your words remain in the open note; enable local storage to save them.'); }
    loadedRaw = raw;
    if (raw === null) { view = initialView(); return; }
    let saved;
    try { saved = JSON.parse(raw); view = validateSaved(saved); }
    catch { throw new Error('Rhythm could not read this browser’s saved preview. Existing data has been left untouched.'); }
    if (saved.schema < 3) {
      const next = clone(view);
      if (saved.schema === 1) migrateNotes(next);
      commit(next);
    }
  }
  function commit(next) {
    let currentRaw;
    try { currentRaw = storage().getItem(STORAGE_KEY); }
    catch { throw new Error('This browser could not save the change. Your words remain in the open note; free some local storage and try again.'); }
    if (currentRaw !== loadedRaw) throw new Error('Rhythm changed in another tab. Please try the choice again.');
    next.revision = view.revision + 1;
    const raw = JSON.stringify({ schema: 3, view: next });
    try { storage().setItem(STORAGE_KEY, raw); }
    catch { throw new Error('This browser could not save the change. Your words remain in the open note; free some local storage and try again.'); }
    view = next;
    loadedRaw = raw;
    reportedError = '';
  }
  function elapsed(active, at) { return Math.min(active.planned_ms, Math.max(0, at - active.started_utc_ms)); }
  function finish(next, active, at) {
    const measured = elapsed(active, at);
    const completed = measured >= active.planned_ms;
    const ended = active.started_utc_ms + measured;
    const record = {
      id: active.id, kind: active.kind, planned_ms: active.planned_ms, elapsed_ms: measured,
      started_utc_ms: active.started_utc_ms, ended_utc_ms: ended, detected_utc_ms: at,
      outcome: completed ? 'completed' : 'ended_early', recovered: false,
      intervals: measured ? [{ started_utc_ms: active.started_utc_ms, ended_utc_ms: ended, elapsed_ms: measured, mapping_certain: true }] : [],
    };
    const checkpoint = next.checkpoint;
    if (completed && active.kind === 'focus') {
      checkpoint.completed_focus++;
      checkpoint.long_rest_due ||= checkpoint.completed_short_rests >= active.settings_snapshot.focus_before_long_rest - 1;
    } else if (completed && active.kind === 'short_rest') {
      checkpoint.completed_short_rests++;
      checkpoint.long_rest_due ||= checkpoint.completed_short_rests >= active.settings_snapshot.focus_before_long_rest - 1;
    } else if (completed && active.kind === 'long_rest') {
      checkpoint.completed_focus = 0;
      checkpoint.completed_short_rests = 0;
      checkpoint.long_rest_due = false;
    }
    checkpoint.state = { state: 'ready', data: { next: completed && active.kind === 'focus'
      ? checkpoint.long_rest_due ? 'long_rest' : 'short_rest' : 'focus' } };
    next.sessions = [record, ...next.sessions.filter(item => item.id !== record.id)].slice(0, 100);
    next.pending_reminder = completed ? reminderFor(record) : null;
    return record;
  }
  function snapshot() {
    const result = clone(view);
    // Keep empty archives in storage so a save finishing after its deadline can
    // still target that original note. Only nonempty records belong in the UI.
    result.archived_notes = result.archived_notes.filter(note => note.body.trim()).reverse()
      .sort((a, b) => b.archived_at_utc_ms - a.archived_at_utc_ms || b.created_at_utc_ms - a.created_at_utc_ms)
      .slice(0, 100);
    const state = result.checkpoint.state;
    if (state.state === 'running') {
      state.data.elapsed_ms = elapsed(state.data, now());
      state.data.last_sample = { epoch: 'browser-preview', monotonic_ms: now(), utc_ms: now() };
      state.data.open_interval.elapsed_ms = state.data.elapsed_ms;
    }
    return result;
  }
  function timerUpdate(terminal = null) {
    return { checkpoint: snapshot().checkpoint, revision: view.revision, terminal,
      notes_revision: view.notes_revision, note_period_id: view.period_note?.id ?? null };
  }
  function poll() {
    load();
    const state = view.checkpoint.state;
    let terminal = null;
    if (state.state === 'running' && elapsed(state.data, now()) >= state.data.planned_ms) {
      const next = clone(view);
      terminal = finish(next, next.checkpoint.state.data, now());
      changeNotePeriod(next, 'break', now());
      commit(next);
    }
    const reminder = view.pending_reminder;
    if (reminder && !reminder.available && now() >= reminder.due_at_utc_ms) {
      const next = clone(view);
      next.pending_reminder.available = true;
      next.pending_reminder.sequence++;
      commit(next);
    }
    return terminal;
  }
  function startHeartbeat() {
    if (timer !== null || disposed) return;
    timer = schedule(() => {
      try {
        const previousRevision = view.revision;
        const terminal = poll();
        emit('rhythm:timer', timerUpdate(terminal));
        publishReminder();
        if (view.revision !== previousRevision && !terminal) emit('rhythm:external_change', null);
      } catch (error) {
        if (error.message !== reportedError) { reportedError = error.message; emit('rhythm:error', error.message); }
      }
    }, 1000);
    timer?.unref?.();
  }
  async function execute(request, expectedReminder = null) {
    load();
    const next = clone(view);
    if (expectedReminder === null && ['snooze_reminder', 'accept_reminder'].includes(request.op)) {
      throw new Error('A reminder choice needs its current invitation.');
    }
    if (expectedReminder !== null) {
      const current = next.pending_reminder;
      if (!reminderMatches(next, current) || !current.available || current.period_id !== expectedReminder.periodId
        || current.sequence !== expectedReminder.sequence) return null;
      if (request.op === 'accept_reminder') request = { op: current.kind === 'focus' ? 'switch_to_break' : 'switch_to_focus', data: request.data };
    }
    const at = now();
    const data = request.data ?? {};
    let terminal = null;
    let draft = null;
    let period_note = null;
    switch (request.op) {
      case 'snooze_reminder': {
        const durationMs = data.duration_ms ?? DEFAULT_DELAY;
        if (!Number.isInteger(durationMs) || durationMs < 60_000 || durationMs > MAX_DELAY) {
          throw new Error('Reminder delay must be between 1 and 60 minutes.');
        }
        // This is a reminder schedule, not another focus or rest period.
        next.pending_reminder = { ...next.pending_reminder, available: false,
          sequence: next.pending_reminder.sequence + 1, postponements: next.pending_reminder.postponements + 1,
          due_at_utc_ms: at + durationMs };
        break;
      }
      case 'import_period_notes': {
        validateNotes(data.notes);
        const before = storage().getItem(STORAGE_KEY);
        next.archived_notes = await mergeNotes(next.archived_notes, data.notes);
        if (storage().getItem(STORAGE_KEY) !== before) throw new Error('Notes changed during import. Please try the import again.');
        next.notes_revision++;
        break;
      }
      case 'extend_focus': {
        const active = next.checkpoint.state;
        if (active.state !== 'running' || active.data.kind !== 'focus'
          || elapsed(active.data, at) >= active.data.planned_ms) throw new Error('Only an active focus period can be extended.');
        if (!Number.isInteger(data.additional_ms) || data.additional_ms < 1
          || active.data.planned_ms + data.additional_ms > MAX_PERIOD) throw new Error('Focus must stay within 24 hours.');
        active.data.planned_ms += data.additional_ms;
        break;
      }
      case 'switch_to_focus':
      case 'switch_to_break': {
        const focus = request.op === 'switch_to_focus';
        const current = next.checkpoint.state;
        if (data.duration_ms != null && (!Number.isInteger(data.duration_ms) || data.duration_ms < 1 || data.duration_ms > MAX_PERIOD)) {
          throw new Error('Each period must be between 1 millisecond and 24 hours.');
        }
        if (current.state === 'running') {
          if ((current.data.kind === 'focus') === focus) {
            return { view: snapshot(), draft: null, period_note: null, terminal: null };
          }
          terminal = finish(next, current.data, at);
        }
        const settings = next.checkpoint.settings;
        const kind = focus ? 'focus' : next.checkpoint.long_rest_due ? 'long_rest' : 'short_rest';
        next.checkpoint.state = { state: 'running', data: {
          id: makeId(), kind, planned_ms: data.duration_ms ?? settings[`${kind === 'focus' ? 'focus' : kind}_ms`],
          elapsed_ms: 0, started_utc_ms: at, settings_snapshot: { ...settings },
          last_sample: { epoch: 'browser-preview', monotonic_ms: at, utc_ms: at }, intervals: [],
          open_interval: { started_utc_ms: at, elapsed_ms: 0, mapping_certain: true }, recovered: false,
        } };
        next.pending_reminder = null;
        changeNotePeriod(next, focus ? 'focus' : 'break', at);
        break;
      }
      case 'save_period_note': {
        period_note = next.period_note?.id === data.id ? next.period_note : next.archived_notes.find(note => note.id === data.id);
        if (!period_note || period_note.version !== data.version) {
          throw new Error('This period note changed in another tab. Your current words are still here; copy them before reloading.');
        }
        if (typeof data.body !== 'string') throw new Error('The note text is invalid.');
        period_note.body = data.body;
        period_note.version++;
        period_note.updated_at_utc_ms = at;
        next.notes_revision++;
        break;
      }
      case 'update_settings':
        validateSettings(data.settings);
        next.checkpoint.settings = clone(data.settings);
        break;
      case 'create_draft':
        if (!['thought', 'intention', 'future_self'].includes(data.content?.kind) || typeof data.content.body !== 'string') {
          throw new Error('The note could not be saved because its content is invalid.');
        }
        draft = { id: makeId(), record_id: null, base_revision_ids: [], content: clone(data.content),
          version: 1, created_at_utc_ms: at, updated_at_utc_ms: at, ...sourceTime(at) };
        next.drafts.push(draft);
        break;
      case 'save_draft':
        draft = next.drafts.find(item => item.id === data.id);
        if (!draft || draft.version !== data.version) throw new Error('This note changed in another tab. Your current words are still here; copy them before reloading.');
        if (typeof data.body !== 'string') throw new Error('The note text is invalid.');
        draft.content.body = data.body;
        draft.version++;
        draft.updated_at_utc_ms = at;
        break;
      default:
        throw new Error('This option is available in the Rhythm desktop app.');
    }
    commit(next);
    publishReminder();
    return { view: snapshot(), draft: clone(draft), period_note: clone(period_note), terminal, reminder: clone(view.pending_reminder) };
  }

  return {
    core: {
      async invoke(command, args = {}) {
        if (disposed) throw new Error('This preview has been closed.');
        switch (command) {
          case 'note_history': load(); return pageNoteHistory(view, args);
          case 'note_history_query': load(); return queryNoteHistory(view, args);
          case 'snapshot': {
            const terminal = poll();
            startHeartbeat();
            if (terminal) emit('rhythm:timer', timerUpdate(terminal));
            publishReminder();
            return snapshot();
          }
          case 'current_reminder': {
            const terminal = poll(); startHeartbeat();
            if (terminal) emit('rhythm:timer', timerUpdate(terminal));
            publishReminder();
            return clone(view.pending_reminder);
          }
          case 'execute': return execute(args.request);
          case 'respond_break_reminder': {
            if (!['accept', 'snooze'].includes(args.action)) throw new Error('Unknown break reminder choice.');
            if (typeof args.periodId !== 'string' || !args.periodId || !Number.isSafeInteger(args.sequence) || args.sequence < 1) return null;
            // Check eligibility against the same loaded state used for the change.
            return execute({ op: args.action === 'snooze' ? 'snooze_reminder' : 'accept_reminder',
              data: { duration_ms: args.durationMs ?? null } }, args);
          }
          case 'platform_status': return { is_browser: true, supports_routines: false, sleep_inclusive: true, notification_note: null };
          case 'set_notification_language':
          case 'ui_ready': return null;
          case 'set_fullscreen':
            if (args.enabled) await globalThis.document?.documentElement.requestFullscreen?.();
            else if (globalThis.document?.fullscreenElement) await globalThis.document.exitFullscreen();
            return null;
          default: throw new Error('This option is available in the Rhythm desktop app.');
        }
      },
    },
    event: {
      async listen(name, handler) {
        if (!listeners.has(name)) listeners.set(name, new Set());
        listeners.get(name).add(handler);
        return () => listeners.get(name)?.delete(handler);
      },
    },
    dispose() {
      disposed = true;
      if (timer !== null) unschedule(timer);
      listeners.clear();
    },
  };
}
