// A read-only, paged archive. It never mutates the active editor or timer state.
const clone = value => structuredClone(value);
const noteKey = note => `${note.source === 'reflection_version' ? 'reflection' : note.source}:${note.id}`;

export function archiveDefaults(currentTimezone = Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC') {
  return { text: '', date_from: null, date_to: null, time_of_day: 'all', kind: 'all', source: 'all',
    order: 'newest', time_basis: 'current', current_timezone: currentTimezone };
}

export function noteTime(note) { return note.created_at_utc_ms ?? note.updated_at_utc_ms ?? note.archived_at_utc_ms; }

export function utcOffsetLabel(minutes) {
  if (!Number.isInteger(minutes) || Math.abs(minutes) > 1440) return null;
  const absolute = Math.abs(minutes);
  return `UTC${minutes < 0 ? '−' : '+'}${String(Math.floor(absolute / 60)).padStart(2, '0')}:${String(absolute % 60).padStart(2, '0')}`;
}

/** Original wall time uses its captured offset, including during a repeated DST hour. */
export function noteTimeLabels(note, { locale = 'en', timeBasis = 'current', currentTimezone = 'UTC' } = {}) {
  const at = noteTime(note);
  const options = { year: 'numeric', month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' };
  const current = new Intl.DateTimeFormat(locale, { ...options, timeZone: currentTimezone,
    timeZoneName: 'short' }).format(new Date(at));
  const utc = new Intl.DateTimeFormat(locale, { ...options, timeZone: 'UTC', timeZoneName: 'short' }).format(new Date(at));
  const offset = utcOffsetLabel(note.source_utc_offset_minutes);
  const original = offset === null ? null : `${new Intl.DateTimeFormat(locale, { ...options, timeZone: 'UTC' })
    .format(new Date(at + note.source_utc_offset_minutes * 60_000))} · ${offset}${note.source_timezone ? ` · ${note.source_timezone}` : ''}`;
  return { primary: timeBasis === 'original' ? original ?? utc : current,
    current, original, utc, originalKnown: original !== null };
}

function checkedPage(page) {
  if (!page || typeof page.revision !== 'string' || !Array.isArray(page.entries) || page.entries.length > 100
    || typeof page.restarted !== 'boolean' || (page.next_cursor !== null && typeof page.next_cursor !== 'object')
    || page.entries.some(note => !note || typeof note.id !== 'string' || typeof note.source !== 'string'
      || typeof note.body !== 'string' || !Number.isSafeInteger(noteTime(note)) || Math.abs(noteTime(note)) > 8_639_999_913_600_000)) {
    throw new Error('archiveReadFailed');
  }
  return page;
}

export class NoteArchiveController {
  constructor({ loadPage, pageSize = 25, onChange = () => {}, query = archiveDefaults() }) {
    if (typeof loadPage !== 'function' || !Number.isInteger(pageSize) || pageSize < 1 || pageSize > 100) {
      throw new Error('Choose a valid archive reader.');
    }
    this.loadPage = loadPage;
    this.pageSize = pageSize;
    this.onChange = onChange;
    this.query = clone(query);
    this.generation = 0;
    this.entries = [];
    this.nextCursor = null;
    this.revision = null;
    this.loading = false;
    this.loaded = false;
    this.error = null;
  }
  invalidate(query = this.query) {
    this.generation++;
    this.query = clone(query);
    this.entries = [];
    this.nextCursor = null;
    this.revision = null;
    this.loading = false;
    this.loaded = false;
    this.error = null;
    this.onChange();
  }
  reset(query = this.query) { this.invalidate(query); return this.loadMore(); }
  async loadMore() {
    if (this.loading || (this.loaded && this.nextCursor === null)) return false;
    const generation = this.generation, cursor = clone(this.nextCursor), query = clone(this.query);
    this.loading = true; this.error = null; this.onChange();
    try {
      const page = checkedPage(await this.loadPage({ query, cursor, limit: this.pageSize }));
      if (generation !== this.generation) return false;
      if (cursor && !page.restarted && page.revision !== this.revision) throw new Error('archiveChanged');
      if (cursor && page.next_cursor && JSON.stringify(cursor) === JSON.stringify(page.next_cursor)) throw new Error('archiveReadFailed');
      const previous = !cursor || page.restarted ? [] : this.entries;
      const seen = new Set(previous.map(noteKey));
      this.entries = [...previous, ...page.entries.filter(note => {
        const key = noteKey(note); if (seen.has(key)) return false; seen.add(key); return true;
      }).map(clone)];
      this.nextCursor = clone(page.next_cursor); this.revision = page.revision; this.loaded = true;
      return true;
    } catch (error) {
      if (generation !== this.generation) return false;
      this.error = error; throw error;
    } finally {
      if (generation === this.generation) { this.loading = false; this.onChange(); }
    }
  }
}

/** Keep this dialog independent of Settings so its form values and scroll survive. */
export function createNoteArchive({ dialog, loadPage, t, locale, onClose = () => {} }) {
  const controller = new NoteArchiveController({ loadPage, onChange: renderResults });
  let controls = {}, results, status, error, more, zoneHint, opener, debounce, composing = false;
  let shownEntries = [], shownContext = '';
  const node = (tag, text, className) => {
    const element = document.createElement(tag);
    if (text != null) element.textContent = text;
    if (className) element.className = className;
    return element;
  };
  const action = (text, handler, className = 'secondary') => {
    const element = node('button', text, className); element.type = 'button';
    element.addEventListener('click', handler); return element;
  };
  const currentZone = () => Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC';
  const quietly = promise => void promise.catch(() => {}); // The result pane owns its read errors.
  function readQuery() {
    const query = { current_timezone: currentZone() };
    for (const [key, input] of Object.entries(controls)) query[key] = input.value || (key.startsWith('date_') ? null : '');
    return query;
  }
  function validation(query) {
    if (query.date_from && query.date_to && query.date_from > query.date_to) return t('archiveDateOrder');
    if (new TextEncoder().encode(query.text).length > 1024 || query.text.split(/\p{White_Space}+/u).filter(Boolean).length > 32) return t('archiveSearchTooLong');
    return null;
  }
  function search(delay = 0) {
    clearTimeout(debounce);
    if (composing) return;
    const query = readQuery();
    controller.invalidate(query);
    const invalid = validation(query);
    if (invalid) { error.textContent = invalid; error.hidden = false; return; }
    if (delay) debounce = setTimeout(() => { if (dialog.open) quietly(controller.loadMore()); }, delay);
    else quietly(controller.loadMore());
  }
  function labelled(key, label, type, choices = null) {
    const labelNode = node('label', t(label));
    const input = node(choices ? 'select' : 'input');
    if (choices) {
      for (const [value, title] of choices) {
        const option = node('option', t(title)); option.value = value; input.append(option);
      }
    } else input.type = type;
    input.name = key;
    input.value = controller.query[key] ?? '';
    controls[key] = input; labelNode.append(input);
    input.addEventListener('change', () => search());
    return labelNode;
  }
  function draw() {
    controls = {};
    const header = node('header', null, 'dialog-header');
    const title = node('h2', t('archiveTitle')); title.id = 'archive-title';
    header.append(title, action(t('archiveClose'), close, 'link'));
    const body = node('div', null, 'archive-body');
    const form = node('form', null, 'archive-filters');
    form.setAttribute('aria-label', t('archiveFilters'));
    const searchLabel = labelled('text', 'archiveSearch', 'search');
    searchLabel.className = 'archive-search';
    controls.text.maxLength = 1024; controls.text.autocomplete = 'off';
    controls.text.placeholder = t('archiveSearchPlaceholder');
    controls.text.addEventListener('input', event => { if (!event.isComposing) search(250); });
    controls.text.addEventListener('compositionstart', () => { composing = true; clearTimeout(debounce); controller.invalidate(); });
    controls.text.addEventListener('compositionend', () => { composing = false; search(250); });
    const filters = node('div', null, 'archive-filter-grid');
    filters.append(labelled('date_from', 'archiveFrom', 'date'), labelled('date_to', 'archiveTo', 'date'),
      labelled('time_of_day', 'archiveTimeOfDay', null, [['all','archiveAnyTime'],['morning','archiveMorning'],['noon','archiveNoon'],['afternoon','archiveAfternoon'],['evening','archiveEvening'],['night','archiveNight']]),
      labelled('kind', 'archiveKind', null, [['all','archiveAnyKind'],['focus','focus'],['break','break'],['thought','archiveThought'],['intention','archiveIntention'],['future_self','archiveFutureSelf']]),
      labelled('source', 'archiveSource', null, [['all','archiveAnySource'],['period','archivePeriodNotes'],['reflection','historyReflection'],['reflection_version','historyVersion'],['draft','historyDraft']]),
      labelled('order', 'archiveOrder', null, [['newest','archiveNewest'],['oldest','archiveOldest']]),
      labelled('time_basis', 'archiveTimeBasis', null, [['current','archiveCurrentTime'],['original','archiveOriginalTime']]));
    zoneHint = node('p', null, 'caption archive-zone-hint');
    const reset = action(t('archiveClear'), () => {
      controller.query = archiveDefaults(currentZone());
      for (const [key, input] of Object.entries(controls)) input.value = controller.query[key] ?? '';
      search(); controls.text.focus({ preventScroll: true });
    }, 'link');
    form.append(searchLabel, filters, zoneHint, reset);
    form.addEventListener('submit', event => { event.preventDefault(); search(); });
    status = node('p', null, 'caption archive-status'); status.setAttribute('role','status'); status.setAttribute('aria-live','polite');
    error = node('p', null, 'routine-error'); error.setAttribute('role','alert'); error.hidden = true;
    results = node('div', null, 'archive-results'); results.setAttribute('role','list'); results.setAttribute('aria-label',t('archiveResults'));
    body.append(form, status, error, results);
    const footer = node('footer', null, 'settings-footer archive-footer');
    more = action(t('archiveMore'), () => quietly(controller.error?.message === 'archiveChanged' || (controller.error && !controller.loaded)
      ? controller.reset() : controller.loadMore()));
    footer.append(more, action(t('archiveClose'), close, 'link'));
    dialog.replaceChildren(header, body, footer);
    shownEntries = []; shownContext = '';
  }
  function renderResults() {
    if (!results) return;
    const query = controller.query;
    zoneHint.textContent = t(query.time_basis === 'original' ? 'archiveOriginalHint' : 'archiveCurrentHint', { zone: query.current_timezone });
    results.setAttribute('aria-busy', String(controller.loading));
    const context = `${locale()}:${query.time_basis}:${query.current_timezone}`;
    if (context !== shownContext || shownEntries.length > controller.entries.length
      || shownEntries.some((note, index) => note !== controller.entries[index])) {
      results.replaceChildren(); shownEntries = [];
    }
    // Appending another page keeps expanded notes and keyboard focus intact.
    for (const note of controller.entries.slice(shownEntries.length)) {
      const entry = node('article', null, 'archive-note'); entry.setAttribute('role','listitem');
      const detail = node('details');
      const summary = node('summary');
      const label = note.source === 'period' ? t(note.kind)
        : t(note.source === 'draft' ? 'historyDraft' : note.source === 'reflection_version' ? 'historyVersion' : 'historyReflection');
      const times = noteTimeLabels(note, { locale: locale(), timeBasis: query.time_basis, currentTimezone: query.current_timezone });
      const title = node('span', label, 'archive-note-kind');
      const time = node('time', times.primary, 'archive-note-time'); time.dateTime = new Date(noteTime(note)).toISOString();
      const preview = node('span', note.body.slice(0, 240).replace(/\s+/gu, ' ').trim(), 'archive-note-preview');
      summary.append(title, time);
      if (query.time_basis === 'original' && !times.originalKnown) summary.append(node('span', t('archiveOriginalUnknown'), 'caption archive-note-unknown'));
      summary.append(preview); detail.append(summary);
      const metadata = !times.originalKnown ? t('archiveOriginalUnknown')
        : query.time_basis === 'current' ? t('archiveOriginalLabel', { date: times.original })
          : t('archiveCurrentLabel', { date: times.current });
      detail.append(node('p', metadata, 'caption archive-note-zone'));
      // Long note text is mounted on deliberate expansion, not for every result.
      detail.addEventListener('toggle', () => {
        if (detail.open && !detail.querySelector('.archive-note-body')) detail.append(node('p', note.body, 'archive-note-body'));
      });
      entry.append(detail); results.append(entry);
    }
    shownEntries = controller.entries.slice(); shownContext = context;
    status.textContent = controller.loading ? t('loadingNotes') : controller.loaded && !controller.entries.length
      ? t('archiveNoResults') : controller.loaded ? t('archiveCount', { count: controller.entries.length }) : '';
    error.hidden = !controller.error;
    error.textContent = controller.error ? t(controller.error?.message === 'archiveChanged' ? 'archiveChanged' : 'archiveReadFailed') : '';
    more.hidden = !controller.nextCursor && !controller.error;
    more.disabled = controller.loading;
    more.textContent = t(controller.error ? 'retryHistory' : 'archiveMore');
  }
  function close() {
    const wasOpen = dialog.open;
    clearTimeout(debounce); composing = false; controller.invalidate();
    if (wasOpen) {
      dialog.close();
      opener?.isConnected && opener.focus({ preventScroll: true });
      onClose();
    }
  }
  dialog.addEventListener('cancel', event => { event.preventDefault(); close(); });
  dialog.addEventListener('click', event => {
    if (event.target !== dialog) return;
    const rect = dialog.getBoundingClientRect();
    if (event.clientX < rect.left || event.clientX > rect.right || event.clientY < rect.top || event.clientY > rect.bottom) close();
  });
  return {
    controller,
    open(trigger = document.activeElement) {
      opener = trigger;
      controller.query.current_timezone = currentZone();
      draw();
      if (!dialog.open) dialog.showModal();
      controls.text.focus({ preventScroll: true });
      search();
    },
    close,
    refresh() { if (dialog.open && !composing) search(); else controller.invalidate(); },
  };
}
