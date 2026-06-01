import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { constants } from 'node:fs';
import { access, readFile } from 'node:fs/promises';
import { test } from 'node:test';
import { runInNewContext } from 'node:vm';

test('GitHub Pages build does not ship inert custom header rules', async () => {
    await assert.rejects(
        access(new URL('../web/_headers', import.meta.url), constants.F_OK),
        { code: 'ENOENT' }
    );

    const readme = await readFile(new URL('../README.md', import.meta.url), 'utf8');
    assert.match(readme, /GitHub Pages deployment cannot apply .*`_headers` rules/);
    assert.match(readme, /Header-only protections such as HSTS, `frame-ancestors`/);
});

test('Pages CSP stays limited to meta-compatible directives', async () => {
    const html = await readFile(new URL('../web/index.html', import.meta.url), 'utf8');
    const csp = html.match(/<meta http-equiv="Content-Security-Policy" content="([^"]+)">/);

    assert.ok(csp, 'index.html should define a meta CSP for GitHub Pages');
    assert.doesNotMatch(csp[1], /\bframe-ancestors\b/);
    assert.doesNotMatch(csp[1], /\bsandbox\b/);
    assert.match(csp[1], /img-src 'self' blob:/);
    assert.match(html, /GitHub Pages cannot serve custom security headers from _headers/);
});

test('Pages CSP hash matches the inline structured data script', async () => {
    const html = await readFile(new URL('../web/index.html', import.meta.url), 'utf8');
    const csp = html.match(/<meta http-equiv="Content-Security-Policy" content="([^"]+)">/);
    const inlineScripts = [...html.matchAll(/<script(?![^>]*\bsrc=)[^>]*>([\s\S]*?)<\/script>/g)];

    assert.ok(csp, 'index.html should define a CSP');
    assert.equal(inlineScripts.length, 1, 'only the JSON-LD script should be inline');
    assert.doesNotMatch(csp[1], /'unsafe-inline'/);

    const hash = createHash('sha256').update(inlineScripts[0][1]).digest('base64');
    assert.match(csp[1], new RegExp(`'sha256-${escapeRegExp(hash)}'`));
});

