import { copy } from './copy.mjs';
import { rhythmCopy } from './rhythm-copy.mjs';
import { companionCopy } from './companion-copy.mjs';
import { dataCopy } from './data-copy.mjs';
import { autoSyncCopy } from './auto-sync-copy.mjs';
import { AutomaticSyncController, validSyncPassphrase } from './auto-sync.mjs';
import { captureProfile, validateProfile, restoreProfile } from './profile-backup.mjs';
import { NoteHistoryPager, collectArchivedNotes } from './note-history.mjs';
import { createNoteArchive, noteTimeLabels } from './note-archive.mjs';
import { noteHistoryCopy } from './note-history-copy.mjs';
import { AdaptiveStore, createAdaptiveState, configureAdaptive, recordRhythm, recordReceptivity, recommendRhythm, resetLearning } from './adaptive.mjs';
import { encryptNotes, decryptNotes } from './sync.mjs';
import { registerOffline } from './install.mjs';
import { BreakInvitationClock } from './break-invitation.mjs';
import { ReminderDelayStore, createReminderDelayState, configureReminderDelay, recordReminderResponse, recommendReminderDelay, resetReminderDelay } from './reminder-delay.mjs';
import { icon } from './icons.mjs';
import { createBrowserBridge } from './browser.mjs';
import { loadBackgrounds, mountBackgroundSettings } from './backgrounds.mjs';
import { CliQuitController, LifecycleBoundary, PeriodNoteController, isActive, personalState, reminderMatchesView, mergeRoutineState, mergeView, normalizedRoutineWindows, resolveLanguage, routineWindowsForEdit } from './model.mjs';

const app = document.querySelector('#app');
const errorBox = document.querySelector('#error');
const announcement = document.querySelector('#announcement');
const closeDialog = document.querySelector('#close-dialog');
const settingsDialog = document.querySelector('#settings-dialog');
const restoreDialog = document.querySelector('#restore-dialog');
const archiveDialog = document.querySelector('#archive-dialog');
const native = window.__TAURI__ ?? createBrowserBridge();
let view, platform, editor, editorTextarea, editorStatus, noteDock, noteToggle;
let busy = false, composing = false, noteOpen = false, fullscreen = false;
let saveTimeout, routineEditor = null, notice = '', advancedOpen = false;
let backgrounds, backgroundDraft, noteHistoryOpen = false, noteSyncPending = false, notesRefresh = null;
let noteSyncChain = Promise.resolve();
let hasEntered = preference('entered', 'false') === 'true';
let languagePreference = preference('language', 'system');
let themePreference = preference('theme', 'dark');
let language = resolveLanguage(languagePreference, navigator.languages);
const adaptiveStore = new AdaptiveStore();
let learning = createAdaptiveState(), learningPeriod = null, pendingReminder = null;
const reminderDelayStore = new ReminderDelayStore();
let reminderDelay = createReminderDelayState();
const breakClock = new BreakInvitationClock();
const lifecycle = new LifecycleBoundary();
let breakFrame = null, nativeBreakChoice = null, choiceHovered = false, choiceKeyboard = false;
let reminderVisibleAt = null, displayedReminderKey = null, notifiedReminderKey = null;
let reminderRevision = 0;
let recoveryIdea = 0, browserReminder = null;
let offlineReady = false, lastTerminalId = null;
let profileRestoring = false;
let syncSettingsUpdate = null, syncStatusTimer = null;
const automaticSync = new AutomaticSyncController({ invoke, onChange: () => syncSettingsUpdate?.() });
const historyPager = new NoteHistoryPager({ loadPage: args => invoke('note_history', args), pageSize: 5, onChange: () => renderNoteHistory() });
const noteArchive = createNoteArchive({ dialog: archiveDialog, loadPage: args => invoke('note_history_query', args),
  t, locale: () => language, onClose: () => { void maybeBeginBreak(); } });
const cliQuit = new CliQuitController({
  blockedReason: () => composing ? t('cliQuitComposing') : busy ? t('cliQuitBusy') : routineEditor ? t('cliQuitRoutineEdit') : null,
  lock: setBusy, flush: flushEditor,
  prepare: (token, error) => invoke('prepare_cli_quit_result', { token, error }), onError: showError,
});

function preference(key, fallback) { try { return localStorage.getItem(`rhythm.${key}`) ?? fallback; } catch { return fallback; } }
function remember(key, value) { try { localStorage.setItem(`rhythm.${key}`, value); } catch { /* Database notes do not depend on UI preferences. */ } }
function t(key, values = {}) {
  let text = noteHistoryCopy[language]?.[key] ?? autoSyncCopy[language]?.[key] ?? dataCopy[language]?.[key] ?? companionCopy[language]?.[key] ?? rhythmCopy[language]?.[key] ?? copy[language]?.[key] ?? noteHistoryCopy.en[key] ?? autoSyncCopy.en[key] ?? dataCopy.en[key] ?? companionCopy.en[key] ?? rhythmCopy.en[key] ?? copy.en[key] ?? key;
  for (const [name, value] of Object.entries(values)) text = text.replaceAll(`{${name}}`, String(value));
  return text;
}
function el(tag, text = null, className = '') {
  const node = document.createElement(tag);
  if (text !== null) node.textContent = text;
  if (className) node.className = className;
  return node;
}
function button(text, handler, className = '', lock = true) {
  const node = el('button', text, className); node.type = 'button'; node.dataset.action = 'true';
  node.addEventListener('click', () => {
    if (lock) void run(handler);
    else if (!busy && !composing && !cliQuit.active) handler();
  }); return node;
}
function iconButton(name, label, handler, className = '') {
  const node = button(null, handler, `icon-button ${className}`, !['note', 'close', 'settings'].includes(name));
  node.append(icon(name)); node.title = label; node.setAttribute('aria-label', label); return node;
}
function actions(...children) { const node = el('div', null, 'actions'); node.append(...children); return node; }
function announce(message) { announcement.textContent = message; }
function showError(error) {
  const message = typeof error === 'string' ? error : error?.message ?? String(error);
  if (closeDialog.open) {
    const target = document.querySelector('#close-error'); target.textContent = message; target.hidden = false; return;
  }
  if (settingsDialog.open) {
    let target = settingsDialog.querySelector('.settings-error');
    if (!target) { target = el('p', null, 'settings-error routine-error'); target.setAttribute('role', 'alert'); settingsDialog.querySelector('.settings-body').append(target); }
    target.textContent = message; target.scrollIntoView({ block: 'nearest' }); return;
  }
  errorBox.replaceChildren(el('p', message), iconButton('close', t('dismiss'), () => { errorBox.hidden = true; }));
  errorBox.hidden = false;
}
function setBusy(value) {
  busy = value;
  document.querySelectorAll('button[data-action]').forEach(node => { node.disabled = value || node.dataset.unavailable === 'true'; });
  if (editorTextarea) editorTextarea.readOnly = value || !editor;
  const fields = document.querySelector('#routine-fields'); if (fields) fields.disabled = value;
  syncSettingsUpdate?.();
}
async function run(operation) {
  if (busy || composing || cliQuit.active) return;
  setBusy(true);
  try { await operation(); } catch (error) { showError(error); } finally { setBusy(cliQuit.active); void maybeBeginBreak(); }
}
async function invoke(command, args = {}) { return native.core.invoke(command, args); }
async function request(op, data) {
  if (profileRestoring) throw new Error(t('finishingRestore'));
  const generation = lifecycle.generation;
  const response = await invoke('execute', { request: data === undefined ? { op } : { op, data } });
  if (!profileRestoring && lifecycle.current(generation) && lifecycle.observe(response.view)) view = mergeView(view, response.view);
  return response;
}
async function refresh() {
  if (profileRestoring) return;
  const generation = lifecycle.generation;
  const snapshot = await invoke('snapshot');
  if (!profileRestoring && lifecycle.current(generation) && lifecycle.observe(snapshot)) view = mergeView(view, snapshot);
}
function applyPreferences() {
  document.documentElement.lang = language;
  document.documentElement.dataset.theme = themePreference;
}
function focusHeading() { app.querySelector('h1')?.focus({ preventScroll: true }); }

