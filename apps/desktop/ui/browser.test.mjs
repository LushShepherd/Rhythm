import test from 'node:test';
import assert from 'node:assert/strict';
import { createBrowserBridge } from './browser.mjs';

function fixture() {
  const values = new Map();
  const storage = { getItem: key => values.get(key) ?? null, setItem: (key, value) => values.set(key, value) };
  let time = 10_000;
  let identifier = 0;
  const timers = new Set();
  const options = { storage, now: () => time, makeId: () => `preview-${++identifier}`,
    setInterval: fn => { timers.add(fn); return fn; }, clearInterval: fn => timers.delete(fn) };
  const bridge = createBrowserBridge(options);
  const request = (op, data) => bridge.core.invoke('execute', { request: { op, data } });
  return { bridge, request, storage, options, values,
    advance(ms) { time += ms; for (const tick of timers) tick(); } };
}

test('break reminder choices preserve note saves, accept once, and reject stale actions', async () => {
  const f = fixture();
  await f.bridge.core.invoke('snapshot');
  const focus = await f.request('switch_to_focus', { duration_ms: 100 });
  const periodId = focus.view.checkpoint.state.data.id;
  f.advance(100);
  const waiting = await f.bridge.core.invoke('snapshot');
  await f.request('save_period_note', { id: waiting.period_note.id, version: waiting.period_note.version, body: 'Before resting.' });
  const response = await f.bridge.core.invoke('respond_break_reminder', { periodId, sequence: 1, action: 'accept', durationMs: 240_000 });
  assert.equal(response.view.checkpoint.state.data.kind, 'short_rest');
  assert.equal(response.view.checkpoint.state.data.planned_ms, 240_000);
  assert.equal(response.view.archived_notes.some(note => note.body === 'Before resting.'), true);
  assert.equal(response.reminder, null);
  assert.equal(await f.bridge.core.invoke('respond_break_reminder', { periodId, sequence: 1, action: 'snooze' }), null);
  f.advance(240_000);
  assert.equal(await f.bridge.core.invoke('respond_break_reminder', { periodId, sequence: 1, action: 'accept' }), null);
});

test('delay defaults to nine minutes without adding a period and rejects superseded reminder actions', async () => {
  const f = fixture();
  await f.bridge.core.invoke('snapshot');
  const first = await f.request('switch_to_focus', { duration_ms: 100 });
  f.advance(100);
  const periodId = first.view.checkpoint.state.data.id;
  const delayed = await f.bridge.core.invoke('respond_break_reminder', { periodId, sequence: 1, action: 'snooze' });
  assert.equal(delayed.view.checkpoint.state.state, 'ready');
  assert.equal(delayed.view.sessions.length, 1);
  assert.equal(delayed.view.checkpoint.completed_focus, 1);
  assert.deepEqual(delayed.reminder, { period_id: periodId, kind: 'focus', sequence: 2,
    due_at_utc_ms: 550_100, available: false, postponements: 1 });
  assert.equal(await f.bridge.core.invoke('respond_break_reminder', { periodId, sequence: 2, action: 'accept' }), null);
  f.advance(539_999);
  assert.equal((await f.bridge.core.invoke('current_reminder')).available, false);
  f.advance(1);
  const due = await f.bridge.core.invoke('current_reminder');
  assert.equal(due.available, true);
  assert.equal(due.sequence, 3);
  assert.equal(await f.bridge.core.invoke('respond_break_reminder', { periodId, sequence: 1, action: 'accept' }), null);
  await f.request('switch_to_focus', { duration_ms: 60_000 });
  assert.equal(await f.bridge.core.invoke('respond_break_reminder', { periodId, sequence: 3, action: 'accept' }), null);
  assert.equal(await f.bridge.core.invoke('current_reminder'), null);
});