test('service worker keeps runtime cache updates alive for fetch events', async () => {
    const sw = await readFile(new URL('../web/sw.js', import.meta.url), 'utf8');

    assert.match(sw, /metadata-remover-v29/);
    assert.match(sw, /'js\/download-names\.js'/);
    assert.match(sw, /'js\/file-lifecycle\.js'/);
    assert.match(sw, /'js\/memory-budget\.js'/);
    assert.match(sw, /'js\/metadata-modal\.js'/);
    assert.match(sw, /'js\/visual-verification-queue\.js'/);
    assert.match(sw, /'js\/worker-errors\.js'/);
    assert.match(sw, /'js\/worker-state\.js'/);
    assert.match(sw, /'js\/zip-download\.js'/);
    assert.match(sw, /networkFirst\(event\.request, new URL\('\.', self\.registration\.scope\)\.toString\(\), event, \{/);
    assert.match(sw, /fallbackOnNonOk: true/);
    assert.match(sw, /networkFirst\(event\.request, null, event, \{/);
    assert.match(sw, /fallbackOnNonOk: !isStrictNonOkAsset\(requestUrl\)/);
    assert.match(sw, /event\?\.waitUntil\(cacheUpdate\)/);
    assert.match(sw, /\.catch\(\(\) => undefined\)/);
});

test('service worker falls back to cache for non-OK navigation responses', async () => {
    const sw = await readFile(new URL('../web/sw.js', import.meta.url), 'utf8');
    const request = { url: 'https://example.test/metadata-remover/' };
    const cachedBody = 'cached app shell';
    const caches = mockServiceWorkerCaches(new Map([
        [request.url, new Response(cachedBody, { status: 200 })]
    ]));
    const networkFirst = loadNetworkFirst(sw, {
        caches,
        fetch: async () => new Response('server error', { status: 500 })
    });
    const waitUntilPromises = [];

    const response = await networkFirst(request, null, {
        waitUntil(promise) {
            waitUntilPromises.push(promise);
        }
    });

    assert.equal(response.status, 200);
    assert.equal(await response.text(), cachedBody);
    assert.equal(caches.puts.length, 0);
    assert.equal(waitUntilPromises.length, 1);
    await Promise.all(waitUntilPromises);
});

test('service worker does not hide non-OK critical asset deploy responses behind stale cache', async () => {
    const sw = await readFile(new URL('../web/sw.js', import.meta.url), 'utf8');

    for (const path of ['js/app.js', 'wasm/metadata_remover_bg.wasm', 'css/style.css']) {
        const request = { url: `https://example.test/metadata-remover/${path}` };
        const caches = mockServiceWorkerCaches(new Map([
            [request.url, new Response(`stale ${path}`, { status: 200 })]
        ]));
        const scopedNetworkFirst = loadNetworkFirst(sw, {
            caches,
            fetch: async () => new Response(`server error for ${path}`, { status: 500 })
        });

        const response = await scopedNetworkFirst(request, null, null, { fallbackOnNonOk: false });

        assert.equal(response.status, 500);
        assert.equal(await response.text(), `server error for ${path}`);
        assert.equal(caches.puts.length, 0);
    }
});

test('service worker keeps original non-OK response when no cache fallback exists', async () => {
    const sw = await readFile(new URL('../web/sw.js', import.meta.url), 'utf8');
    const request = { url: 'https://example.test/metadata-remover/missing.js' };
    const caches = mockServiceWorkerCaches(new Map());
    const networkFirst = loadNetworkFirst(sw, {
        caches,
        fetch: async () => new Response('missing', { status: 404 })
    });

    const response = await networkFirst(request, null);

    assert.equal(response.status, 404);
    assert.equal(await response.text(), 'missing');
    assert.equal(caches.puts.length, 0);
});

test('service worker precache assets exist in the web build source', async () => {
    const sw = await readFile(new URL('../web/sw.js', import.meta.url), 'utf8');
    const assets = serviceWorkerAssets(sw);

    assert.ok(assets.includes('icon-192.png'));
    assert.ok(assets.includes('icon-512.png'));

    for (const asset of assets) {
        if (asset === '.') continue;
        await access(new URL(`../web/${asset}`, import.meta.url), constants.F_OK);
    }
});

test('manifest and social image assets exist', async () => {
    const [html, manifestText] = await Promise.all([
        readFile(new URL('../web/index.html', import.meta.url), 'utf8'),
        readFile(new URL('../web/manifest.webmanifest', import.meta.url), 'utf8')
    ]);
    const manifest = JSON.parse(manifestText);
    const localOgImage = html.match(/<meta property="og:image" content="https:\/\/ianonymous3000\.github\.io\/metadata-remover\/([^"]+)">/);

    assert.ok(localOgImage, 'index.html should point og:image at a local Pages asset');
    await access(new URL(`../web/${localOgImage[1]}`, import.meta.url), constants.F_OK);

    for (const icon of manifest.icons) {
        await access(new URL(`../web/${icon.src}`, import.meta.url), constants.F_OK);
    }
});

function escapeRegExp(value) {
    return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

function serviceWorkerAssets(sw) {
    const assetBlock = sw.match(/const ASSETS = \[([\s\S]*?)\];/);
    assert.ok(assetBlock, 'sw.js should declare a precache asset list');
    return [...assetBlock[1].matchAll(/'([^']+)'/g)].map((match) => match[1]);
}

function loadNetworkFirst(sw, { caches, fetch }) {
    return runInNewContext(`${sw}\nnetworkFirst;`, {
        caches,
        fetch,
        location: { origin: 'https://example.test' },
        self: {
            registration: { scope: 'https://example.test/metadata-remover/' },
            clients: { claim: () => Promise.resolve() },
            addEventListener() {},
            skipWaiting: () => Promise.resolve()
        },
        URL,
        Response
    });
}

function mockServiceWorkerCaches(entries) {
    const puts = [];
    return {
        puts,
        async open() {
            return {
                async put(request, response) {
                    puts.push({ request, response });
                    entries.set(cacheKey(request), response.clone());
                }
            };
        },
        async match(request) {
            return entries.get(cacheKey(request));
        },
        async keys() {
            return [];
        },
        async delete() {
            return true;
        }
    };
}

function cacheKey(request) {
    return typeof request === 'string' ? request : request.url;
}
