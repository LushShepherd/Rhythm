// Optional, user-initiated file exchange. This module has no network access,
// credential storage, timer state, or sensor data. Providers only receive an
// authenticated encrypted envelope. Keys and passwords are never persisted.
import { validateNoteTimezone } from './note-provenance.mjs';
const FORMAT = 'rhythm-encrypted-notes';
const ITERATIONS = 600_000;
const MAX_BYTES = 8 * 1024 * 1024;
const MAX_ENVELOPE = 12 * 1024 * 1024;
const MAX_NOTES = 1000;
const encoder = new TextEncoder();
const decoder = new TextDecoder('utf-8', { fatal: true });
const aad = version => encoder.encode(`${FORMAT}:${version}:PBKDF2-SHA256:${ITERATIONS}:AES-256-GCM`);

function cryptoApi() {
  if (!globalThis.crypto?.subtle) throw new Error('Encrypted file exchange needs a secure browser or the desktop app.');
  return globalThis.crypto;
}

function checkPassword(password) {
  if (typeof password !== 'string' || password.length > 1024 || [...password].length < 12) {
    throw new Error('Use a passphrase of at least 12 characters, up to 1,024 characters.');
  }
}

/** Validate and copy ONLY portable archived-note fields. Never include live notes. */
export function validateNotes(notes) {
  if (!Array.isArray(notes) || notes.length > MAX_NOTES) throw new Error('A notes file can contain up to 1,000 archived notes.');
  let total = 0;
  return notes.map(note => {
    if (!note || typeof note !== 'object' || typeof note.id !== 'string' || !note.id
      || note.id.length > 256 || !['focus', 'break'].includes(note.kind) || typeof note.body !== 'string'
      || note.body.length > 100_000 || !Number.isSafeInteger(note.version) || note.version < 1) {
      throw new Error('The file contains an invalid archived note.');
    }
    const bytes = encoder.encode(note.body);
    total += bytes.byteLength;
    if (bytes.byteLength > 100_000 || total > MAX_BYTES) throw new Error('The notes file exceeds the text size limit.');
    if (decoder.decode(bytes) !== note.body || decoder.decode(encoder.encode(note.id)) !== note.id) {
      throw new Error('The file contains invalid Unicode text.');
    }
    for (const field of ['created_at_utc_ms', 'updated_at_utc_ms', 'archived_at_utc_ms']) {
      if (!Number.isSafeInteger(note[field]) || Math.abs(note[field]) > 8_640_000_000_000_000) {
        throw new Error('Only archived notes with valid dates can be exchanged.');
      }
    }
    return { id: note.id, kind: note.kind, body: note.body, version: note.version,
      created_at_utc_ms: note.created_at_utc_ms, updated_at_utc_ms: note.updated_at_utc_ms,
      archived_at_utc_ms: note.archived_at_utc_ms, ...validateNoteTimezone(note) };
  });
}

async function fingerprint(note) {
  // Matches the Rust archive importer. IDs/version are local metadata; text and
  // timestamps identify a portable variant, so repeated exchanges cannot multiply it.
  const fields = [note.kind, note.body, note.created_at_utc_ms, note.updated_at_utc_ms, note.archived_at_utc_ms];
  if (note.source_timezone != null) fields.push(note.source_timezone, note.source_utc_offset_minutes);
  const canonical = JSON.stringify(fields);
  const digest = await cryptoApi().subtle.digest('SHA-256', encoder.encode(canonical));
  return `import:${[...new Uint8Array(digest)].map(byte => byte.toString(16).padStart(2, '0')).join('')}`;
}

/** Preserve local notes and every distinct incoming variant; never choose a winner. */
export async function mergeNotes(existing, incoming) {
  // Existing storage can grow beyond an individual exchange's 1,000-note limit.
  if (!Array.isArray(existing)) throw new Error('Existing notes are invalid.');
  const local = existing.map(note => validateNotes([note])[0]);
  const imported = validateNotes(incoming);
  const result = [...local];
  const ids = new Set(local.map(note => note.id));
  const known = new Set(await Promise.all(local.map(fingerprint)));
  for (const note of imported) {
    const id = await fingerprint(note);
    if (known.has(id)) continue;
    // A reserved import ID with different content is corruption, not permission
    // to overwrite it. The atomic caller leaves its store unchanged on failure.
    if (ids.has(id)) throw new Error('An imported note identifier conflicts with local data. No notes were replaced.');
    result.push({ ...note, id });
    ids.add(id);
    known.add(id);
  }
  return result;
}

function base64(bytes) {
  let binary = '';
  for (let offset = 0; offset < bytes.length; offset += 8192) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + 8192));
  }
  return btoa(binary);
}

