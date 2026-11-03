import test from 'node:test';
import assert from 'node:assert/strict';
import { NoteArchiveController, archiveDefaults, noteTimeLabels, utcOffsetLabel } from './note-archive.mjs';
import { queryNoteHistory } from './history-query.mjs';
import { noteHistoryCopy } from './note-history-copy.mjs';
import { NoteHistoryPager, noteHistoryEntries, pageNoteHistory } from './note-history.mjs';

function fixture(count = 81) {
  return { archived_notes: Array.from({ length: count }, (_, index) => ({
    id: `note-${String(index).padStart(3, '0')}`, kind: index % 2 ? 'break' : 'focus',
    body: `Tea and a walk ${index}`, version: 1, created_at_utc_ms: Date.UTC(2026, 10, 2, 8, index),
    updated_at_utc_ms: Date.UTC(2026, 10, 2, 8, index), archived_at_utc_ms: Date.UTC(2026, 10, 2, 8, index + 1),
    source_timezone: 'America/Los_Angeles', source_utc_offset_minutes: -480,
  })), drafts: [], reflections: [] };
}
function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
function page(id, cursor = null) {
  return { entries: [{ id, source: 'period', body: id, created_at_utc_ms: 1 }],
    revision: 'one', next_cursor: cursor, restarted: false };
}

test('archive pages every match while the quick recent reader requests only five', async () => {
  const view = fixture();
  const recent = new NoteHistoryPager({ loadPage: args => pageNoteHistory(view, args), pageSize: 5 });
  await recent.reset();
  assert.equal(recent.entries.length, 5);
  assert.ok(recent.nextCursor);
  const archive = new NoteArchiveController({ pageSize: 17, query: { ...archiveDefaults('UTC'), text: 'tea walk', kind: 'focus' },
    loadPage: args => queryNoteHistory(view, args) });
  await archive.reset();
  const first = archive.entries[0];
  while (archive.nextCursor) await archive.loadMore();
  assert.equal(archive.entries.length, 41);
  assert.equal(archive.entries[0], first); // Appending preserves the already displayed cards.
  assert.equal(new Set(archive.entries.map(note => note.id)).size, 41);
  assert.equal(archive.entries[0].id, 'note-080');
  assert.equal(archive.entries.at(-1).id, 'note-000');
  assert.equal(await archive.loadMore(), false);
});

test('a new filter fences delayed successes and errors and snapshots caller query values', async () => {
  const first = deferred(), second = deferred(), third = deferred();
  const pending = [first, second, third], sent = [];
  const archive = new NoteArchiveController({ loadPage: args => { sent.push(args); return pending[sent.length - 1].promise; } });
  const query = { ...archiveDefaults('UTC'), text: 'earlier' };
  const staleSuccess = archive.reset(query);
  query.text = 'mutated by caller';
  const staleError = archive.reset({ ...archiveDefaults('UTC'), text: 'middle' });
  const latest = archive.reset({ ...archiveDefaults('UTC'), text: 'latest' });
  third.resolve(page('latest')); await latest;
  first.resolve(page('earlier')); second.reject(new Error('Outdated failure'));
  assert.equal(await staleSuccess, false); assert.equal(await staleError, false);
  assert.equal(sent[0].query.text, 'earlier');
  assert.equal(archive.query.text, 'latest');
  assert.deepEqual(archive.entries.map(note => note.id), ['latest']);
  assert.equal(archive.error, null); assert.equal(archive.loading, false);
});

test('closing invalidates an unfinished archive read', async () => {
  const pending = deferred();
  const archive = new NoteArchiveController({ loadPage: () => pending.promise });
  const request = archive.reset(); archive.invalidate();
  pending.resolve(page('late'));
  assert.equal(await request, false);
  assert.deepEqual(archive.entries, []); assert.equal(archive.loaded, false);
});

test('failed pagination keeps earlier results and retries its same cursor', async () => {
  const view = fixture(); let failing = false;
  const archive = new NoteArchiveController({ pageSize: 20, query: archiveDefaults('UTC'), loadPage: args => {
    if (failing) throw new Error('Temporary unavailable');
    return queryNoteHistory(view, args);
  } });
  await archive.reset(); const cursor = structuredClone(archive.nextCursor);
  failing = true; await assert.rejects(archive.loadMore(), /Temporary unavailable/);
  assert.equal(archive.entries.length, 20); assert.deepEqual(archive.nextCursor, cursor);
  failing = false; await archive.loadMore();
  assert.equal(archive.entries.length, 40); assert.equal(archive.error, null);
});

