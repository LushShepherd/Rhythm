// Portable application preferences only. Never copy arbitrary browser storage,
// transient notifications, credentials, caches, or an in-progress timer here.
import { validateAdaptiveState } from './adaptive.mjs';
import { validateReminderDelayState } from './reminder-delay.mjs';
import { decodeBackgroundSnapshot } from './backgrounds.mjs';

export const PROFILE_KEYS = Object.freeze([
  'rhythm.language', 'rhythm.theme', 'rhythm.entered',
  'rhythm.adaptive.v1', 'rhythm.reminder-delay.v1',
]);
const MAX_BYTES = 20 * 1024 * 1024;
const encoder = new TextEncoder();
const object = value => value !== null && typeof value === 'object' && !Array.isArray(value);
const exactKeys = (value, keys) => object(value)
  && Object.keys(value).sort().join(',') === [...keys].sort().join(',');

function validatePreferences(preferences) {
  if (!object(preferences) || Object.keys(preferences).some(key => !PROFILE_KEYS.includes(key))) {
    throw new Error('The backup contains unsupported application preferences.');
  }
  for (const [key, raw] of Object.entries(preferences)) {
    if (typeof raw !== 'string' || encoder.encode(raw).length > 65_536) throw new Error('The backup contains invalid application preferences.');
    switch (key) {
      case 'rhythm.language':
        if (!['system', 'en', 'zh-Hans', 'zh-Hant'].includes(raw)) throw new Error('The backup contains an unsupported language preference.');
        break;
      case 'rhythm.theme':
        if (!['system', 'light', 'dark'].includes(raw)) throw new Error('The backup contains an unsupported appearance preference.');
        break;
      case 'rhythm.entered':
        if (!['true', 'false'].includes(raw)) throw new Error('The backup contains an invalid opening preference.');
        break;
      default: {
        let value;
        try { value = JSON.parse(raw); }
        catch { throw new Error('The backup contains unreadable learning preferences.'); }
        if (key === 'rhythm.adaptive.v1') validateAdaptiveState(value);
        else validateReminderDelayState(value);
      }
    }
  }
}

function readPreferences(storage) {
  const preferences = {};
  for (const key of PROFILE_KEYS) {
    const value = storage.getItem(key);
    if (value !== null) preferences[key] = value;
  }
  return preferences;
}

function writePreferences(storage, preferences) {
  for (const key of PROFILE_KEYS) {
    if (Object.hasOwn(preferences, key)) storage.setItem(key, preferences[key]);
    else storage.removeItem(key);
  }
}

/** Validation has no storage effects and does not normalize saved learning bytes. */
export async function validateProfile(snapshot, options = {}) {
  if (!exactKeys(snapshot, ['schema', 'preferences', 'backgrounds']) || snapshot.schema !== 1) {
    throw new Error('This complete backup uses an unsupported profile format.');
  }
  let encoded;
  try { encoded = JSON.stringify(snapshot); }
  catch { throw new Error('The complete backup profile is invalid.'); }
  if (encoder.encode(encoded).length > MAX_BYTES) throw new Error('The complete backup profile exceeds the size limit.');
  validatePreferences(snapshot.preferences);
  await decodeBackgroundSnapshot(snapshot.backgrounds, options);
  return snapshot;
}

/** Capture committed settings, including custom images retained behind built-ins. */
export async function captureProfile({ backgrounds, storage = globalThis.localStorage, ...options } = {}) {
  if (!backgrounds || !storage) throw new Error('The complete profile is not available for backup.');
  // Await image persistence first so all preferences are read from the same UI
  // operation boundary. The caller prevents settings edits until capture ends.
  const backgroundSnapshot = await backgrounds.exportSnapshot(options);
  const snapshot = { schema: 1, preferences: readPreferences(storage), backgrounds: backgroundSnapshot };
  await validateProfile(snapshot, options);
  return snapshot;
}

/** Repeatable application of a durable native restore intent. The caller must
 * acknowledge that intent only after this succeeds, and block ordinary editing
 * while it is pending. A crash can safely replay these exact writes at startup.
 */
export async function restoreProfile(snapshot, { backgrounds, storage = globalThis.localStorage, ...options } = {}) {
  if (!backgrounds || !storage) throw new Error('The profile cannot be restored on this device.');
  await validateProfile(snapshot, options);
  await backgrounds.saveChain.catch(() => {});
  const previous = readPreferences(storage);
  // Keep the original in-memory record for rollback, including unreadable old
  // preference strings. Incoming validation must never require discarding them.
  const previousBackgrounds = backgrounds.createDraft();
  const previousLoadError = backgrounds.loadError;
  let imagesSaved = false;
  let preferencesStarted = false;
  try {
    await backgrounds.restoreSnapshot(snapshot.backgrounds, options);
    imagesSaved = true;
    preferencesStarted = true;
    writePreferences(storage, snapshot.preferences);
  } catch (error) {
    let rollbackFailed = false;
    // Try every key: one blocked key must not prevent restoring other values.
    if (preferencesStarted) {
      for (const key of PROFILE_KEYS) {
        try {
          if (Object.hasOwn(previous, key)) storage.setItem(key, previous[key]);
          else storage.removeItem(key);
        } catch { rollbackFailed = true; }
      }
    }
    if (imagesSaved) {
      try { await backgrounds.save(previousBackgrounds); backgrounds.loadError = previousLoadError; }
      catch { rollbackFailed = true; }
    }
    if (rollbackFailed) throw new Error('Restore could not finish saving preferences. The complete safety backup is preserved. Reopen Rhythm to retry the pending restore.', { cause: error });
    throw new Error('Restore could not save the complete profile. Previous preferences were kept. Reopen Rhythm to retry.', { cause: error });
  } finally { previousBackgrounds.dispose(); }
}
