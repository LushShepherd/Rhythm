import test from 'node:test';
import assert from 'node:assert/strict';
import { CliQuitController, DraftController, LifecycleBoundary, PeriodNoteController, mergeRoutineState, mergeView, normalizedRoutineWindows, personalState, reminderMatchesView, resolveLanguage, routineWindowsForEdit } from './model.mjs';
import { copy } from './copy.mjs';
import { rhythmCopy } from './rhythm-copy.mjs';

test('Focus remains visible while awaiting rest; fresh starts are Init and finished rest stays Break', () => {
  const ready = { checkpoint: { state: { state: 'ready', data: { next: 'focus' } } }, sessions: [] };
  assert.equal(personalState(ready), 'init');
  assert.equal(personalState(ready, true), 'break');
  assert.equal(personalState({ ...ready, period_note: { kind: 'break' } }), 'break');
  assert.equal(personalState({ ...ready, sessions: [{ kind: 'short_rest' }] }), 'break');
  for (const next of ['short_rest', 'long_rest']) {
    assert.equal(personalState({ checkpoint: { state: { state: 'ready', data: { next } } } }, true), 'focus');
  }
  for (const state of ['paused', 'recovery_required']) {
    assert.equal(personalState({ checkpoint: { state: { state, data: { kind: 'focus' } } } }), 'break');
  }
  for (const kind of ['focus', 'short_rest', 'long_rest']) {
    assert.equal(personalState({ checkpoint: { state: { state: 'running', data: { kind } } } }), kind === 'focus' ? 'focus' : 'break');
  }
});

test('restoring another database rebases revisions and invalidates old pending requests', () => {
  const boundary = new LifecycleBoundary();
  boundary.interrupt(900);
  const oldRequest = boundary.generation;
  boundary.replace(4);
  assert.equal(boundary.current(oldRequest), false);
  assert.equal(boundary.preparing, false);
  assert.equal(boundary.interrupted, true);
  assert.equal(boundary.observe({ revision: 5, checkpoint: { state: { state: 'running', data: { kind: 'focus' } } } }), true);
  assert.equal(boundary.interrupted, false);
});

test('a lifecycle interruption cancels pending choices and rejects old updates until a fresh explicit Focus', () => {
  const boundary = new LifecycleBoundary();
  const choice = boundary.generation;
  boundary.prepare();
  assert.equal(boundary.current(choice), false);
  assert.equal(boundary.interrupted, true);
  assert.equal(boundary.preparing, true);
  const ready = { revision: 12, checkpoint: { state: { state: 'ready', data: { next: 'focus' } } } };
  assert.equal(boundary.interrupt(12), true);
  assert.equal(boundary.preparing, false);
  assert.equal(boundary.observe({ ...ready, revision: 11 }), false);
  assert.equal(boundary.observe(ready), true);
  assert.equal(boundary.interrupted, true);
  const focus = { revision: 13, checkpoint: { state: { state: 'running', data: { kind: 'focus', id: 'new' } } } };
  assert.equal(boundary.observe({ ...focus, revision: 12 }), true);
  assert.equal(boundary.interrupted, true);
  assert.equal(boundary.observe(focus), true);
  assert.equal(boundary.interrupted, false);
  // A delayed duplicate interruption cannot cancel a newer deliberate period.
  assert.equal(boundary.interrupt(12), false);
  assert.equal(boundary.interrupted, false);
});

test('a second lifecycle notice also fences requests when its ready checkpoint has not changed', () => {
  const boundary = new LifecycleBoundary();
  boundary.prepare(); boundary.interrupt(4);
  const old = boundary.generation;
  boundary.prepare();
  assert.equal(boundary.current(old), false);
  assert.equal(boundary.observe({ revision: 5, checkpoint: { state: { state: 'running', data: { kind: 'focus' } } } }), true);
  assert.equal(boundary.interrupted, true);
  assert.equal(boundary.preparing, true);
  assert.equal(boundary.interrupt(4), true);
  assert.equal(boundary.preparing, false);
  assert.equal(boundary.interrupted, true);
});

