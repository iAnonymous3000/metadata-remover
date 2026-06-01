import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { before, test } from 'node:test';

import {
    applyAnalysisFailure,
    applyProcessFailure,
    applyProcessSuccess,
    clearFileCollection,
    removeFileRecord
} from '../web/js/file-lifecycle.js';
import { cleanedFilename, sanitizeZipEntryName, uniqueFilename } from '../web/js/download-names.js';
import {
    imageDimensionsFromBytes,
    retainedFileBytesForBudget,
    visualVerificationTransientBytesForBudget
} from '../web/js/memory-budget.js';
import {
    MODAL_LOADING_MESSAGE,
    MODAL_PROCESSING_MESSAGE,
    renderMetadataModalInto,
    resetMetadataModalElement
} from '../web/js/metadata-modal.js';
import {
    groupMetadataEntriesByCategory,
    metadataSummary
} from '../web/js/metadata-summary.js';
import { buildSampleExifSegment } from '../web/js/sample-jpeg.js';
import {
    budgetSkippedVisualProof,
    createVisualVerificationQueue,
    isBudgetSkippedVisualProof,
    isUnavailableVisualProof,
    unavailableVisualProof
} from '../web/js/visual-verification-queue.js';
import { friendlyFileError } from '../web/js/worker-errors.js';
import {
    beginFileMessage,
    createWorkerFileState,
    finishFileMessage,
    forgetFailedRequestData,
    handleWorkerControlMessage,
    shouldSkipFileMessage,
    storeFileData
} from '../web/js/worker-state.js';
import { createZip } from '../web/js/zip-download.js';
import initWasm, {
    analyze_file,
    detect_file_type,
    process_file,
    validate_file
} from '../web/wasm/metadata_remover.js';

const textEncoder = new TextEncoder();
const textDecoder = new TextDecoder();

before(async () => {
    const wasmBytes = await readFile(new URL('../web/wasm/metadata_remover_bg.wasm', import.meta.url));
    await initWasm({ module_or_path: wasmBytes });
});

test('metadataSummary prioritizes specific user-facing metadata', () => {
    const entries = [
        { category: 'EXIF', name: 'EXIF Data', value: '253 bytes' },
        { category: 'EXIF', name: 'Camera Model', value: 'SampleCam X1' },
        { category: 'EXIF', name: 'DateTime', value: '2024:01:01 12:00:00' },
        { category: 'EXIF', name: 'DateTimeOriginal', value: '2023:12:31 23:59:58' },
        { category: 'EXIF', name: 'Software', value: 'Browser sample file' },
        { category: 'Location', name: 'GPS / Location data', value: '37.774900, -122.419400' }
    ];

    assert.equal(
        metadataSummary(entries),
        'GPS location: 37.774900, -122.419400 / camera model: SampleCam X1 / timestamp: 2023:12:31 23:59:58 / 2 more'
    );
});

test('metadataSummary handles clean and generic metadata states', () => {
    assert.equal(metadataSummary([]), 'No removable metadata');
    assert.equal(
        metadataSummary([{ category: 'XMP', name: 'XMP Data', value: '12 bytes' }]),
        'XMP data'
    );
    assert.equal(
        metadataSummary([
            { category: 'Content Credentials', name: 'JPEG APP11/JUMBF', value: '42 bytes' },
            { category: 'Embedded artwork', name: 'Attached picture', value: '128 bytes' },
            { category: 'EPUB metadata', name: 'identifier', value: 'urn:uuid:secret' }
        ]),
        'Content Credentials / embedded audio artwork / EPUB package metadata'
    );
    assert.equal(
        metadataSummary([
            { category: 'Audio metadata', name: 'APEv2 tag', value: '64 bytes; removed during MP3 cleaning' },
            { category: 'Embedded image metadata: XMP', name: 'OPS/images/cover.jpg: XMP Data', value: '95 bytes' }
        ]),
        'audio tail metadata / embedded image metadata'
    );
    assert.equal(
        metadataSummary([{ category: 'Limited verification', name: 'Embedded package images', value: '1 packaged image preserved' }]),
        'limited verification'
    );
});

test('metadata grouping treats prototype-like categories as plain data', () => {
    const entries = [
        { category: '__proto__', name: 'polluted', value: 'no' },
        { category: 'constructor', name: 'ctor', value: 'no' },
        { category: 'EXIF', name: 'Camera Model', value: 'SampleCam X1' },
        { category: '__proto__', name: 'second', value: 'still data' }
    ];

    const grouped = groupMetadataEntriesByCategory(entries);

    assert.ok(grouped instanceof Map);
    assert.deepEqual([...grouped.keys()], ['__proto__', 'constructor', 'EXIF']);
    assert.deepEqual(
        grouped.get('__proto__').map((entry) => entry.name),
        ['polluted', 'second']
    );
    assert.equal(Object.getPrototypeOf({}).polluted, undefined);
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
        entries.find((entry) => entry.name === 'DateTimeOriginal')?.value,
        '2023:12:31 23:59:58'
    );
    assert.equal(
        entries.find((entry) => entry.name === 'DateTime')?.value,
        '2024:01:01 12:00:00'
    );

    const processed = process_file(sample);
    assert.equal(processed.verification.metadata_found.length, 0);
    assert.equal(containsAscii(processed.cleaned, 'SampleCam X1'), false);
    assert.equal(containsAscii(processed.cleaned, '2023:12:31 23:59:58'), false);
    assert.equal(containsAscii(processed.cleaned, '2024:01:01 12:00:00'), false);
    assert.equal(containsAscii(processed.cleaned, '37.774900'), false);
});