test('revised history restarts cleanly while mismatched or repeated cursors fail visibly', async () => {
  const view = fixture();
  const archive = new NoteArchiveController({ pageSize: 20, query: archiveDefaults('UTC'), loadPage: args => queryNoteHistory(view, args) });
  await archive.reset();
  view.archived_notes[0].body = 'Changed tea walk'; view.archived_notes[0].version++;
  await archive.loadMore(); assert.equal(archive.entries.length, 20);
  let count = 0;
  const cursor = { key: 'same' };
  const repeated = new NoteArchiveController({ loadPage: () => page(`row-${++count}`, cursor) });
  await repeated.reset(); await assert.rejects(repeated.loadMore(), /archiveReadFailed/);
  assert.deepEqual(repeated.entries.map(note => note.id), ['row-1']);
  const changed = new NoteArchiveController({ loadPage: ({ cursor: after }) => ({ ...page('one', cursor), revision: after ? 'two' : 'one' }) });
  await changed.reset(); await assert.rejects(changed.loadMore(), /archiveChanged/);
});

test('unsafe timestamps or malformed backend pages do not enter the result list', async () => {
  const archive = new NoteArchiveController({ loadPage: () => ({ ...page('bad'), entries: [{ ...page('bad').entries[0], created_at_utc_ms: Number.MAX_SAFE_INTEGER }] }) });
  await assert.rejects(archive.reset(), /archiveReadFailed/);
  assert.deepEqual(archive.entries, []);
});

test('the same note moves between current zones but preserves its original local time', () => {
  const note = { created_at_utc_ms: Date.UTC(2026, 10, 2, 7, 30), archived_at_utc_ms: Date.UTC(2026, 10, 3),
    source_timezone: 'America/Los_Angeles', source_utc_offset_minutes: -480 };
  const la = noteTimeLabels(note, { currentTimezone: 'America/Los_Angeles' });
  const tokyo = noteTimeLabels(note, { currentTimezone: 'Asia/Tokyo' });
  assert.match(la.primary, /Nov 1, 2026/); assert.match(tokyo.primary, /Nov 2, 2026/);
  assert.equal(la.original, tokyo.original); assert.match(la.original, /UTC−08:00.*America\/Los_Angeles/);
  assert.equal(noteTimeLabels(note, { currentTimezone: 'Asia/Tokyo', timeBasis: 'original' }).primary, la.original);
  assert.equal(note.created_at_utc_ms, Date.UTC(2026, 10, 2, 7, 30));
});

test('captured offsets distinguish both occurrences of a repeated DST hour', () => {
  const first = noteTimeLabels({ created_at_utc_ms: Date.UTC(2026, 10, 1, 8, 30), source_timezone: 'America/Los_Angeles', source_utc_offset_minutes: -420 });
  const second = noteTimeLabels({ created_at_utc_ms: Date.UTC(2026, 10, 1, 9, 30), source_timezone: 'America/Los_Angeles', source_utc_offset_minutes: -480 });
  assert.match(first.original, /01:30.*UTC−07:00/); assert.match(second.original, /01:30.*UTC−08:00/);
});

test('legacy notes have an explicitly unknown original and a UTC fallback, not inferred device time', () => {
  const note = { created_at_utc_ms: Date.UTC(2026, 10, 2, 7, 30) };
  const labels = noteTimeLabels(note, { timeBasis: 'original', currentTimezone: 'Asia/Tokyo' });
  assert.equal(labels.originalKnown, false); assert.equal(labels.original, null);
  assert.equal(labels.primary, labels.utc); assert.notEqual(labels.primary, labels.current);
  assert.match(labels.primary, /UTC/);
  assert.equal(utcOffsetLabel(345), 'UTC+05:45');
});

test('reflection and draft normalization preserve recorded origins without filling legacy gaps', () => {
  const view = fixture(0);
  view.reflections = [{ id: 'record', heads: [{ id: 'head', content: { kind: 'thought', body: 'Stored' },
    created_at_utc_ms: 1, source_timezone: 'Asia/Kathmandu', source_utc_offset_minutes: 345 }] }];
  view.drafts = [{ id: 'draft', content: { kind: 'intention', body: 'Unfinished' }, created_at_utc_ms: 2,
    updated_at_utc_ms: 3, version: 1 }];
  const entries = noteHistoryEntries(view);
  assert.equal(entries[0].source_timezone, 'Asia/Kathmandu'); assert.equal(entries[0].source_utc_offset_minutes, 345);
  assert.equal(entries[1].source_timezone, null); assert.equal(entries[1].source_utc_offset_minutes, null);
});

test('archive labels exist in every supported language', () => {
  const keys = Object.keys(noteHistoryCopy.en).sort();
  for (const language of ['zh-Hans', 'zh-Hant']) assert.deepEqual(Object.keys(noteHistoryCopy[language]).sort(), keys);
});
