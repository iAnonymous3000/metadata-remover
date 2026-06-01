const CACHE_NAME = 'metadata-remover-v29';
const ASSETS = [
    '.',
    'index.html',
    'manifest.webmanifest',
    'icon.svg',
    'icon-192.png',
    'icon-512.png',
    'css/style.css',
    'js/app.js',
    'js/download-names.js',
    'js/file-lifecycle.js',
    'js/memory-budget.js',
    'js/metadata-modal.js',
    'js/metadata-summary.js',
    'js/sample-jpeg.js',
    'js/visual-verification-queue.js',
    'js/worker-errors.js',
    'js/worker-state.js',
    'js/worker.js',
    'js/zip-download.js',
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
        event.respondWith(networkFirst(event.request, new URL('.', self.registration.scope).toString(), event, {
            fallbackOnNonOk: true
        }));
        return;
    }

    event.respondWith(networkFirst(event.request, null, event, {
        fallbackOnNonOk: !isStrictNonOkAsset(requestUrl)
    }));
});

function networkFirst(request, fallbackUrl = null, event = null, {
    fallbackOnNonOk = true
} = {}) {
    const networkResponse = fetch(request);
    const cacheUpdate = networkResponse
        .then((response) => {
            if (!response.ok) return undefined;
            return caches.open(CACHE_NAME)
                .then((cache) => cache.put(request, response.clone()));
        })
        .catch(() => undefined);

    event?.waitUntil(cacheUpdate);

    return networkResponse
        .then((response) => {
            if (response.ok || !fallbackOnNonOk) return response;
            return cachedFallback(request, fallbackUrl)
                .then((fallback) => fallback.type === 'error' ? response : fallback);
        })
        .catch(() => cachedFallback(request, fallbackUrl));
}

function isStrictNonOkAsset(url) {
    return /\.(?:css|js|wasm|webmanifest|svg|png)$/i.test(url.pathname);
}

function cachedFallback(request, fallbackUrl) {
    return caches.match(request).then((cached) => {
        if (cached) return cached;
        if (!fallbackUrl) return Response.error();
        return caches.match(fallbackUrl).then((fallback) => fallback || Response.error());
    });
}