test('WASM process path rejects malformed input before cleaning', () => {
    const malformedJpeg = Uint8Array.of(
        0xff, 0xd8, 0xff, 0xfe, 0x00, 0x08, 0x73, 0x65, 0x63, 0x72, 0x65, 0x74
    );

    assert.equal(detect_file_type(malformedJpeg), 'jpeg');
    assert.throws(
        () => process_file(malformedJpeg),
        (error) => {
            assert.equal(error.fileType, 'jpeg');
            assert.equal(error.error, 'JPEG missing EOI marker');
            return true;
        }
    );
});

test('SVG sanitizer handles unquoted attributes, script boundary payloads, and data URI review notes', () => {
    const activeSvg = asciiBytes('<svg xmlns="http://www.w3.org/2000/svg" onload=track()><script>const payload = "</script><image href=https://tracker.example/pixel.png onload=steal()></script>";</script><rect style=filter:url(https://tracker.example/filter.svg#x) width=10 height=10/></svg>');

    assert.equal(detect_file_type(activeSvg), 'svg');
    const analyzed = analyze_file(activeSvg);
    assert.equal(analyzed.fileType, 'svg');
    assert.ok(analyzed.metadata.metadata_found.some((entry) => entry.name === 'script/foreignObject elements'));
    assert.ok(analyzed.metadata.metadata_found.some((entry) => entry.name === 'removable attributes'));

    const processed = process_file(activeSvg);
    const cleanedText = textDecoder.decode(processed.cleaned);
    for (const removed of ['<script', '</script', 'tracker.example', 'onload', 'steal']) {
        assert.equal(cleanedText.includes(removed), false, `cleaned SVG still contains ${removed}`);
    }
    assert.equal(processed.verification.metadata_found.length, 0);

    const dataSvg = asciiBytes('<svg xmlns="http://www.w3.org/2000/svg"><image href="data:image/jpeg;base64,/9j/4AAQSkZJRg==" width="10" height="10"/></svg>');
    const dataProcessed = process_file(dataSvg);
    assert.ok(dataProcessed.verification.metadata_found.some((entry) => (
        entry.category === 'Limited verification'
        && entry.name === 'Embedded data URI content'
    )));
    assert.equal(textDecoder.decode(dataProcessed.cleaned).includes('data:image/jpeg'), true);
});

test('MP3, PDF, and DOCX clean through the real WASM process and verification path', () => {
    const mp3 = makeMp3WithId3v1();
    assert.equal(detect_file_type(mp3), 'mp3');
    assert.ok(analyze_file(mp3).metadata.metadata_found.some((entry) => entry.name === 'ID3v1 tag'));
    const cleanMp3 = process_file(mp3);
    assert.equal(cleanMp3.verification.metadata_found.length, 0);
    assert.equal(containsAscii(cleanMp3.cleaned, 'Secret Song'), false);

    const pdf = makePdfWithInfo();
    assert.equal(detect_file_type(pdf), 'pdf');
    assert.ok(analyze_file(pdf).metadata.metadata_found.some((entry) => entry.category === 'Info' && entry.name === 'Author'));
    const cleanPdf = process_file(pdf);
    assert.equal(cleanPdf.verification.metadata_found.length, 0);
    assert.equal(containsAscii(cleanPdf.cleaned, 'Alice Author'), false);

    const docx = makeDocxWithCoreProperties();
    assert.equal(detect_file_type(docx), 'docx');
    assert.ok(analyze_file(docx).metadata.metadata_found.some((entry) => entry.value.includes('Secret Author')));
    const cleanDocx = process_file(docx);
    assert.equal(cleanDocx.verification.metadata_found.length, 0);
    assert.equal(containsAscii(cleanDocx.cleaned, 'Secret Author'), false);
});

test('metadata modal renders lifecycle states before empty or cleaned fallbacks', () => {
    const { modalBody, calls, renderers } = modalHarness();

    renderMetadataModalInto({
        id: 'loading-file',
        status: 'loading',
        metadata: emptyMetadataForTest('svg')
    }, modalBody, renderers);
    assert.equal(modalBody.dataset.fileId, 'loading-file');
    assert.equal(modalBody.textContent, MODAL_LOADING_MESSAGE);
    assert.deepEqual(calls.splice(0), ['pending']);
    assert.doesNotMatch(modalBody.textContent, /No removable metadata/);

    renderMetadataModalInto({
        id: 'processing-file',
        status: 'processing',
        cleanedData: Uint8Array.of(1, 2, 3),
        metadata: emptyMetadataForTest('svg')
    }, modalBody, renderers);
    assert.equal(modalBody.dataset.fileId, 'processing-file');
    assert.equal(modalBody.textContent, MODAL_PROCESSING_MESSAGE);
    assert.deepEqual(calls.splice(0), ['pending']);
    assert.doesNotMatch(modalBody.textContent, /Cleaned details/);

    renderMetadataModalInto({
        id: 'pending-empty-file',
        status: 'pending',
        metadata: emptyMetadataForTest('svg')
    }, modalBody, renderers);
    assert.equal(modalBody.textContent, 'No removable metadata found in this file.');
    assert.deepEqual(calls.splice(0), ['no-metadata']);

    renderMetadataModalInto({
        id: 'cleaned-file',
        status: 'done',
        cleanedData: Uint8Array.of(1),
        metadata: emptyMetadataForTest('jpeg')
    }, modalBody, renderers);
    assert.equal(modalBody.textContent, 'Cleaned details');
    assert.deepEqual(calls.splice(0), ['cleaned']);
});

