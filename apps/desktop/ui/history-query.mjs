// Full browser-journal search. Native Windows uses the equivalent SQLite query.
// UTC orders notes; explicit display-zone selection controls only date/day filters.
import { noteHistoryEntries } from './note-history.mjs';

const encoder = new TextEncoder();
const decoder = new TextDecoder('utf-8', { fatal: true });
const keys = ['text', 'date_from', 'date_to', 'time_of_day', 'kind', 'source', 'order', 'time_basis', 'current_timezone'];
const oneOf = (value, options) => typeof value === 'string' && options.includes(value);
const keyOf = note => `${note.source === 'reflection_version' ? 'reflection' : note.source}:${note.id}`;
const fail = message => { throw new Error(message); };

function validDate(value) {
  if (value === null || value === undefined || value === '') return null;
  if (typeof value !== 'string' || !/^\d{4}-\d{2}-\d{2}$/.test(value) || value.startsWith('0000-')) {
    return fail('Search dates must use YYYY-MM-DD.');
  }
  const date = new Date(`${value}T00:00:00.000Z`);
  if (!Number.isFinite(date.getTime()) || date.toISOString().slice(0, 10) !== value) return fail('Choose a valid search date.');
  return value;
}

export function normalizeHistoryQuery(input = {}) {
  if (!input || typeof input !== 'object' || Array.isArray(input) || Object.keys(input).some(key => !keys.includes(key))) {
    return fail('The note search is invalid.');
  }
  const query = {
    text: input.text ?? '', date_from: validDate(input.date_from), date_to: validDate(input.date_to),
    time_of_day: input.time_of_day ?? 'all', kind: input.kind ?? 'all', source: input.source ?? 'all',
    order: input.order ?? 'newest', time_basis: input.time_basis ?? 'current', current_timezone: input.current_timezone ?? 'UTC',
  };
  if (typeof query.text !== 'string' || encoder.encode(query.text).length > 1024
    || decoder.decode(encoder.encode(query.text)) !== query.text) return fail('Search text is limited to 1,024 UTF-8 bytes.');
  // Keep literal punctuation (% and _ included); no regular expression or SQL comes from input.
  // Unicode White_Space matches Rust's split_whitespace (including U+0085,
  // excluding the zero-width BOM) rather than JavaScript's different \s set.
  const words = query.text.split(/\p{White_Space}+/u).filter(Boolean).map(word => word.toLowerCase());
  if (words.length > 32) return fail('Search supports up to 32 words.');
  query.text = words.join(' ');
  if (!oneOf(query.time_of_day, ['all', 'morning', 'noon', 'afternoon', 'evening', 'night'])
    || !oneOf(query.kind, ['all', 'focus', 'break', 'thought', 'intention', 'future_self'])
    || !oneOf(query.source, ['all', 'period', 'reflection', 'reflection_version', 'draft'])
    || !oneOf(query.order, ['newest', 'oldest']) || !oneOf(query.time_basis, ['current', 'original'])) {
    return fail('The note search filter is unsupported.');
  }
  if (typeof query.current_timezone !== 'string' || query.current_timezone.length > 128) return fail('Choose a valid display timezone.');
  try { new Intl.DateTimeFormat('en', { timeZone: query.current_timezone }).format(0); }
  catch { return fail('Choose a valid display timezone.'); }
  if (query.date_from !== null && query.date_to !== null && query.date_from > query.date_to) return fail('The start date must not follow the end date.');
  return query;
}

function compareKeys(left, right) {
  const a = Array.from(left, character => character.codePointAt(0));
  const b = Array.from(right, character => character.codePointAt(0));
  for (let index = 0; index < Math.min(a.length, b.length); index++) {
    if (a[index] !== b[index]) return a[index] - b[index];
  }
  return a.length - b.length;
}

async function hash(value) {
  const bytes = await globalThis.crypto.subtle.digest('SHA-256', encoder.encode(JSON.stringify(value)));
  return [...new Uint8Array(bytes)].map(byte => byte.toString(16).padStart(2, '0')).join('');
}

