const STATES = ['focus', 'break'];
const RECORD_KEY = 'backgrounds.v1';
const MAX_UPLOAD_BYTES = 20 * 1024 * 1024;
const MAX_SAVED_BYTES = 5 * 1024 * 1024;
const IMAGE_TYPES = new Set(['image/jpeg', 'image/png', 'image/webp', 'image/avif']);

export const BUILTIN_BACKGROUNDS = Object.freeze({
  forest: { label: 'Forest', src: 'assets/focus-still.png' },
  coast: { label: 'Coast', src: 'assets/break-still.png' },
  dusk: { label: 'Dusk', src: null },
});

const words = {
  backgrounds: 'Backgrounds', focus: 'Focus', break: 'Break', forest: 'Forest', coast: 'Coast', dusk: 'Dusk',
  customBackground: 'My image', chooseImage: 'Choose image', preparingImage: 'Preparing image…',
  focusBackground: 'Focus background', breakBackground: 'Break background',
  chooseFocusImage: 'Choose an image for Focus', chooseBreakImage: 'Choose an image for Break',
};
function translate(t, key) {
  const translated = t?.(key);
  return translated && translated !== key ? translated : words[key] ?? key;
}
function defaults() {
  return { schema: 1, focus: { choice: 'forest', custom: null }, break: { choice: 'coast', custom: null } };
}
function clonePreferences(value) {
  return { schema: 1, ...Object.fromEntries(STATES.map(state => [state, {
    choice: value[state].choice,
    custom: value[state].custom ? { ...value[state].custom } : null,
  }])) };
}
export function validateBackgrounds(value) {
  if (value?.schema !== 1) throw new Error('These saved background preferences cannot be read.');
  for (const state of STATES) {
    const item = value[state];
    if (!item || ![...Object.keys(BUILTIN_BACKGROUNDS), 'custom'].includes(item.choice)) {
      throw new Error('Choose a built-in background or your own image.');
    }
    if (item.custom && (!(item.custom.blob instanceof Blob) || item.custom.blob.size === 0
      || item.custom.blob.size > MAX_SAVED_BYTES || !IMAGE_TYPES.has(item.custom.blob.type)
      || typeof item.custom.name !== 'string')) throw new Error('The saved background image is invalid.');
    if (item.choice === 'custom' && !item.custom) throw new Error('Choose an image before using a custom background.');
  }
  return value;
}

const portableKeys = (value, keys) => value !== null && typeof value === 'object' && !Array.isArray(value)
  && Object.keys(value).sort().join(',') === [...keys].sort().join(',');

function encodeImage(bytes) {
  let binary = '';
  for (let offset = 0; offset < bytes.length; offset += 8192) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + 8192));
  }
  return btoa(binary);
}

function decodeImageBytes(base64) {
  if (typeof base64 !== 'string' || base64.length === 0
    || base64.length > 4 * Math.ceil(MAX_SAVED_BYTES / 3) || base64.length % 4 !== 0
    || /[^A-Za-z0-9+/=]/.test(base64)) throw new Error('The backup contains an invalid background image.');
  let binary;
  try { binary = atob(base64); }
  catch { throw new Error('The backup contains an invalid background image.'); }
  const bytes = Uint8Array.from(binary, character => character.charCodeAt(0));
  if (bytes.length === 0 || bytes.length > MAX_SAVED_BYTES || encodeImage(bytes) !== base64) {
    throw new Error('The backup contains an invalid background image.');
  }
  return bytes;
}

function imageSignature(bytes, mime) {
  const starts = (signature, offset = 0) => signature.every((value, index) => bytes[index + offset] === value);
  if (mime === 'image/jpeg') return starts([0xff, 0xd8, 0xff]);
  if (mime === 'image/png') return starts([137, 80, 78, 71, 13, 10, 26, 10]);
  if (mime === 'image/webp') return starts([82, 73, 70, 70]) && starts([87, 69, 66, 80], 8);
  if (mime === 'image/avif' && bytes.length >= 16 && starts([102, 116, 121, 112], 4)) {
    const boxEnd = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength).getUint32(0);
    if (boxEnd < 16 || boxEnd > bytes.length) return false;
    for (let offset = 8; offset + 4 <= boxEnd; offset += 4) {
      if (offset !== 12 && (starts([97, 118, 105, 102], offset) || starts([97, 118, 105, 115], offset))) return true;
    }
  }
  return false;
}

