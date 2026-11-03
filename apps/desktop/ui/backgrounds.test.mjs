import test from 'node:test';
import assert from 'node:assert/strict';
import { BackgroundDraft, loadBackgrounds, validateBackgrounds, validateImageFile } from './backgrounds.mjs';

function repository() {
  let value = null;
  return { read: async () => structuredClone(value), write: async next => { value = structuredClone(next); } };
}

test('Focus and Break defaults and saved choices remain independent across reloads', async () => {
  const storage = repository();
  const preferences = await loadBackgrounds({ repository: storage });
  const draft = preferences.createDraft();
  assert.equal(draft.value.focus.choice, 'forest'); assert.equal(draft.value.break.choice, 'coast');
  draft.choose('focus', 'dusk');
  await preferences.save(draft);
  const reopened = await loadBackgrounds({ repository: storage });
  assert.equal(reopened.value.focus.choice, 'dusk'); assert.equal(reopened.value.break.choice, 'coast');
});

test('custom image bytes stay local and survive choosing a built-in scene and back', async () => {
  const storage = repository(), preferences = await loadBackgrounds({ repository: storage });
  const draft = preferences.createDraft();
  const blob = new Blob(['test image bytes'], { type: 'image/jpeg' });
  draft.setImage('break', blob, 'my-background.jpg');
  draft.choose('break', 'forest');
  assert.equal(draft.value.break.custom.name, 'my-background.jpg');
  draft.choose('break', 'custom'); await preferences.save(draft);
  const reopened = await loadBackgrounds({ repository: storage });
  assert.equal(await reopened.value.break.custom.blob.text(), 'test image bytes');
  assert.equal(reopened.value.focus.choice, 'forest');
});

test('a cancelled settings draft never changes saved preferences', async () => {
  const preferences = await loadBackgrounds({ repository: repository() });
  const draft = preferences.createDraft(); draft.choose('focus', 'coast'); draft.dispose();
  assert.equal(preferences.createDraft().value.focus.choice, 'forest');
  await assert.rejects(preferences.save(draft), /Reopen Settings/);
});

test('failed persistence retains both previous selections and the editable draft', async () => {
  const preferences = await loadBackgrounds({ repository: { read: async () => null, write: async () => { throw new Error('Quota exceeded'); } } });
  const draft = preferences.createDraft(); draft.choose('focus', 'coast'); draft.choose('break', 'dusk');
  await assert.rejects(preferences.save(draft), /Quota exceeded/);
  assert.equal(preferences.value.focus.choice, 'forest'); assert.equal(preferences.value.break.choice, 'coast');
  assert.equal(draft.value.focus.choice, 'coast'); assert.equal(draft.value.break.choice, 'dusk');
});

test('loading unavailable preferences remains usable but does not claim to save successfully', async () => {
  const preferences = await loadBackgrounds({ repository: { read: async () => { throw new Error('Storage blocked'); }, write: async () => { throw new Error('Storage blocked'); } } });
  assert.equal(preferences.value.focus.choice, 'forest'); assert.equal(preferences.loadError, 'Storage blocked');
  await assert.rejects(preferences.save(preferences.createDraft()), /Storage blocked/);
});

test('unfinished uploads cannot be saved and invalid images or selections are rejected', async () => {
  const preferences = await loadBackgrounds({ repository: repository() });
  const draft = preferences.createDraft(); draft.pending = 1;
  await assert.rejects(preferences.save(draft), /finish preparing/);
  assert.throws(() => draft.choose('focus', 'custom'), /Choose an image/);
  assert.throws(() => draft.choose('focus', 'unknown'), /Choose a built-in/);
  assert.throws(() => draft.setImage('break', new Blob(['bad'], { type: 'image/svg+xml' }), 'image.svg'), /invalid/);
  assert.throws(() => validateBackgrounds({ schema: 2 }), /cannot be read/);
  assert.throws(() => validateImageFile({ type: 'image/gif', size: 200 }), /JPEG/);
  assert.throws(() => validateImageFile({ type: 'image/png', size: 21 * 1024 * 1024 }), /20 MB/);
  assert.doesNotThrow(() => validateImageFile({ type: 'image/png', size: 200 }));
});

test('draft copies cannot mutate the live background configuration', async () => {
  const preferences = await loadBackgrounds({ repository: repository() });
  const first = preferences.createDraft(), second = new BackgroundDraft(preferences.value);
  first.choose('break', 'forest');
  assert.equal(second.value.break.choice, 'coast'); assert.equal(preferences.value.break.choice, 'coast');
});
