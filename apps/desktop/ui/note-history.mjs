// Note history is fetched only when the person opens or pages the archive.
// The timer's small snapshot remains independent of the total journal size.
const encoder = new TextEncoder();
const clone = value => structuredClone(value);
const keyOf = note => `${note.source === 'reflection_version' ? 'reflection' : note.source}:${note.id}`;
const atOf = note => note.archived_at_utc_ms ?? note.updated_at_utc_ms ?? note.created_at_utc_ms;

function compareKeys(left, right) {
  // SQLite BINARY compares UTF-8 code points, not locale order or UTF-16 units.
  const a = Array.from(left, char => char.codePointAt(0));
  const b = Array.from(right, char => char.codePointAt(0));
  for (let index = 0; index < Math.min(a.length, b.length); index++) {
    if (a[index] !== b[index]) return a[index] - b[index];
  }
  return a.length - b.length;
}

function checkPage(page) {
  if (!page || typeof page.revision !== 'string' || !Array.isArray(page.entries)
    || page.entries.length > 100 || typeof page.restarted !== 'boolean'
    || (page.next_cursor !== null && typeof page.next_cursor !== 'object')) {
    throw new Error('The older notes could not be read. Please try again.');
  }
  return page;
}

export class NoteHistoryPager {
  constructor({ loadPage, pageSize = 50, onChange = () => {} }) {
    if (typeof loadPage !== 'function' || !Number.isInteger(pageSize) || pageSize < 1 || pageSize > 100) {
      throw new Error('Choose a valid note history reader.');
    }
    this.loadPage = loadPage;
    this.pageSize = pageSize;
    this.onChange = onChange;
    this.generation = 0;
    this.entries = [];
    this.nextCursor = null;
    this.revision = null;
    this.loading = false;
    this.loaded = false;
    this.error = null;
  }
  invalidate() {
    this.generation++;
    this.entries = [];
    this.nextCursor = null;
    this.revision = null;
    this.loading = false;
    this.loaded = false;
    this.error = null;
    this.onChange();
  }
  async reset() {
    this.invalidate();
    return this.loadMore();
  }
  async loadMore() {
    if (this.loading || (this.loaded && !this.nextCursor)) return false;
    const generation = this.generation;
    const cursor = this.nextCursor;
    this.loading = true;
    this.error = null;
    this.onChange();
    try {
      const page = checkPage(await this.loadPage({ cursor, limit: this.pageSize, periodsOnly: false }));
      if (generation !== this.generation) return false;
      if (cursor && !page.restarted && page.revision !== this.revision) {
        throw new Error('Your notes changed while loading. Open the history again to refresh it.');
      }
      const previous = !cursor || page.restarted ? [] : this.entries;
      const seen = new Set(previous.map(keyOf));
      this.entries = [...previous, ...page.entries.filter(note => {
        const key = keyOf(note);
        if (seen.has(key)) return false;
        seen.add(key); return true;
      }).map(clone)];
      this.nextCursor = clone(page.next_cursor);
      this.revision = page.revision;
      this.loaded = true;
      return true;
    } catch (error) {
      if (generation !== this.generation) return false;
      this.error = error;
      throw error;
    } finally {
      if (generation === this.generation) {
        this.loading = false;
        this.onChange();
      }
    }
  }
}

/** Read every archived period note for encrypted exchange, never a partial page.
 * Its package limit is explicit; complete database backups have no note-count cap.
 */
export async function collectArchivedNotes(loadPage, { pageSize = 100, maxNotes = 1000 } = {}) {
  let cursor = null, revision = null;
  const notes = [], seen = new Set();
  do {
    const page = checkPage(await loadPage({ cursor, limit: pageSize, periodsOnly: true }));
    if (page.restarted || (revision !== null && page.revision !== revision)) {
      throw new Error('Your notes changed while preparing the file. Please export again.');
    }
    revision = page.revision;
    for (const entry of page.entries) {
      if (entry.source !== 'period' || seen.has(entry.id)) {
        throw new Error('The archived notes could not be read consistently. Please export again.');
      }
      seen.add(entry.id);
      const { source: _source, record_id: _record, ...note } = entry;
      notes.push(clone(note));
      if (notes.length > maxNotes) {
        throw new Error('Encrypted note exchange supports up to 1,000 notes per file. Create a complete backup to preserve every note.');
      }
    }
    if (page.next_cursor && JSON.stringify(page.next_cursor) === JSON.stringify(cursor)) {
      throw new Error('The older notes could not be read consistently. Please try again.');
    }
    cursor = page.next_cursor;
  } while (cursor);
  return notes;
}