test('invalid or failed reminder choices do not consume the pending break', async () => {
  const f = fixture();
  await f.bridge.core.invoke('snapshot');
  const focus = await f.request('switch_to_focus', { duration_ms: 100 });
  f.advance(100);
  const periodId = focus.view.checkpoint.state.data.id;
  assert.equal(await f.bridge.core.invoke('respond_break_reminder', { action: 'accept' }), null);
  assert.equal(await f.bridge.core.invoke('respond_break_reminder', { periodId: '', action: 'accept' }), null);
  assert.equal(await f.bridge.core.invoke('respond_break_reminder', { periodId, action: 'accept' }), null);
  await assert.rejects(f.bridge.core.invoke('respond_break_reminder', { periodId, action: 'dismiss' }), /Unknown/);
  await assert.rejects(f.bridge.core.invoke('respond_break_reminder', { periodId, sequence: 1, action: 'accept', durationMs: -1 }), /Each period/);
  await assert.rejects(f.bridge.core.invoke('respond_break_reminder', { periodId, sequence: 1, action: 'snooze', durationMs: 1 }), /between 1 and 60/);
  const valid = await f.bridge.core.invoke('respond_break_reminder', { periodId, sequence: 1, action: 'accept' });
  assert.equal(valid.view.checkpoint.state.data.kind, 'short_rest');
});

test('a reminder cannot overwrite another tab change made after its state was loaded', async () => {
  const f = fixture();
  await f.bridge.core.invoke('snapshot');
  const focus = await f.request('switch_to_focus', { duration_ms: 100 });
  f.advance(100);
  const waiting = f.values.get('rhythm.browser.v1');
  const newer = await f.request('switch_to_focus', { duration_ms: 60_000 });
  const read = f.storage.getItem;
  let firstRead = true;
  f.storage.getItem = key => {
    if (firstRead) { firstRead = false; return waiting; }
    return read(key);
  };
  await assert.rejects(f.bridge.core.invoke('respond_break_reminder', {
    periodId: focus.view.checkpoint.state.data.id, sequence: 1, action: 'accept',
  }), /another tab/);
  f.storage.getItem = read;
  const current = await f.bridge.core.invoke('snapshot');
  assert.equal(current.checkpoint.state.data.kind, 'focus');
  assert.equal(current.checkpoint.state.data.id, newer.view.checkpoint.state.data.id);
  assert.equal(current.sessions.length, 1);
});

test('custom reminder delays survive reopening and become available once at the deadline', async () => {
  const f = fixture();
  const reminders = [];
  await f.bridge.event.listen('rhythm:reminder', event => reminders.push(event.payload));
  await f.bridge.core.invoke('snapshot');
  const focus = await f.request('switch_to_focus', { duration_ms: 100 });
  f.advance(100);
  const periodId = focus.view.checkpoint.state.data.id;
  const delayed = await f.bridge.core.invoke('respond_break_reminder', { periodId, sequence: 1, action: 'snooze', durationMs: 120_000 });
  const reopened = createBrowserBridge(f.options);
  assert.deepEqual(await reopened.core.invoke('current_reminder'), delayed.reminder);
  f.advance(119_999);
  assert.equal((await reopened.core.invoke('current_reminder')).available, false);
  f.advance(1);
  assert.equal((await reopened.core.invoke('current_reminder')).sequence, 3);
  f.advance(1000);
  assert.equal(reminders.filter(item => item?.available && item.sequence === 3).length, 1);
  assert.equal((await f.bridge.core.invoke('snapshot')).sessions.length, 1);
  reopened.dispose();
});