function unbase64(value, expectedLength = null) {
  if (typeof value !== 'string' || value.length > MAX_ENVELOPE || value.length % 4 !== 0) {
    throw new Error('The encrypted notes file is invalid.');
  }
  // Avoid a repeated-group regular expression over a multi-megabyte payload:
  // some engines exhaust their regular-expression stack even on valid base64.
  const padding = value.endsWith('==') ? 2 : value.endsWith('=') ? 1 : 0;
  if (/[^A-Za-z0-9+/]/.test(value.slice(0, value.length - padding))) throw new Error('The encrypted notes file is invalid.');
  const bytes = Uint8Array.from(atob(value), char => char.charCodeAt(0));
  if (base64(bytes) !== value || (expectedLength !== null && bytes.length !== expectedLength)) {
    throw new Error('The encrypted notes file is invalid.');
  }
  return bytes;
}

async function deriveKey(password, salt) {
  checkPassword(password);
  const api = cryptoApi();
  const material = encoder.encode(password);
  try {
    const base = await api.subtle.importKey('raw', material, 'PBKDF2', false, ['deriveKey']);
    return await api.subtle.deriveKey({ name: 'PBKDF2', hash: 'SHA-256', iterations: ITERATIONS, salt },
      base, { name: 'AES-GCM', length: 256 }, false, ['encrypt', 'decrypt']);
  } finally { material.fill(0); }
}

export async function encryptNotes(notes, password) {
  checkPassword(password);
  const validated = validateNotes(notes);
  const version = validated.some(note => note.source_timezone != null) ? 2 : 1;
  const plain = encoder.encode(JSON.stringify({ version, notes: validated }));
  if (plain.length > MAX_BYTES) throw new Error('The notes file exceeds the 8 MB package limit.');
  const api = cryptoApi();
  const salt = api.getRandomValues(new Uint8Array(32));
  const iv = api.getRandomValues(new Uint8Array(12));
  try {
    const key = await deriveKey(password, salt);
    const ciphertext = await api.subtle.encrypt({ name: 'AES-GCM', iv, additionalData: aad(version), tagLength: 128 }, key, plain);
    return JSON.stringify({ format: FORMAT, version, kdf: 'PBKDF2-SHA256', iterations: ITERATIONS,
      cipher: 'AES-256-GCM', salt: base64(salt), iv: base64(iv), ciphertext: base64(new Uint8Array(ciphertext)) });
  } finally { plain.fill(0); }
}

export async function decryptNotes(text, password) {
  checkPassword(password);
  if (typeof text !== 'string' || text.length > MAX_ENVELOPE) throw new Error('The encrypted notes file is too large.');
  let envelope;
  try { envelope = JSON.parse(text); } catch { throw new Error('The encrypted notes file is invalid.'); }
  if (!envelope || typeof envelope !== 'object' || Array.isArray(envelope)
    || Object.keys(envelope).sort().join(',') !== 'cipher,ciphertext,format,iterations,iv,kdf,salt,version'
    || envelope.format !== FORMAT || ![1, 2].includes(envelope.version) || envelope.kdf !== 'PBKDF2-SHA256'
    || envelope.iterations !== ITERATIONS || envelope.cipher !== 'AES-256-GCM') {
    throw new Error('This encrypted notes format is unsupported.');
  }
  // Validate fixed KDF parameters and bounded data before any expensive derivation.
  const salt = unbase64(envelope.salt, 32);
  const iv = unbase64(envelope.iv, 12);
  const ciphertext = unbase64(envelope.ciphertext);
  if (ciphertext.length < 16 || ciphertext.length > MAX_BYTES + 16) throw new Error('The encrypted notes file is invalid.');
  const key = await deriveKey(password, salt);
  let plain;
  try {
    plain = new Uint8Array(await cryptoApi().subtle.decrypt(
      { name: 'AES-GCM', iv, additionalData: aad(envelope.version), tagLength: 128 }, key, ciphertext));
  } catch { throw new Error('The passphrase is incorrect or the file has been changed. No notes were imported.'); }
  try {
    const payload = JSON.parse(decoder.decode(plain));
    if (!payload || Object.keys(payload).sort().join(',') !== 'notes,version' || payload.version !== envelope.version) {
      throw new Error('This notes payload is unsupported.');
    }
    const notes = validateNotes(payload.notes);
    if (payload.version === 1 && notes.some(note => note.source_timezone != null)) {
      throw new Error('Original time zones require the newer notes format.');
    }
    return notes;
  } finally { plain.fill(0); }
}

/** Replaceable provider contract: read/write operate on user-selected files only.
 * read() returns text, a File, or null when cancelled. write(file) receives an
 * object { name, type, text }; it chooses a location via the host's file UI.
 * Providers never receive plaintext notes or passwords.
 */
export function createFileProvider({ read, write }) {
  if (typeof read !== 'function' || typeof write !== 'function') throw new Error('Choose a valid file provider.');
  return Object.freeze({
    id: 'user-selected-file',
    async export(notes, password) {
      const text = await encryptNotes(notes, password);
      return write({ name: 'rhythm-notes.rhythm', type: 'application/json', text });
    },
    async import(password) {
      const file = await read();
      if (file == null) return null;
      if (typeof file !== 'string' && (!Number.isFinite(file.size) || file.size > MAX_ENVELOPE || typeof file.text !== 'function')) {
        throw new Error('Choose an encrypted Rhythm notes file under 12 MB.');
      }
      return decryptNotes(typeof file === 'string' ? file : await file.text(), password);
    },
  });
}
