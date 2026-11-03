import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile, access } from 'node:fs/promises';
import vm from 'node:vm';
import { registerOffline } from './install.mjs';

const directory = new URL('./', import.meta.url);
const workerSource = await readFile(new URL('sw.js', directory), 'utf8');
const scope = 'https://rhythm.example/companion/';
const prefix = `rhythm-shell:${encodeURIComponent(scope)}:`;

function worker() {
  const listeners = new Map();
  const requests = [], deleted = [], matched = [], network = [], opened = [];
  const cache = { addAll: async values => requests.push(...values),
    match: async key => { matched.push(key); return { cached: key }; } };
  const environment = {
    URL, Request,
    self: { registration: { scope }, addEventListener: (name, callback) => listeners.set(name, callback) },
    caches: {
      open: async key => { opened.push(key); return cache; },
      keys: async () => [`${prefix}old`, 'unrelated-app', 'rhythm-shell:another-scope:old', ...new Set(opened)],
      delete: async key => { deleted.push(key); return true; },
    },
    fetch: async request => { network.push(request); return { network: request.url }; },
  };
  vm.runInNewContext(workerSource, environment);
  return { listeners, requests, deleted, matched, network, environment, cache };
}

test('offline registration is optional, skips native shells and unsupported or insecure browsers', async () => {
  let registrations = 0;
  const environment = { isSecureContext: true, navigator: { serviceWorker: {
    register: async () => { registrations++; return { active: true }; },
  } } };
  assert.equal((await registerOffline({ ...environment, __TAURI__: {} })).supported, false);
  assert.equal((await registerOffline({ ...environment, isSecureContext: false })).supported, false);
  assert.equal((await registerOffline({ isSecureContext: true })).supported, false);
  assert.equal(registrations, 0);
  const result = await registerOffline(environment);
  assert.equal(result.ready, true);
  assert.equal(registrations, 1);
});

test('registration stays in its application directory and reports failure without crashing', async () => {
  let options;
  const result = await registerOffline({ isSecureContext: true, navigator: { serviceWorker: {
    register: async (script, opts) => { options = opts; assert.equal(script.href, new URL('sw.js', directory).href); return {}; },
  } } });
  assert.equal(options.scope, directory.href);
  assert.equal(options.updateViaCache, 'none');
  assert.equal(result.ready, false);
  const failure = await registerOffline({ isSecureContext: true, navigator: { serviceWorker: {
    register: async () => { throw new Error('Storage denied'); },
  } } });
  assert.equal(failure.ready, false);
  assert.equal(failure.error.message, 'Storage denied');
});

test('the complete local app shell and its imports are available for offline installation', async () => {
  const f = worker();
  let installing;
  f.listeners.get('install')({ waitUntil: promise => { installing = promise; } });
  await installing;
  const names = new Set(f.requests.map(request => request.url.slice(scope.length)));
  for (const request of f.requests) {
    assert.equal(request.url.startsWith(scope), true);
    assert.equal(request.cache, 'reload');
    const name = request.url.slice(scope.length);
    assert.equal(name.includes('.test.'), false);
    await access(new URL(name, directory));
    if (name.endsWith('.mjs')) {
      const contents = await readFile(new URL(name, directory), 'utf8');
      for (const imported of contents.matchAll(/(?:from\s*|import\s*)['"]\.\/([^'"]+)['"]/g)) {
        assert.equal(names.has(imported[1]), true, `Missing offline module: ${imported[1]}`);
      }
    }
  }
  const manifest = JSON.parse(await readFile(new URL('manifest.webmanifest', directory), 'utf8'));
  assert.equal(manifest.start_url, './');
  assert.equal(manifest.scope, './');
  assert.equal(manifest.display, 'standalone');
  for (const icon of manifest.icons) await access(new URL(icon.src, directory));
});

test('activation removes only obsolete caches for this exact application scope', async () => {
  const f = worker();
  let installing;
  f.listeners.get('install')({ waitUntil: promise => { installing = promise; } });
  await installing;
  let activating;
  f.listeners.get('activate')({ waitUntil: promise => { activating = promise; } });
  await activating;
  assert.deepEqual(f.deleted, [`${prefix}old`]);
  // No clients.claim or skipWaiting API is present in this fixture. Installation
  // and activation succeeding proves they never take over an open editor.
});

test('offline handling never intercepts notes, custom images, sync providers, or writes', async () => {
  const f = worker();
  const dispatch = (url, method = 'GET', mode = 'cors') => {
    let response;
    f.listeners.get('fetch')({ request: { url, method, mode }, respondWith: promise => { response = promise; } });
    return response;
  };
  assert.equal(dispatch(`${scope}notes.json`), undefined);
  assert.equal(dispatch(`${scope}assets/custom-photo.png`), undefined);
  assert.equal(dispatch('https://cloud.example/backup.json'), undefined);
  assert.equal(dispatch(`${scope}index.html`, 'POST'), undefined);
  assert.equal(dispatch(`${scope}app.mjs?user-data=private`), undefined);
  assert.deepEqual(await dispatch(`${scope}app.mjs`), { cached: `${scope}app.mjs` });
  assert.deepEqual(await dispatch(`${scope}?launch=home`, 'GET', 'navigate'), { cached: `${scope}index.html` });
  assert.deepEqual(f.network, []);
});

test('notification clicks only bring forward this application after explicit interaction', async () => {
  const f = worker();
  let focused = 0, closed = 0, opened = 0, handling;
  f.environment.self.clients = {
    matchAll: async () => [
      { url: 'https://rhythm.example/another-app/', focus: async () => { throw new Error('Wrong application'); } },
      { url: `${scope}index.html`, focus: async () => { focused++; } },
    ],
    openWindow: async () => { opened++; },
  };
  f.listeners.get('notificationclick')({ notification: { close: () => { closed++; } }, waitUntil: promise => { handling = promise; } });
  await handling;
  assert.equal(focused, 1);
  assert.equal(closed, 1);
  assert.equal(opened, 0);
});