test('a late pre-interruption reminder cannot replace the newly started Focus or a newer completion', () => {
  const reminder = { period_id: 'old-break', kind: 'break', sequence: 1 };
  const view = { checkpoint: { state: { state: 'ready', data: { next: 'focus' } } },
    sessions: [{ id: 'old-break', kind: 'short_rest', outcome: 'completed' }] };
  assert.equal(reminderMatchesView(view, reminder), true);
  assert.equal(reminderMatchesView({ ...view, checkpoint: { state: { state: 'running', data: { kind: 'focus', id: 'new-focus' } } } }, reminder), false);
  assert.equal(reminderMatchesView({ ...view, sessions: [{ id: 'newer-break', kind: 'short_rest', outcome: 'completed' }, ...view.sessions] }, reminder), false);
  assert.equal(reminderMatchesView({ ...view, sessions: [{ id: 'old-break', kind: 'short_rest', outcome: 'ended_early' }] }, reminder), false);
  assert.equal(reminderMatchesView(null, reminder), false);
});

test('lifecycle save preserves visible composition and its final text in the original archived note', async () => {
  const started = deferred(), release = deferred();
  const writes = [];
  let archived = false;
  const editor = new PeriodNoteController({
    note: { id: 'interrupted-focus', kind: 'focus', body: 'Earlier', version: 3 },
    request: async (op, data) => {
      writes.push({ op, ...data, archived });
      if (writes.length === 1) { started.resolve(); await release.promise; }
      return { period_note: { id: data.id, body: data.body, version: data.version + 1 } };
    },
  });
  const saved = editor.flushForInterruption('Keep this\n候');
  await started.promise;
  archived = true;
  // Compositionend refines the old controller even though a Break note now exists.
  editor.change('Keep this\n候選文字');
  release.resolve();
  await saved;
  assert.deepEqual(writes.map(({ id, version, body }) => ({ id, version, body })), [
    { id: 'interrupted-focus', version: 3, body: 'Keep this\n候' },
    { id: 'interrupted-focus', version: 4, body: 'Keep this\n候選文字' },
  ]);
  assert.equal(writes[1].archived, true);
  assert.equal(editor.dirty, false);
  assert.equal(editor.body, 'Keep this\n候選文字');
});

test('failed lifecycle save retains the visible note text and its original version for retry', async () => {
  let fail = true;
  const editor = new PeriodNoteController({
    note: { id: 'old-period', kind: 'focus', body: 'Saved', version: 2 },
    request: async (_op, data) => {
      if (fail) throw new Error('Storage unavailable');
      return { period_note: { id: data.id, body: data.body, version: data.version + 1 } };
    },
  });
  await assert.rejects(editor.flushForInterruption('Visible unsaved text\n尚未完成'), /Storage unavailable/);
  assert.equal(editor.periodId, 'old-period');
  assert.equal(editor.body, 'Visible unsaved text\n尚未完成');
  assert.equal(editor.dirty, true);
  assert.equal(editor.draft.version, 2);
  fail = false;
  await editor.flush();
  assert.equal(editor.dirty, false);
  assert.equal(editor.draft.version, 3);
});

test('the three-state interface is translated into all supported languages', () => {
  for (const language of ['zh-Hans', 'zh-Hant']) assert.deepEqual(Object.keys(rhythmCopy[language]).sort(), Object.keys(rhythmCopy.en).sort());
});

test('late note replies cannot restore the previous period after a boundary', () => {
  const current = { revision: 5, notes_revision: 9, period_note: { id: 'break' }, archived_notes: [{ id: 'focus' }] };
  const late = { revision: 6, notes_revision: 8, period_note: { id: 'focus' }, archived_notes: [] };
  assert.deepEqual(mergeView(current, late), { ...current, revision: 6 });
});

