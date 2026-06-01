export const VISUAL_THUMBNAIL_MAX_WIDTH = 360;
export const VISUAL_THUMBNAIL_MAX_HEIGHT = 260;
export const VISUAL_PIXEL_COMPARE_MAX = 4_000_000;
export const VISUAL_PIXEL_BUFFER_MULTIPLIER = 6;
export const VISUAL_DECODE_EXPANSION_FACTOR = 4;
export const VISUAL_DECODED_IMAGE_BUFFER_MULTIPLIER = 2;
export const VISUAL_UNKNOWN_DIMENSION_THRESHOLD_BYTES = 1 * 1024 * 1024;
export const VISUAL_UNKNOWN_DIMENSION_TRANSIENT_BYTES = 512 * 1024 * 1024;
const DEFAULT_DIMENSION_READ_BYTES = 2 * 1024 * 1024;
const ACTIVE_SOURCE_COPY_STATUSES = new Set(['loading', 'pending', 'processing']);
const JPEG_START_OF_FRAME_MARKERS = new Set([
    0xc0, 0xc1, 0xc2, 0xc3, 0xc5, 0xc6, 0xc7, 0xc9, 0xca, 0xcb, 0xcd, 0xce, 0xcf
]);
const BMFF_CONTAINER_BOXES = new Set([
    'meta', 'iprp', 'ipco', 'moov', 'trak', 'mdia', 'minf', 'stbl'
]);

export const VISUAL_PROOF_BUDGET_OPTIONS = Object.freeze({
    thumbnailMaxWidth: VISUAL_THUMBNAIL_MAX_WIDTH,
    thumbnailMaxHeight: VISUAL_THUMBNAIL_MAX_HEIGHT,
    pixelCompareMax: VISUAL_PIXEL_COMPARE_MAX,
    pixelBufferMultiplier: VISUAL_PIXEL_BUFFER_MULTIPLIER,
    decodeExpansionFactor: VISUAL_DECODE_EXPANSION_FACTOR,
    decodedImageBufferMultiplier: VISUAL_DECODED_IMAGE_BUFFER_MULTIPLIER,
    unknownDimensionThresholdBytes: VISUAL_UNKNOWN_DIMENSION_THRESHOLD_BYTES,
    unknownDimensionTransientBytes: VISUAL_UNKNOWN_DIMENSION_TRANSIENT_BYTES
});

export function retainedFileBytesForBudget(file, visualProofOptions = {}) {
    return sourceFileBytesForBudget(file.sourceFile)
        + activeWorkerSourceCopyBytesForBudget(file)
        + sourceFileBytesForBudget(file.visualSourceFile)
        + (file.cleanedData?.byteLength ?? 0)
        + visualProofBytesForBudget(file.visualProof, visualProofOptions);
}

export function sourceFileBytesForBudget(file) {
    return file?.size ?? 0;
}

export function activeWorkerSourceCopyBytesForBudget(file) {
    if (!file?.sourceFile || !ACTIVE_SOURCE_COPY_STATUSES.has(file.status)) {
        return 0;
    }
    return sourceFileBytesForBudget(file.sourceFile);
}

export function visualProofBytesForBudget(proof, {
    thumbnailMaxWidth = VISUAL_THUMBNAIL_MAX_WIDTH,
    thumbnailMaxHeight = VISUAL_THUMBNAIL_MAX_HEIGHT
} = {}) {
    if (!proof) return 0;

    let total = 0;
    for (const snapshot of [proof.originalSnapshot, proof.cleanedSnapshot]) {
        if (!snapshot?.width || !snapshot?.height) continue;
        const scale = Math.min(
            1,
            thumbnailMaxWidth / snapshot.width,
            thumbnailMaxHeight / snapshot.height
        );
        total += Math.max(1, Math.round(snapshot.width * scale))
            * Math.max(1, Math.round(snapshot.height * scale))
            * 4;
    }
    return total;
}

