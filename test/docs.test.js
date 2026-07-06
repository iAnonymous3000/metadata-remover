import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';

const SUPPORTED_FORMATS = 'JPEG, PNG, WebP, AVIF, GIF, HEIC/HEIF, TIFF, SVG, MP4/MOV, MP3, FLAC, WAV, PDF, DOCX, XLSX, PPTX, ODT, ODS, ODP, EPUB';
const README_FORMATS = [
    'JPEG',
    'PNG',
    'WebP',
    'AVIF',
    'GIF',
    'HEIC/HEIF',
    'TIFF',
    'SVG',
    'MP4/MOV',
    'MP3',
    'FLAC',
    'WAV',
    'PDF',
    'DOCX',
    'PPTX',
    'XLSX',
    'ODT/ODS/ODP',
    'EPUB'
];

test('public docs describe the current advertised format set', async () => {
    const [readme, indexHtml] = await Promise.all([
        readFile(new URL('../README.md', import.meta.url), 'utf8'),
        readFile(new URL('../web/index.html', import.meta.url), 'utf8')
    ]);

    assert.match(indexHtml, new RegExp(`Supports: ${SUPPORTED_FORMATS}`));

    for (const format of README_FORMATS) {
        assert.match(readme, new RegExp(`\\*\\*${format}\\*\\*`));
    }
});

test('minimal docs keep parser limits visible without extra planning files', async () => {
    const readme = await readFile(new URL('../README.md', import.meta.url), 'utf8');

    assert.doesNotMatch(readme, /ROADMAP\.md|BUILDING\.md|CONTRIBUTING\.md|CODE_OF_CONDUCT\.md/);
    assert.match(readme, /`wasm-pack` 0\.14\.x/);
    assert.match(readme, /Binaryen `wasm-opt`/);
    assert.match(readme, /cargo audit --deny warnings/);
    assert.match(readme, /Cleaned files are validated as the same file type/);
    assert.match(readme, /individual files over 100 MB/);
    assert.match(readme, /AVIF, HEIC\/HEIF, and MP4\/MOV cleaning target known ISO-BMFF metadata structures/);
    assert.match(readme, /EPUB cleaning preserves required package fields/);
    assert.match(readme, /MP3, FLAC, and WAV are supported; OGG\/Opus is not supported yet/);
    assert.match(readme, /Embedded images inside Office, ODF, and EPUB packages are recursively cleaned/);
    assert.match(readme, /Visual verification is limited to browser-renderable JPEG, PNG, WebP, AVIF, GIF, and cleaned SVG previews/);
    assert.match(readme, /SVG active content and external references are removed/);
    assert.match(readme, /SVG base64 data URIs holding supported raster images are decoded, recursively cleaned/);
    assert.match(readme, /Unknown\/private TIFF tags and uncommon or `ftyp`-less video variants/);
    assert.match(readme, /does not detect steganographic content/);
});
