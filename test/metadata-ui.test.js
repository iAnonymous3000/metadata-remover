import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { before, test } from 'node:test';

import { metadataSummary } from '../web/js/metadata-summary.js';
import { buildSampleExifSegment } from '../web/js/sample-jpeg.js';
import initWasm, {
    analyze_file,
    detect_file_type,
    process_file,
    validate_file
} from '../web/wasm/metadata_remover.js';

const textEncoder = new TextEncoder();

before(async () => {
    const wasmBytes = await readFile(new URL('../web/wasm/metadata_remover_bg.wasm', import.meta.url));
    await initWasm({ module_or_path: wasmBytes });
});

test('metadataSummary prioritizes specific user-facing metadata', () => {
    const entries = [
        { category: 'EXIF', name: 'EXIF Data', value: '253 bytes' },
        { category: 'EXIF', name: 'Camera Model', value: 'SampleCam X1' },
        { category: 'EXIF', name: 'DateTime', value: '2024:01:01 12:00:00' },
        { category: 'EXIF', name: 'Software', value: 'Browser sample file' },
        { category: 'Location', name: 'GPS / Location data', value: '37.774900, -122.419400' }
    ];

    assert.equal(
        metadataSummary(entries),
        'GPS location: 37.774900, -122.419400 / camera model: SampleCam X1 / timestamp: 2024:01:01 12:00:00 / 1 more'
    );
});

test('metadataSummary handles clean and generic metadata states', () => {
    assert.equal(metadataSummary([]), 'No removable metadata');
    assert.equal(
        metadataSummary([{ category: 'XMP', name: 'XMP Data', value: '12 bytes' }]),
        'XMP data'
    );
});

test('sample EXIF segment decodes and cleans through the real WASM parser', () => {
    const sample = concatBytes([
        Uint8Array.of(0xff, 0xd8),
        buildSampleExifSegment(),
        Uint8Array.of(0xff, 0xd9)
    ]);

    assert.equal(detect_file_type(sample), 'jpeg');
    assert.doesNotThrow(() => validate_file(sample));

    const result = analyze_file(sample);
    const entries = result.metadata.metadata_found;
    assert.equal(result.fileType, 'jpeg');
    assert.equal(
        entries.find((entry) => entry.name === 'GPS / Location data')?.value,
        '37.774900, -122.419400'
    );
    assert.equal(
        entries.find((entry) => entry.name === 'Camera Model')?.value,
        'SampleCam X1'
    );
    assert.equal(
        entries.find((entry) => entry.name === 'DateTime')?.value,
        '2024:01:01 12:00:00'
    );

    const processed = process_file(sample);
    assert.equal(processed.verification.metadata_found.length, 0);
    assert.equal(containsAscii(processed.cleaned, 'SampleCam X1'), false);
    assert.equal(containsAscii(processed.cleaned, '2024:01:01 12:00:00'), false);
    assert.equal(containsAscii(processed.cleaned, '37.774900'), false);
});

function concatBytes(parts) {
    const totalLength = parts.reduce((sum, part) => sum + part.length, 0);
    const combined = new Uint8Array(totalLength);
    let offset = 0;
    for (const part of parts) {
        combined.set(part, offset);
        offset += part.length;
    }
    return combined;
}

function containsAscii(data, value) {
    const needle = textEncoder.encode(value);
    for (let i = 0; i <= data.length - needle.length; i++) {
        let matched = true;
        for (let j = 0; j < needle.length; j++) {
            if (data[i + j] !== needle[j]) {
                matched = false;
                break;
            }
        }
        if (matched) return true;
    }
    return false;
}