export function visualVerificationTransientBytesForBudget(file, {
    pixelCompareMax = VISUAL_PIXEL_COMPARE_MAX,
    pixelBufferMultiplier = VISUAL_PIXEL_BUFFER_MULTIPLIER,
    decodeExpansionFactor = VISUAL_DECODE_EXPANSION_FACTOR,
    decodedImageBufferMultiplier = VISUAL_DECODED_IMAGE_BUFFER_MULTIPLIER,
    unknownDimensionThresholdBytes = VISUAL_UNKNOWN_DIMENSION_THRESHOLD_BYTES,
    unknownDimensionTransientBytes = VISUAL_UNKNOWN_DIMENSION_TRANSIENT_BYTES
} = {}) {
    const cleanedBytes = file?.cleanedData?.byteLength ?? 0;
    const expansionFactor = Math.max(1, decodeExpansionFactor);

    if (file?.type === 'svg') {
        return Math.ceil(cleanedBytes * expansionFactor);
    }

    const visualSourceBytes = sourceFileBytesForBudget(file?.visualSourceFile || file?.sourceFile);
    if (visualSourceBytes === 0 && cleanedBytes === 0) {
        return 0;
    }

    const compareHeadroom = Math.max(0, pixelCompareMax)
        * 4
        * Math.max(1, pixelBufferMultiplier);
    const decodeHeadroom = (visualSourceBytes + cleanedBytes) * expansionFactor;
    const decodedPixels = Math.max(
        pixelCountForDimensions(file?.visualSourceDimensions),
        pixelCountForDimensions(file?.cleanedVisualDimensions)
    );
    const decodedImageHeadroom = decodedPixels > 0
        ? bytesForPixels(decodedPixels, decodedImageBufferMultiplier)
        : 0;
    const actualCompareHeadroom = decodedPixels > 0
        ? bytesForPixels(Math.min(decodedPixels, Math.max(0, pixelCompareMax)), pixelBufferMultiplier)
        : compareHeadroom;
    const unknownDimensionHeadroom = (
        decodedPixels === 0
        && visualSourceBytes + cleanedBytes >= Math.max(0, unknownDimensionThresholdBytes)
    )
        ? Math.max(0, unknownDimensionTransientBytes)
        : 0;

    return Math.ceil(Math.max(
        actualCompareHeadroom,
        decodeHeadroom,
        decodedImageHeadroom,
        unknownDimensionHeadroom
    ));
}

export async function imageDimensionsFromBlob(fileType, blob, {
    byteLimit = DEFAULT_DIMENSION_READ_BYTES
} = {}) {
    if (!blob || typeof blob.slice !== 'function' || typeof blob.arrayBuffer !== 'function') {
        return null;
    }

    const limit = Math.max(0, Math.min(blob.size ?? byteLimit, byteLimit));
    if (limit === 0) {
        return null;
    }

    const buffer = await blob.slice(0, limit).arrayBuffer();
    return imageDimensionsFromBytes(fileType, new Uint8Array(buffer));
}

export function imageDimensionsFromBytes(fileType, data) {
    const bytes = bytesView(data);
    if (!bytes || bytes.length === 0) {
        return null;
    }

    switch (fileType) {
        case 'jpeg':
            return jpegDimensions(bytes);
        case 'png':
            return pngDimensions(bytes);
        case 'gif':
            return gifDimensions(bytes);
        case 'webp':
            return webpDimensions(bytes);
        case 'avif':
            return bmffImageDimensions(bytes);
        default:
            return null;
    }
}

function bytesView(data) {
    if (data instanceof Uint8Array) {
        return data;
    }
    if (ArrayBuffer.isView(data)) {
        return new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
    }
    if (data instanceof ArrayBuffer) {
        return new Uint8Array(data);
    }
    return null;
}

function pixelCountForDimensions(dimensions) {
    const width = Number(dimensions?.width);
    const height = Number(dimensions?.height);
    if (!Number.isFinite(width) || !Number.isFinite(height) || width <= 0 || height <= 0) {
        return 0;
    }
    return Math.min(Number.MAX_SAFE_INTEGER, width * height);
}

function bytesForPixels(pixelCount, multiplier) {
    return Math.min(
        Number.MAX_SAFE_INTEGER,
        pixelCount * 4 * Math.max(1, multiplier)
    );
}

function dimensions(width, height) {
    if (!Number.isFinite(width) || !Number.isFinite(height) || width <= 0 || height <= 0) {
        return null;
    }
    return { width, height };
}

function pngDimensions(bytes) {
    if (
        bytes.length < 24
        || bytes[0] !== 0x89
        || bytes[1] !== 0x50
        || bytes[2] !== 0x4e
        || bytes[3] !== 0x47
        || bytes[4] !== 0x0d
        || bytes[5] !== 0x0a
        || bytes[6] !== 0x1a
        || bytes[7] !== 0x0a
        || ascii(bytes, 12, 16) !== 'IHDR'
    ) {
        return null;
    }

    return dimensions(readU32BE(bytes, 16), readU32BE(bytes, 20));
}

function gifDimensions(bytes) {
    if (
        bytes.length < 10
        || (ascii(bytes, 0, 6) !== 'GIF87a' && ascii(bytes, 0, 6) !== 'GIF89a')
    ) {
        return null;
    }

    return dimensions(readU16LE(bytes, 6), readU16LE(bytes, 8));
}