test('file lifecycle releases source bytes and asks the worker to forget failed files', () => {
    const analyzedFile = {
        id: 'analysis-failed',
        type: 'unknown',
        sourceFile: { size: 12 },
        status: 'loading',
        errorMessage: null
    };
    const analysisForget = applyAnalysisFailure(
        analyzedFile,
        Object.assign(new Error('Invalid SVG'), { fileType: 'svg' })
    );

    assert.equal(analyzedFile.type, 'svg');
    assert.equal(analyzedFile.sourceFile, null);
    assert.equal(analyzedFile.status, 'error');
    assert.equal(analyzedFile.errorMessage, 'Invalid SVG');
    assert.deepEqual(analysisForget, { type: 'forget', id: 'analysis-failed' });

    const processedFile = {
        id: 'process-failed',
        sourceFile: { size: 34 },
        visualSourceFile: { size: 34 },
        status: 'processing',
        errorMessage: null
    };
    const processForget = applyProcessFailure(processedFile, new Error('Invalid PDF'));

    assert.equal(processedFile.sourceFile, null);
    assert.equal(processedFile.visualSourceFile, null);
    assert.equal(processedFile.status, 'error');
    assert.equal(processedFile.errorMessage, 'Invalid PDF');
    assert.deepEqual(processForget, { type: 'forget', id: 'process-failed' });
});

test('file lifecycle releases source bytes after successful processing', () => {
    const sourceFile = { size: 56 };
    const file = {
        id: 'processed',
        type: 'jpeg',
        sourceFile,
        visualSourceFile: null,
        metadata: emptyMetadataForTest('jpeg'),
        verification: null,
        cleanedData: null,
        cleanedSize: null,
        status: 'processing'
    };

    applyProcessSuccess(
        file,
        {
            cleanedBuffer: Uint8Array.of(1, 2, 3).buffer,
            verification: emptyMetadataForTest('jpeg')
        },
        (fileType) => fileType === 'jpeg'
    );

    assert.deepEqual([...file.cleanedData], [1, 2, 3]);
    assert.equal(file.cleanedSize, 3);
    assert.equal(file.sourceFile, null);
    assert.equal(file.visualSourceFile, sourceFile);
    assert.equal(file.status, 'done');

    const reviewFile = {
        id: 'review',
        type: 'pdf',
        sourceFile: { size: 78 },
        visualSourceFile: null,
        metadata: emptyMetadataForTest('pdf'),
        verification: null,
        cleanedData: null,
        cleanedSize: null,
        status: 'processing'
    };

    applyProcessSuccess(
        reviewFile,
        {
            cleanedBuffer: Uint8Array.of(9).buffer,
            verification: {
                file_type: 'pdf',
                metadata_found: [{ category: 'Limited verification', name: 'Embedded content', value: 'review' }],
                total_metadata_bytes: 0
            }
        },
        () => false
    );

    assert.equal(reviewFile.sourceFile, null);
    assert.equal(reviewFile.visualSourceFile, null);
    assert.equal(reviewFile.status, 'warning');
});

test('memory budget drops processed visual source after background proof is ready', () => {
    const sourceFile = { size: 10_000_000 };
    const file = {
        id: 'processed-visual',
        type: 'jpeg',
        sourceFile,
        visualSourceFile: null,
        visualProof: null,
        metadata: emptyMetadataForTest('jpeg'),
        verification: null,
        cleanedData: null,
        cleanedSize: null,
        status: 'processing'
    };

    applyProcessSuccess(
        file,
        {
            cleanedBuffer: Uint8Array.of(1, 2, 3).buffer,
            verification: emptyMetadataForTest('jpeg')
        },
        (fileType) => fileType === 'jpeg'
    );

    assert.equal(retainedFileBytesForBudget(file), sourceFile.size + file.cleanedData.byteLength);

    file.visualProof = {
        originalSnapshot: { width: 720, height: 520 },
        cleanedSnapshot: { width: 720, height: 520 }
    };
    file.visualSourceFile = null;

    assert.equal(retainedFileBytesForBudget(file), file.cleanedData.byteLength + (360 * 260 * 4 * 2));
    assert.ok(retainedFileBytesForBudget(file) < sourceFile.size);
});