async function verifyBrowserImage(blob) {
  if (typeof Image !== 'function') throw new Error('Background image validation is unavailable.');
  const url = URL.createObjectURL(blob);
  try {
    const image = new Image(); image.src = url; await image.decode();
    if (!image.naturalWidth || !image.naturalHeight) throw new Error('Empty image');
  } catch { throw new Error('The backup contains a background image that cannot be opened.'); }
  finally { URL.revokeObjectURL(url); }
}

/** Decode and validate the complete portable record before any saved image changes. */
export async function decodeBackgroundSnapshot(snapshot, { verifyImage = verifyBrowserImage } = {}) {
  if (!portableKeys(snapshot, ['schema', 'focus', 'break']) || snapshot.schema !== 1) {
    throw new Error('This backup uses unsupported background preferences.');
  }
  const value = { schema: 1 };
  for (const state of STATES) {
    const item = snapshot[state];
    if (!portableKeys(item, ['choice', 'custom'])
      || ![...Object.keys(BUILTIN_BACKGROUNDS), 'custom'].includes(item.choice)) {
      throw new Error('The backup contains invalid background preferences.');
    }
    let custom = null;
    if (item.custom !== null) {
      if (!portableKeys(item.custom, ['name', 'mime', 'base64']) || typeof item.custom.name !== 'string'
        || item.custom.name.length > 180 || !IMAGE_TYPES.has(item.custom.mime)) {
        throw new Error('The backup contains an invalid background image.');
      }
      const bytes = decodeImageBytes(item.custom.base64);
      if (!imageSignature(bytes, item.custom.mime)) throw new Error('The background image type does not match its contents.');
      const blob = new Blob([bytes], { type: item.custom.mime });
      await verifyImage(blob);
      custom = { name: item.custom.name, blob };
    }
    value[state] = { choice: item.choice, custom };
  }
  return validateBackgrounds(value);
}

function indexedRepository() {
  let opening;
  function open() {
    if (!opening) opening = new Promise((resolve, reject) => {
      if (!globalThis.indexedDB) { reject(new Error('This device cannot save background images right now.')); return; }
      const request = indexedDB.open('rhythm-appearance', 1);
      request.onupgradeneeded = () => request.result.createObjectStore('preferences');
      request.onsuccess = () => {
        const database = request.result;
        database.onversionchange = () => { database.close(); opening = null; };
        resolve(database);
      };
      request.onerror = () => { opening = null; reject(new Error('Background preferences could not be opened.')); };
      request.onblocked = () => { opening = null; reject(new Error('Close another Rhythm window before saving backgrounds.')); };
    });
    return opening;
  }
  return {
    async read() {
      const database = await open();
      return new Promise((resolve, reject) => {
        const transaction = database.transaction('preferences', 'readonly');
        const request = transaction.objectStore('preferences').get(RECORD_KEY);
        request.onsuccess = () => resolve(request.result ?? null);
        request.onerror = () => reject(new Error('Background preferences could not be read.'));
      });
    },
    async write(value) {
      const database = await open();
      return new Promise((resolve, reject) => {
        const transaction = database.transaction('preferences', 'readwrite');
        transaction.objectStore('preferences').put(value, RECORD_KEY);
        transaction.oncomplete = () => resolve();
        transaction.onerror = transaction.onabort = () => reject(new Error('The backgrounds could not be saved. Free some space on this device and try again.'));
      });
    },
  };
}

class ImageSources {
  constructor() { this.urls = new Map(); }
  source(item) {
    if (item.choice !== 'custom') return BUILTIN_BACKGROUNDS[item.choice].src;
    if (!this.urls.has(item.custom.blob)) this.urls.set(item.custom.blob, URL.createObjectURL(item.custom.blob));
    return this.urls.get(item.custom.blob);
  }
  dispose() { for (const url of this.urls.values()) URL.revokeObjectURL(url); this.urls.clear(); }
}

