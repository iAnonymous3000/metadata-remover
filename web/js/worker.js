const scriptUrl = import.meta.url;
const basePath = scriptUrl.substring(0, scriptUrl.lastIndexOf('/js/'));
const WASM_PATH = `${basePath}/wasm/metadata_remover.js`;

let wasmModule = null;
const fileData = new Map();
const ready = initWasm();

self.addEventListener('message', (event) => {
    handleMessage(event.data);
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

async function handleMessage(message) {
    if (message.type === 'clear') {
        fileData.clear();
        return;
    }

    if (message.type === 'forget') {
        fileData.delete(message.id);
        return;
    }

    try {
        await ready;

        if (message.type === 'analyze') {
            await analyzeFile(message);
        } else if (message.type === 'process') {
            await processFile(message);
        }
    } catch (e) {
        const fatal = isFatalWasmError(e);
        self.postMessage({
            type: 'failed',
            requestId: message.requestId,
            id: message.id,
            fatal,
            fileType: e.fileType,
            error: errorToString(e)
        });

        if (fatal) {
            fileData.clear();
            setTimeout(() => self.close(), 0);
        }
    }
}

async function analyzeFile(message) {
    const buffer = await message.file.arrayBuffer();
    const data = new Uint8Array(buffer);
    const fileType = wasmModule.detect_file_type(data);

    if (fileType === 'unknown') {
        throw new Error('Unsupported or invalid file type.');
    }

    try {
        wasmModule.validate_file(data);
    } catch (e) {
        throw fileError(fileType, e);
    }

    const metadata = wasmModule.extract_metadata(data);
    fileData.set(message.id, data);

    self.postMessage({
        type: 'analyzed',
        requestId: message.requestId,
        id: message.id,
        fileType,
        metadata
    });
}

async function processFile(message) {
    let data = fileData.get(message.id);
    if (!data && message.file) {
        const buffer = await message.file.arrayBuffer();
        data = new Uint8Array(buffer);
        fileData.set(message.id, data);
    }

    if (!data) {
        throw new Error('File data is no longer available.');
    }

    const fileType = wasmModule.detect_file_type(data);
    let cleaned;
    try {
        cleaned = wasmModule.remove_metadata(data);
    } catch (e) {
        throw fileError(fileType, e);
    }

    const verification = wasmModule.extract_metadata(cleaned);
    const cleanedBuffer = cleaned.buffer.slice(cleaned.byteOffset, cleaned.byteOffset + cleaned.byteLength);

    self.postMessage({
        type: 'processed',
        requestId: message.requestId,
        id: message.id,
        verification,
        cleanedBuffer
    }, [cleanedBuffer]);
}

function errorToString(error) {
    if (error instanceof Error && error.message) {
        return error.message;
    }
    return String(error || 'Unknown error');
}

function friendlyFileError(fileType, error) {
    const message = errorToString(error);
    const lower = message.toLowerCase();
    const typeLabel = fileTypeLabel(fileType);

    if (
        lower.includes('invalid segment length')
        || lower.includes('invalid scan segment length')
        || lower.includes('truncated')
        || lower.includes('file too small')
        || lower.includes('not a valid')
    ) {
        return `This file appears to be corrupt or is not a valid ${typeLabel}.`;
    }

    if (lower.includes('unsupported')) {
        return 'This file type is not supported.';
    }

    return message;
}

function fileError(fileType, error) {
    const friendly = new Error(friendlyFileError(fileType, error));
    friendly.fileType = fileType;
    return friendly;
}

function fileTypeLabel(fileType) {
    const labels = {
        jpeg: 'JPEG',
        png: 'PNG',
        webp: 'WebP',
        gif: 'GIF',
        pdf: 'PDF'
    };
    return labels[fileType] || 'file';
}

function isFatalWasmError(error) {
    if (typeof WebAssembly !== 'undefined' && error instanceof WebAssembly.RuntimeError) {
        return true;
    }

    const message = errorToString(error).toLowerCase();
    return message.includes('unreachable') || message.includes('memory access out of bounds');
}
