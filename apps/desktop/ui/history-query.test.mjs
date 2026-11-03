import test from 'node:test';
import assert from 'node:assert/strict';
import { queryNoteHistory, normalizeHistoryQuery } from './history-query.mjs';
import { createBrowserBridge } from './browser.mjs';

const at = value => Date.parse(value);
function note(id, body, created, { archived = created, zone, offset, kind = 'focus' } = {}) {
  return { id, body, kind, version: 1, created_at_utc_ms: created, updated_at_utc_ms: created,
    archived_at_utc_ms: archived, ...(zone === undefined ? {} : { source_timezone: zone, source_utc_offset_minutes: offset }) };
}
const view = entries => ({ archived_notes: entries, reflections: [], drafts: [] });

test('full journal search finds old notes beyond first thousand and treats punctuation literally', async () => {
  const entries = Array.from({ length: 1105 }, (_, index) => note(`p-${index}`, index === 3 ? "ÉCOLE 中文 100% _ ' OR 1=1" : 'ordinary writing', index));
  const page = await queryNoteHistory(view(entries), { query: { text: 'école 中文 100% _' }, limit: 10 });
  assert.deepEqual(page.entries.map(note => note.id), ['p-3']);
  assert.equal(page.next_cursor, null);
  assert.equal((await queryNoteHistory(view(entries), { query: { text: "' OR 1=1" } })).entries.length, 1);
  assert.equal((await queryNoteHistory(view(entries), { query: { text: 'absent' } })).entries.length, 0);
});

test('writing-time order and tie keys are stable across both paging directions', async () => {
  const values = view([note('a', 'first', 1, { archived: 100 }), note('c', 'third', 2), note('b', 'second', 2)]);
  const first = await queryNoteHistory(values, { query: { order: 'oldest' }, limit: 2 });
  assert.deepEqual(first.entries.map(note => note.id), ['a', 'b']);
  const next = await queryNoteHistory(values, { query: { order: 'oldest' }, cursor: first.next_cursor, limit: 2 });
  assert.deepEqual(next.entries.map(note => note.id), ['c']);
  assert.equal(next.next_cursor, null);
  assert.deepEqual((await queryNoteHistory(values)).entries.map(note => note.id), ['b', 'c', 'a']);
});

test('current-zone date/day filters change calendar views without changing UTC; original zone stays fixed', async () => {
  const created = at('2026-11-02T07:30:00Z');
  const values = view([note('travel', 'same instant', created, { zone: 'America/Los_Angeles', offset: -480 })]);
  const la = { date_from: '2026-11-01', date_to: '2026-11-01', time_of_day: 'night', current_timezone: 'America/Los_Angeles' };
  const shanghai = { date_from: '2026-11-02', date_to: '2026-11-02', time_of_day: 'afternoon', current_timezone: 'Asia/Shanghai' };
  assert.equal((await queryNoteHistory(values, { query: la })).entries.length, 1);
  assert.equal((await queryNoteHistory(values, { query: shanghai })).entries[0].created_at_utc_ms, created);
  assert.equal((await queryNoteHistory(values, { query: { ...la, time_basis: 'original', current_timezone: 'Asia/Shanghai' } })).entries.length, 1);
  assert.equal((await queryNoteHistory(values, { query: { ...shanghai, time_basis: 'original' } })).entries.length, 0);
});

test('repeated DST hour remains two notes; original filters do not guess legacy origin', async () => {
  const values = view([
    note('first', 'first 1:30', at('2026-11-01T08:30:00Z'), { zone: 'America/Los_Angeles', offset: -420 }),
    note('second', 'second 1:30', at('2026-11-01T09:30:00Z'), { zone: 'America/Los_Angeles', offset: -480 }),
    note('unknown', 'legacy', at('2026-11-01T09:31:00Z')),
  ]);
  const query = { date_from: '2026-11-01', date_to: '2026-11-01', time_of_day: 'night', time_basis: 'original' };
  assert.deepEqual((await queryNoteHistory(values, { query })).entries.map(note => note.id), ['second', 'first']);
  assert.equal((await queryNoteHistory(values, { query: { time_basis: 'original' } })).entries.length, 3);
  assert.equal((await queryNoteHistory(values, { query: { ...query, time_basis: 'current', current_timezone: 'America/Los_Angeles' } })).entries.length, 3);
});

