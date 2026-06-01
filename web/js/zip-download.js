const ZIP32_MAX = 0xffffffff;
const ZIP16_MAX = 0xffff;
const CRC_CHUNK_BYTES = 4 * 1024 * 1024;

// DOS timestamp: 1980-01-01 00:00:00, avoiding fresh metadata in ZIP entries.
const ZIP_TIME = 0;
const ZIP_DATE = 0x0021;

const CRC32_TABLE = Array.from({ length: 256 }, (_, index) => {
    let value = index;
    for (let bit = 0; bit < 8; bit++) {
        value = value & 1 ? 0xedb88320 ^ (value >>> 1) : value >>> 1;
    }
    return value >>> 0;
});

export async function createZip(entries, {
    onProgress = null,
    shouldCancel = null
} = {}) {
    if (entries.length > ZIP16_MAX) {
        throw new Error('Too many files for this ZIP download.');
    }

    const totalBytes = entries.reduce((sum, entry) => sum + entryDataSize(entry.data), 0);
    let processedBytes = 0;

    const reportProgress = (completedEntries) => {
        onProgress?.({
            completedEntries,
            totalEntries: entries.length,
            processedBytes,
            totalBytes
        });
    };

    const localParts = [];
    const centralParts = [];
    let offset = 0;
    reportProgress(0);

    for (const [index, entry] of entries.entries()) {
        throwIfCanceled(shouldCancel);

        const nameBytes = new TextEncoder().encode(entry.name);
        if (nameBytes.byteLength > ZIP16_MAX) {
            throw new Error('A cleaned filename is too long for ZIP download.');
        }

        const data = await entryDataBytes(entry.data);
        throwIfCanceled(shouldCancel);
        if (data.byteLength > ZIP32_MAX || offset > ZIP32_MAX) {
            throw new Error('Batch is too large for ZIP download.');
        }

        const crc = await crc32(data, {
            shouldCancel,
            onChunk(chunkBytes) {
                processedBytes += chunkBytes;
                reportProgress(index);
            }
        });
        const localHeader = zipHeader(30, 0x04034b50);
        localHeader.setUint16(4, 20, true);
        localHeader.setUint16(6, 0x0800, true);
        localHeader.setUint16(10, ZIP_TIME, true);
        localHeader.setUint16(12, ZIP_DATE, true);
        localHeader.setUint32(14, crc, true);
        localHeader.setUint32(18, data.byteLength, true);
        localHeader.setUint32(22, data.byteLength, true);
        localHeader.setUint16(26, nameBytes.byteLength, true);
        localParts.push(localHeader, nameBytes, data);

        const centralHeader = zipHeader(46, 0x02014b50);
        centralHeader.setUint16(4, 20, true);
        centralHeader.setUint16(6, 20, true);
        centralHeader.setUint16(8, 0x0800, true);
        centralHeader.setUint16(12, ZIP_TIME, true);
        centralHeader.setUint16(14, ZIP_DATE, true);
        centralHeader.setUint32(16, crc, true);
        centralHeader.setUint32(20, data.byteLength, true);
        centralHeader.setUint32(24, data.byteLength, true);
        centralHeader.setUint16(28, nameBytes.byteLength, true);
        centralHeader.setUint32(42, offset, true);
        centralParts.push(centralHeader, nameBytes);

        offset += localHeader.byteLength + nameBytes.byteLength + data.byteLength;
        reportProgress(index + 1);
    }

    const centralOffset = offset;
    const centralSize = centralParts.reduce((size, part) => size + part.byteLength, 0);
    if (centralSize > ZIP32_MAX || centralOffset > ZIP32_MAX) {
        throw new Error('Batch is too large for ZIP download.');
    }

    const end = zipHeader(22, 0x06054b50);
    end.setUint16(8, entries.length, true);
    end.setUint16(10, entries.length, true);
    end.setUint32(12, centralSize, true);
    end.setUint32(16, centralOffset, true);

    return new Blob([...localParts, ...centralParts, end], { type: 'application/zip' });
}

async function entryDataBytes(data) {
    if (data instanceof Uint8Array) {
        return data;
    }
    if (data instanceof ArrayBuffer) {
        return new Uint8Array(data);
    }
    if (ArrayBuffer.isView(data)) {
        return new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
    }
    if (data && typeof data.arrayBuffer === 'function') {
        return new Uint8Array(await data.arrayBuffer());
    }
    throw new Error('ZIP entry data is not readable.');
}

function entryDataSize(data) {
    if (data instanceof Uint8Array || data instanceof ArrayBuffer || ArrayBuffer.isView(data)) {
        return data.byteLength;
    }
    if (Number.isFinite(data?.size)) {
        return data.size;
    }
    return 0;
}

function zipHeader(size, signature) {
    const bytes = new Uint8Array(size);
    const view = new DataView(bytes.buffer);
    view.setUint32(0, signature, true);
    return view;
}

async function crc32(data, {
    onChunk = null,
    shouldCancel = null
} = {}) {
    let crc = 0xffffffff;
    for (let offset = 0; offset < data.byteLength; offset += CRC_CHUNK_BYTES) {
        throwIfCanceled(shouldCancel);
        const end = Math.min(data.byteLength, offset + CRC_CHUNK_BYTES);
        for (let index = offset; index < end; index++) {
            crc = CRC32_TABLE[(crc ^ data[index]) & 0xff] ^ (crc >>> 8);
        }
        onChunk?.(end - offset);
        await yieldToWorker();
    }
    return (crc ^ 0xffffffff) >>> 0;
}

function throwIfCanceled(shouldCancel) {
    if (shouldCancel?.()) {
        throw new Error('ZIP download was canceled.');
    }
}

function yieldToWorker() {
    return new Promise((resolve) => setTimeout(resolve, 0));
}