export class BackgroundDraft {
  constructor(value) {
    this.value = clonePreferences(validateBackgrounds(value));
    this.sources = new ImageSources();
    this.pending = 0;
    this.disposed = false;
    this.uploadTokens = { focus: 0, break: 0 };
  }
  choose(state, choice) {
    if (!STATES.includes(state)) throw new Error('Unknown background state.');
    const next = clonePreferences(this.value); next[state].choice = choice;
    validateBackgrounds(next); this.value = next;
  }
  setImage(state, blob, name) {
    if (!STATES.includes(state)) throw new Error('Unknown background state.');
    const next = clonePreferences(this.value);
    next[state] = { choice: 'custom', custom: { blob, name: name.slice(0, 180) } };
    validateBackgrounds(next); this.value = next;
  }
  dispose() { this.disposed = true; this.sources.dispose(); }
}

export class BackgroundPreferences {
  constructor(repository, value = defaults(), loadError = null) {
    this.repository = repository; this.value = clonePreferences(validateBackgrounds(value));
    this.loadError = loadError; this.sources = new ImageSources(); this.saveChain = Promise.resolve();
  }
  createDraft() { return new BackgroundDraft(this.value); }
  async exportSnapshot(options = {}) {
    await this.saveChain;
    if (this.loadError) throw new Error('Background preferences could not be read. Reopen Rhythm and retry the backup.');
    const value = clonePreferences(validateBackgrounds(this.value));
    const snapshot = { schema: 1 };
    for (const state of STATES) {
      const item = value[state];
      snapshot[state] = { choice: item.choice, custom: item.custom ? {
        name: item.custom.name, mime: item.custom.blob.type,
        base64: encodeImage(new Uint8Array(await item.custom.blob.arrayBuffer())),
      } : null };
    }
    await decodeBackgroundSnapshot(snapshot, options);
    return snapshot;
  }
  async restoreSnapshot(snapshot, options = {}) {
    const value = await decodeBackgroundSnapshot(snapshot, options);
    const draft = new BackgroundDraft(value);
    try { await this.save(draft); }
    finally { draft.dispose(); }
    this.sources.dispose();
  }
  save(draft) {
    if (draft.disposed) return Promise.reject(new Error('Reopen Settings before changing backgrounds.'));
    if (draft.pending) return Promise.reject(new Error('Wait for the image to finish preparing.'));
    const next = clonePreferences(validateBackgrounds(draft.value));
    const operation = this.saveChain.catch(() => {}).then(async () => {
      // Do not change the visible selection unless both states have been saved.
      await this.repository.write(next);
      this.value = next; this.loadError = null;
    });
    this.saveChain = operation; return operation;
  }
  async apply(landscape) {
    if (!landscape) return;
    await Promise.all(STATES.map(async state => {
      const scene = landscape.querySelector(`.scene-${state}`);
      if (!scene) return;
      const item = this.value[state], source = this.sources.source(item);
      const key = source ?? item.choice;
      if (scene.dataset.backgroundSource === key) return;
      scene.dataset.backgroundPending = key;
      if (source) {
        const image = new Image(); image.className = 'scene-image'; image.alt = ''; image.decoding = 'async'; image.src = source;
        try { await image.decode(); }
        catch { return; /* The local fallback stays visible if an asset fails to decode. */ }
        if (!scene.isConnected || scene.dataset.backgroundPending !== key) return;
        scene.replaceChildren(image);
      } else scene.replaceChildren();
      scene.dataset.background = item.choice;
      scene.dataset.backgroundSource = key;
    }));
  }
  dispose() { this.sources.dispose(); }
}

export async function loadBackgrounds({ repository = indexedRepository() } = {}) {
  try {
    const saved = await repository.read();
    return new BackgroundPreferences(repository, saved ? validateBackgrounds(saved) : defaults());
  } catch (error) {
    return new BackgroundPreferences(repository, defaults(), error?.message ?? String(error));
  }
}

export function validateImageFile(file) {
  if (!file || !IMAGE_TYPES.has(file.type)) throw new Error('Choose a JPEG, PNG, WebP, or AVIF image.');
  if (file.size === 0 || file.size > MAX_UPLOAD_BYTES) throw new Error('Choose an image smaller than 20 MB.');
}