test('date and period boundaries are inclusive and explicit', async () => {
  const entries = Array.from({ length: 24 }, (_, hour) => note(`h${hour}`, `hour ${hour}`, at(`2026-11-02T${String(hour).padStart(2, '0')}:00:00Z`)));
  const expected = { morning: [5, 6, 7, 8, 9, 10, 11], noon: [12, 13], afternoon: [14, 15, 16, 17], evening: [18, 19, 20, 21], night: [0, 1, 2, 3, 4, 22, 23] };
  for (const [period, hours] of Object.entries(expected)) {
    const result = await queryNoteHistory(view(entries), { query: { time_of_day: period, date_from: '2026-11-02', date_to: '2026-11-02', order: 'oldest' } });
    assert.deepEqual(result.entries.map(note => Number(note.id.slice(1))), hours);
  }
});

test('changed search or changed history resets an old cursor', async () => {
  const values = view([note('a', 'alpha', 1), note('b', 'beta', 2)]);
  const first = await queryNoteHistory(values, { limit: 1 });
  const changed = await queryNoteHistory(values, { query: { text: 'alpha' }, cursor: first.next_cursor, limit: 1 });
  assert.equal(changed.restarted, true); assert.equal(changed.entries[0].id, 'a');
  values.archived_notes.push(note('c', 'gamma', 3));
  const newer = await queryNoteHistory(values, { cursor: first.next_cursor, limit: 1 });
  assert.equal(newer.restarted, true); assert.equal(newer.entries[0].id, 'c');
});

test('source and kind categories include reflections and drafts without pretending legacy timezone is known', async () => {
  const values = view([note('period', 'period words', 3, { kind: 'break' })]);
  values.reflections.push({ id: 'record', created_at_utc_ms: 1, heads: [{ id: 'revision', content: { kind: 'thought', body: 'reflection words' }, created_at_utc_ms: 2, author_sequence: 1 }] });
  values.drafts.push({ id: 'draft', content: { kind: 'intention', body: 'unfinished words' }, created_at_utc_ms: 1, updated_at_utc_ms: 4, version: 1 });
  assert.equal((await queryNoteHistory(values, { query: { source: 'reflection' } })).entries[0].body, 'reflection words');
  assert.equal((await queryNoteHistory(values, { query: { kind: 'intention' } })).entries[0].source, 'draft');
  assert.equal((await queryNoteHistory(values, { query: { kind: 'break' } })).entries.length, 1);
});

test('invalid ranges, zones, overlong Unicode text and malformed cursors fail explicitly', async () => {
  for (const query of [{ text: '字'.repeat(342) }, { text: 'word '.repeat(33) }, { text: '\ud800' },
    { date_from: '2026-02-30' }, { date_from: '2026-2-3' }, { date_from: '0000-01-01' },
    { date_from: '2026-11-03', date_to: '2026-11-02' }, { current_timezone: 'bad/timezone' },
    { order: 'drop table' }, { unexpected: true }]) assert.throws(() => normalizeHistoryQuery(query));
  await assert.rejects(queryNoteHistory(view([]), { limit: 0 }));
  await assert.rejects(queryNoteHistory(view([]), { cursor: { key: 'x', at_utc_ms: 1, revision: 'bad', query_hash: '0'.repeat(64) } }));
  assert.deepEqual(normalizeHistoryQuery({ text: '  ÉCOLE  中文 ', date_from: '' }).text, 'école 中文');
  assert.equal(normalizeHistoryQuery({ text: 'alpha\u0085beta' }).text, 'alpha beta');
});

test('browser bridge searches full stored history rather than its bounded timer snapshot', async () => {
  const values = new Map();
  const storage = { getItem: key => values.get(key) ?? null, setItem: (key, value) => values.set(key, value) };
  const bridge = createBrowserBridge({ storage, setInterval: () => 1, clearInterval: () => {} });
  try {
    const base = await bridge.core.invoke('snapshot');
    base.archived_notes = Array.from({ length: 150 }, (_, index) => note(`p-${index}`, index === 1 ? 'find this oldest writing' : 'other note', index));
    storage.setItem('rhythm.browser.v1', JSON.stringify({ schema: 3, view: base }));
    assert.equal((await bridge.core.invoke('snapshot')).archived_notes.length, 100);
    const result = await bridge.core.invoke('note_history_query', { query: { text: 'oldest writing' } });
    assert.deepEqual(result.entries.map(note => note.id), ['p-1']);
  } finally { bridge.dispose(); }
});