test('rest completion offers Focus and extending rest does not advance the short-rest cadence', async () => {
  const f = fixture();
  await f.bridge.core.invoke('snapshot');
  const rest = await f.request('switch_to_break', { duration_ms: 100 });
  f.advance(100);
  const reminder = await f.bridge.core.invoke('current_reminder');
  assert.equal(reminder.period_id, rest.view.checkpoint.state.data.id);
  assert.equal(reminder.kind, 'break');
  const delayed = await f.bridge.core.invoke('respond_break_reminder', {
    periodId: reminder.period_id, sequence: 1, action: 'snooze', durationMs: 60_000,
  });
  assert.equal(delayed.view.checkpoint.completed_short_rests, 1);
  assert.equal(delayed.view.checkpoint.state.state, 'ready');
  f.advance(60_000);
  const repeated = await f.bridge.core.invoke('respond_break_reminder', {
    periodId: reminder.period_id, sequence: 3, action: 'snooze', durationMs: 60_000,
  });
  assert.equal(repeated.reminder.postponements, 2);
  assert.equal(repeated.view.checkpoint.completed_short_rests, 1);
  f.advance(60_000);
  const accepted = await f.bridge.core.invoke('respond_break_reminder', {
    periodId: reminder.period_id, sequence: 5, action: 'accept', durationMs: 600_000,
  });
  assert.equal(accepted.view.checkpoint.state.data.kind, 'focus');
  assert.equal(accepted.view.checkpoint.state.data.planned_ms, 600_000);
  assert.equal(accepted.view.sessions.length, 1);
  assert.equal(accepted.reminder, null);
});

test('failed postponement keeps the available invitation and never records extra rest', async () => {
  const f = fixture();
  await f.bridge.core.invoke('snapshot');
  const rest = await f.request('switch_to_break', { duration_ms: 100 });
  f.advance(100);
  const original = f.values.get('rhythm.browser.v1');
  const set = f.storage.setItem;
  f.storage.setItem = () => { throw new Error('quota'); };
  await assert.rejects(f.bridge.core.invoke('respond_break_reminder', {
    periodId: rest.view.checkpoint.state.data.id, sequence: 1, action: 'snooze',
  }), /could not save/);
  assert.equal(f.values.get('rhythm.browser.v1'), original);
  f.storage.setItem = set;
  const pending = await f.bridge.core.invoke('current_reminder');
  assert.equal(pending.sequence, 1);
  assert.equal(pending.available, true);
  assert.equal((await f.bridge.core.invoke('snapshot')).checkpoint.completed_short_rests, 1);
});

test('legacy completed periods gain one invitation while malformed reminder data stays untouched', async () => {
  const f = fixture();
  await f.bridge.core.invoke('snapshot');
  await f.request('switch_to_focus', { duration_ms: 100 });
  f.advance(100);
  const key = 'rhythm.browser.v1';
  const old = JSON.parse(f.values.get(key));
  old.schema = 2; delete old.view.pending_reminder;
  f.values.set(key, JSON.stringify(old));
  const reminder = await f.bridge.core.invoke('current_reminder');
  assert.equal(reminder.kind, 'focus');
  assert.equal(reminder.sequence, 1);
  assert.equal(JSON.parse(f.values.get(key)).schema, 3);
  const corrupt = JSON.parse(f.values.get(key));
  corrupt.view.pending_reminder.due_at_utc_ms = 'bad clock';
  const bytes = JSON.stringify(corrupt); f.values.set(key, bytes);
  await assert.rejects(f.bridge.core.invoke('current_reminder'), /left untouched/);
  assert.equal(f.values.get(key), bytes);
});

test('preview starts with the requested defaults and switches directly without restarting the same state', async () => {
  const f = fixture();
  const first = await f.bridge.core.invoke('snapshot');
  assert.deepEqual(first.checkpoint.settings, { focus_ms: 1_500_000, short_rest_ms: 300_000,
    long_rest_ms: 1_800_000, focus_before_long_rest: 4 });
  assert.equal(first.checkpoint.state.state, 'ready');
  const focus = await f.request('switch_to_focus', { duration_ms: null });
  f.advance(12_000);
  const repeated = await f.request('switch_to_focus', { duration_ms: null });
  assert.equal(repeated.view.checkpoint.state.data.id, focus.view.checkpoint.state.data.id);
  assert.equal(repeated.view.checkpoint.state.data.elapsed_ms, 12_000);
  const rest = await f.request('switch_to_break', { duration_ms: null });
  assert.equal(rest.view.checkpoint.state.state, 'running');
  assert.equal(rest.view.checkpoint.state.data.kind, 'short_rest');
  assert.equal(rest.terminal.outcome, 'ended_early');
  assert.equal(rest.terminal.elapsed_ms, 12_000);
  assert.equal(rest.view.checkpoint.completed_focus, 0);
});