function periodOf(hour) {
  if (hour >= 5 && hour <= 11) return 'morning';
  if (hour >= 12 && hour <= 13) return 'noon';
  if (hour >= 14 && hour <= 17) return 'afternoon';
  if (hour >= 18 && hour <= 21) return 'evening';
  return 'night';
}

function calendarMatcher(query) {
  if (query.date_from === null && query.date_to === null && query.time_of_day === 'all') return () => true;
  const formatter = new Intl.DateTimeFormat('en-CA-u-ca-gregory-nu-latn', {
    timeZone: query.current_timezone, year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', hourCycle: 'h23',
  });
  return note => {
    const instant = note.created_at_utc_ms;
    if (!Number.isSafeInteger(instant) || Math.abs(instant) > 8_640_000_000_000_000) return false;
    let date, hour;
    if (query.time_basis === 'original') {
      const offset = note.source_utc_offset_minutes;
      if (!Number.isInteger(offset) || Math.abs(offset) > 1440 || !note.source_timezone) return false;
      const shifted = new Date(instant + offset * 60_000);
      if (!Number.isFinite(shifted.getTime())) return false;
      date = shifted.toISOString().slice(0, 10); hour = shifted.getUTCHours();
    } else {
      const parts = Object.fromEntries(formatter.formatToParts(instant).map(part => [part.type, part.value]));
      date = `${parts.year.padStart(4, '0')}-${parts.month}-${parts.day}`;
      hour = Number(parts.hour);
    }
    return (query.date_from === null || date >= query.date_from) && (query.date_to === null || date <= query.date_to)
      && (query.time_of_day === 'all' || periodOf(hour) === query.time_of_day);
  };
}

export async function queryNoteHistory(view, { query: input = {}, cursor = null, limit = 50 } = {}) {
  const query = normalizeHistoryQuery(input);
  if (!Number.isInteger(limit) || limit < 1 || limit > 100) return fail('History pages contain 1 to 100 notes.');
  if (cursor && (typeof cursor !== 'object' || typeof cursor.key !== 'string' || !cursor.key
    || encoder.encode(cursor.key).length > 512 || !Number.isSafeInteger(cursor.at_utc_ms)
    || !/^[0-9a-f]{64}$/i.test(cursor.revision ?? '') || !/^[0-9a-f]{64}$/i.test(cursor.query_hash ?? ''))) {
    return fail('The note search cursor is invalid.');
  }
  const rows = noteHistoryEntries(view);
  const metadata = rows.map(note => [keyOf(note), note.source, note.created_at_utc_ms, note.updated_at_utc_ms,
    note.archived_at_utc_ms, note.version, note.source_timezone ?? null, note.source_utc_offset_minutes ?? null])
    .sort((a, b) => compareKeys(a[0], b[0]));
  const [revision, queryHash] = await Promise.all([hash(metadata), hash(query)]);
  const restarted = Boolean(cursor && (cursor.revision !== revision || cursor.query_hash !== queryHash));
  if (restarted) cursor = null;
  const words = query.text.split(' ').filter(Boolean), calendar = calendarMatcher(query);
  const direction = query.order === 'oldest' ? 1 : -1;
  const filtered = rows.filter(note => {
    if (!/[^\p{White_Space}]/u.test(note.body) || (query.kind !== 'all' && note.kind !== query.kind)
      || (query.source !== 'all' && note.source !== query.source)) return false;
    if (words.length) {
      const lower = note.body.toLowerCase();
      if (!words.every(word => lower.includes(word))) return false;
    }
    if (!calendar(note)) return false;
    if (cursor && !(direction * (note.created_at_utc_ms - cursor.at_utc_ms) > 0
      || (note.created_at_utc_ms === cursor.at_utc_ms && compareKeys(keyOf(note), cursor.key) > 0))) return false;
    return true;
  }).sort((a, b) => direction * (a.created_at_utc_ms - b.created_at_utc_ms) || compareKeys(keyOf(a), keyOf(b)));
  const entries = structuredClone(filtered.slice(0, limit));
  const last = entries.at(-1);
  return { revision, entries, restarted,
    next_cursor: filtered.length > limit ? { revision, query_hash: queryHash, at_utc_ms: last.created_at_utc_ms, key: keyOf(last) } : null };
}
