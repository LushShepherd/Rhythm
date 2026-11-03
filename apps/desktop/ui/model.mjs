export const LANGUAGES = ['en', 'zh-Hans', 'zh-Hant'];

export function resolveLanguage(preference, systemLanguages = ['en']) {
  if (LANGUAGES.includes(preference)) return preference;
  for (const language of systemLanguages) {
    const lower = language.toLowerCase();
    if (lower.startsWith('zh')) {
      return /hant|\b(tw|hk|mo)\b/.test(lower) ? 'zh-Hant' : 'zh-Hans';
    }
    if (lower.startsWith('en')) return 'en';
  }
  return 'en';
}

export function isActive(checkpoint) {
  return ['running', 'paused', 'recovery_required'].includes(checkpoint?.state?.state);
}

// Native execution can deliver its timer event after the command response at the
// same revision. Only a completed, still-ready checkpoint owns an invitation.
export function completedInvitation(update, { revision = -1, lastTerminalId = null, visible = false, focused = false } = {}) {
  const terminal = update?.terminal;
  if (!terminal || update.revision < revision || terminal.id === lastTerminalId
    || terminal.outcome !== 'completed' || update.checkpoint?.state?.state !== 'ready') return null;
  return { id: terminal.id, kind: terminal.kind, auto: terminal.kind === 'focus' && visible && focused };
}

export function reminderMatchesView(view, reminder) {
  const state = view?.checkpoint?.state, latest = view?.sessions?.[0];
  if (!reminder || state?.state !== 'ready' || latest?.id !== reminder.period_id || latest.outcome !== 'completed') return false;
  return reminder.kind === 'focus' ? latest.kind === 'focus' && ['short_rest', 'long_rest'].includes(state.data?.next)
    : reminder.kind === 'break' && ['short_rest', 'long_rest'].includes(latest.kind) && state.data?.next === 'focus';
}

// The interface has only three personal states. Legacy paused/recovery checkpoints
// are rest until the person explicitly chooses Focus again.
export function personalState(view, hasEntered = false) {
  const state = view?.checkpoint?.state;
  if (state?.state === 'running') return state.data.kind === 'focus' ? 'focus' : 'break';
  // Finishing Focus only creates an invitation; recovery starts after a choice.
  if (state?.state === 'ready' && ['short_rest', 'long_rest'].includes(state.data?.next)) return 'focus';
  if (state && state.state !== 'ready') return 'break';
  return hasEntered || view?.period_note?.kind === 'break' || view?.sessions?.length > 0
    || (state?.data?.next && state.data.next !== 'focus') ? 'break' : 'init';
}

/** A native interruption invalidates choices already waiting on an editor save. */
export class LifecycleBoundary {
  constructor() { this.generation = 0; this.revision = -1; this.interrupted = false; this.preparing = false; }
  prepare() { this.generation++; this.interrupted = true; this.preparing = true; }
  replace(revision) {
    if (!Number.isSafeInteger(revision) || revision < 0) throw new Error('Invalid restored timer revision.');
    this.generation++; this.revision = revision; this.interrupted = true; this.preparing = false;
  }
  interrupt(revision) {
    if (!Number.isSafeInteger(revision) || revision < this.revision
      || (revision === this.revision && !this.preparing)) return false;
    this.generation++; this.revision = revision; this.interrupted = true; this.preparing = false;
    return true;
  }
  current(generation) { return generation === this.generation; }
  allowsRevision(revision) { return Number.isSafeInteger(revision) && revision >= this.revision; }
  observe(update) {
    if (!this.allowsRevision(update?.revision)) return false;
    // The worker permits a fresh Focus only after an explicit request. A later
    // running checkpoint also covers deliberate starts through the local CLI.
    if (!this.preparing && update.revision > this.revision
      && update.checkpoint?.state?.state === 'running' && update.checkpoint.state.data.kind === 'focus') {
      this.interrupted = false;
    }
    return true;
  }
}

// A completed request can arrive after a newer timer event. Never show older timer state.
export function mergeView(current, incoming) {
  if (!current) return incoming;
  let merged = incoming.revision >= current.revision ? incoming
    : { ...incoming, checkpoint: current.checkpoint, revision: current.revision };
  if ((current.routines_revision ?? -1) > (incoming.routines_revision ?? -1)) {
    merged = mergeRoutineState(merged, current);
  }
  if ((current.notes_revision ?? -1) > (incoming.notes_revision ?? -1)) {
    merged = { ...merged, notes_revision: current.notes_revision,
      period_note: current.period_note, archived_notes: current.archived_notes };
  }
  return merged;
}

// Routine changes and timer ticks have separate generations. Never resurrect an
// expired or dismissed invitation from a response that arrived after a newer event.
export function mergeRoutineState(current, incoming) {
  if (!current || (incoming.routines_revision ?? -1) < (current.routines_revision ?? -1)) return current;
  return { ...current, routines_revision: incoming.routines_revision,
    routines: incoming.routines, invitations: incoming.invitations,
    routine_windows: incoming.routine_windows, selected_routine_window: incoming.selected_routine_window,
    routine_next: incoming.routine_next, routine_error: incoming.routine_error };
}