export async function prepareBackgroundImage(file) {
  validateImageFile(file);
  const url = URL.createObjectURL(file);
  try {
    const image = new Image(); image.src = url; await image.decode();
    if (!image.naturalWidth || !image.naturalHeight) throw new Error('This image could not be opened.');
    const ratio = Math.min(1, 1920 / Math.max(image.naturalWidth, image.naturalHeight));
    const canvas = document.createElement('canvas');
    canvas.width = Math.max(1, Math.round(image.naturalWidth * ratio));
    canvas.height = Math.max(1, Math.round(image.naturalHeight * ratio));
    const context = canvas.getContext('2d', { alpha: false });
    context.fillStyle = '#25312f'; context.fillRect(0, 0, canvas.width, canvas.height);
    context.drawImage(image, 0, 0, canvas.width, canvas.height);
    const blob = await new Promise(resolve => canvas.toBlob(resolve, 'image/jpeg', .86));
    if (!blob || blob.size > MAX_SAVED_BYTES) throw new Error('This image could not be prepared. Try a smaller image.');
    return blob;
  } catch (error) {
    if (error?.name === 'EncodingError') throw new Error('This image could not be opened. Try another image.');
    throw error;
  } finally { URL.revokeObjectURL(url); }
}

export function mountBackgroundSettings(container, { draft, t, onError = () => {}, onChange = () => {} }) {
  const section = document.createElement('section'); section.className = 'background-settings';
  const heading = document.createElement('h3'); heading.textContent = translate(t, 'backgrounds'); section.append(heading);
  const grid = document.createElement('div'); grid.className = 'background-grid'; section.append(grid);
  for (const state of STATES) {
    const row = document.createElement('fieldset'); row.className = 'background-choice';
    const legend = document.createElement('legend'); legend.textContent = translate(t, state); row.append(legend);
    const preview = document.createElement('div'); preview.className = 'background-thumbnail'; preview.setAttribute('aria-hidden', 'true');
    const select = document.createElement('select'); select.setAttribute('aria-label', translate(t, `${state}Background`));
    const redraw = () => {
      const item = draft.value[state];
      select.replaceChildren();
      for (const choice of [...Object.keys(BUILTIN_BACKGROUNDS), ...(item.custom ? ['custom'] : [])]) {
        const option = document.createElement('option'); option.value = choice; option.textContent = translate(t, choice === 'custom' ? 'customBackground' : choice); select.append(option);
      }
      select.value = item.choice; preview.dataset.background = item.choice; preview.replaceChildren();
      const source = draft.sources.source(item);
      if (source) { const image = document.createElement('img'); image.alt = ''; image.src = source; preview.append(image); }
    };
    select.addEventListener('change', () => { draft.choose(state, select.value); redraw(); onChange(); });
    const picker = document.createElement('input'); picker.type = 'file'; picker.accept = [...IMAGE_TYPES].join(','); picker.hidden = true; picker.tabIndex = -1;
    picker.setAttribute('aria-label', translate(t, state === 'focus' ? 'chooseFocusImage' : 'chooseBreakImage'));
    const upload = document.createElement('button'); upload.type = 'button'; upload.className = 'background-upload secondary'; upload.dataset.action = 'true';
    upload.textContent = translate(t, 'chooseImage'); upload.setAttribute('aria-label', picker.getAttribute('aria-label'));
    upload.addEventListener('click', () => picker.click());
    picker.addEventListener('change', async () => {
      const file = picker.files?.[0]; if (!file) return;
      const token = ++draft.uploadTokens[state];
      draft.pending++; upload.disabled = true; select.disabled = true; upload.textContent = translate(t, 'preparingImage');
      try {
        const blob = await prepareBackgroundImage(file);
        if (draft.disposed || token !== draft.uploadTokens[state]) return;
        draft.setImage(state, blob, file.name); redraw(); onChange();
      } catch (error) { if (!draft.disposed && token === draft.uploadTokens[state]) onError(error); }
      finally {
        draft.pending--;
        if (token === draft.uploadTokens[state]) { picker.value = ''; upload.disabled = false; select.disabled = false; upload.textContent = translate(t, 'chooseImage'); }
      }
    });
    row.append(preview, select, upload, picker); grid.append(row); redraw();
  }
  container.append(section); return section;
}