test('a period editor keeps the original id and serializes words typed while it archives', async () => {
  let release, started;
  const pending = new Promise(resolve => { release = resolve; });
  const firstStarted = new Promise(resolve => { started = resolve; });
  const writes = [];
  const editor = new PeriodNoteController({ note: { id: 'focus-1', kind: 'focus', body: '', version: 1 },
    request: async (op, data) => {
      writes.push({ op, ...data });
      if (writes.length === 1) { started(); await pending; }
      return { period_note: { ...data, version: data.version + 1, archived_at_utc_ms: 123 } };
    },
  });
  editor.change('First line'); const saving = editor.flush(); await firstStarted;
  editor.change('First line\n最后的字'); release(); await saving;
  assert.deepEqual(writes.map(write => [write.op, write.id, write.version]), [
    ['save_period_note', 'focus-1', 1], ['save_period_note', 'focus-1', 2],
  ]);
  assert.equal(writes[1].body, 'First line\n最后的字');
  assert.equal(editor.dirty, false);
});

test('a failed period save preserves text and retries the original version', async () => {
  let failing = true;
  const editor = new PeriodNoteController({ note: { id: 'break-1', kind: 'break', body: 'Before', version: 3 },
    request: async (_op, data) => {
      assert.equal(data.id, 'break-1'); assert.equal(data.version, 3);
      if (failing) throw new Error('Storage unavailable');
      return { period_note: { ...data, version: 4 } };
    },
  });
  editor.change('Keep these words'); await assert.rejects(editor.flush(), /Storage unavailable/);
  assert.equal(editor.body, 'Keep these words'); assert.equal(editor.dirty, true);
  failing = false; await editor.flush(); assert.equal(editor.dirty, false);
});

test('all supported languages contain the complete interface vocabulary', () => {
  for (const language of ['zh-Hans', 'zh-Hant']) {
    assert.deepEqual(Object.keys(copy[language]).sort(), Object.keys(copy.en).sort());
  }
});

test('Chinese system preferences preserve Traditional script and explicit override', () => {
  for (const locale of ['zh-TW', 'zh-HK', 'zh-MO', 'zh-Hant-US']) {
    assert.equal(resolveLanguage('system', [locale]), 'zh-Hant');
  }
  assert.equal(resolveLanguage('system', ['zh-CN']), 'zh-Hans');
  assert.equal(resolveLanguage('en', ['zh-TW']), 'en');
  assert.equal(resolveLanguage('system', ['fr-FR']), 'en');
});

test('a late response cannot rewind the timer revision', () => {
  const current = { revision: 5, checkpoint: { state: 'new' } };
  const incoming = { revision: 4, checkpoint: { state: 'old' }, drafts: ['saved'] };
  assert.deepEqual(mergeView(current, incoming), { ...incoming, ...current });
});

test('late responses cannot resurrect dismissed invitations or older routine definitions', () => {
  const current = { revision: 4, checkpoint: 'current timer', routines_revision: 8,
    routines: [{ id: 'a', enabled: false }], invitations: [], routine_next: [], routine_error: null };
  const incoming = { revision: 5, checkpoint: 'new timer', routines_revision: 7,
    routines: [{ id: 'a', enabled: true }], invitations: [{ routine_id: 'a' }], drafts: ['saved'] };
  const result = mergeView(current, incoming);
  assert.equal(result.revision, 5);
  assert.equal(result.checkpoint, 'new timer');
  assert.equal(result.routines_revision, 8);
  assert.deepEqual(result.invitations, []);
  assert.deepEqual(result.routines, current.routines);
  assert.deepEqual(result.drafts, ['saved']);
  assert.equal(mergeRoutineState(current, incoming), current);
});

test('new routine state does not rewind an independent newer timer event', () => {
  const current = { revision: 9, checkpoint: 'running', routines_revision: 3, invitations: [] };
  const incoming = { revision: 8, checkpoint: 'ready', routines_revision: 4,
    routines: [], invitations: [{ routine_id: 'a' }], routine_next: [], routine_error: null };
  const result = mergeView(current, incoming);
  assert.equal(result.revision, 9);
  assert.equal(result.checkpoint, 'running');
  assert.equal(result.routines_revision, 4);
  assert.deepEqual(result.invitations, incoming.invitations);
});