test('memory budget estimates transient visual decode and compare headroom', () => {
    assert.equal(
        visualVerificationTransientBytesForBudget({
            type: 'jpeg',
            visualSourceFile: { size: 10 },
            cleanedData: Uint8Array.of(1, 2)
        }, {
            pixelCompareMax: 10,
            pixelBufferMultiplier: 6,
            decodeExpansionFactor: 2
        }),
        240
    );

    assert.equal(
        visualVerificationTransientBytesForBudget({
            type: 'jpeg',
            visualSourceFile: { size: 100 },
            cleanedData: new Uint8Array(200)
        }, {
            pixelCompareMax: 1,
            pixelBufferMultiplier: 1,
            decodeExpansionFactor: 3
        }),
        900
    );

    assert.equal(
        visualVerificationTransientBytesForBudget({
            type: 'jpeg',
            visualSourceFile: { size: 2_000_000 },
            cleanedData: new Uint8Array(1)
        }, {
            pixelCompareMax: 1,
            pixelBufferMultiplier: 1,
            decodeExpansionFactor: 1,
            unknownDimensionThresholdBytes: 1_000_000,
            unknownDimensionTransientBytes: 512_000_000
        }),
        512_000_000
    );

    assert.equal(
        visualVerificationTransientBytesForBudget({
            type: 'svg',
            visualSourceFile: { size: 1_000_000 },
            cleanedData: new Uint8Array(100)
        }, {
            pixelCompareMax: 1_000_000,
            decodeExpansionFactor: 2
        }),
        200
    );

    assert.equal(
        visualVerificationTransientBytesForBudget({
            type: 'jpeg',
            visualSourceFile: { size: 10 },
            cleanedData: new Uint8Array(10),
            visualSourceDimensions: { width: 10_000, height: 10_000 },
            cleanedVisualDimensions: { width: 10_000, height: 10_000 }
        }, {
            pixelCompareMax: 4_000_000,
            pixelBufferMultiplier: 6,
            decodedImageBufferMultiplier: 2,
            decodeExpansionFactor: 1
        }),
        800_000_000
    );
});

test('memory budget parses raster dimensions without browser decode', () => {
    assert.deepEqual(imageDimensionsFromBytes('png', Uint8Array.of(
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a,
        0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
        0x00, 0x00, 0x04, 0x00,
        0x00, 0x00, 0x02, 0x00
    )), { width: 1024, height: 512 });
    assert.deepEqual(imageDimensionsFromBytes('gif', Uint8Array.of(
        0x47, 0x49, 0x46, 0x38, 0x39, 0x61,
        0x20, 0x03, 0x58, 0x02
    )), { width: 800, height: 600 });
    assert.deepEqual(imageDimensionsFromBytes('jpeg', Uint8Array.of(
        0xff, 0xd8, 0xff, 0xc0, 0x00, 0x11, 0x08,
        0x02, 0x58, 0x03, 0x20,
        0x03, 0x01, 0x11, 0x00, 0x02, 0x11, 0x00, 0x03, 0x11, 0x00
    )), { width: 800, height: 600 });
    assert.deepEqual(imageDimensionsFromBytes('webp', Uint8Array.of(
        0x52, 0x49, 0x46, 0x46, 0x1e, 0x00, 0x00, 0x00,
        0x57, 0x45, 0x42, 0x50,
        0x56, 0x50, 0x38, 0x58, 0x0a, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00,
        0x1f, 0x03, 0x00,
        0x57, 0x02, 0x00,
        0x00, 0x00
    )), { width: 800, height: 600 });
    assert.deepEqual(imageDimensionsFromBytes('avif', concatBytes([
        bmffBox('ftyp', concatBytes([asciiBytes('avif'), Uint8Array.of(0, 0, 0, 0)])),
        bmffBox('meta', concatBytes([
            Uint8Array.of(0, 0, 0, 0),
            bmffBox('iprp', bmffBox('ipco', bmffBox('ispe', concatBytes([
                Uint8Array.of(0, 0, 0, 0),
                u32be(640),
                u32be(480)
            ]))))
        ]))
    ])), { width: 640, height: 480 });
});

test('visual verification queue caps concurrent background work', async () => {
    const queue = createVisualVerificationQueue({ concurrency: 2 });
    const releases = [];
    let active = 0;
    let maxActive = 0;

    const jobs = Array.from({ length: 4 }, (_, index) => queue.enqueue(async () => {
        active += 1;
        maxActive = Math.max(maxActive, active);
        await new Promise((resolve) => {
            releases[index] = resolve;
        });
        active -= 1;
        return index;
    }));

    await settleMicrotasks();
    assert.equal(queue.activeCount, 2);
    assert.equal(queue.pendingCount, 2);
    assert.equal(maxActive, 2);

    releases[0]();
    await jobs[0];
    await settleMicrotasks();

    assert.equal(queue.activeCount, 2);
    assert.equal(queue.pendingCount, 1);
    assert.equal(maxActive, 2);

    releases[1]();
    releases[2]();
    await Promise.all([jobs[1], jobs[2]]);
    await settleMicrotasks();
    assert.equal(queue.activeCount, 1);
    assert.equal(queue.pendingCount, 0);

    releases[3]();
    assert.deepEqual(await Promise.all(jobs), [0, 1, 2, 3]);
    assert.equal(queue.activeCount, 0);
    assert.equal(queue.pendingCount, 0);
    assert.equal(maxActive, 2);
});

