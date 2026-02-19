const CACHE_NAME = 'blockcell-v2';
const STATIC_ASSETS = [
  '/manifest.json',
  '/icon.svg',
];

// Install: cache only non-HTML static assets
self.addEventListener('install', (event) => {
  event.waitUntil(
    caches.open(CACHE_NAME).then((cache) => {
      return cache.addAll(STATIC_ASSETS).catch(() => {
        // Some assets may not exist yet, that's ok
      });
    })
  );
  // Take over immediately without waiting for old tabs to close
  self.skipWaiting();
});

// Activate: clean old caches and claim all clients immediately
self.addEventListener('activate', (event) => {
  event.waitUntil(
    caches.keys().then((keys) =>
      Promise.all(keys.filter((k) => k !== CACHE_NAME).map((k) => caches.delete(k)))
    ).then(() => self.clients.claim())
  );
});

// Fetch: network-first for API, cache-first for static
self.addEventListener('fetch', (event) => {
  const url = new URL(event.request.url);

  // Skip non-GET requests
  if (event.request.method !== 'GET') return;

  // Skip non-http(s) schemes (e.g. chrome-extension://)
  if (!url.protocol.startsWith('http')) return;

  // Skip WebSocket upgrade requests
  if (url.pathname.includes('/ws')) return;

  // API requests: network-first, no caching
  if (url.pathname.startsWith('/v1/') || url.pathname.startsWith('/api/')) {
    event.respondWith(
      fetch(event.request).catch(() => caches.match(event.request))
    );
    return;
  }

  // index.html and SPA routes: ALWAYS network-first, never serve stale HTML.
  // Stale index.html referencing old hashed JS bundles causes blank pages after
  // a rebuild because the old asset filenames no longer exist.
  if (url.pathname === '/' || url.pathname === '/index.html' || !url.pathname.includes('.')) {
    event.respondWith(
      fetch(event.request).catch(() => caches.match('/index.html'))
    );
    return;
  }

  // Hashed JS/CSS assets (e.g. /assets/index-CFTGbKt3.js): cache-first.
  // These are content-addressed so caching is safe indefinitely.
  if (url.pathname.startsWith('/assets/')) {
    event.respondWith(
      caches.match(event.request).then((cached) => {
        if (cached) return cached;
        return fetch(event.request).then((response) => {
          if (response.ok) {
            const clone = response.clone();
            caches.open(CACHE_NAME).then((cache) => cache.put(event.request, clone));
          }
          return response;
        });
      })
    );
    return;
  }

  // Other static assets (manifest, icons, sw.js itself): network-first
  event.respondWith(
    fetch(event.request).catch(() => caches.match(event.request))
  );
});
