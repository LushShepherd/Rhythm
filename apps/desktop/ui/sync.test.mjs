import test from 'node:test';
import assert from 'node:assert/strict';
import { encryptNotes, decryptNotes, mergeNotes, validateNotes, createFileProvider } from './sync.mjs';

const password = 'a quiet place to return';
const note = (body = 'A little space.\n简体 · 繁體  ', id = 'local-1') => ({ id, kind: 'focus', body,
  version: 2, created_at_utc_ms: 10, updated_at_utc_ms: 20, archived_at_utc_ms: 30 });

test('encrypted exchange preserves exact Unicode and omits unrelated private fields', async () => {
  const source = { ...note(), heart_rate: 74, session_id: 'private-session' };
  const encrypted = await encryptNotes([source], password);
  assert.ok(!encrypted.includes('A little space'));
  assert.ok(!encrypted.includes('heart_rate'));
  assert.deepEqual(await decryptNotes(encrypted, password), [note()]);
  const second = JSON.parse(await encryptNotes([source], password));
  const first = JSON.parse(encrypted);
  assert.notEqual(first.salt, second.salt);
  assert.notEqual(first.iv, second.iv);
  assert.notEqual(first.ciphertext, second.ciphertext);
});

test('wrong passwords and tampering fail without returning any notes', async () => {
  const encrypted = await encryptNotes([note()], password);
  await assert.rejects(decryptNotes(encrypted, 'this password is wrong'), /incorrect or the file/);
  const altered = JSON.parse(encrypted);
  altered.ciphertext = (altered.ciphertext[0] === 'A' ? 'B' : 'A') + altered.ciphertext.slice(1);
  await assert.rejects(decryptNotes(JSON.stringify(altered), password), /incorrect or the file/);
  const modifiedIv = JSON.parse(encrypted);
  modifiedIv.iv = 'AAAAAAAAAAAAAAAA';
  await assert.rejects(decryptNotes(JSON.stringify(modifiedIv), password), /incorrect or the file/);
});

test('untrusted envelopes reject arbitrary KDF work, extra fields, and malformed encodings', async () => {
  const envelope = JSON.parse(await encryptNotes([], password));
  for (const patch of [{ iterations: 2_147_483_647 }, { version: 3 }, { extra: true }, { cipher: 'AES-CBC' }]) {
    await assert.rejects(decryptNotes(JSON.stringify({ ...envelope, ...patch }), password), /unsupported/);
  }
  for (const patch of [{ salt: 'bad' }, { iv: 'AA==' }, { ciphertext: '!bad!' }, { ciphertext: '' }]) {
    await assert.rejects(decryptNotes(JSON.stringify({ ...envelope, ...patch }), password), /invalid/);
  }
  await assert.rejects(decryptNotes('x'.repeat(12 * 1024 * 1024 + 1), password), /too large/);
});

test('portable validation rejects live notes, unsafe dates, oversized Unicode, and weak passphrases', async () => {
  for (const patch of [{ archived_at_utc_ms: null }, { created_at_utc_ms: NaN }, { version: 0 },
    { updated_at_utc_ms: 8_640_000_000_000_001 }, { kind: 'sensor' }, { body: '字'.repeat(34_000) }]) {
    assert.throws(() => validateNotes([{ ...note(), ...patch }]));
  }
  assert.throws(() => validateNotes(Array.from({ length: 1001 }, () => note())), /1,000/);
  assert.throws(() => validateNotes([note('\ud800')]), /Unicode/);
  await assert.rejects(encryptNotes([note()], 'short'), /12 characters/);
});

test('multi-megabyte encrypted archives round-trip without regex stack exhaustion', async () => {
  const notes = Array.from({ length: 24 }, (_, index) => note('a'.repeat(99_000), `note-${index}`));
  const encoded = await encryptNotes(notes, password);
  assert.ok(encoded.length > 3_000_000);
  assert.deepEqual(await decryptNotes(encoded, password), notes);
});

test('merge preserves divergent versions and makes repeated exchanges idempotent', async () => {
  const original = note('original');
  const edited = { ...note('edited'), version: 3 };
  const merged = await mergeNotes([original], [edited, edited, { ...original, id: 'copied-id', version: 99 }]);
  assert.equal(merged.length, 2);
  assert.deepEqual(merged[0], original);
  assert.match(merged[1].id, /^import:[0-9a-f]{64}$/);
  assert.equal(merged[1].body, 'edited');
  assert.deepEqual(await mergeNotes(merged, [edited, ...merged]), merged);
  const otherDevice = await mergeNotes([], merged);
  assert.equal(otherDevice.length, 2);
  assert.deepEqual(await mergeNotes(merged, otherDevice), merged);
  assert.deepEqual(original, note('original'));
});

test('a forged reserved import ID never overwrites an unrelated local note', async () => {
  const [imported] = await mergeNotes([], [note('incoming')]);
  const occupied = { ...note('local words'), id: imported.id };
  await assert.rejects(mergeNotes([occupied], [note('incoming')]), /conflicts/);
  assert.equal(occupied.body, 'local words');
});

test('file provider receives encrypted data only and cancellation makes no changes', async () => {
  let saved;
  const provider = createFileProvider({ read: async () => saved?.text ?? null,
    write: async file => { saved = file; return 'saved'; } });
  assert.equal(await provider.import(password), null);
  assert.equal(await provider.export([note()], password), 'saved');
  assert.equal(saved.name, 'rhythm-notes.rhythm');
  assert.ok(!saved.text.includes(note().body));
  assert.deepEqual(await provider.import(password), [note()]);
});

test('version-two manual exchange preserves original zones and rejects a downgrade', async () => {
  const source = { ...note(), source_timezone: 'Asia/Shanghai', source_utc_offset_minutes: 480 };
  const [imported] = await mergeNotes([], [source]);
  assert.equal(imported.id, 'import:1e3a2483fa5dc8d13f793264ada3c31920ed0e10dc3dc2cece6c84a648b625ca'); // Shared Rust fixture.
  const encrypted = await encryptNotes([source], password);
  assert.equal(JSON.parse(encrypted).version, 2);
  assert.deepEqual(await decryptNotes(encrypted, password), [source]);
  const downgraded = { ...JSON.parse(encrypted), version: 1 };
  await assert.rejects(decryptNotes(JSON.stringify(downgraded), password), /incorrect or the file/);
  const variants = await mergeNotes([source], [{ ...source, source_timezone: 'UTC', source_utc_offset_minutes: 0 }]);
  assert.equal(variants.length, 2);
  assert.deepEqual(await mergeNotes(variants, variants), variants);
  assert.equal(JSON.parse(await encryptNotes([note()], password)).version, 1);
});