function syncPeriodNote() {
  if (profileRestoring) return Promise.resolve();
  const generation = lifecycle.generation;
  const task = noteSyncChain.catch(() => {}).then(async () => {
    if (profileRestoring || !lifecycle.current(generation)) return;
    if (composing) { noteSyncPending = true; return; }
    if (editor?.periodId === view?.period_note?.id) { renderNoteHistory(); return; }
    if (editor) await editor.flush();
    if (profileRestoring || !lifecycle.current(generation)) return;
    if (composing) { noteSyncPending = true; return; }
    const note = view?.period_note;
    editor = note ? new PeriodNoteController({ note, request, onChange: updateSaveState }) : null;
    noteSyncPending = false;
    if (editorTextarea) {
      editorTextarea.value = editor?.body ?? '';
      editorTextarea.readOnly = busy || !editor;
    }
    updateSaveState(); renderNoteHistory();
  });
  noteSyncChain = task;
  return task;
}
function refreshPeriodNotes() {
  if (notesRefresh) return notesRefresh;
  notesRefresh = (async () => {
    await refresh(); await syncPeriodNote();
    if (noteHistoryOpen) await historyPager.reset(); else historyPager.invalidate();
    noteArchive.refresh();
  })()
    .catch(showError).finally(() => { notesRefresh = null; });
  return notesRefresh;
}
function updateSaveState() {
  if (!editorStatus) return;
  editorStatus.textContent = editor?.failure ? t('saveFailed') : '';
  editorStatus.hidden = !editor?.failure;
  editorStatus.classList.toggle('failed', Boolean(editor?.failure));
  const retry = noteDock?.querySelector('.note-retry'); if (retry) retry.hidden = !editor?.failure;
}
async function flushEditor() {
  clearTimeout(saveTimeout);
  if (composing) throw new Error(t('cliQuitComposing'));
  if (editor) await editor.flush();
}
function flushLifecycleEditor() {
  clearTimeout(saveTimeout);
  const current = editor;
  if (!current) return Promise.resolve();
  // Capture the original editor before a timer event changes the note period.
  // Composition stays active; only text already exposed in the textarea is saved.
  return current.flushForInterruption(editorTextarea?.value ?? current.body);
}
function clearLifecycleChoices() {
  nativeBreakChoice = null; learningPeriod = null; notifiedReminderKey = null;
  clearBreakInvitation();
}
function onLifecyclePrepare(event) {
  if (profileRestoring) return;
  lifecycle.prepare();
  clearLifecycleChoices();
  const token = typeof event?.payload?.token === 'string' ? event.payload.token : null;
  void (async () => {
    let failure = null;
    try { await flushLifecycleEditor(); }
    catch (error) { failure = error?.message ?? String(error); showError(error); }
    // Shutdown waits only for this bounded native handshake. A failed save is
    // reported honestly and leaves the old editor's text available for retry.
    if (token) {
      try { await invoke('lifecycle_saved', { token, error: failure }); }
      catch (error) { showError(error); }
    }
  })();
}
function onLifecycleInterrupted(update) {
  if (profileRestoring) return;
  if (!lifecycle.interrupt(update?.revision)) return;
  clearLifecycleChoices();
  hasEntered = true; remember('entered', 'true');
  const saved = flushLifecycleEditor();
  onTimer(update);
  if (view) renderState();
  // The old editor remains in place during IME composition. Otherwise switch
  // note ownership only after its serialized save has reached the worker.
  void saved.then(() => refreshPeriodNotes()).catch(showError);
}
function setNoteOpen(open, focus = true) {
  if (!noteDock || composing) return;
  if (open && !noteOpen) {
    noteHistoryOpen = false;
    renderNoteHistory();
  }
  noteOpen = open;
  noteDock.classList.toggle('open', open);
  noteDock.querySelector('.note-panel').hidden = !open;
  noteToggle.setAttribute('aria-expanded', String(open));
  noteToggle.setAttribute('aria-label', t(open ? 'closeNote' : 'openNote'));
  noteToggle.title = t(open ? 'closeNote' : 'openNote');
  if (focus) (open ? editorTextarea : noteToggle)?.focus({ preventScroll: true });
  if (!open) void flushEditor().catch(showError);
}
function mountNote() {
  if (noteDock) return;
  noteDock = el('aside'); noteDock.id = 'note-dock'; noteDock.setAttribute('aria-label', t('note'));
  const panel = el('div', null, 'note-panel'); panel.id = 'note-panel'; panel.hidden = true;
  const title = el('label', t('note'), 'note-title'); title.htmlFor = 'personal-note';
  const close = iconButton('close', t('closeNote'), () => setNoteOpen(false), 'note-close');
  editorTextarea = el('textarea'); editorTextarea.id = 'personal-note'; editorTextarea.value = editor?.body ?? '';
  editorTextarea.placeholder = t('notePlaceholder'); editorTextarea.spellcheck = true;
  editorTextarea.readOnly = !editor;
  const schedule = () => { clearTimeout(saveTimeout); saveTimeout = setTimeout(() => { if (!composing) void flushEditor().catch(showError); }, 400); };
  editorTextarea.addEventListener('compositionstart', () => { composing = true; clearTimeout(saveTimeout); });
  editorTextarea.addEventListener('compositionend', () => {
    composing = false; editor?.change(editorTextarea.value);
    if (noteSyncPending) void syncPeriodNote().catch(showError); else schedule();
    void maybeBeginBreak();
  });
  editorTextarea.addEventListener('input', () => { editor?.change(editorTextarea.value); if (!composing) schedule(); });
  editorTextarea.addEventListener('blur', () => { if (!composing) void flushEditor().catch(showError); });
  editorStatus = el('span', null, 'note-meta'); editorStatus.setAttribute('role', 'status');
  const retry = button(t('retry'), async () => { await flushEditor(); await syncPeriodNote(); }, 'link note-retry'); retry.hidden = true;
  const history = el('div', null, 'note-history'); history.hidden = true;
  const tools = el('div', null, 'note-tools');
  tools.append(iconButton('history', t('pastNotes'), async () => {
    await flushEditor(); await refresh(); await syncPeriodNote();
    noteHistoryOpen = !noteHistoryOpen; renderNoteHistory();
    if (noteHistoryOpen) await historyPager.reset();
  }, 'note-history-button'));
  panel.append(title, close, editorTextarea, history, editorStatus, retry, tools);
  noteToggle = iconButton('note', t('openNote'), () => setNoteOpen(!noteOpen), 'note-toggle');
  noteToggle.id = 'note-toggle'; noteToggle.setAttribute('aria-controls', 'note-panel'); noteToggle.setAttribute('aria-expanded', 'false');
  noteDock.append(panel, noteToggle); app.append(noteDock); updateSaveState(); renderNoteHistory();
  setNoteOpen(noteOpen, false);
}