test('visual verification queue skips aborted pending jobs', async () => {
    const queue = createVisualVerificationQueue({ concurrency: 1 });
    const firstRelease = deferred();
    let ranSecond = false;
    const abortController = new AbortController();

    const first = queue.enqueue(async () => {
        await firstRelease.promise;
        return 'first';
    });
    const second = queue.enqueue(async () => {
        ranSecond = true;
        return 'second';
    }, { signal: abortController.signal });

    await settleMicrotasks();
    assert.equal(queue.activeCount, 1);
    assert.equal(queue.pendingCount, 1);

    abortController.abort();
    await second;
    assert.equal(ranSecond, false);
    assert.equal(queue.pendingCount, 0);

    firstRelease.resolve();
    assert.equal(await first, 'first');
    await settleMicrotasks();
    assert.equal(queue.activeCount, 0);
});

test('budget-skipped visual proof stays identifiable for modal retry', () => {
    const proof = budgetSkippedVisualProof();

    assert.equal(proof.status, 'unavailable');
    assert.equal(proof.summary, 'Visual check skipped');
    assert.equal(isBudgetSkippedVisualProof(proof), true);
    assert.equal(isBudgetSkippedVisualProof({ ...proof, retryReason: 'other' }), false);
    assert.equal(isBudgetSkippedVisualProof(null), false);
});

test('unavailable visual proof stays identifiable for one explicit retry', () => {
    const proof = unavailableVisualProof('Browser decode failed.');

    assert.equal(proof.status, 'unavailable');
    assert.equal(proof.summary, 'Visual check unavailable');
    assert.equal(proof.detail, 'Browser decode failed.');
    assert.equal(isUnavailableVisualProof(proof), true);
    assert.equal(isUnavailableVisualProof(budgetSkippedVisualProof()), false);
});

test('app source gates visual proof retries and retention by headroom and retry reason', async () => {
    const app = await readFile(new URL('../web/js/app.js', import.meta.url), 'utf8');

    assert.match(app, /scheduleVisualVerification\(file, \{ force: true, requireHeadroom: true \}\)/);
    assert.match(app, /scheduleVisualVerification\(current, \{ requireHeadroom: true \}\);/);
    assert.match(app, /force && isBudgetSkippedVisualProof\(file\.visualProof\) && !!file\.visualSourceFile/);
    assert.match(app, /canRetryUnavailableVisualProof\(file, force\)/);
    assert.match(app, /VISUAL_UNAVAILABLE_RETRY_LIMIT = 1/);
    assert.match(app, /file\.visualProofPromise && !retryBudgetSkip && !retryUnavailable/);
    assert.match(app, /revokeVisualProof\(file, \{ keepVisualSource: true \}\)/);
    assert.match(app, /shouldRetainVisualSourceAfterProof\(current\)/);
    assert.match(app, /releaseBudgetSkippedVisualSource\(file\)/);
    assert.match(app, /file\.visualProofAbortController\?\.abort\(\)/);
    assert.match(app, /hasVisualVerificationHeadroom\(file\)/);
    assert.match(app, /retryBudgetSkippedVisualVerificationsSoon\(\)/);
    assert.match(app, /isCurrentVisualProofJob\(current, generation\)/);
    assert.doesNotMatch(app, /await visualPromise/);
});

test('app lets WASM content detection decide files with missing or wrong extensions', async () => {
    const [app, html] = await Promise.all([
        readFile(new URL('../web/js/app.js', import.meta.url), 'utf8'),
        readFile(new URL('../web/index.html', import.meta.url), 'utf8')
    ]);

    assert.match(app, /const filesToCheck = Array\.from\(newFiles\);/);
    assert.doesNotMatch(app, /SUPPORTED_EXTENSIONS/);
    assert.doesNotMatch(html, /\baccept=/);
});

test('worker state helpers clear and forget retained file data', () => {
    const state = createWorkerFileState();
    state.fileData.set('a', Uint8Array.of(1));
    state.fileData.set('b', Uint8Array.of(2));

    assert.equal(handleWorkerControlMessage(state, null), true);
    assert.equal(state.fileData.size, 2);

    beginFileMessage(state, { type: 'analyze', id: 'a' });
    assert.equal(handleWorkerControlMessage(state, { type: 'forget', id: 'a' }), true);
    assert.equal(state.fileData.has('a'), false);
    assert.equal(state.forgottenIds.has('a'), true);
    finishFileMessage(state, { type: 'analyze', id: 'a' });
    assert.equal(state.forgottenIds.has('a'), false);

    assert.equal(handleWorkerControlMessage(state, { type: 'analyze', id: 'b' }), false);
    forgetFailedRequestData(state, { type: 'analyze', id: 'b' });
    assert.equal(state.fileData.has('b'), false);

    state.fileData.set('c', Uint8Array.of(3));
    forgetFailedRequestData(state, { type: 'ready', id: 'c' });
    assert.equal(state.fileData.has('c'), true);

    assert.equal(handleWorkerControlMessage(state, { type: 'clear' }), true);
    assert.equal(state.fileData.size, 0);
    assert.equal(state.forgottenIds.size, 0);
    assert.equal(state.pendingFileRequests.size, 0);
});

