import test from 'node:test';
import assert from 'node:assert/strict';
import { NoteHistoryPager, collectArchivedNotes, pageNoteHistory } from './note-history.mjs';

function fixture(count = 235) {
  return { archived_notes: Array.from({ length: count }, (_, index) => ({
    id: `note-${String(index).padStart(4, '0')}`, kind: index % 2 ? 'break' : 'focus',
    body: `Note ${index}\n简体 · 繁體  `, version: 1,
    created_at_utc_ms: 1, updated_at_utc_ms: 2, archived_at_utc_ms: 1_000 - Math.floor(index / 3),
  })), period_note: { id: 'current', body: 'Current writing', archived_at_utc_ms: null },
  drafts: [], reflections: [] };
}
function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

test('all older notes are reachable beyond 100 without duplicates or lost timestamp ties', async () => {
  const view = fixture();
  view.archived_notes.unshift({ ...view.archived_notes[0], id: 'blank', body: '\u2003\n\t', archived_at_utc_ms: 2_000 });
  const pager = new NoteHistoryPager({ loadPage: options => pageNoteHistory(view, options), pageSize: 17 });
  assert.equal(await pager.reset(), true);
  while (pager.nextCursor) await pager.loadMore();
  assert.equal(pager.entries.length, 235);
  assert.equal(new Set(pager.entries.map(note => note.id)).size, 235);
  pager.entries.forEach((note, index) => {
    assert.equal(note.id, `note-${String(index).padStart(4, '0')}`);
    assert.equal(note.body, `Note ${index}\n简体 · 繁體  `);
  });
  assert.equal(await pager.loadMore(), false);
});

test('history preserves legacy reflection variants and drafts without an identical migrated duplicate', async () => {
  const view = fixture(1);
  view.archived_notes.push({ id: 'migrated', kind: 'break', body: 'Earlier thought', version: 1,
    created_at_utc_ms: 10, updated_at_utc_ms: 11, archived_at_utc_ms: 12 });
  view.drafts = [
    { id: 'old-draft', content: { kind: 'thought', body: 'Earlier thought', session_id: null }, record_id: null,
      version: 1, created_at_utc_ms: 10, updated_at_utc_ms: 11 },
    { id: 'unfinished', content: { kind: 'intention', body: 'Something still in progress' },
      version: 2, created_at_utc_ms: 15, updated_at_utc_ms: 16 },
  ];
  view.reflections = [{ id: 'record', created_at_utc_ms: 20, heads: ['a', 'b'].map(id => ({
    id, content: { kind: 'thought', body: `Preserved ${id}` }, author_sequence: 1, created_at_utc_ms: 21,
  })) }];
  const first = await pageNoteHistory(view);
  assert.equal(first.entries.length, 5);
  assert.equal(first.entries.filter(note => note.source === 'reflection').length, 2);
  assert.equal(first.entries.filter(note => note.body === 'Earlier thought').length, 1);
  view.drafts[0].content.body = 'A newer unfinished thought';
  view.drafts[0].version++;
  const changed = await pageNoteHistory(view);
  assert.equal(changed.entries.length, 6);
  assert.notEqual(changed.revision, first.revision);
  assert.equal((await pageNoteHistory(view, { periodsOnly: true })).entries.length, 2);
});

test('changed history replaces existing pages instead of mixing old and new data', async () => {
  const view = fixture(120);
  const pager = new NoteHistoryPager({ loadPage: options => pageNoteHistory(view, options), pageSize: 20 });
  await pager.reset();
  const cursor = pager.nextCursor;
  view.archived_notes[0].body = 'A late saved correction';
  view.archived_notes[0].version++;
  await pager.loadMore();
  assert.equal(pager.entries.length, 20);
  assert.equal(pager.entries[0].body, 'A late saved correction');
  assert.notEqual(pager.nextCursor.revision, cursor.revision);
  await assert.rejects(pageNoteHistory(view, { cursor, periodsOnly: true }), /cursor/);
  await assert.rejects(pageNoteHistory(view, { limit: 101 }), /1 to 100/);
});

test('opening a different history fences delayed results and delayed failures', async () => {
  const old = deferred(), newer = deferred();
  let requests = 0;
  const pager = new NoteHistoryPager({ loadPage: () => ++requests === 1 ? old.promise : newer.promise });
  const first = pager.reset();
  const second = pager.reset();
  newer.resolve(await pageNoteHistory(fixture(1)));
  assert.equal(await second, true);
  old.reject(new Error('The old request failed late'));
  assert.equal(await first, false);
  assert.equal(pager.error, null);
  assert.equal(pager.entries.length, 1);
  assert.equal(pager.loading, false);
});

test('a failed older page retains displayed notes and can be retried', async () => {
  const view = fixture(80);
  let fail = false;
  const pager = new NoteHistoryPager({ pageSize: 30, loadPage: options => {
    if (fail) throw new Error('Storage unavailable');
    return pageNoteHistory(view, options);
  } });
  await pager.reset();
  fail = true;
  await assert.rejects(pager.loadMore(), /Storage unavailable/);
  assert.equal(pager.entries.length, 30);
  assert.equal(pager.loading, false);
  fail = false;
  await pager.loadMore();
  assert.equal(pager.entries.length, 60);
  assert.equal(pager.error, null);
});

test('encrypted export collects all pages and does not leak history-only fields', async () => {
  const view = fixture();
  const notes = await collectArchivedNotes(options => pageNoteHistory(view, options), { pageSize: 17 });
  assert.equal(notes.length, 235);
  assert.deepEqual(notes, view.archived_notes);
  assert.equal(notes.some(note => 'source' in note || 'record_id' in note), false);
});

test('encrypted export never produces a silently partial or mixed-generation archive', async () => {
  const view = fixture(120);
  let requests = 0;
  await assert.rejects(collectArchivedNotes(options => {
    if (++requests === 2) { view.archived_notes[0].version++; view.archived_notes[0].body = 'New text'; }
    return pageNoteHistory(view, options);
  }), /changed while preparing/);
  await assert.rejects(collectArchivedNotes(options => pageNoteHistory(view, options), { maxNotes: 100 }), /complete backup/);
  assert.equal(view.archived_notes.length, 120);
});