test('completed periods emit one terminal event and never start focus automatically', async () => {
  const f = fixture();
  const events = [];
  await f.bridge.event.listen('rhythm:timer', event => events.push(event.payload));
  await f.bridge.core.invoke('snapshot');
  await f.request('switch_to_focus', { duration_ms: 100 });
  f.advance(200);
  f.advance(100);
  assert.equal(events.filter(event => event.terminal).length, 1);
  const completed = events.find(event => event.terminal);
  assert.equal(completed.terminal.elapsed_ms, 100);
  assert.equal(completed.checkpoint.state.state, 'ready');
  assert.equal(completed.checkpoint.state.data.next, 'short_rest');
  await f.request('switch_to_break', { duration_ms: 100 });
  f.advance(200);
  const current = await f.bridge.core.invoke('snapshot');
  assert.equal(current.checkpoint.state.state, 'ready');
  assert.equal(current.checkpoint.state.data.next, 'focus');
});

test('three actual short breaks offer long rest and finishing it resets the cadence', async () => {
  const f = fixture();
  await f.bridge.core.invoke('snapshot');
  for (let count = 1; count <= 4; count++) {
    await f.request('switch_to_focus', { duration_ms: 100 });
    f.advance(100);
    const rest = await f.request('switch_to_break', { duration_ms: null });
    assert.equal(rest.view.checkpoint.state.data.kind, count === 4 ? 'long_rest' : 'short_rest');
    assert.equal(rest.view.checkpoint.state.data.planned_ms, count === 4 ? 1_800_000 : 300_000);
    f.advance(rest.view.checkpoint.state.data.planned_ms);
  }
  const current = await f.bridge.core.invoke('snapshot');
  assert.equal(current.checkpoint.completed_focus, 0);
  assert.equal(current.checkpoint.completed_short_rests, 0);
  assert.equal(current.checkpoint.long_rest_due, false);
});

test('shared multilingual note survives state switches and a new bridge without normalization', async () => {
  const f = fixture();
  const words = 'A little space.\n简体 · 繁體  ';
  const saved = await f.request('create_draft', { content: { kind: 'thought', body: words,
    session_id: null, reveal_at_utc_ms: null } });
  await f.request('switch_to_focus', { duration_ms: null });
  await f.request('switch_to_break', { duration_ms: null });
  const reopened = createBrowserBridge(f.options);
  const loaded = await reopened.core.invoke('snapshot');
  assert.equal(loaded.drafts[0].content.body, words);
  assert.equal(loaded.drafts[0].id, saved.draft.id);
  const cleared = await reopened.core.invoke('execute', { request: { op: 'save_draft', data: {
    id: saved.draft.id, version: saved.draft.version, body: '',
  } } });
  assert.equal(cleared.draft.content.body, '');
  assert.equal(cleared.draft.version, 2);
  reopened.dispose();
});

test('competing tabs reject stale note saves while preserving the committed version', async () => {
  const f = fixture();
  const saved = await f.request('create_draft', { content: { kind: 'thought', body: 'original' } });
  const other = createBrowserBridge(f.options);
  await other.core.invoke('execute', { request: { op: 'save_draft', data: {
    id: saved.draft.id, version: 1, body: 'newer words',
  } } });
  await assert.rejects(f.request('save_draft', { id: saved.draft.id, version: 1, body: 'stale words' }), /another tab/);
  assert.equal((await f.bridge.core.invoke('snapshot')).drafts[0].content.body, 'newer words');
  other.dispose();
});

