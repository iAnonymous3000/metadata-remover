import {
    beginFileMessage,
    clearStoredFileData,
    createWorkerFileState,
    deleteStoredFileData,
    finishFileMessage,
    forgetFailedRequestData,
    getStoredFileData,
    handleWorkerControlMessage,
    shouldSkipFileMessage,
    storeFileData
} from './worker-state.js';
import { errorToString, fileError, isFatalWasmError } from './worker-errors.js';
import { createZip } from './zip-download.js';

const scriptUrl = import.meta.url;
const basePath = scriptUrl.substring(0, scriptUrl.lastIndexOf('/js/'));
const WASM_PATH = `${basePath}/wasm/metadata_remover.js`;

let wasmModule = null;
const fileState = createWorkerFileState();
const canceledZipRequests = new Set();
const ready = initWasm();
let messageQueue = Promise.resolve();

self.addEventListener('message', (event) => {
    const message = event.data;
    if (message?.type === 'cancel-zip' && message.requestId) {
        canceledZipRequests.add(message.requestId);
        return;
    }

    if (handleWorkerControlMessage(fileState, message)) {
        return;
    }

    const generation = beginFileMessage(fileState, message);
    messageQueue = messageQueue.then(
        () => runQueuedMessage(message, generation),
        () => runQueuedMessage(message, generation)
    );
});

async function initWasm() {
    try {
        const wasm = await import(WASM_PATH);
        await wasm.default();
        wasmModule = wasm;
        self.postMessage({ type: 'ready', version: wasm.version?.() });
    } catch (e) {
        self.postMessage({ type: 'fatal', error: errorToString(e) });
        throw e;
    }
}

async function runQueuedMessage(message, generation) {
    try {
        await handleMessage(message, generation);
    } finally {
        finishFileMessage(fileState, message);
    }
}

async function handleMessage(message, generation) {
    try {
        await ready;
        if (shouldSkipFileMessage(fileState, message, generation)) {
            return;
        }

        if (message.type === 'analyze') {
            await analyzeFile(message, generation);
        } else if (message.type === 'process') {
            await processFile(message, generation);
        } else if (message.type === 'zip') {
            await createZipDownload(message);
        }
    } catch (e) {
        const fatal = isFatalWasmError(e);
        forgetFailedRequestData(fileState, message);
        self.postMessage({
            type: 'failed',
            requestId: message.requestId,
            id: message.id,
            fatal,
            fileType: e.fileType,
            error: errorToString(e)
        });

        if (fatal) {
            clearStoredFileData(fileState);
            setTimeout(() => self.close(), 0);
        }
    } finally {
        if (message.type === 'zip') {
            canceledZipRequests.delete(message.requestId);
        }
    }
}

async function analyzeFile(message, generation) {
    const buffer = await message.file.arrayBuffer();
    const data = new Uint8Array(buffer);
    if (shouldSkipFileMessage(fileState, message, generation)) {
        return;
    }

    let result;
    try {
        result = wasmModule.analyze_file(data);
    } catch (e) {
        throw fileError(e?.fileType || 'unknown', e);
    }

    if (!storeFileData(fileState, message.id, data, generation)) {
        return;
    }

    self.postMessage({
        type: 'analyzed',
        requestId: message.requestId,
        id: message.id,
        fileType: result.fileType,
        metadata: result.metadata
    });
}

async function processFile(message, generation) {
    let data = getStoredFileData(fileState, message.id);
    if (!data && message.file) {
        const buffer = await message.file.arrayBuffer();
        data = new Uint8Array(buffer);
        if (!storeFileData(fileState, message.id, data, generation)) {
            return;
        }
    }

    if (shouldSkipFileMessage(fileState, message, generation)) {
        return;
    }

    if (!data) {
        throw new Error('File data is no longer available.');
    }

    let result;
    try {
        result = wasmModule.process_file(data);
    } catch (e) {
        throw fileError(e?.fileType || wasmModule.detect_file_type(data), e);
    }

    const { cleaned, verification } = result;
    const cleanedBuffer = cleaned.buffer.slice(cleaned.byteOffset, cleaned.byteOffset + cleaned.byteLength);

    deleteStoredFileData(fileState, message.id);
    self.postMessage({
        type: 'processed',
        requestId: message.requestId,
        id: message.id,
        verification,
        cleanedBuffer
    }, [cleanedBuffer]);
}

async function createZipDownload(message) {
    const zip = await createZip(message.entries || [], {
        shouldCancel: () => canceledZipRequests.has(message.requestId),
        onProgress(progress) {
            self.postMessage({
                type: 'zip-progress',
                requestId: message.requestId,
                ...progress
            });
        }
    });

    self.postMessage({
        type: 'zip-created',
        requestId: message.requestId,
        blob: zip
    });
}