function jpegDimensions(bytes) {
    if (bytes.length < 4 || bytes[0] !== 0xff || bytes[1] !== 0xd8) {
        return null;
    }

    let offset = 2;
    while (offset + 3 < bytes.length) {
        while (offset < bytes.length && bytes[offset] === 0xff) {
            offset += 1;
        }
        if (offset >= bytes.length) {
            return null;
        }

        const marker = bytes[offset];
        offset += 1;
        if (marker === 0xd9 || marker === 0xda) {
            return null;
        }
        if (marker === 0x01 || (marker >= 0xd0 && marker <= 0xd7)) {
            continue;
        }

        if (offset + 2 > bytes.length) {
            return null;
        }
        const segmentLength = readU16BE(bytes, offset);
        if (segmentLength < 2 || offset + segmentLength > bytes.length) {
            return null;
        }

        if (JPEG_START_OF_FRAME_MARKERS.has(marker) && segmentLength >= 7) {
            return dimensions(readU16BE(bytes, offset + 5), readU16BE(bytes, offset + 3));
        }
        offset += segmentLength;
    }

    return null;
}

function webpDimensions(bytes) {
    if (
        bytes.length < 30
        || ascii(bytes, 0, 4) !== 'RIFF'
        || ascii(bytes, 8, 12) !== 'WEBP'
    ) {
        return null;
    }

    let offset = 12;
    while (offset + 8 <= bytes.length) {
        const chunkType = ascii(bytes, offset, offset + 4);
        const chunkSize = readU32LE(bytes, offset + 4);
        const chunkStart = offset + 8;
        const chunkEnd = chunkStart + chunkSize;
        if (chunkEnd > bytes.length) {
            return null;
        }

        if (chunkType === 'VP8X' && chunkSize >= 10) {
            const width = 1 + readU24LE(bytes, chunkStart + 4);
            const height = 1 + readU24LE(bytes, chunkStart + 7);
            return dimensions(width, height);
        }
        if (chunkType === 'VP8 ' && chunkSize >= 10) {
            if (
                bytes[chunkStart + 3] === 0x9d
                && bytes[chunkStart + 4] === 0x01
                && bytes[chunkStart + 5] === 0x2a
            ) {
                return dimensions(
                    readU16LE(bytes, chunkStart + 6) & 0x3fff,
                    readU16LE(bytes, chunkStart + 8) & 0x3fff
                );
            }
        }
        if (chunkType === 'VP8L' && chunkSize >= 5 && bytes[chunkStart] === 0x2f) {
            const packed = readU32LE(bytes, chunkStart + 1);
            return dimensions((packed & 0x3fff) + 1, ((packed >>> 14) & 0x3fff) + 1);
        }

        offset = chunkEnd + (chunkSize % 2);
    }

    return null;
}

function bmffImageDimensions(bytes, start = 0, end = bytes.length, depth = 0) {
    if (depth > 8) {
        return null;
    }

    let offset = start;
    while (offset + 8 <= end) {
        let size = readU32BE(bytes, offset);
        const type = ascii(bytes, offset + 4, offset + 8);
        let headerSize = 8;
        if (size === 1) {
            if (offset + 16 > end) {
                return null;
            }
            size = readU64BE(bytes, offset + 8);
            headerSize = 16;
        } else if (size === 0) {
            size = end - offset;
        }

        if (!size || size < headerSize || offset + size > end) {
            return null;
        }

        const bodyStart = offset + headerSize;
        const boxEnd = offset + size;
        if (type === 'ispe' && bodyStart + 12 <= boxEnd) {
            return dimensions(readU32BE(bytes, bodyStart + 4), readU32BE(bytes, bodyStart + 8));
        }

        if (BMFF_CONTAINER_BOXES.has(type)) {
            const childStart = type === 'meta' ? bodyStart + 4 : bodyStart;
            const found = bmffImageDimensions(bytes, childStart, boxEnd, depth + 1);
            if (found) {
                return found;
            }
        }

        offset = boxEnd;
    }

    return null;
}

function ascii(bytes, start, end) {
    if (start < 0 || end > bytes.length || start > end) {
        return '';
    }
    let value = '';
    for (let offset = start; offset < end; offset += 1) {
        value += String.fromCharCode(bytes[offset]);
    }
    return value;
}

function readU16BE(bytes, offset) {
    return (bytes[offset] << 8) | bytes[offset + 1];
}

function readU16LE(bytes, offset) {
    return bytes[offset] | (bytes[offset + 1] << 8);
}

function readU24LE(bytes, offset) {
    return bytes[offset] | (bytes[offset + 1] << 8) | (bytes[offset + 2] << 16);
}

function readU32BE(bytes, offset) {
    return (
        bytes[offset] * 0x1000000
        + ((bytes[offset + 1] << 16) | (bytes[offset + 2] << 8) | bytes[offset + 3])
    ) >>> 0;
}

function readU32LE(bytes, offset) {
    return (
        bytes[offset]
        + bytes[offset + 1] * 0x100
        + bytes[offset + 2] * 0x10000
        + bytes[offset + 3] * 0x1000000
    ) >>> 0;
}

function readU64BE(bytes, offset) {
    const high = readU32BE(bytes, offset);
    const low = readU32BE(bytes, offset + 4);
    if (high > 0x1fffff) {
        return 0;
    }
    return high * 0x100000000 + low;
}