test('failed persistence cannot partially switch the timer or report a note as saved', async () => {
  const f = fixture();
  const focus = await f.request('switch_to_focus', { duration_ms: null });
  const originalSet = f.storage.setItem;
  f.storage.setItem = () => { throw new Error('quota'); };
  await assert.rejects(f.request('switch_to_break', { duration_ms: null }), /could not save/);
  await assert.rejects(f.request('create_draft', { content: { kind: 'thought', body: 'keep this' } }), /could not save/);
  f.storage.setItem = originalSet;
  const current = await f.bridge.core.invoke('snapshot');
  assert.equal(current.checkpoint.state.data.id, focus.view.checkpoint.state.data.id);
  assert.equal(current.sessions.length, 0);
  assert.equal(current.drafts.length, 0);
});

test('reopening after a deadline reconciles once, and malformed saved data is never overwritten', async () => {
  const f = fixture();
  await f.request('switch_to_focus', { duration_ms: 100 });
  f.advance(1000);
  const reopened = createBrowserBridge(f.options);
  const current = await reopened.core.invoke('snapshot');
  assert.equal(current.checkpoint.state.state, 'ready');
  assert.equal(current.sessions.length, 1);
  assert.equal((await reopened.core.invoke('snapshot')).sessions.length, 1);
  reopened.dispose();
  const key = [...f.values.keys()][0];
  f.values.set(key, 'broken saved data');
  await assert.rejects(f.request('switch_to_focus', {}), /left untouched/);
  assert.equal(f.values.get(key), 'broken saved data');
});

test('settings changes affect the next period and invalid settings leave storage unchanged', async () => {
  const f = fixture();
  const focus = await f.request('switch_to_focus', { duration_ms: null });
  const settings = { focus_ms: 600_000, short_rest_ms: 120_000, long_rest_ms: 900_000, focus_before_long_rest: 2 };
  await f.request('update_settings', { settings });
  assert.equal((await f.bridge.core.invoke('snapshot')).checkpoint.state.data.planned_ms, focus.view.checkpoint.state.data.planned_ms);
  await assert.rejects(f.request('update_settings', { settings: { ...settings, focus_ms: 0 } }), /between/);
  const rest = await f.request('switch_to_break', { duration_ms: null });
  assert.equal(rest.view.checkpoint.state.data.planned_ms, 120_000);
});

test('each direct Focus and Break switch archives a separate note that survives reopening', async () => {
  const f = fixture();
  assert.equal((await f.bridge.core.invoke('snapshot')).period_note, null);
  const focus = (await f.request('switch_to_focus', {})).view.period_note;
  assert.equal(focus.kind, 'focus');
  assert.equal(focus.version, 1);
  const words = 'One focus.\n留一点空间。  ';
  const saved = await f.request('save_period_note', { id: focus.id, version: 1, body: words });
  assert.equal(saved.period_note.version, 2);
  const rest = (await f.request('switch_to_break', {})).view;
  assert.equal(rest.period_note.kind, 'break');
  assert.equal(rest.period_note.body, '');
  assert.notEqual(rest.period_note.id, focus.id);
  assert.equal(rest.archived_notes.length, 1);
  assert.equal(rest.archived_notes[0].id, focus.id);
  assert.equal(rest.archived_notes[0].body, words);
  assert.equal(rest.archived_notes[0].version, 2);
  await f.request('save_period_note', { id: rest.period_note.id, version: 1, body: 'Resting.' });
  const next = (await f.request('switch_to_focus', {})).view;
  assert.equal(next.period_note.body, '');
  assert.deepEqual(next.archived_notes.map(note => note.body), ['Resting.', words]);
  const repeated = (await f.request('switch_to_focus', {})).view;
  assert.equal(repeated.period_note.id, next.period_note.id);
  assert.equal(repeated.notes_revision, next.notes_revision);
  const reopened = createBrowserBridge(f.options);
  const loaded = await reopened.core.invoke('snapshot');
  assert.deepEqual(loaded.period_note, next.period_note);
  assert.deepEqual(loaded.archived_notes, next.archived_notes);
  reopened.dispose();
});