/** Shared normalization for the recent panel, manual exports, and full archive. */
export function noteHistoryEntries(view, { periodsOnly = false } = {}) {
  const notes = (view.archived_notes ?? []).filter(note => note.archived_at_utc_ms !== null)
    .map(note => ({ ...clone(note), source: 'period', record_id: null }));
  if (!periodsOnly) {
    for (const reflection of view.reflections ?? []) {
      for (const head of reflection.heads ?? []) notes.push({
        id: head.id, source: 'reflection', kind: head.content.kind, body: head.content.body,
        version: head.author_sequence ?? 1, record_id: reflection.id,
        created_at_utc_ms: head.created_at_utc_ms ?? reflection.created_at_utc_ms,
        updated_at_utc_ms: head.created_at_utc_ms ?? reflection.created_at_utc_ms,
        archived_at_utc_ms: head.created_at_utc_ms ?? reflection.created_at_utc_ms,
        source_timezone: head.source_timezone ?? null,
        source_utc_offset_minutes: head.source_utc_offset_minutes ?? null,
      });
    }
    for (const draft of view.drafts ?? []) {
      // Older browser migrations copied standalone thought drafts into period
      // notes. Hide only that identical copy, not a subsequently changed draft.
      const migrated = draft.content.kind === 'thought' && !draft.record_id && !draft.content.session_id
        && notes.some(note => note.source === 'period' && note.kind === 'break'
          && note.body === draft.content.body && note.created_at_utc_ms === draft.created_at_utc_ms
          && note.updated_at_utc_ms === draft.updated_at_utc_ms);
      if (!migrated) notes.push({
        id: draft.id, source: 'draft', kind: draft.content.kind, body: draft.content.body,
        version: draft.version, created_at_utc_ms: draft.created_at_utc_ms,
        updated_at_utc_ms: draft.updated_at_utc_ms, archived_at_utc_ms: null,
        record_id: draft.record_id ?? null,
        source_timezone: draft.source_timezone ?? null,
        source_utc_offset_minutes: draft.source_utc_offset_minutes ?? null,
      });
    }
  }
  return notes;
}

/** The browser bridge passes its full stored view, not the latest-100 UI view. */
export async function pageNoteHistory(view, { cursor = null, limit = 50, periodsOnly = false } = {}) {
  if (!Number.isInteger(limit) || limit < 1 || limit > 100) throw new Error('History pages contain 1 to 100 notes.');
  if (cursor && (typeof cursor.key !== 'string' || !cursor.key || encoder.encode(cursor.key).length > 512
    || !Number.isSafeInteger(cursor.at_utc_ms) || !/^[0-9a-f]{64}$/i.test(cursor.revision ?? '')
    || cursor.periods_only !== periodsOnly)) throw new Error('The note history cursor is invalid.');
  const notes = noteHistoryEntries(view, { periodsOnly });
  notes.sort((left, right) => atOf(right) - atOf(left) || compareKeys(keyOf(left), keyOf(right)));
  const metadata = notes.map(note => [keyOf(note), note.source, atOf(note), note.version]);
  const hash = await globalThis.crypto.subtle.digest('SHA-256', encoder.encode(JSON.stringify(metadata)));
  const revision = Array.from(new Uint8Array(hash), byte => byte.toString(16).padStart(2, '0')).join('');
  const restarted = Boolean(cursor && cursor.revision !== revision);
  const effective = restarted ? null : cursor;
  const visible = notes.filter(note => note.body.trim() && (!effective || atOf(note) < effective.at_utc_ms
    || (atOf(note) === effective.at_utc_ms && compareKeys(keyOf(note), effective.key) > 0)));
  const entries = visible.slice(0, limit), last = entries.at(-1);
  const next_cursor = visible.length > limit ? {
    revision, at_utc_ms: atOf(last), key: keyOf(last), periods_only: periodsOnly,
  } : null;
  return { revision, entries, next_cursor, restarted };
}