test('worker state skips stale queued jobs after forget or clear', () => {
    const state = createWorkerFileState();
    const firstGeneration = beginFileMessage(state, { type: 'analyze', id: 'forgotten' });

    assert.equal(storeFileData(state, 'forgotten', Uint8Array.of(1), firstGeneration), true);
    assert.equal(handleWorkerControlMessage(state, { type: 'forget', id: 'forgotten' }), true);
    assert.equal(shouldSkipFileMessage(state, { type: 'analyze', id: 'forgotten' }, firstGeneration), true);
    assert.equal(storeFileData(state, 'forgotten', Uint8Array.of(2), firstGeneration), false);
    finishFileMessage(state, { type: 'analyze', id: 'forgotten' });
    assert.equal(state.forgottenIds.has('forgotten'), false);

    const secondGeneration = beginFileMessage(state, { type: 'process', id: 'older' });
    assert.equal(handleWorkerControlMessage(state, { type: 'clear' }), true);
    assert.equal(shouldSkipFileMessage(state, { type: 'process', id: 'older' }, secondGeneration), true);
    assert.equal(storeFileData(state, 'older', Uint8Array.of(3), secondGeneration), false);
    finishFileMessage(state, { type: 'process', id: 'older' });
    assert.equal(state.pendingFileRequests.size, 0);
});

test('worker file errors keep ZIP compression failures specific', () => {
    assert.equal(
        friendlyFileError('docx', new Error('Unsupported ZIP compression method 9')),
        'DOCX uses unsupported ZIP compression.'
    );
    assert.equal(
        friendlyFileError('unknown', new Error('Unsupported file type')),
        'This file type is not supported.'
    );
});

test('download names are safe for ZIP entries and remain unique', () => {
    assert.equal(sanitizeZipEntryName('../secret.jpg'), 'secret.jpg');
    assert.equal(sanitizeZipEntryName('..\\..\\secret.pdf'), 'secret.pdf');
    assert.equal(sanitizeZipEntryName('...\u0000\t'), 'file');
    assert.equal(sanitizeZipEntryName('CON'), 'CON_file');
    assert.equal(sanitizeZipEntryName('NUL.txt'), 'NUL_file.txt');
    assert.equal(cleanedFilename('../album/photo.jpg...'), 'photo_clean.jpg');
    assert.equal(cleanedFilename('NUL.txt'), 'NUL_file_clean.txt');

    const usedNames = new Set();
    assert.equal(uniqueFilename('../photo_clean.jpg', usedNames), 'photo_clean.jpg');
    assert.equal(uniqueFilename('folder\\PHOTO_clean.jpg', usedNames), 'PHOTO_clean-2.jpg');
    assert.equal(uniqueFilename('..', usedNames), 'file');
    assert.equal(uniqueFilename('CON', usedNames), 'CON_file');
});

test('ZIP download helper accepts blobs and reports build progress', async () => {
    const progress = [];
    const zipBlob = await createZip([
        { name: 'one.txt', data: new Blob([asciiBytes('one')]) },
        { name: 'two.txt', data: asciiBytes('two') }
    ], {
        onProgress(update) {
            progress.push({ ...update });
        }
    });

    assert.equal(zipBlob.type, 'application/zip');
    const zipBytes = new Uint8Array(await zipBlob.arrayBuffer());
    const view = new DataView(zipBytes.buffer, zipBytes.byteOffset, zipBytes.byteLength);

    assert.equal(view.getUint32(0, true), 0x04034b50);
    assert.equal(view.getUint32(zipBytes.byteLength - 22, true), 0x06054b50);
    assert.equal(containsAscii(zipBytes, 'one.txt'), true);
    assert.equal(containsAscii(zipBytes, 'two.txt'), true);
    assert.equal(progress[0].completedEntries, 0);
    assert.equal(progress.at(-1).completedEntries, 2);
    assert.equal(progress.at(-1).totalEntries, 2);
    assert.equal(progress.at(-1).processedBytes, 6);
    assert.equal(progress.at(-1).totalBytes, 6);
});

test('ZIP download helper can cancel during chunked CRC work', async () => {
    let canceled = false;
    await assert.rejects(
        createZip([
            { name: 'large.bin', data: new Uint8Array((4 * 1024 * 1024) + 1) }
        ], {
            shouldCancel: () => canceled,
            onProgress(update) {
                if (update.processedBytes > 0) {
                    canceled = true;
                }
            }
        }),
        /ZIP download was canceled/
    );
});