test('deadline archives the original note and a delayed composition save updates only that archive', async () => {
  const f = fixture();
  const events = [];
  await f.bridge.event.listen('rhythm:timer', event => events.push(event.payload));
  await f.bridge.core.invoke('snapshot');
  const focus = (await f.request('switch_to_focus', { duration_ms: 100 })).view.period_note;
  f.advance(100);
  const terminal = events.find(event => event.terminal);
  const after = await f.bridge.core.invoke('snapshot');
  assert.equal(after.period_note.kind, 'break');
  assert.notEqual(after.period_note.id, focus.id);
  assert.equal(terminal.note_period_id, after.period_note.id);
  assert.equal(terminal.notes_revision, after.notes_revision);
  assert.equal(after.archived_notes.length, 0, 'an empty archive stays out of the interface');
  const late = await f.request('save_period_note', { id: focus.id, version: 1, body: '最后一个字。' });
  assert.equal(late.period_note.id, focus.id);
  assert.notEqual(late.period_note.archived_at_utc_ms, null);
  assert.equal(late.view.period_note.id, after.period_note.id);
  assert.equal(late.view.period_note.body, '');
  assert.equal(late.view.archived_notes[0].body, '最后一个字。');
  f.advance(100);
  assert.equal((await f.bridge.core.invoke('snapshot')).period_note.id, after.period_note.id);
  assert.equal(events.filter(event => event.terminal).length, 1);
});

test('a completed timed Break creates an independent untimed Break note, then archives it on Focus', async () => {
  const f = fixture();
  await f.bridge.core.invoke('snapshot');
  await f.request('switch_to_focus', {});
  const timed = (await f.request('switch_to_break', { duration_ms: 100 })).view.period_note;
  await f.request('save_period_note', { id: timed.id, version: 1, body: 'Timed rest' });
  f.advance(100);
  const untimed = (await f.bridge.core.invoke('snapshot')).period_note;
  assert.equal(untimed.kind, 'break');
  assert.notEqual(untimed.id, timed.id);
  assert.equal(untimed.body, '');
  await f.request('save_period_note', { id: untimed.id, version: 1, body: 'A little longer' });
  const focus = (await f.request('switch_to_focus', {})).view;
  assert.equal(focus.period_note.kind, 'focus');
  assert.deepEqual(focus.archived_notes.map(note => note.body), ['A little longer', 'Timed rest']);
});

test('storage failures roll back note archiving together with the timer transition', async () => {
  const f = fixture();
  const focus = (await f.request('switch_to_focus', {})).view.period_note;
  await f.request('save_period_note', { id: focus.id, version: 1, body: 'Keep me here' });
  const before = [...f.values.values()][0];
  const originalSet = f.storage.setItem;
  f.storage.setItem = () => { throw new Error('quota'); };
  await assert.rejects(f.request('switch_to_break', {}), /could not save/);
  await assert.rejects(f.request('save_period_note', { id: focus.id, version: 2, body: 'Not committed' }), /could not save/);
  assert.equal([...f.values.values()][0], before);
  f.storage.setItem = originalSet;
  const current = await f.bridge.core.invoke('snapshot');
  assert.equal(current.period_note.body, 'Keep me here');
  assert.equal(current.period_note.version, 2);
  assert.equal(current.archived_notes.length, 0);
  assert.equal(current.checkpoint.state.data.kind, 'focus');
});

test('archived note saves reject stale versions and whitespace never creates a visible archive', async () => {
  const f = fixture();
  const focus = (await f.request('switch_to_focus', {})).view.period_note;
  await f.request('save_period_note', { id: focus.id, version: 1, body: '\n  ' });
  const rest = (await f.request('switch_to_break', {})).view;
  assert.equal(rest.archived_notes.length, 0);
  const other = createBrowserBridge(f.options);
  await other.core.invoke('execute', { request: { op: 'save_period_note', data: { id: focus.id, version: 2, body: 'Newer' } } });
  await assert.rejects(f.request('save_period_note', { id: focus.id, version: 2, body: 'Stale' }), /another tab/);
  const current = await f.bridge.core.invoke('snapshot');
  assert.equal(current.archived_notes[0].body, 'Newer');
  assert.equal(current.period_note.id, rest.period_note.id);
  assert.equal(current.period_note.body, '');
  other.dispose();
});

