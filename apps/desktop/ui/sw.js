// Change VERSION whenever shipped app-shell files change. A new worker waits for
// all older windows to close; it never reloads or takes over an active note editor.
const VERSION = '2026-11-02-note-archive-2';
const SCOPE = new URL(self.registration.scope);
const PREFIX = `rhythm-shell:${encodeURIComponent(SCOPE.href)}:`;
const CACHE = `${PREFIX}${VERSION}`;
const SHELL = [
  'index.html', 'styles.css', 'app.mjs', 'copy.mjs', 'rhythm-copy.mjs',
  'companion-copy.mjs', 'data-copy.mjs', 'auto-sync-copy.mjs', 'auto-sync.mjs', 'profile-backup.mjs', 'note-history.mjs', 'icons.mjs', 'model.mjs', 'browser.mjs', 'backgrounds.mjs',
  'note-archive.mjs', 'note-history-copy.mjs', 'history-query.mjs', 'note-provenance.mjs',
  'adaptive.mjs', 'sync.mjs', 'install.mjs', 'break-invitation.mjs', 'reminder-delay.mjs', 'manifest.webmanifest',
  'assets/focus-still.png', 'assets/break-still.png',
  'assets/rhythm-icon.svg', 'assets/rhythm-icon.png', 'assets/rhythm-icon-192.png', 'assets/rhythm-icon-512.png',
].map(path => new URL(path, SCOPE).href);
const ALLOWED = new Set(SHELL);
const INDEX = new URL('index.html', SCOPE).href;

self.addEventListener('install', event => {
  event.waitUntil(caches.open(CACHE).then(cache => cache.addAll(
    SHELL.map(url => new Request(url, { cache: 'reload', credentials: 'same-origin' })),
  )));
});

self.addEventListener('activate', event => {
  event.waitUntil(caches.keys().then(names => Promise.all(
    names.filter(name => name.startsWith(PREFIX) && name !== CACHE).map(name => caches.delete(name)),
  )));
});

self.addEventListener('fetch', event => {
  const request = event.request;
  if (request.method !== 'GET') return;
  const url = new URL(request.url);
  if (url.origin !== SCOPE.origin) return;
  const navigation = request.mode === 'navigate'
    && (url.pathname === SCOPE.pathname || url.pathname === new URL(INDEX).pathname);
  const key = navigation ? INDEX : request.url;
  // No runtime caching: only the fixed public shell can enter this cache.
  // In particular, notes, sync traffic and custom images never pass through it.
  if (!ALLOWED.has(key)) return;
  event.respondWith(caches.open(CACHE).then(async cache => (await cache.match(key)) ?? fetch(request)));
});

// Bringing Rhythm forward is permitted only after the person taps a notification.
self.addEventListener('notificationclick', event => {
  event.notification.close();
  event.waitUntil((async () => {
    const windows = await self.clients.matchAll({ type: 'window', includeUncontrolled: true });
    const current = windows.find(client => {
      const url = new URL(client.url);
      return url.origin === SCOPE.origin && (url.pathname === SCOPE.pathname || url.pathname === new URL(INDEX).pathname);
    });
    if (current) await current.focus();
    else await self.clients.openWindow(SCOPE.href);
  })());
});