function renderNoteHistory() {
  if (!noteDock) return;
  noteDock.classList.toggle('show-history', noteHistoryOpen);
  const history = noteDock.querySelector('.note-history');
  history.hidden = !noteHistoryOpen; editorTextarea.hidden = noteHistoryOpen;
  const toggle = noteDock.querySelector('.note-history-button');
  toggle.setAttribute('aria-pressed', String(noteHistoryOpen));
  toggle.title = t('pastNotes'); toggle.setAttribute('aria-label', toggle.title);
  noteDock.querySelector('.note-title').textContent = t(noteHistoryOpen ? 'pastNotes' : 'note');
  history.replaceChildren();
  const notes = historyPager.entries.slice(0, 5);
  history.append(el('p', t('recentNotesHint'), 'caption'));
  if (!notes.length && !historyPager.loading && !historyPager.error) history.append(el('p', t('noPastNotes'), 'caption'));
  for (const note of notes) {
    const entry = el('section', null, 'past-note');
    const label = note.source === 'period' ? t(note.kind)
      : t(note.source === 'draft' ? 'historyDraft' : note.source === 'reflection_version' ? 'historyVersion' : 'historyReflection');
    const times = noteTimeLabels(note, { locale: language, currentTimezone: Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC' });
    entry.append(el('h3', `${label} · ${times.primary}`), el('p', note.body));
    history.append(entry);
  }
  if (historyPager.error) history.append(el('p', historyPager.error?.message ?? String(historyPager.error), 'caption'));
  if (historyPager.loading) history.append(el('p', t('loadingNotes'), 'caption'));
  else if (historyPager.error) history.append(button(t('retryHistory'), () => historyPager.reset(), 'secondary'));
  history.append(allNotesButton('link'));
}

function allNotesButton(className) {
  const trigger = button(t('allNotes'), () => openAllNotes(trigger), className);
  return trigger;
}

async function openAllNotes(trigger) {
  await flushEditor();
  reminderVisibleAt = null; breakClock.tick({ active: false });
  noteArchive.open(trigger);
}

function saveLearning(next) { learning = adaptiveStore.save(next); }
function feedback(response) {
  try { if (learning.enabled) saveLearning(recordReceptivity(learning, { response })); } catch (error) { showError(error); }
}
function closeReminder() { browserReminder?.close(); browserReminder = null; }
function clearBreakInvitation() {
  reminderRevision += 1;
  pendingReminder = null; breakClock.clear();
  if (breakFrame !== null) cancelAnimationFrame(breakFrame);
  breakFrame = null; choiceHovered = false; choiceKeyboard = false;
  displayedReminderKey = null; reminderVisibleAt = null;
  closeReminder();
}
function displayState() { return lifecycle.interrupted ? 'break' : pendingReminder?.kind ?? personalState(view, hasEntered); }
function reminderKey(reminder = pendingReminder) { return reminder ? `${reminder.period_id}:${reminder.sequence}` : null; }
function onReminder(reminder) {
  if (profileRestoring) return;
  if (reminder && (lifecycle.interrupted || !reminderMatchesView(view, reminder))) return;
  if (reminder && pendingReminder?.period_id === reminder.period_id && reminder.sequence < pendingReminder.sequence) return;
  reminderRevision += 1;
  const previousKey = reminderKey();
  pendingReminder = reminder;
  if (!reminder?.available || reminderKey() !== previousKey) {
    breakClock.clear(); reminderVisibleAt = null; choiceHovered = false; choiceKeyboard = false;
    displayedReminderKey = null; closeReminder();
  }
  if (!reminder) clearBreakInvitation();
  if (view) { renderState(); void maybeBeginBreak(); }
  if (reminder?.available) notifyBrowserBreak(reminder);
}
async function refreshReminder() {
  const revision = ++reminderRevision;
  const reminder = await invoke('current_reminder');
  // A newer event, read, or state transition takes precedence over an in-flight read.
  if (revision === reminderRevision) onReminder(reminder);
}
function recordDelayChoice(reminder, response, responseMs) {
  if (!reminder || !reminderDelay.enabled) return;
  try {
    reminderDelay = reminderDelayStore.save(recordReminderResponse(reminderDelay, {
      kind: reminder.kind, response, responseMs, postponements: reminder.postponements,
      context: learning.context,
    }));
  } catch (error) { showError(error); }
}
function observePeriod() {
  if (!learning.enabled || !learningPeriod || learningPeriod.long) return;
  const actualMs = Date.now() - learningPeriod.started;
  if (actualMs <= 0 || actualMs > 86_400_000) return;
  try { saveLearning(recordRhythm(learning, { kind: learningPeriod.kind, actualMs,
    plannedMs: learningPeriod.plannedMs, context: learningPeriod.context, at: learningPeriod.started, manual: true })); }
  catch (error) { showError(error); }
}
async function switchState(kind, { automatic = false, durationMs = null, snooze = false } = {}) {
  if (lifecycle.preparing || (lifecycle.interrupted && (automatic || kind !== 'focus'))) return;
  const generation = lifecycle.generation;
  await flushEditor();
  if (!lifecycle.current(generation)) return;
  const reminder = pendingReminder;
  const responseMs = reminderVisibleAt === null ? null : Date.now() - reminderVisibleAt;
  const previousPeriod = learningPeriod;
  const settings = view.checkpoint.settings;
  const long = kind === 'break' && view.checkpoint.long_rest_due;
  const baseMs = settings[kind === 'focus' ? 'focus_ms' : long ? 'long_rest_ms' : 'short_rest_ms'];
  const plannedMs = durationMs ?? (long ? baseMs : recommendRhythm(learning, { kind, baseMs }).durationMs);
  await request(kind === 'focus' ? 'switch_to_focus' : 'switch_to_break', { duration_ms: plannedMs });
  if (!lifecycle.current(generation)) return;
  if ((!automatic || learningPeriod?.extended) && !snooze) observePeriod();
  if (reminder && !automatic && !snooze) {
    feedback('accept'); recordDelayChoice(reminder, 'accept', responseMs);
  }
  learningPeriod = snooze && previousPeriod?.kind === 'focus' ? { ...previousPeriod, activeId: view.checkpoint.state.data.id }
    : { kind, long, started: Date.now(), plannedMs, context: learning.context, activeId: view.checkpoint.state.data.id };
  clearBreakInvitation();
  await syncPeriodNote();
  hasEntered = true; remember('entered', 'true');
  renderState(); if (!automatic) focusHeading(); announce(t(kind === 'focus' ? 'focusReady' : 'breakReady'));
}
async function maybeBeginBreak() {
  if (!view || busy || composing || cliQuit.active || lifecycle.interrupted) return;
  if (nativeBreakChoice) {
    const choice = nativeBreakChoice; nativeBreakChoice = null;
    await run(() => respondToBreak(choice.action, { periodId: choice.period_id, sequence: choice.sequence,
      kind: choice.kind, reveal: choice.action === 'accept' }));
    return;
  }
  if (!pendingReminder?.available || settingsDialog.open || archiveDialog.open || closeDialog.open || view.checkpoint.state.state !== 'ready'
    || document.visibilityState !== 'visible' || !document.hasFocus()) return;
  const key = reminderKey();
  if (displayedReminderKey !== key) {
    displayedReminderKey = key; reminderVisibleAt = Date.now();
    breakClock.reset(key);
    announce(t(pendingReminder.kind === 'focus' ? 'inlineBreakHint' : 'inlineFocusHint'));
  }
  reminderVisibleAt ??= Date.now();
  if (breakFrame === null) breakFrame = requestAnimationFrame(updateBreakClock);
}
function updateBreakClock() {
  breakFrame = null;
  if (!pendingReminder?.available) return;
  const control = app.querySelector('.transition-control');
  choiceHovered = window.matchMedia('(hover: hover)').matches && Boolean(control?.matches(':hover'));
  const editing = document.activeElement?.matches('input,textarea,select,[contenteditable="true"]');
  const active = Boolean(control?.classList.contains('reminding')) && document.visibilityState === 'visible'
    && document.hasFocus() && !settingsDialog.open && !archiveDialog.open && !closeDialog.open && !editing;
  if (!active) reminderVisibleAt = null;
  else reminderVisibleAt ??= Date.now();
  const result = breakClock.tick({ active, blocked: busy || composing || choiceHovered || choiceKeyboard });
  const reduced = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  const progress = control?.querySelector('.break-ring-progress');
  if (progress) progress.style.strokeDashoffset = String(reduced ? 0 : 100 * (1 - result.progress));
  if (result.ready) void run(() => respondToBreak('accept', { automatic: true }));
  if (pendingReminder?.available) breakFrame = requestAnimationFrame(updateBreakClock);
}
async function respondToBreak(action, { periodId = pendingReminder?.period_id, sequence = pendingReminder?.sequence,
  kind = pendingReminder?.kind, automatic = false, reveal = false } = {}) {
  if (lifecycle.interrupted) return;
  const generation = lifecycle.generation;
  if (!periodId || !Number.isInteger(sequence) || !['focus', 'break'].includes(kind) || !['accept', 'snooze'].includes(action)) return;
  const answeredReminder = pendingReminder?.period_id === periodId && pendingReminder.sequence === sequence ? { ...pendingReminder } : null;
  const responseMs = reminderVisibleAt === null ? null : Date.now() - reminderVisibleAt;
  await flushEditor();
  if (!lifecycle.current(generation) || lifecycle.interrupted) return;
  if (automatic && (document.visibilityState !== 'visible' || !document.hasFocus() || settingsDialog.open || archiveDialog.open || closeDialog.open
    || document.activeElement?.matches('input,textarea,select,[contenteditable="true"]'))) {
    breakClock.clear(); if (pendingReminder?.available) breakClock.reset(reminderKey()); return;
  }
  const target = kind === 'focus' ? 'break' : 'focus';
  const long = target === 'break' && Boolean(view.checkpoint.long_rest_due);
  const baseMs = view.checkpoint.settings[target === 'focus' ? 'focus_ms' : long ? 'long_rest_ms' : 'short_rest_ms'];
  const durationMs = action === 'snooze' ? recommendReminderDelay(reminderDelay, { kind }).durationMs
    : long ? baseMs : recommendRhythm(learning, { kind: target, baseMs }).durationMs;
  const response = await invoke('respond_break_reminder', { periodId, sequence, action, durationMs, reveal });
  if (!lifecycle.current(generation) || lifecycle.interrupted) return;
  if (!response) {
    await refresh(); await refreshReminder(); await syncPeriodNote(); renderState(); return;
  }
  view = mergeView(view, response.view);
  if (!automatic) {
    feedback(action); recordDelayChoice(answeredReminder, action, responseMs);
  }
  // The waiting interval and a default choice are not evidence of focused work.
  learningPeriod = action === 'accept' ? { kind: target, long, started: Date.now(), plannedMs: durationMs,
    context: learning.context, activeId: view.checkpoint.state.data.id } : null;
  await refreshReminder();
  await syncPeriodNote(); hasEntered = true; remember('entered', 'true'); renderState();
  announce(t(action === 'accept' ? target === 'focus' ? 'focusReady' : 'breakReady' : kind === 'focus' ? 'stayFocusedHint' : 'keepRestingHint'));
}
function syncFullscreenControl() {
  const control = document.querySelector('#fullscreen-toggle'); if (!control) return;
  control.replaceChildren(icon(fullscreen ? 'collapse' : 'expand'));
  control.title = t(fullscreen ? 'exitFull' : 'full'); control.setAttribute('aria-label', control.title);
}
function drawShell() {
  app.replaceChildren(); noteDock = null;
  const landscape = el('div', null, 'landscape'); landscape.setAttribute('aria-hidden', 'true');
  landscape.append(el('div', null, 'scene scene-focus'), el('div', null, 'scene scene-break'));
  if (backgrounds) void backgrounds.apply(landscape).catch(showError);
  const header = el('header', null, 'app-header');
  const brand = el('span', null, 'brand'); brand.append(icon('rhythm', 'brand-mark'), document.createTextNode('Rhythm'));
  const controls = el('div', null, 'header-controls');
  const fullControl = iconButton(fullscreen ? 'collapse' : 'expand', t(fullscreen ? 'exitFull' : 'full'), async () => {
    await invoke('set_fullscreen', { enabled: !fullscreen }); fullscreen = !fullscreen;
    if (platform?.is_browser) fullscreen = Boolean(document.fullscreenElement);
    syncFullscreenControl();
  });
  fullControl.id = 'fullscreen-toggle'; controls.append(fullControl);
  const settings = iconButton('settings', t('settings'), openSettings); settings.id = 'settings-toggle';
  settings.setAttribute('aria-haspopup', 'dialog'); controls.append(settings); header.append(brand, controls);
  app.append(landscape, header, el('main', null, 'stage'));
  renderState();
}
function renderState() {
  applyPreferences();
  const state = displayState();
  document.documentElement.dataset.state = state; app.dataset.state = state;
  const stage = app.querySelector('.stage'); if (!stage) return;
  // A reminder expands only the existing switch. Headings, scenery and notes
  // retain their nodes, animation state and focus during postponements.
  if (stage.dataset.state === state && stage.dataset.language === language && stage.querySelector('.transition-control')) {
    updateTransition(); setBusy(busy); return;
  }
  stage.dataset.state = state; stage.dataset.language = language;
  stage.replaceChildren();
  const heading = el('h1', null, 'state-heading'); heading.tabIndex = -1;
  if (state !== 'init') heading.append(icon(state, 'state-icon'));
  heading.append(document.createTextNode(t(state === 'init' ? 'welcome' : state))); stage.append(heading);
  stage.append(el('p', t(`${state}Hint`), 'state-hint'));
  mountTransition(stage, state);
  if (state === 'break') {
    const suggestions = ['recoveryWalk', 'recoveryEyes', 'recoveryWater', 'recoveryBreathe'];
    const suggestionText = el('span', t(suggestions[recoveryIdea]));
    const suggestion = button(null, () => {
      recoveryIdea = (recoveryIdea + 1) % suggestions.length;
      suggestionText.textContent = t(suggestions[recoveryIdea]);
      announce(suggestionText.textContent);
    }, 'recovery-suggestion', false);
    suggestion.title = t('recoveryIdeas'); suggestion.append(icon('spark'), suggestionText);
    stage.append(suggestion);
  }
  if (state !== 'init') { mountNote(); noteDock.hidden = false; }
  else if (noteDock) noteDock.hidden = true;
  setBusy(busy);
}
function mountTransition(stage, state) {
  const target = state === 'focus' ? 'break' : 'focus';
  const control = el('div', null, 'transition-control');
  control.setAttribute('role', 'group');
  const transition = button(null, () => pendingReminder?.available ? respondToBreak('accept') : switchState(target), 'transition-button');
  transition.setAttribute('aria-label', t(target === 'focus' ? 'enterFocus' : 'takeBreak'));
  transition.append(icon(target), el('span', t(target)), icon('arrow', 'transition-arrow'));
  const ring = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  ring.setAttribute('viewBox', '0 0 24 24'); ring.setAttribute('class', 'choice-ring'); ring.setAttribute('aria-hidden', 'true');
  for (const className of ['break-ring-track', 'break-ring-progress']) {
    const circle = document.createElementNS('http://www.w3.org/2000/svg', 'circle');
    for (const [key, value] of Object.entries({ cx: 12, cy: 12, r: 9, pathLength: 100, class: className })) circle.setAttribute(key, value);
    ring.append(circle);
  }
  transition.append(ring);
  const stayChoice = el('div', null, 'stay-choice');
  const stay = button(null, () => respondToBreak('snooze'), 'stay-button');
  stay.append(icon(state === 'focus' ? 'focus' : 'break'), el('span', t(state === 'focus' ? 'stayFocused' : 'keepResting')));
  stayChoice.append(stay); control.append(transition, stayChoice); stage.append(control);
  control.addEventListener('pointerenter', () => { choiceHovered = true; });
  control.addEventListener('pointerleave', () => { choiceHovered = false; });
  control.addEventListener('focusin', () => { if (pendingReminder?.available) choiceKeyboard = true; });
  control.addEventListener('focusout', event => { if (!control.contains(event.relatedTarget)) choiceKeyboard = false; });
  updateTransition();
}
function updateTransition() {
  const control = app.querySelector('.transition-control'); if (!control) return;
  const available = Boolean(pendingReminder?.available);
  control.classList.toggle('reminding', available);
  const stay = control.querySelector('.stay-button');
  stay.dataset.unavailable = String(!available); stay.disabled = busy || !available; stay.tabIndex = available ? 0 : -1;
  control.querySelector('.stay-choice').inert = !available;
  const state = displayState();
  control.setAttribute('aria-label', t(available ? state === 'focus' ? 'inlineBreakHint' : 'inlineFocusHint' : state === 'focus' ? 'takeBreak' : 'enterFocus'));
  if (available && control.contains(document.activeElement)) choiceKeyboard = true;
}
function render() { renderState(); if (settingsDialog.open) drawSettings(); }

function openSettings() {
  reminderVisibleAt = null; breakClock.tick({ active: false });
  if (noteOpen) setNoteOpen(false, false);
  backgroundDraft = backgrounds?.createDraft();
  notice = ''; drawSettings(); settingsDialog.showModal();
  if (!platform?.is_browser) {
    void automaticSync.refresh();
    clearInterval(syncStatusTimer);
    syncStatusTimer = setInterval(() => { if (settingsDialog.open) void automaticSync.refresh(); }, 5000);
  }
}
function closeSettings() {
  clearInterval(syncStatusTimer); syncStatusTimer = null;
  settingsDialog.querySelectorAll('.auto-sync-settings input').forEach(input => { input.value = ''; });
  backgroundDraft?.dispose(); backgroundDraft = null;
  settingsDialog.close(); document.querySelector('#settings-toggle')?.focus();
  void maybeBeginBreak();
}
function drawSettings() {
  const previousScroll = settingsDialog.open ? settingsDialog.querySelector('.settings-body')?.scrollTop ?? 0 : 0;
  syncSettingsUpdate = null;
  const oldAdvanced = settingsDialog.querySelector('.advanced-settings'); if (oldAdvanced) advancedOpen = oldAdvanced.open;
  settingsDialog.replaceChildren();
  const header = el('header', null, 'dialog-header');
  const title = el('h2', t('settings')); title.id = 'settings-title';
  header.append(title, iconButton('close', t('closeSettings'), closeSettings));
  const main = el('div', null, 'settings-body');
  const form = el('form'); const fields = el('div', null, 'fields'); const inputs = {};
  form.append(el('h3', t('usualRhythm')));
  for (const [key, label] of [['focus_ms', 'focusMinutes'], ['short_rest_ms', 'shortMinutes'], ['long_rest_ms', 'longMinutes'], ['focus_before_long_rest', 'every']]) {
    const duration = key !== 'focus_before_long_rest';
    const item = field(t(label), 'number', duration ? view.checkpoint.settings[key] / 60000 : view.checkpoint.settings[key] - 1, duration ? 1 : 0, duration ? 1440 : 999);
    inputs[key] = item.input; fields.append(item.label);
  }
  form.append(fields);
  const learningSettings = mountLearningSettings(form);
  const backgroundSection = el('div');
  if (backgrounds && backgroundDraft) mountBackgroundSettings(backgroundSection, { draft: backgroundDraft, t, onError: showError });
  const advanced = el('details', null, 'advanced-settings'); advanced.open = advancedOpen;
  advanced.append(el('summary', t('advancedSettings')));
  const choices = el('div', null, 'fields');
  const languages = selectField(t('language'), [['system', t('system')], ['en', 'English'], ['zh-Hans', '简体中文'], ['zh-Hant', '繁體中文']], languagePreference);
  const themes = selectField(t('appearance'), [['dark', t('dark')], ['light', t('light')], ['system', t('system')]], themePreference);
  choices.append(languages.label, themes.label); advanced.append(choices);
  const delaySettings = mountReminderDelaySettings(advanced);
  advanced.append(allNotesButton('secondary'));
  if (platform?.is_browser) {
    advanced.append(el('p', t('browserStorage'), 'caption'));
    if (offlineReady) advanced.append(el('p', t('offlineReady'), 'caption'));
  }
  else {
    renderRoutineSettings(advanced);
    advanced.append(el('h3', t('localData')), el('p', t('dataHint'), 'caption'));
    const restore = button(t('restoreData'), restoreBackup, 'link');
    restore.dataset.unavailable = String(isActive(view.checkpoint)); restore.disabled = isActive(view.checkpoint);
    advanced.append(actions(button(t('backupData'), createCompleteBackup, 'secondary'),
      button(t('exportMarkdown'), () => savePortableCopy('export_markdown', 'exportSaved'), 'secondary'), restore));
    if (isActive(view.checkpoint)) advanced.append(el('p', t('restoreBetween'), 'caption'));
  }
  mountReminderSettings(advanced); mountAutomaticSyncSettings(advanced); mountSyncSettings(advanced);
  // Routine editing has its own form; keep it outside the duration form.
  const save = button(t('done'), async () => {
    if (!form.reportValidity()) return;
    const settings = {};
    for (const [key, input] of Object.entries(inputs)) {
      const value = Number(input.value), duration = key !== 'focus_before_long_rest';
      if (!Number.isInteger(value) || value < (duration ? 1 : 0) || value > (duration ? 1440 : 999)) throw new Error(t('validation'));
      settings[key] = duration ? value * 60000 : value + 1;
    }
    let nextLearning = learningSettings();
    const nextDelay = delaySettings();
    const durationsChanged = ['focus_ms', 'short_rest_ms', 'long_rest_ms'].some(key => settings[key] !== view.checkpoint.settings[key]);
    if (durationsChanged) nextLearning = resetLearning(nextLearning, 'rhythm');
    await request('update_settings', { settings });
    if (durationsChanged || nextLearning.enabled !== learning.enabled) learningPeriod = null;
    saveLearning(nextLearning);
    reminderDelay = reminderDelayStore.save(nextDelay);
    if (backgrounds && backgroundDraft) {
      await backgrounds.save(backgroundDraft); backgroundDraft.dispose(); backgroundDraft = backgrounds.createDraft();
    }
    languagePreference = languages.select.value; themePreference = themes.select.value;
    language = resolveLanguage(languagePreference, navigator.languages);
    remember('language', languagePreference); remember('theme', themePreference);
    await invoke('set_notification_language', { language });
    notice = t('settingsSaved'); announce(notice);
    drawShell(); drawSettings();
  }, 'primary');
  save.setAttribute('form', 'rhythm-settings'); form.id = 'rhythm-settings';
  form.addEventListener('submit', event => { event.preventDefault(); save.click(); });
  main.append(form, backgroundSection, advanced);
  if (notice) main.append(el('p', notice, 'notice caption'));
  const footer = el('footer', null, 'settings-footer'); footer.append(save);
  settingsDialog.append(header, main, footer); setBusy(busy);
  main.scrollTop = previousScroll;
}

function mountReminderDelaySettings(section) {
  const group = el('section', null, 'reminder-delay-settings');
  group.append(el('h3', t('delaySettings'), 'section-title'));
  const delay = field(t('delayMinutes'), 'number', reminderDelay.baseMs / 60_000, 1, 60);
  delay.input.setAttribute('form', 'rhythm-settings');
  const consent = el('label', null, 'check-label');
  const enabled = el('input'); enabled.type = 'checkbox'; enabled.checked = reminderDelay.enabled;
  consent.append(enabled, document.createTextNode(t('learnDelay')));
  const hint = el('p', t('delayLearningHint'), 'caption');
  group.append(delay.label, consent, hint);
  group.append(button(t('resetDelay'), () => {
    reminderDelay = reminderDelayStore.save(resetReminderDelay(reminderDelay));
    hint.textContent = t('delayReset'); announce(t('delayReset'));
  }, 'link'));
  section.append(group);
  return () => configureReminderDelay(reminderDelay, { baseMs: Number(delay.input.value) * 60_000, enabled: enabled.checked });
}

function mountLearningSettings(form) {
  const section = el('details', null, 'learning-settings');
  section.append(el('summary', t('learning')), el('p', t('learningHint'), 'caption'));
  const consent = el('label', null, 'check-label'); const enabled = el('input'); enabled.type = 'checkbox'; enabled.checked = learning.enabled;
  consent.append(enabled, document.createTextNode(t('learningToggle'))); section.append(consent);
  const context = selectField(t('context'), ['general', 'work', 'study', 'creative'].map(key => [key, t(key)]), learning.context);
  section.append(context.label);
  const bounds = el('div', null, 'fields'); const limits = {};
  for (const [key, kind, edge] of [['focusMin', 'focus', 'minMs'], ['focusMax', 'focus', 'maxMs'], ['breakMin', 'break', 'minMs'], ['breakMax', 'break', 'maxMs']]) {
    const control = field(t(key), 'number', learning.bounds[kind][edge] / 60_000, 1, 1440);
    limits[key] = control.input; bounds.append(control.label);
  }
  section.append(bounds, el('p', t('learningWaiting'), 'caption'));
  if (learning.enabled) {
    const focus = recommendRhythm(learning, { kind: 'focus', baseMs: view.checkpoint.settings.focus_ms });
    const rest = recommendRhythm(learning, { kind: 'break', baseMs: view.checkpoint.settings.short_rest_ms });
    section.append(el('p', t('learningResult', { focus: Math.round(focus.durationMs / 6000) / 10,
      break: Math.round(rest.durationMs / 6000) / 10, count: focus.samples + rest.samples }), 'caption'));
    section.append(el('p', t('reminderGentle'), 'caption'));
  }
  section.append(button(t('resetLearning'), () => { saveLearning(resetLearning(learning)); learningPeriod = null; notice = t('learningReset'); drawSettings(); }, 'link'));
  form.append(section);
  return () => configureAdaptive(learning, { enabled: enabled.checked, context: context.select.value,
    bounds: { focus: { minMs: Number(limits.focusMin.value) * 60_000, maxMs: Number(limits.focusMax.value) * 60_000 },
      break: { minMs: Number(limits.breakMin.value) * 60_000, maxMs: Number(limits.breakMax.value) * 60_000 } } });
}

function mountReminderSettings(section) {
  if (!platform?.is_browser) return;
  section.append(el('h3', t('reminderSettings'), 'section-title'), el('p', t('browserLimit'), 'caption'));
  section.append(button(t('browserReminders'), async () => {
    if (!('Notification' in window)) throw new Error(t('remindersDenied'));
    const permission = await Notification.requestPermission();
    if (permission !== 'granted') throw new Error(t('remindersDenied'));
    notice = t('remindersEnabled'); drawSettings();
  }, 'secondary'));
}

function mountAutomaticSyncSettings(section) {
  const group = el('section', null, 'auto-sync-settings');
  group.append(el('h3', t('autoSyncTitle'), 'section-title'));
  section.append(group);
  if (platform?.is_browser) {
    group.append(el('p', t('autoSyncBrowser'), 'caption'));
    return;
  }
  group.append(el('p', t('autoSyncHint'), 'caption'), el('p', t('autoSyncScope'), 'caption'));
  const status = el('p', null, 'auto-sync-status caption'); status.setAttribute('role', 'status');
  const lastChecked = el('p', null, 'caption');
  const error = el('p', null, 'routine-error caption'); error.setAttribute('role', 'alert');
  group.append(status, lastChecked, error);
  const setup = el('div', null, 'auto-sync-setup');
  const privacy = el('p', t('autoSyncPrivacy'), 'caption'); privacy.id = 'auto-sync-privacy';
  const password = field(t('autoSyncPhrase'), 'password', '');
  const repeated = field(t('autoSyncRepeat'), 'password', '');
  for (const input of [password.input, repeated.input]) {
    input.autocomplete = 'new-password'; input.required = false; input.maxLength = 1024;
    input.setAttribute('aria-describedby', privacy.id);
  }
  setup.append(privacy, password.label, repeated.label);
  const controls = [];
  const control = (label, handler, className = 'secondary') => {
    const node = el('button', t(label), className); node.type = 'button'; node.dataset.action = 'true';
    node.addEventListener('click', () => {
      if (busy || composing || cliQuit.active || automaticSync.action || node.disabled) return;
      void handler();
    });
    controls.push(node); return node;
  };
  const connect = control('autoSyncConnect', async () => {
    if (!validSyncPassphrase(password.input.value)) { automaticSync.fail(new Error('autoSyncPhraseInvalid')); password.input.focus(); return; }
    if (password.input.value !== repeated.input.value) { automaticSync.fail(new Error('autoSyncPhraseMismatch')); repeated.input.focus(); return; }
    const passphrase = password.input.value;
    password.input.value = ''; repeated.input.value = '';
    await automaticSync.connect(passphrase);
  });
  setup.append(actions(connect));
  const retry = control('autoSyncRetry', () => automaticSync.retry());
  const disconnect = control('autoSyncDisconnect', () => automaticSync.disconnect(), 'link');
  const connectionActions = actions(retry, disconnect);
  const disconnectHint = el('p', t('autoSyncDisconnectHint'), 'caption');
  group.append(setup, connectionActions, disconnectHint);
  syncSettingsUpdate = () => {
    if (!group.isConnected) return;
    const current = automaticSync.status;
    const state = current?.state;
    const action = automaticSync.action;
    const keys = {
      not_configured: 'autoSyncNotConfigured', disconnected: 'autoSyncDisconnected',
      connecting: 'autoSyncConnecting', syncing: 'autoSyncSyncing', ready: 'autoSyncReady', error: 'autoSyncError',
    };
    status.textContent = t(action === 'connect_sync' ? 'autoSyncConnecting'
      : action === 'disconnect_sync' ? 'autoSyncDisconnecting' : keys[state] ?? 'autoSyncChecking');
    const last = current?.last_success_at_utc_ms;
    lastChecked.hidden = last == null;
    lastChecked.textContent = last == null ? '' : t('autoSyncLastChecked', { date: date(last) });
    const message = automaticSync.error ?? current?.error;
    error.hidden = !message; error.textContent = message ? t(message) : '';
    const needsConnect = !current?.connected || current.reconnect_required;
    setup.hidden = !current || state === 'not_configured' || !needsConnect;
    connectionActions.hidden = !current?.connected;
    disconnectHint.hidden = !current?.connected;
    retry.hidden = Boolean(current?.reconnect_required);
    connect.textContent = t(current?.reconnect_required ? 'autoSyncReconnect' : 'autoSyncConnect');
    for (const node of controls) {
      const unavailable = !current || state === 'not_configured' || Boolean(action) || state === 'connecting'
        || (node === retry && state === 'syncing');
      node.dataset.unavailable = String(unavailable);
      node.disabled = busy || unavailable;
    }
    password.input.disabled = repeated.input.disabled = busy || Boolean(action) || state === 'connecting';
  };
  // drawSettings calls setBusy after adding the section to the live dialog.
}

function mountSyncSettings(section) {
  section.append(el('h3', t('syncTitle'), 'section-title'), el('p', t('syncHint'), 'caption'), el('p', t('syncScope'), 'caption'));
  const password = field(t('syncPassword'), 'password', ''); password.input.minLength = 12;
  password.input.autocomplete = 'off'; password.input.required = false; section.append(password.label);
  const file = el('input'); file.type = 'file'; file.accept = '.rhythm,application/json'; file.hidden = true;
  file.addEventListener('change', () => { void run(async () => {
    const chosen = file.files?.[0]; if (!chosen) return;
    if (chosen.size > 12 * 1024 * 1024) throw new Error('This encrypted note file is too large.');
    const notes = await decryptNotes(await chosen.text(), password.input.value);
    await flushEditor(); await request('import_period_notes', { notes });
    notice = t('syncImported'); password.input.value = ''; historyPager.invalidate(); renderNoteHistory(); drawSettings();
  }).finally(() => { file.value = ''; }); });
  section.append(actions(button(t('syncExport'), async () => {
    await flushEditor(); await refresh();
    const notes = await collectArchivedNotes(args => invoke('note_history', args));
    const encrypted = await encryptNotes(notes, password.input.value);
    if (!platform.is_browser) {
      const path = await invoke('save_encrypted_notes', { contents: encrypted });
      if (path === null) return;
    } else {
      const url = URL.createObjectURL(new Blob([encrypted], { type: 'application/json' }));
      const link = el('a'); link.href = url; link.download = 'rhythm-notes.rhythm'; document.body.append(link); link.click(); link.remove();
      setTimeout(() => URL.revokeObjectURL(url), 60_000);
    }
    password.input.value = ''; notice = t(platform.is_browser ? 'syncDownload' : 'syncSaved'); drawSettings();
  }, 'secondary'), button(t('syncImport'), () => {
    if (password.input.value.length < 12) throw new Error(t('syncPassword'));
    file.click();
  }, 'secondary')), file);
}

function notifyBrowserBreak(reminder) {
  if (!platform?.is_browser || !reminder.available || document.hasFocus() || !('Notification' in window)
    || Notification.permission !== 'granted' || notifiedReminderKey === reminderKey(reminder)) return;
  try {
    browserReminder = new Notification('Rhythm', { body: t(reminder.kind === 'focus' ? 'breakChoiceHint' : 'browserFocusHint'),
      tag: 'rhythm-transition', silent: true, requireInteraction: false });
    notifiedReminderKey = reminderKey(reminder);
    // Deliberately no window.focus(): a reminder never takes foreground focus.
  } catch { /* The inline invitation remains available if system delivery is unsupported. */ }
}

async function savePortableCopy(command, successKey) {
  await flushEditor(); const path = await invoke(command);
  if (path !== null) { notice = t(successKey, { path }); announce(notice); drawSettings(); }
}
async function createCompleteBackup() {
  if (backgroundDraft?.pending) throw new Error(t('waitForImage'));
  await flushEditor();
  const profile = await captureProfile({ backgrounds });
  const path = await invoke('backup_data', { profile });
  if (path !== null) { notice = t('backupSaved', { path }); announce(notice); drawSettings(); }
}
function reloadPersonalPreferences({ strict = false } = {}) {
  languagePreference = preference('language', 'system');
  themePreference = preference('theme', 'dark');
  hasEntered = preference('entered', 'false') === 'true';
  language = resolveLanguage(languagePreference, navigator.languages);
  try { learning = adaptiveStore.load(); }
  catch (error) { if (strict) throw error; learning = createAdaptiveState(); showError(error); }
  try { reminderDelay = reminderDelayStore.load(); }
  catch (error) { if (strict) throw error; reminderDelay = createReminderDelayState(); showError(error); }
  learningPeriod = null; applyPreferences();
}
async function finishProfileRestore(pending) {
  if (!pending) return;
  await restoreProfile(pending.profile, { backgrounds });
  const acknowledged = await invoke('acknowledge_profile_restore', { token: pending.token });
  if (!acknowledged) throw new Error(t('restoreChanged'));
}
function confirmRestore(selected) {
  const title = el('h2', t('restoreTitle')); title.id = 'restore-title';
  const file = el('p', selected.file_name, 'restore-file'); file.title = selected.file_name;
  const detail = el('p', t(selected.profile ? 'restoreCompleteDetail' : 'restoreLegacyDetail')); detail.id = 'restore-detail';
  const safety = el('p', t('restoreSafety'), 'restore-safety'); safety.id = 'restore-safety';
  // The outer action stays locked while these two explicit choices remain usable.
  const cancel = el('button', t('restoreCancel'), 'secondary'); cancel.type = 'button';
  const accept = el('button', t('restoreAccept'), 'primary'); accept.type = 'button';
  cancel.addEventListener('click', () => restoreDialog.close('cancel'));
  accept.addEventListener('click', () => restoreDialog.close('restore'));
  restoreDialog.replaceChildren(title, file, detail, safety, actions(cancel, accept));
  restoreDialog.returnValue = '';
  return new Promise(resolve => {
    restoreDialog.addEventListener('close', () => {
      settingsDialog.classList.remove('confirmation-behind');
      const heading = settingsDialog.querySelector('h2');
      if (heading) { heading.tabIndex = -1; heading.focus({ preventScroll: true }); }
      resolve(restoreDialog.returnValue === 'restore');
    }, { once: true });
    settingsDialog.classList.add('confirmation-behind');
    restoreDialog.showModal(); cancel.focus();
  });
}
restoreDialog.addEventListener('cancel', event => { event.preventDefault(); restoreDialog.close('cancel'); });
async function restoreBackup() {
  if (isActive(view.checkpoint)) throw new Error(t('restoreBetween'));
  if (backgroundDraft?.pending) throw new Error(t('waitForImage'));
  await flushEditor();
  const selected = await invoke('choose_restore_data'); if (selected === null) return;
  let currentProfile;
  try {
    // Decode images and validate learning before offering replacement.
    if (selected.profile) await validateProfile(selected.profile);
    if (!await confirmRestore(selected)) return;
    await refresh();
    if (isActive(view.checkpoint)) throw new Error(t('restoreBetween'));
    currentProfile = await captureProfile({ backgrounds });
  } finally {
    // A cancelled or invalid selection cannot be committed later.
    if (!currentProfile) await invoke('cancel_restore_data', { token: selected.token });
  }
  let result;
  profileRestoring = true;
  try {
    result = await invoke('commit_restore_data', { token: selected.token, currentProfile });
    // Old editor versions can never write into the replacement database.
    noteDock?.remove(); noteDock = null; editorTextarea = null; editorStatus = null;
    editor = null; routineEditor = null; noteOpen = false; noteHistoryOpen = false;
    historyPager.invalidate(); clearLifecycleChoices();
    noteArchive.close();
    backgroundDraft?.dispose(); backgroundDraft = null;
    await finishProfileRestore(result.pending_profile);
    reloadPersonalPreferences({ strict: true });
    view = await invoke('snapshot');
    lifecycle.replace(view.revision); lastTerminalId = null;
    profileRestoring = false;
    await syncPeriodNote();
    await invoke('set_notification_language', { language });
    backgroundDraft = backgrounds.createDraft();
    notice = t(selected.profile ? 'restoreSaved' : 'legacyRestoreSaved', { path: result.safety_backup });
    drawShell(); drawSettings(); await invoke('ui_ready');
  } catch (error) {
    if (!result) { profileRestoring = false; throw error; }
    // The durable pending profile is retried before the editor opens on reload.
    view = null;
    app.querySelector('.stage')?.replaceChildren(el('h1', 'Rhythm', 'state-heading'));
    settingsDialog.replaceChildren(el('div', t('restoreReload'), 'settings-body'),
      el('p', error?.message ?? String(error), 'caption'),
      button(t('reload'), () => location.reload(), 'primary'));
  }
}
function updateRoutineDisplays() {
  const list = document.querySelector('#routine-list'); if (list) renderRoutineList(list);
  const error = document.querySelector('#routine-error'); if (error) { error.textContent = view?.routine_error ?? ''; error.hidden = !view?.routine_error; }
  setBusy(busy);
}
function onRoutines(update) { if (profileRestoring) return; view = mergeRoutineState(view, update); if (!routineEditor) updateRoutineDisplays(); }
async function onExternalChange() {
  if (profileRestoring) return;
  try {
    await refresh();
    if (view.checkpoint.state.state !== 'ready') {
      if (learningPeriod?.activeId !== view.checkpoint.state.data.id) learningPeriod = null;
    }
    await refreshReminder();
    await syncPeriodNote(); renderState(); if (!routineEditor) updateRoutineDisplays();
    if (noteHistoryOpen) await historyPager.reset();
    noteArchive.refresh();
    void maybeBeginBreak();
  } catch (error) { showError(error); }
}
function onTimer(update) {
  if (profileRestoring) return;
  if (!view || update.revision < view.revision || !lifecycle.observe(update)) return;
  const before = displayState();
  view = { ...view, checkpoint: update.checkpoint, revision: update.revision };
  if (update.terminal && update.terminal.id !== lastTerminalId) {
    lastTerminalId = update.terminal.id;
    hasEntered = true; remember('entered', 'true');
    view.sessions = [update.terminal, ...(view.sessions ?? []).filter(item => item.id !== update.terminal.id)].slice(0, 100);
    // Waiting for a reminder is not evidence of continued focused work.
    learningPeriod = null;
    void refreshReminder().catch(showError);
  }
  if (update.checkpoint.state.state === 'running') {
    clearBreakInvitation();
    if (!busy && learningPeriod?.activeId !== update.checkpoint.state.data.id) learningPeriod = null;
  }
  // Never replace the note or settings controls when a timer tick arrives.
  if (before !== displayState() || update.terminal) renderState();
  if (update.terminal || (update.note_period_id !== undefined && update.note_period_id !== view.period_note?.id)) void refreshPeriodNotes();
  void maybeBeginBreak();
}
function openCloseDialog() {
  if (cliQuit.active || restoreDialog.open) return;
  reminderVisibleAt = null; breakClock.tick({ active: false });
  document.querySelector('#close-error').hidden = true;
  document.querySelector('#close-title').textContent = t('quitTitle');
  document.querySelector('#close-detail').textContent = t('quitDetail');
  document.querySelector('#keep-open').textContent = t('stay');
  document.querySelector('#quit-app').textContent = t('quit');
  if (!closeDialog.open) closeDialog.showModal();
}
document.querySelector('#keep-open').addEventListener('click', () => closeDialog.close());
document.querySelector('#quit-app').addEventListener('click', () => { void run(async () => { await flushEditor(); await invoke('quit'); }); });
settingsDialog.addEventListener('cancel', event => { event.preventDefault(); closeSettings(); });
closeDialog.addEventListener('close', () => { void maybeBeginBreak(); });
settingsDialog.addEventListener('click', event => {
  if (event.target !== settingsDialog) return;
  const rect = settingsDialog.getBoundingClientRect();
  if (event.clientX < rect.left || event.clientX > rect.right || event.clientY < rect.top || event.clientY > rect.bottom) closeSettings();
});
document.addEventListener('pointerdown', event => {
  if (noteOpen && !noteDock?.contains(event.target) && !composing) setNoteOpen(false, false);
});
document.addEventListener('keydown', event => {
  if (event.key !== 'Escape' || event.isComposing || composing || cliQuit.active) return;
  if (settingsDialog.open || archiveDialog.open || closeDialog.open) return;
  if (noteOpen) { event.preventDefault(); setNoteOpen(false); }
  else if (fullscreen) { fullscreen = false; syncFullscreenControl(); void invoke('set_fullscreen', { enabled: false }).catch(showError); }
});
document.addEventListener('fullscreenchange', () => { fullscreen = Boolean(document.fullscreenElement); syncFullscreenControl(); });
document.addEventListener('visibilitychange', () => {
  if (document.visibilityState === 'hidden') { reminderVisibleAt = null; breakClock.tick({ active: false }); }
  if (document.visibilityState === 'hidden' && !composing) void flushEditor().catch(showError);
  if (document.visibilityState === 'visible') { closeReminder(); void onExternalChange(); }
});
window.addEventListener('blur', () => { reminderVisibleAt = null; breakClock.tick({ active: false }); });
window.addEventListener('focus', () => { void maybeBeginBreak(); });
window.addEventListener('beforeunload', event => {
  if (editor?.dirty || editor?.saving || composing) { event.preventDefault(); event.returnValue = ''; }
});

async function init() {
  try {
    platform = await invoke('platform_status');
    backgrounds = await loadBackgrounds();
    if (!platform.is_browser) {
      profileRestoring = true;
      await finishProfileRestore(await invoke('pending_profile_restore'));
      profileRestoring = false;
    }
    reloadPersonalPreferences();
    await native.event.listen('rhythm:timer', event => onTimer(event.payload));
    await native.event.listen('rhythm:routines', event => onRoutines(event.payload));
    await native.event.listen('rhythm:reminder', event => onReminder(event.payload));
    await native.event.listen('rhythm:lifecycle_prepare', onLifecyclePrepare);
    await native.event.listen('rhythm:lifecycle_interrupted', event => onLifecycleInterrupted(event.payload));
    await native.event.listen('rhythm:external_change', () => { void onExternalChange(); });
    if (!platform.is_browser) {
      await native.event.listen('rhythm:sync', event => {
        try { automaticSync.receive(event.payload); } catch (error) { automaticSync.fail(error); }
      });
      await native.event.listen('rhythm:sync_history', () => {
        if (profileRestoring) return;
        if (noteHistoryOpen) void historyPager.reset().catch(showError); else historyPager.invalidate();
        noteArchive.refresh();
      });
    }
    await native.event.listen('rhythm:error', event => showError(event.payload));
    await native.event.listen('rhythm:close_requested', openCloseDialog);
    await native.event.listen('rhythm:break_action', event => {
      if (lifecycle.interrupted) return;
      if (!['accept', 'snooze'].includes(event.payload?.action) || typeof event.payload.period_id !== 'string'
        || !Number.isInteger(event.payload.sequence) || !['focus', 'break'].includes(event.payload.kind)) return;
      if (!pendingReminder?.available || reminderKey(event.payload) !== reminderKey()) return;
      nativeBreakChoice = event.payload; void maybeBeginBreak();
    });
    await native.event.listen('rhythm:cli_quit_requested', event => { void cliQuit.request(event.payload.token); });
    await native.event.listen('rhythm:cli_quit_cancelled', event => cliQuit.cancel(event.payload.token, event.payload.error));
    await refresh(); await syncPeriodNote();
    await invoke('set_notification_language', { language });
    drawShell(); await invoke('ui_ready');
    await refreshReminder();
    if (platform.is_browser) void registerOffline().then(result => { offlineReady = result.ready; });
    if (backgrounds.loadError) showError(backgrounds.loadError);
  } catch (error) {
    app.replaceChildren(el('main', t('openingFailed'), 'stage'), button(t('reload'), () => location.reload(), 'primary')); showError(error);
  }
}
void init();

function field(labelText, type, value, min, max) {
  const label = el('label', labelText);
  const input = el('input'); input.type = type; input.value = value;
  if (min !== undefined) input.min = String(min);
  if (max !== undefined) input.max = String(max);
  input.step = '1'; input.required = true;
  label.append(input);
  return { label, input };
}
function selectField(labelText, choices, value) {
  const label = el('label', labelText);
  const select = el('select');
  for (const [value, text] of choices) { const option = el('option', text); option.value = value; select.append(option); }
  select.value = value; label.append(select);
  return { label, select };
}

function date(utc) {
  return new Intl.DateTimeFormat(language, { dateStyle: 'medium', timeStyle: 'short' }).format(new Date(utc));
}
function invitationDuration(milliseconds) {
  const minutes = milliseconds / 60000;
  return t(minutes === 1 ? 'invitationDurationOne' : 'invitationDuration', { minutes });
}


function windowRange(window) { return `${window.start}–${window.end}`; }
function beginRoutineEdit(routine = null) {
  routineEditor = routine ? { ...routine, weekdays: [...routine.weekdays], local_time: null,
    windows: routineWindowsForEdit(routine), wasLegacy: Boolean(routine.local_time) } : {
    id: null, name: '', weekdays: [1, 2, 3, 4, 5], windows: routineWindowsForEdit(null),
    timezone: 'system', focus_ms: view.checkpoint.settings.focus_ms, enabled: true,
  };
  refreshRoutineEditor(true);
}

// A routine edit must not rebuild the whole Settings dialog: doing so resets
// its scroll position and discards unsaved duration/appearance controls.
function refreshRoutineEditor(focusName = false) {
  const existing = settingsDialog.querySelector('.routines-settings');
  if (!existing) return;
  const body = settingsDialog.querySelector('.settings-body');
  const scrollTop = body?.scrollTop ?? 0;
  const holder = el('div');
  renderRoutineSettings(holder);
  existing.replaceWith(holder.firstElementChild);
  setBusy(busy);
  if (body) body.scrollTop = scrollTop;
  if (focusName) requestAnimationFrame(() => {
    if (settingsDialog.open && routineEditor) settingsDialog.querySelector('#routine-name')?.focus({ preventScroll: true });
  });
}

function routineInput(routine) {
  const schedule = routine.local_time ? { local_time: routine.local_time } : { windows: normalizedRoutineWindows(routine.windows) };
  return { name: routine.name, weekdays: [...routine.weekdays], ...schedule,
    timezone: routine.timezone, focus_ms: routine.focus_ms, enabled: routine.enabled };
}

function renderRoutineList(list) {
  list.replaceChildren();
  const routines = view?.routines ?? [];
  if (!routines.length) list.append(el('p', t('noRoutines'), 'muted'));
  for (const routine of routines) {
    const card = el('article', null, 'card routine-card');
    const days = routine.weekdays.map(day => t(`weekday${day}`)).join(' · ');
    const times = routine.local_time ? t('routineLegacyTime', { time: routine.local_time }) : routine.windows.map(windowRange).join(' · ');
    card.append(el('h3', routine.name), el('p', days, 'caption'), el('p', times, 'caption'));
    card.append(el('p', `${invitationDuration(routine.focus_ms)} · ${routine.timezone === 'system' ? t('zoneSystem') : routine.timezone}`, 'caption'));
    const next = (view.routine_next ?? []).find(item => item.id === routine.id)?.next_utc_ms;
    card.append(el('p', !routine.enabled ? t('routineDisabled') : next == null ? t('routineNoNext') : t('routineNext', { date: date(next) }), 'caption'));
    const toggle = button(t(routine.enabled ? 'routineDisable' : 'routineEnable'), async () => {
      await request('save_routine', { id: routine.id, routine: { ...routineInput(routine), enabled: !routine.enabled } });
      updateRoutineDisplays();
    }, 'secondary');
    card.append(actions(button(t('routineEdit'), () => beginRoutineEdit(routine), 'link'), toggle,
      button(t('routineDelete'), async () => {
        if (!window.confirm(t('routineDeleteConfirm', { name: routine.name }))) return;
        await request('delete_routine', { id: routine.id });
        if (routineEditor?.id === routine.id) { routineEditor = null; refreshRoutineEditor(); }
        else updateRoutineDisplays();
      }, 'link')));
    list.append(card);
  }
}

function renderRoutineSettings(main) {
  const section = el('section', null, 'routines-settings');
  section.append(el('h2', t('routineTitle'), 'section-title'), el('p', t('routineHint'), 'muted'));
  const error = el('p', view.routine_error ?? '', 'routine-error');
  error.id = 'routine-error'; error.hidden = !view.routine_error;
  section.append(error);
  const list = el('div'); list.id = 'routine-list'; renderRoutineList(list); section.append(list);
  if (!routineEditor) {
    section.append(actions(button(t('routineAdd'), () => beginRoutineEdit(), 'secondary')));
    main.append(section); return;
  }
  const routine = routineEditor;
  const form = el('form', null, 'card routine-form');
  const fieldset = el('fieldset'); fieldset.id = 'routine-fields';
  fieldset.append(el('legend', t(routine.id ? 'routineEdit' : 'routineAdd')));
  const name = field(t('routineName'), 'text', routine.name);
  name.input.id = 'routine-name'; name.input.maxLength = 120;
  name.input.addEventListener('input', () => { routine.name = name.input.value; });
  fieldset.append(name.label);
  const days = el('fieldset', null, 'weekday-choices');
  days.append(el('legend', t('routineDays')));
  for (let day = 1; day <= 7; day++) {
    const label = el('label', null, 'check-label');
    const input = el('input'); input.type = 'checkbox'; input.checked = routine.weekdays.includes(day);
    input.addEventListener('change', () => {
      routine.weekdays = input.checked ? [...routine.weekdays, day].sort((a, b) => a - b) : routine.weekdays.filter(value => value !== day);
    });
    label.append(input, document.createTextNode(t(`weekday${day}`))); days.append(label);
  }
  fieldset.append(days);
  if (routine.wasLegacy) fieldset.append(el('p', t('routineLegacyEdit'), 'caption'));
  const ranges = el('fieldset', null, 'routine-time-ranges');
  ranges.append(el('legend', t('routineRanges')));
  const rangeRows = el('div');
  const drawRanges = () => {
    rangeRows.replaceChildren();
    routine.windows.forEach((window, index) => {
      const row = el('div', null, 'routine-time-range');
      const start = field(t('routineStart'), 'time', window.start); start.input.step = '60';
      start.input.id = `routine-window-${index}-start`;
      start.input.setAttribute('aria-label', t('routineStartNumber', { number: index + 1 }));
      start.input.addEventListener('input', () => { window.start = start.input.value; });
      const end = field(t('routineEnd'), 'time', window.end === '24:00' ? '00:00' : window.end); end.input.step = '60';
      end.input.setAttribute('aria-label', t('routineEndNumber', { number: index + 1 }));
      end.input.setAttribute('aria-describedby', 'routine-midnight-hint');
      end.input.addEventListener('input', () => { window.end = end.input.value; });
      const remove = button(t('routineRemoveRange'), () => {
        if (routine.windows.length <= 1) return;
        routine.windows.splice(index, 1); drawRanges();
        fieldset.disabled = false;
        document.querySelector(`#routine-window-${Math.min(index, routine.windows.length - 1)}-start`)?.focus();
      }, 'link');
      remove.setAttribute('aria-label', t('routineRemoveNumber', { number: index + 1 }));
      remove.hidden = routine.windows.length === 1;
      row.append(start.label, end.label, remove); rangeRows.append(row);
    });
    addRange.hidden = routine.windows.length >= 12;
  };
  const addRange = button(t('routineAddRange'), () => {
    if (routine.windows.length >= 12) return;
    const latestEnd = routine.windows.map(window => window.end === '00:00' ? '24:00' : window.end).sort().at(-1);
    const start = latestEnd <= '14:30' ? '14:30' : latestEnd === '24:00' ? '' : latestEnd;
    const endHour = start ? Math.min(Number(start.slice(0, 2)) + 1, 24) : 0;
    const end = start === '14:30' ? '18:00' : start ? `${String(endHour).padStart(2, '0')}:${endHour === 24 ? '00' : start.slice(3)}` : '';
    routine.windows.push({ start, end }); drawRanges();
    fieldset.disabled = false;
    document.querySelector(`#routine-window-${routine.windows.length - 1}-start`)?.focus();
  }, 'secondary');
  drawRanges();
  const midnightHint = el('p', t('routineMidnightHint'), 'caption'); midnightHint.id = 'routine-midnight-hint';
  ranges.append(rangeRows, actions(addRange), midnightHint); fieldset.append(ranges);
  const fields = el('div', null, 'fields');
  const duration = field(t('focusMinutes'), 'number', routine.focus_ms / 60000, 1, 1440);
  duration.input.addEventListener('input', () => { routine.focus_ms = Number(duration.input.value) * 60000; });
  const zone = selectField(t('routineZone'), [['system', t('zoneSystem')], ['chosen', t('zoneChosen')]], routine.timezone === 'system' ? 'system' : 'chosen');
  const zoneName = field(t('zoneName'), 'text', routine.timezone === 'system' ? Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC' : routine.timezone);
  zoneName.label.hidden = routine.timezone === 'system'; zoneName.input.required = routine.timezone !== 'system';
  zone.select.addEventListener('change', () => {
    const system = zone.select.value === 'system';
    routine.timezone = system ? 'system' : zoneName.input.value;
    zoneName.label.hidden = system; zoneName.input.required = !system;
  });
  zoneName.input.addEventListener('input', () => { if (zone.select.value !== 'system') routine.timezone = zoneName.input.value; });
  fields.append(duration.label, zone.label, zoneName.label); fieldset.append(fields);
  fieldset.append(el('p', t('routineCycleHint'), 'caption'));
  const enabled = el('label', null, 'check-label');
  const enabledInput = el('input'); enabledInput.type = 'checkbox'; enabledInput.checked = routine.enabled;
  enabledInput.addEventListener('change', () => { routine.enabled = enabledInput.checked; });
  enabled.append(enabledInput, document.createTextNode(t('routineEnabled'))); fieldset.append(enabled);
  const save = button(t('routineSave'), async () => {
    // The common action wrapper locks this fieldset before invoking the handler.
    // Enable it just for native validity checks, then lock it during the save.
    fieldset.disabled = false;
    // Leave invalid controls enabled so their focus and native validation message
    // survive until the action wrapper releases the busy state.
    if (!form.reportValidity()) return;
    fieldset.disabled = busy;
    if (!routine.weekdays.length) throw new Error(t('routinePickDay'));
    if (!Number.isInteger(routine.focus_ms / 60000)) throw new Error(t('validation'));
    let input;
    try { input = routineInput(routine); } catch (error) { throw new Error(t(error.message)); }
    await request('save_routine', { id: routine.id, routine: input });
    routineEditor = null; notice = t('routineSaved'); announce(notice); refreshRoutineEditor();
  }, 'primary');
  fieldset.append(actions(save, button(t('routineCancel'), () => { routineEditor = null; refreshRoutineEditor(); }, 'link')));
  form.addEventListener('submit', event => { event.preventDefault(); save.click(); });
  form.append(fieldset); section.append(form, el('p', t('routineClockHint'), 'caption')); main.append(section);
}