test('legacy shared notes are copied once to the archive without changing their drafts', async () => {
  const f = fixture();
  const words = 'Before the redesign\n以前的随记';
  const old = await f.request('create_draft', { content: { kind: 'thought', body: words, session_id: null } });
  await f.request('switch_to_focus', {});
  const key = [...f.values.keys()][0];
  const saved = JSON.parse(f.values.get(key));
  saved.schema = 1;
  delete saved.view.notes_revision;
  delete saved.view.period_note;
  delete saved.view.archived_notes;
  f.values.set(key, JSON.stringify(saved));
  const reopened = createBrowserBridge(f.options);
  const migrated = await reopened.core.invoke('snapshot');
  assert.equal(migrated.archived_notes.length, 1);
  assert.equal(migrated.archived_notes[0].body, words);
  assert.equal(migrated.archived_notes[0].kind, 'break');
  assert.equal(migrated.period_note.body, '');
  assert.equal(migrated.period_note.kind, 'focus');
  assert.deepEqual(migrated.drafts[0], old.draft);
  assert.equal(JSON.parse(f.values.get(key)).schema, 3);
  assert.deepEqual((await reopened.core.invoke('snapshot')).archived_notes, migrated.archived_notes);
  reopened.dispose();
});

test('failed legacy migration retains the original saved data for a later retry', async () => {
  const f = fixture();
  const old = (await f.request('create_draft', { content: { kind: 'thought', body: 'Keep the old note' } })).view;
  delete old.notes_revision; delete old.period_note; delete old.archived_notes;
  const key = [...f.values.keys()][0];
  const original = JSON.stringify({ schema: 1, view: old });
  f.values.set(key, original);
  const originalSet = f.storage.setItem;
  f.storage.setItem = () => { throw new Error('quota'); };
  await assert.rejects(f.bridge.core.invoke('snapshot'), /could not save/);
  assert.equal(f.values.get(key), original);
  f.storage.setItem = originalSet;
  const migrated = await f.bridge.core.invoke('snapshot');
  assert.equal(migrated.archived_notes[0].body, 'Keep the old note');
  assert.equal(migrated.period_note, null);
});

test('skipping short rests never earns a long break and extension preserves the current note', async () => {
  const f = fixture();
  await f.bridge.core.invoke('snapshot');
  for (let i = 0; i < 5; i++) {
    await f.request('switch_to_focus', { duration_ms: 100 });
    f.advance(100);
    const rest = await f.request('switch_to_break', { duration_ms: 100 });
    assert.equal(rest.view.checkpoint.state.data.kind, 'short_rest');
    await f.request('switch_to_focus', { duration_ms: 100 });
  }
  const current = await f.bridge.core.invoke('snapshot');
  const extended = await f.request('extend_focus', { additional_ms: 200 });
  assert.equal(extended.view.checkpoint.state.data.planned_ms, 300);
  assert.equal(extended.view.period_note.id, current.period_note.id);
  assert.equal(extended.view.checkpoint.state.data.id, current.checkpoint.state.data.id);
  f.advance(300);
  await assert.rejects(f.request('extend_focus', { additional_ms: 200 }), /active focus/);
});