test('expired routine windows and selection cannot return with a late timer response', () => {
  const oldWindow = { routine_id: 'weekdays', due_at_utc_ms: 1, expires_at_utc_ms: 2,
    window: { start: '08:30', end: '12:00' }, focus_ms: 1800000 };
  const current = { revision: 9, checkpoint: 'running', routines_revision: 12,
    routine_windows: [], selected_routine_window: null };
  const incoming = { revision: 10, checkpoint: 'paused', routines_revision: 11,
    routine_windows: [oldWindow], selected_routine_window: oldWindow };
  const result = mergeView(current, incoming);
  assert.equal(result.checkpoint, 'paused');
  assert.deepEqual(result.routine_windows, []);
  assert.equal(result.selected_routine_window, null);
  const newWindow = { ...oldWindow, due_at_utc_ms: 3, window: { start: '14:30', end: '18:00' } };
  const updated = mergeRoutineState(result, { routines_revision: 13,
    routine_windows: [newWindow], selected_routine_window: newWindow });
  assert.equal(updated.checkpoint, 'paused');
  assert.deepEqual(updated.selected_routine_window, newWindow);
});

test('the routine editor preserves split ranges and converts legacy reminders only in its copy', () => {
  const legacy = { local_time: '09:00' };
  assert.deepEqual(routineWindowsForEdit(legacy), [{ start: '09:00', end: '09:15' }]);
  assert.deepEqual(legacy, { local_time: '09:00' });
  assert.deepEqual(routineWindowsForEdit({ local_time: '23:55' }), [{ start: '23:55', end: '24:00' }]);
  const existing = { windows: [{ start: '08:30', end: '12:00' }, { start: '14:30', end: '18:00' }] };
  const editable = routineWindowsForEdit(existing);
  editable[0].start = '09:00';
  assert.equal(existing.windows[0].start, '08:30');
});

test('split ranges sort for saving and midnight remains the end of the same day', () => {
  const ranges = [{ start: '14:30', end: '18:00' }, { start: '08:30', end: '12:00' }];
  assert.deepEqual(normalizedRoutineWindows(ranges), [ranges[1], ranges[0]]);
  assert.equal(ranges[0].start, '14:30');
  assert.deepEqual(normalizedRoutineWindows([{ start: '22:00', end: '00:00' }]), [{ start: '22:00', end: '24:00' }]);
  assert.doesNotThrow(() => normalizedRoutineWindows([{ start: '08:30', end: '12:00' }, { start: '12:00', end: '18:00' }]));
});

test('invalid or overlapping daily ranges cannot be submitted', () => {
  assert.throws(() => normalizedRoutineWindows([]), /routineWindowCount/);
  assert.throws(() => normalizedRoutineWindows(Array(13).fill({ start: '08:30', end: '12:00' })), /routineWindowCount/);
  for (const window of [{ start: '', end: '12:00' }, { start: '8:30', end: '12:00' }, { start: '24:00', end: '24:00' }]) {
    assert.throws(() => normalizedRoutineWindows([window]), /routineWindowTime/);
  }
  for (const window of [{ start: '12:00', end: '12:00' }, { start: '18:00', end: '08:30' }]) {
    assert.throws(() => normalizedRoutineWindows([window]), /routineWindowOrder/);
  }
  assert.throws(() => normalizedRoutineWindows([{ start: '08:30', end: '12:00' }, { start: '11:59', end: '18:00' }]), /routineWindowOverlap/);
});

