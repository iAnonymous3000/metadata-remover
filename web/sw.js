const CACHE_NAME = 'metadata-remover-v9';
const ASSETS = [
    '.',
    'index.html',
    'manifest.webmanifest',
    'icon.svg',
    'icon-192.png',
    'icon-512.png',
    'css/style.css',
    'js/app.js',
    'js/worker.js',
    'wasm/metadata_remover.js',
    'wasm/metadata_remover_bg.wasm'
];

self.addEventListener('install', (event) => {
    event.waitUntil(
        caches.open(CACHE_NAME)
            .then((cache) => cache.addAll(ASSETS.map((path) => new URL(path, self.registration.scope).toString())))
            .then(() => self.skipWaiting())
    );
});

self.addEventListener('activate', (event) => {
    event.waitUntil(
        caches.keys()
            .then((keys) => Promise.all(keys.filter((key) => key !== CACHE_NAME).map((key) => caches.delete(key))))
            .then(() => self.clients.claim())
    );
});

self.addEventListener('fetch', (event) => {
    const requestUrl = new URL(event.request.url);
    if (event.request.method !== 'GET' || requestUrl.origin !== location.origin) {
        return;
    }

    if (event.request.mode === 'navigate' || event.request.headers.get('accept')?.includes('text/html')) {
        event.respondWith(networkFirst(event.request, new URL('.', self.registration.scope).toString()));
        return;
    }

    event.respondWith(networkFirst(event.request));
});

function networkFirst(request, fallbackUrl = null) {
    return fetch(request).then((response) => {
        if (response.ok) {
            const copy = response.clone();
            caches.open(CACHE_NAME).then((cache) => cache.put(request, copy));
        }
        return response;
    }).catch(() => cachedFallback(request, fallbackUrl));
}

function cachedFallback(request, fallbackUrl) {
    return caches.match(request).then((cached) => {
        if (cached) return cached;
        if (!fallbackUrl) return Response.error();
        return caches.match(fallbackUrl).then((fallback) => fallback || Response.error());
    });
}