test('archive imports are idempotent, preserve divergent words and leave the active note alone', async () => {
  const f = fixture();
  const current = (await f.request('switch_to_focus', {})).view.period_note;
  const note = { id: 'shared', kind: 'break', body: 'One version', version: 1,
    created_at_utc_ms: 1, updated_at_utc_ms: 2, archived_at_utc_ms: 3 };
  await f.request('import_period_notes', { notes: [note] });
  await f.request('import_period_notes', { notes: [note, { ...note, body: 'A different version' }] });
  const result = (await f.request('import_period_notes', { notes: [note, { ...note, body: 'A different version' }] })).view;
  assert.equal(result.archived_notes.length, 2);
  assert.equal(result.period_note.id, current.id);
  assert.equal(new Set(result.archived_notes.map(note => note.id)).size, 2);
});

test('history reads the complete stored archive across pages while snapshots retain only the latest 100', async () => {
  const f = fixture();
  const initial = await f.bridge.core.invoke('snapshot');
  const notes = Array.from({ length: 237 }, (_, i) => ({
    id: `older-${String(i).padStart(4, '0')}`, kind: i % 2 ? 'focus' : 'break',
    body: `Note ${i}\n简体 · 繁體  `, version: 1, created_at_utc_ms: 1000 + i,
    updated_at_utc_ms: 1000 + i, archived_at_utc_ms: 2000 + i,
  }));
  initial.archived_notes = [...notes, { ...notes[0], id: 'empty-archive', body: ' \n\t' }];
  initial.notes_revision = 238;
  const bytes = JSON.stringify({ schema: 3, view: initial });
  f.values.set('rhythm.browser.v1', bytes);
  const snapshot = await f.bridge.core.invoke('snapshot');
  assert.equal(snapshot.archived_notes.length, 100);
  assert.equal(snapshot.archived_notes.some(note => note.id === notes[0].id), false);
  const read = async bridge => {
    const entries = [], pageSizes = [];
    let cursor = null;
    do {
      const page = await bridge.core.invoke('note_history', { cursor, limit: 100, periodsOnly: true });
      assert.equal(page.restarted, false);
      pageSizes.push(page.entries.length); entries.push(...page.entries); cursor = page.next_cursor;
    } while (cursor);
    return { entries, pageSizes };
  };
  const all = await read(f.bridge);
  assert.deepEqual(all.pageSizes, [100, 100, 37]);
  assert.deepEqual(all.entries.map(note => note.id), [...notes].reverse().map(note => note.id));
  assert.equal(all.entries.at(-1).body, notes[0].body);
  assert.equal(new Set(all.entries.map(note => note.id)).size, 237);
  assert.equal(f.values.get('rhythm.browser.v1'), bytes);
  const reopened = createBrowserBridge(f.options);
  // History is a complete read API even when a timer snapshot has not run yet.
  assert.deepEqual(await read(reopened), all);
  reopened.dispose(); f.bridge.dispose();
});

test('history detects a saved change from another tab before accepting an older cursor', async () => {
  const f = fixture();
  const view = await f.bridge.core.invoke('snapshot');
  const note = index => ({ id: `history-${index}`, kind: 'focus', body: `Words ${index}`,
    version: 1, created_at_utc_ms: index, updated_at_utc_ms: index, archived_at_utc_ms: index });
  view.archived_notes = [note(1), note(2), note(3)];
  view.notes_revision = 3;
  f.values.set('rhythm.browser.v1', JSON.stringify({ schema: 3, view }));
  await f.bridge.core.invoke('snapshot');
  const first = await f.bridge.core.invoke('note_history', { cursor: null, limit: 2, periodsOnly: true });
  assert.deepEqual(first.entries.map(item => item.id), ['history-3', 'history-2']);
  view.archived_notes.push(note(4)); view.notes_revision++;
  const newer = JSON.stringify({ schema: 3, view });
  f.values.set('rhythm.browser.v1', newer);
  const second = await f.bridge.core.invoke('note_history', { cursor: first.next_cursor, limit: 2, periodsOnly: true });
  assert.equal(second.restarted, true);
  assert.notEqual(second.revision, first.revision);
  assert.deepEqual(second.entries.map(item => item.id), ['history-4', 'history-3']);
  assert.equal(f.values.get('rhythm.browser.v1'), newer);
  f.bridge.dispose();
});