test('autosave serializes edits made during a pending write without duplicating the draft', async () => {
  let release;
  const gate = new Promise(resolve => { release = resolve; });
  let markStarted;
  const started = new Promise(resolve => { markStarted = resolve; });
  const calls = [];
  const editor = new DraftController({
    content: { kind: 'thought', body: '', session_id: null, reveal_at_utc_ms: null },
    request: async (op, data) => {
      calls.push({ op, data });
      if (calls.length === 1) { markStarted(); await gate; }
      return { draft: { id: 'draft', version: calls.length, content: data.content ?? { body: data.body } } };
    },
  });
  editor.change('第一行\n');
  const first = editor.flush();
  await started;
  editor.change('第一行\n第二行  ');
  const second = editor.flush();
  release();
  await Promise.all([first, second]);
  assert.equal(calls.length, 2);
  assert.equal(calls[0].op, 'create_draft');
  assert.deepEqual(calls[1].data, { id: 'draft', version: 1, body: '第一行\n第二行  ' });
  assert.equal(editor.dirty, false);
  assert.equal(editor.draft.version, 2);
});

test('failed save preserves unsaved words and retries the same version', async () => {
  let fail = true;
  const versions = [];
  const editor = new DraftController({
    draft: { id: 'draft', version: 7, content: { body: 'before' } },
    request: async (_, data) => {
      versions.push(data.version);
      if (fail) throw new Error('Disk unavailable');
      return { draft: { id: 'draft', version: 8, content: { body: data.body } } };
    },
  });
  editor.change('keep this\n留住這些話');
  await assert.rejects(editor.flush(), /Disk unavailable/);
  assert.equal(editor.body, 'keep this\n留住這些話');
  assert.equal(editor.draft.version, 7);
  assert.equal(editor.dirty, true);
  fail = false;
  await editor.flush();
  assert.deepEqual(versions, [7, 7]);
  assert.equal(editor.dirty, false);
});

test('blank optional fields do not create empty drafts, existing drafts can be cleared', async () => {
  let calls = 0;
  const editor = new DraftController({ content: { body: '' }, request: async () => { calls++; } });
  await editor.flush();
  assert.equal(calls, 0);
  const existing = new DraftController({
    draft: { id: 'a', version: 1, content: { body: 'clear me' } },
    request: async (_, data) => ({ draft: { id: 'a', version: 2, content: { body: data.body } } }),
  });
  existing.change('');
  await existing.flush();
  assert.equal(existing.draft.content.body, '');
});

function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

test('CLI quit saves pending words before readiness and stays locked through the final commit', async () => {
  const saved = deferred();
  const writing = deferred();
  const events = [];
  const editor = new DraftController({
    content: { kind: 'thought', body: '' },
    request: async (_, data) => {
      events.push('save started'); writing.resolve();
      await saved.promise;
      events.push('save committed');
      return { draft: { id: 'words', version: 1, content: data.content } };
    },
  });
  editor.change('Keep this before closing.\n简体 · 繁體');
  const quit = new CliQuitController({
    blockedReason: () => null,
    lock: value => events.push(value ? 'locked' : 'unlocked'),
    flush: () => editor.flush(),
    prepare: async (token, error) => { events.push({ token, error }); },
  });
  const ready = quit.request('quit-1');
  await writing.promise;
  assert.deepEqual(events, ['locked', 'save started']);
  assert.equal(quit.active, true);
  saved.resolve();
  assert.equal(await ready, true);
  assert.equal(editor.dirty, false);
  assert.equal(editor.draft.content.body, 'Keep this before closing.\n简体 · 繁體');
  assert.deepEqual(events, ['locked', 'save started', 'save committed', { token: 'quit-1', error: null }]);
  assert.equal(quit.active, true);
  quit.cancel('quit-1', 'The final checkpoint could not be saved.');
  assert.equal(events.at(-1), 'unlocked');
  assert.equal(quit.active, false);
});

test('a blocked CLI quit reports its reason without altering another operation or incomplete text', async () => {
  for (const reason of ['Another action is running', 'Finish composing this character', 'Save or cancel this routine']) {
    const replies = [];
    const quit = new CliQuitController({
      blockedReason: () => reason,
      lock: () => assert.fail('A rejection must not change the existing lock'),
      flush: () => assert.fail('A rejection must not flush incomplete text'),
      prepare: async (token, error) => replies.push({ token, error }),
    });
    assert.equal(await quit.request('blocked'), false);
    assert.deepEqual(replies, [{ token: 'blocked', error: reason }]);
    assert.equal(quit.cancel('blocked', reason), false);
    assert.equal(quit.active, false);
  }
});