// A legacy reminder keeps its old behavior until the person saves the editor.
// The editor offers its original fifteen-minute span as a starting point.
export function routineWindowsForEdit(routine) {
  if (routine?.windows?.length) return routine.windows.map(window => ({ ...window }));
  if (!routine?.local_time) return [{ start: '08:30', end: '12:00' }];
  const [hours, minutes] = routine.local_time.split(':').map(Number);
  const end = Math.min(hours * 60 + minutes + 15, 1440);
  return [{ start: routine.local_time,
    end: `${String(Math.floor(end / 60)).padStart(2, '0')}:${String(end % 60).padStart(2, '0')}` }];
}

// Time inputs use 00:00 for midnight; the API distinguishes the end of this day.
export function normalizedRoutineWindows(windows) {
  if (!windows.length || windows.length > 12) throw new Error('routineWindowCount');
  const result = windows.map(window => ({ start: window.start, end: window.end === '00:00' ? '24:00' : window.end }));
  for (const window of result) {
    if (!/^([01]\d|2[0-3]):[0-5]\d$/.test(window.start)
      || !/^(([01]\d|2[0-3]):[0-5]\d|24:00)$/.test(window.end)) throw new Error('routineWindowTime');
    if (window.end <= window.start) throw new Error('routineWindowOrder');
  }
  result.sort((a, b) => a.start.localeCompare(b.start));
  if (result.some((window, index) => index > 0 && window.start < result[index - 1].end)) {
    throw new Error('routineWindowOverlap');
  }
  return result;
}

/** A CLI quit owns the editor lock until durable shutdown or cancellation. */
export class CliQuitController {
  constructor({ blockedReason, lock, flush, prepare, onError = () => {} }) {
    this.blockedReason = blockedReason;
    this.lock = lock;
    this.flush = flush;
    this.prepare = prepare;
    this.onError = onError;
    this.token = null;
  }
  get active() { return this.token !== null; }
  async request(token) {
    if (typeof token !== 'string' || !token || this.token === token) return false;
    const blocked = this.blockedReason();
    if (blocked || this.active) {
      // Rejecting another request must not release an existing action's lock.
      try { await this.prepare(token, blocked || 'Rhythm is already preparing to quit.'); } catch { /* A timed-out request is already cancelled. */ }
      return false;
    }
    this.token = token;
    this.lock(true);
    try {
      await this.flush();
    } catch (error) {
      if (this.token !== token) return false;
      const message = typeof error === 'string' ? error : error?.message ?? String(error);
      try { await this.prepare(token, message); } catch { /* Keep the original save error. */ }
      this.cancel(token, error);
      return false;
    }
    // Cancellation while a save was pending invalidates its later readiness reply.
    if (this.token !== token) return false;
    try {
      await this.prepare(token, null);
      // Stay locked: Rust still has to commit the final timer checkpoint and reply
      // to the CLI before exiting. A commit failure arrives through cancel().
      return this.token === token;
    } catch (error) {
      this.cancel(token, error);
      return false;
    }
  }
  cancel(token, error) {
    if (this.token !== token) return false;
    this.token = null;
    this.lock(false);
    if (error) this.onError(error);
    return true;
  }
}

/** One editor owns its version and serializes all writes. Text remains in memory on failure. */
export class DraftController {
  constructor({ draft = null, content, request, onChange = () => {} }) {
    this.draft = draft;
    this.content = draft?.content ?? content;
    this.body = draft?.content.body ?? content?.body ?? '';
    this.savedBody = draft?.content.body ?? '';
    this.request = request;
    this.onChange = onChange;
    this.saving = false;
    this.failure = null;
    this.chain = Promise.resolve();
  }
  get dirty() { return this.body !== this.savedBody; }
  change(body) { this.body = body; this.failure = null; this.onChange(); }
  flush() {
    const operation = this.chain.catch(() => {}).then(async () => {
      this.saving = true;
      this.onChange();
      try {
        while (this.dirty) {
          const body = this.body;
          const response = this.draft
            ? await this.request('save_draft', { id: this.draft.id, version: this.draft.version, body })
            : await this.request('create_draft', { content: { ...this.content, body } });
          if (!response.draft) throw new Error('The saved draft was not returned.');
          this.draft = response.draft;
          this.savedBody = body;
          this.failure = null;
        }
      } catch (error) {
        this.failure = error;
        throw error;
      } finally {
        this.saving = false;
        this.onChange();
      }
      return this.draft;
    });
    this.chain = operation;
    return operation;
  }
}

/** A note owns its period id even if a deadline arrives during a pending save. */
export class PeriodNoteController extends DraftController {
  constructor({ note, request, onChange }) {
    if (!note?.id) throw new Error('A period is required before writing a note.');
    super({
      draft: { id: note.id, version: note.version, content: { body: note.body } },
      request: async (_op, data) => {
        const response = await request('save_period_note', data);
        const saved = response.period_note;
        if (!saved || saved.id !== note.id) throw new Error('The saved period note was not returned.');
        return { draft: { id: saved.id, version: saved.version, content: { body: saved.body } } };
      }, onChange,
    });
    this.periodId = note.id;
    this.kind = note.kind;
  }
  flushForInterruption(visibleBody = this.body) {
    // Preserve the current DOM text without committing or cancelling the IME.
    // A later compositionend can refine this same period, including after it
    // has been archived. Never retarget the write to the new Break note.
    this.change(visibleBody);
    return this.flush();
  }
}