test('metadata modal reset helper clears stale state and restores focus only when open', () => {
    const { modalBody } = modalHarness();
    const modal = modalHarnessElement();
    let focusCount = 0;
    const focusedElement = {
        focus() {
            focusCount += 1;
        }
    };

    modalBody.dataset.fileId = 'removed-file';
    modalBody.innerHTML = '<section>Stale details</section>';

    resetMetadataModalElement(modal, modalBody, { lastFocusedElement: focusedElement });

    assert.equal(modal.classList.contains('hidden'), true);
    assert.equal(Object.hasOwn(modalBody.dataset, 'fileId'), false);
    assert.equal(modalBody.innerHTML, '');
    assert.equal(modalBody.textContent, '');
    assert.equal(focusCount, 1);

    modalBody.dataset.fileId = 'another-file';
    modalBody.innerHTML = '<section>More stale details</section>';

    resetMetadataModalElement(modal, modalBody, { lastFocusedElement: focusedElement });

    assert.equal(Object.hasOwn(modalBody.dataset, 'fileId'), false);
    assert.equal(modalBody.innerHTML, '');
    assert.equal(focusCount, 1);
});

test('file collection helpers reset modal state when records are cleared or removed', () => {
    const calls = [];
    const files = new Map([
        ['open-file', { id: 'open-file' }],
        ['other-file', { id: 'other-file' }]
    ]);

    clearFileCollection(files, lifecycleHarness(calls));

    assert.equal(files.size, 0);
    assert.deepEqual(calls, [
        'revoke:open-file',
        'revoke:other-file',
        'worker:clear',
        'drop-feedback',
        'reset:false',
        'render-list',
        'actions'
    ]);

    calls.length = 0;
    const removableFiles = new Map([
        ['open-file', { id: 'open-file' }],
        ['other-file', { id: 'other-file' }]
    ]);
    removeFileRecord(removableFiles, 'open-file', lifecycleHarness(calls, 'open-file'));

    assert.equal(removableFiles.has('open-file'), false);
    assert.equal(removableFiles.has('other-file'), true);
    assert.deepEqual(calls, [
        'revoke:open-file',
        'worker:forget:open-file',
        'row:open-file',
        'reset:false',
        'actions'
    ]);

    calls.length = 0;
    removeFileRecord(removableFiles, 'other-file', lifecycleHarness(calls, 'open-file'));

    assert.equal(removableFiles.has('other-file'), false);
    assert.deepEqual(calls, [
        'revoke:other-file',
        'worker:forget:other-file',
        'row:other-file',
        'actions'
    ]);
});

function asciiBytes(value) {
    return textEncoder.encode(value);
}

function emptyMetadataForTest(fileType) {
    return {
        file_type: fileType,
        metadata_found: [],
        total_metadata_bytes: 0
    };
}

function modalHarness() {
    const calls = [];
    const modalBody = {
        dataset: {},
        _innerHTML: '',
        textContent: '',
        set innerHTML(value) {
            this._innerHTML = value;
            this.textContent = stripTags(value).trim();
        },
        get innerHTML() {
            return this._innerHTML;
        }
    };
    const renderers = {
        renderError(message) {
            calls.push('error');
            return `<div>${escapeHtmlForTest(message)}</div>`;
        },
        renderPending(message) {
            calls.push('pending');
            return `<div>${escapeHtmlForTest(message)}</div>`;
        },
        renderCleaned() {
            calls.push('cleaned');
            return '<section>Cleaned details</section>';
        },
        renderNoMetadata(_file, message) {
            calls.push('no-metadata');
            return `<section>${escapeHtmlForTest(message)}</section>`;
        },
        renderMetadataDetails(_file, metadata, title) {
            calls.push('details');
            return `<section>${escapeHtmlForTest(title)} ${escapeHtmlForTest(metadata.metadata_found[0]?.name || '')}</section>`;
        }
    };

    return { modalBody, calls, renderers };
}

function modalHarnessElement(initiallyHidden = false) {
    const classes = new Set(initiallyHidden ? ['hidden'] : []);
    return {
        classList: {
            add(name) {
                classes.add(name);
            },
            contains(name) {
                return classes.has(name);
            }
        }
    };
}

function lifecycleHarness(calls, openModalFileId = null) {
    return {
        revokeVisualProof(file) {
            calls.push(`revoke:${file.id}`);
        },
        postWorkerControl(message) {
            calls.push(message.id ? `worker:${message.type}:${message.id}` : `worker:${message.type}`);
        },
        clearDropFeedback() {
            calls.push('drop-feedback');
        },
        resetMetadataModal(options) {
            calls.push(`reset:${options.restoreFocus}`);
        },
        renderFileList() {
            calls.push('render-list');
        },
        fileRowElement(id) {
            return {
                remove() {
                    calls.push(`row:${id}`);
                }
            };
        },
        shouldResetMetadataModal(id) {
            return id === openModalFileId;
        },
        updateActions() {
            calls.push('actions');
        }
    };
}

function stripTags(value) {
    return value.replace(/<[^>]*>/g, '');
}

function escapeHtmlForTest(value) {
    return String(value)
        .replaceAll('&', '&amp;')
        .replaceAll('<', '&lt;')
        .replaceAll('>', '&gt;')
        .replaceAll('"', '&quot;')
        .replaceAll("'", '&#039;');
}

async function settleMicrotasks() {
    await Promise.resolve();
    await Promise.resolve();
}

function deferred() {
    let resolve;
    let reject;
    const promise = new Promise((promiseResolve, promiseReject) => {
        resolve = promiseResolve;
        reject = promiseReject;
    });
    return { promise, resolve, reject };
}

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