test('a failed draft save prevents CLI quit and preserves the unsaved words for retry', async () => {
  const replies = [], locks = [], errors = [];
  const editor = new DraftController({
    draft: { id: 'words', version: 2, content: { body: 'earlier' } },
    request: async () => { throw new Error('Disk unavailable'); },
  });
  editor.change('Still here\n還在這裡');
  const quit = new CliQuitController({
    blockedReason: () => null,
    lock: value => locks.push(value),
    flush: () => editor.flush(),
    prepare: async (token, error) => replies.push({ token, error }),
    onError: error => errors.push(error.message),
  });
  assert.equal(await quit.request('cannot-save'), false);
  assert.deepEqual(replies, [{ token: 'cannot-save', error: 'Disk unavailable' }]);
  assert.deepEqual(locks, [true, false]);
  assert.deepEqual(errors, ['Disk unavailable']);
  assert.equal(editor.body, 'Still here\n還在這裡');
  assert.equal(editor.dirty, true);
  assert.equal(editor.draft.version, 2);
});

test('a cancelled save cannot later authorize quit or release a newer quit lock', async () => {
  const firstSave = deferred();
  const newerReply = deferred();
  const replies = [], locks = [];
  let saves = 0;
  const quit = new CliQuitController({
    blockedReason: () => null,
    lock: value => locks.push(value),
    flush: async () => { if (++saves === 1) await firstSave.promise; },
    prepare: async (token, error) => { replies.push({ token, error }); await newerReply.promise; },
  });
  const first = quit.request('old');
  assert.equal(quit.cancel('old', 'Timed out'), true);
  const second = quit.request('new');
  firstSave.resolve();
  assert.equal(await first, false);
  assert.deepEqual(replies, [{ token: 'new', error: null }]);
  assert.equal(quit.cancel('old', 'Late cancellation'), false);
  assert.deepEqual(locks, [true, false, true]);
  newerReply.resolve();
  assert.equal(await second, true);
  assert.equal(quit.active, true);
});

test('a rejected stale readiness reply cannot unlock a newer quit', async () => {
  const firstReply = deferred();
  const started = deferred();
  const locks = [];
  const quit = new CliQuitController({
    blockedReason: () => null,
    lock: value => locks.push(value),
    flush: async () => {},
    prepare: async token => { if (token === 'old') { started.resolve(); await firstReply.promise; } },
  });
  const first = quit.request('old');
  await started.promise;
  quit.cancel('old', 'Timed out');
  assert.equal(await quit.request('new'), true);
  firstReply.reject(new Error('Expired quit token'));
  assert.equal(await first, false);
  assert.deepEqual(locks, [true, false, true]);
  assert.equal(quit.active, true);
});

test('only fresh ready completions invite recovery, never late manual transition events', async () => {
  const { completedInvitation } = await import('./model.mjs');
  const event = { revision: 5, terminal: { id: 'focus-1', kind: 'focus', outcome: 'completed' }, checkpoint: { state: { state: 'ready' } } };
  assert.deepEqual(completedInvitation(event, { revision: 5, visible: true, focused: true }), { id: 'focus-1', kind: 'focus', auto: true });
  assert.equal(completedInvitation(event, { revision: 6 }), null);
  assert.equal(completedInvitation(event, { lastTerminalId: 'focus-1' }), null);
  assert.equal(completedInvitation({ ...event, checkpoint: { state: { state: 'running' } } }), null);
  assert.equal(completedInvitation({ ...event, terminal: { ...event.terminal, outcome: 'ended_early' } }), null);
  assert.equal(completedInvitation(event, { visible: true, focused: false }).auto, false);
  assert.equal(completedInvitation(event, { visible: false, focused: true }).auto, false);
});
