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
            processFile(message);
        }
    } catch (e) {
        self.postMessage({
            type: 'failed',
            requestId: message.requestId,
            id: message.id,
            error: errorToString(e)
        });
    }
}

async function analyzeFile(message) {
    const buffer = await message.file.arrayBuffer();
    const data = new Uint8Array(buffer);
    const fileType = wasmModule.detect_file_type(data);

    if (fileType === 'unknown') {
        throw new Error('Unsupported or invalid file type.');
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

function processFile(message) {
    const data = fileData.get(message.id);
    if (!data) {
        throw new Error('File data is no longer available.');
    }

    const cleaned = wasmModule.remove_metadata(data);
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