function bmffBox(type, payload) {
    const typeBytes = asciiBytes(type);
    const box = new Uint8Array(8 + payload.length);
    box.set(u32be(box.length), 0);
    box.set(typeBytes, 4);
    box.set(payload, 8);
    return box;
}

function u32be(value) {
    return Uint8Array.of(
        (value >>> 24) & 0xff,
        (value >>> 16) & 0xff,
        (value >>> 8) & 0xff,
        value & 0xff
    );
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

function makeMp3WithId3v1() {
    const audio = Uint8Array.of(0xff, 0xfb, 0x90, 0x64, 1, 2, 3, 4);
    const tag = new Uint8Array(128);
    tag.set(asciiBytes('TAG'), 0);
    tag.set(asciiBytes('Secret Song'), 3);
    tag.set(asciiBytes('Secret Artist'), 33);
    tag.set(asciiBytes('2026'), 93);
    return concatBytes([audio, tag]);
}

function makePdfWithInfo() {
    const objects = [
        '1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n',
        '2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n',
        '3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>\nendobj\n',
        '4 0 obj\n<< /Author (Alice Author) /Producer (Secret Producer) >>\nendobj\n'
    ];
    let body = '%PDF-1.4\n';
    const offsets = [];
    for (const object of objects) {
        offsets.push(body.length);
        body += object;
    }
    const xrefOffset = body.length;
    body += `xref\n0 ${objects.length + 1}\n`;
    body += '0000000000 65535 f \n';
    for (const offset of offsets) {
        body += `${String(offset).padStart(10, '0')} 00000 n \n`;
    }
    body += `trailer\n<< /Size ${objects.length + 1} /Root 1 0 R /Info 4 0 R >>\n`;
    body += `startxref\n${xrefOffset}\n%%EOF\n`;
    return asciiBytes(body);
}

function makeDocxWithCoreProperties() {
    return storedZip([
        ['[Content_Types].xml', asciiBytes('<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/></Types>')],
        ['word/document.xml', asciiBytes('<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Keep me</w:t></w:r></w:p></w:body></w:document>')],
        ['docProps/core.xml', asciiBytes('<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:creator>Secret Author</dc:creator><cp:lastModifiedBy>Secret Reviewer</cp:lastModifiedBy></cp:coreProperties>')]
    ]);
}

function storedZip(entries) {
    const localParts = [];
    const centralParts = [];
    let offset = 0;

    for (const [name, data] of entries) {
        const nameBytes = asciiBytes(name);
        const crc = crc32(data);
        const local = new Uint8Array(30 + nameBytes.length);
        writeU32(local, 0, 0x04034b50);
        writeU16(local, 4, 20);
        writeU16(local, 6, 0x0800);
        writeU16(local, 8, 0);
        writeU32(local, 14, crc);
        writeU32(local, 18, data.length);
        writeU32(local, 22, data.length);
        writeU16(local, 26, nameBytes.length);
        local.set(nameBytes, 30);
        localParts.push(local, data);

        const central = new Uint8Array(46 + nameBytes.length);
        writeU32(central, 0, 0x02014b50);
        writeU16(central, 4, 20);
        writeU16(central, 6, 20);
        writeU16(central, 8, 0x0800);
        writeU16(central, 10, 0);
        writeU32(central, 16, crc);
        writeU32(central, 20, data.length);
        writeU32(central, 24, data.length);
        writeU16(central, 28, nameBytes.length);
        writeU32(central, 42, offset);
        central.set(nameBytes, 46);
        centralParts.push(central);

        offset += local.length + data.length;
    }

    const centralOffset = offset;
    const centralSize = centralParts.reduce((sum, part) => sum + part.length, 0);
    const eocd = new Uint8Array(22);
    writeU32(eocd, 0, 0x06054b50);
    writeU16(eocd, 8, entries.length);
    writeU16(eocd, 10, entries.length);
    writeU32(eocd, 12, centralSize);
    writeU32(eocd, 16, centralOffset);

    return concatBytes([...localParts, ...centralParts, eocd]);
}

function writeU16(data, offset, value) {
    data[offset] = value & 0xff;
    data[offset + 1] = (value >>> 8) & 0xff;
}

function writeU32(data, offset, value) {
    data[offset] = value & 0xff;
    data[offset + 1] = (value >>> 8) & 0xff;
    data[offset + 2] = (value >>> 16) & 0xff;
    data[offset + 3] = (value >>> 24) & 0xff;
}

const CRC32_TABLE = (() => {
    const table = new Uint32Array(256);
    for (let i = 0; i < table.length; i++) {
        let value = i;
        for (let bit = 0; bit < 8; bit++) {
            value = value & 1 ? 0xedb88320 ^ (value >>> 1) : value >>> 1;
        }
        table[i] = value >>> 0;
    }
    return table;
})();

function crc32(data) {
    let crc = 0xffffffff;
    for (const byte of data) {
        crc = CRC32_TABLE[(crc ^ byte) & 0xff] ^ (crc >>> 8);
    }
    return (crc ^ 0xffffffff) >>> 0;
}
