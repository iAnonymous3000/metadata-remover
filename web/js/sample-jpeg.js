export async function createSampleJpegFile() {
    const jpeg = await createCanvasJpegBytes();
    if (jpeg.length < 2 || jpeg[0] !== 0xff || jpeg[1] !== 0xd8) {
        throw new Error('Unable to create a valid sample JPEG.');
    }

    const exifSegment = buildSampleExifSegment();
    const withExif = new Uint8Array(jpeg.length + exifSegment.length);
    withExif.set(jpeg.subarray(0, 2), 0);
    withExif.set(exifSegment, 2);
    withExif.set(jpeg.subarray(2), 2 + exifSegment.length);

    return new File([withExif], 'sample-location-photo.jpg', {
        type: 'image/jpeg',
        lastModified: Date.UTC(2024, 0, 1)
    });
}

async function createCanvasJpegBytes() {
    const canvas = document.createElement('canvas');
    canvas.width = 1;
    canvas.height = 1;
    const context = canvas.getContext('2d');
    if (!context || typeof canvas.toBlob !== 'function') {
        throw new Error('This browser cannot create the sample JPEG.');
    }

    context.fillStyle = '#0d1117';
    context.fillRect(0, 0, 1, 1);

    const blob = await new Promise((resolve) => {
        canvas.toBlob(resolve, 'image/jpeg', 0.9);
    });
    if (!blob) {
        throw new Error('This browser could not encode the sample JPEG.');
    }

    return new Uint8Array(await blob.arrayBuffer());
}

export function buildSampleExifSegment() {
    const exif = concatBytes([asciiBytes('Exif\0\0'), buildSampleTiff()]);
    if (exif.length + 2 > 0xffff) {
        throw new Error('Sample EXIF data is too large.');
    }

    const segment = new Uint8Array(4 + exif.length);
    segment[0] = 0xff;
    segment[1] = 0xe1;
    writeUint16BE(segment, 2, exif.length + 2);
    segment.set(exif, 4);
    return segment;
}

function buildSampleTiff() {
    const rootEntryCount = 6;
    const rootDataOffset = 8 + 2 + rootEntryCount * 12 + 4;
    const extraParts = [];
    let nextOffset = rootDataOffset;

    function reserve(bytes) {
        const offset = nextOffset;
        extraParts.push(bytes);
        nextOffset += bytes.length;
        return offset;
    }

    const makeBytes = asciiBytes('Metadata Remover Demo');
    const modelBytes = asciiBytes('SampleCam X1');
    const softwareBytes = asciiBytes('Browser sample file');
    const dateTimeBytes = asciiBytes('2024:01:01 12:00:00');
    const dateTimeOriginalBytes = asciiBytes('2023:12:31 23:59:58');

    const makeOffset = reserve(makeBytes);
    const modelOffset = reserve(modelBytes);
    const softwareOffset = reserve(softwareBytes);
    const dateTimeOffset = reserve(dateTimeBytes);
    const exifIfdOffset = nextOffset;
    reserve(buildSampleExifIfd(exifIfdOffset, dateTimeOriginalBytes));
    const gpsOffset = nextOffset;
    reserve(buildSampleGpsIfd(gpsOffset));

    const tiff = new Uint8Array(nextOffset);
    tiff[0] = 0x4d;
    tiff[1] = 0x4d;
    writeUint16BE(tiff, 2, 42);
    writeUint32BE(tiff, 4, 8);
    writeUint16BE(tiff, 8, rootEntryCount);

    let entryOffset = 10;
    writeIfdEntry(tiff, entryOffset, 0x010f, 2, makeBytes.length, makeOffset);
    entryOffset += 12;
    writeIfdEntry(tiff, entryOffset, 0x0110, 2, modelBytes.length, modelOffset);
    entryOffset += 12;
    writeIfdEntry(tiff, entryOffset, 0x0131, 2, softwareBytes.length, softwareOffset);
    entryOffset += 12;
    writeIfdEntry(tiff, entryOffset, 0x0132, 2, dateTimeBytes.length, dateTimeOffset);
    entryOffset += 12;
    writeIfdEntry(tiff, entryOffset, 0x8769, 4, 1, exifIfdOffset);
    entryOffset += 12;
    writeIfdEntry(tiff, entryOffset, 0x8825, 4, 1, gpsOffset);
    writeUint32BE(tiff, 10 + rootEntryCount * 12, 0);

    let dataOffset = rootDataOffset;
    for (const part of extraParts) {
        tiff.set(part, dataOffset);
        dataOffset += part.length;
    }

    return tiff;
}

function buildSampleExifIfd(exifIfdOffset, dateTimeOriginalBytes) {
    const exifEntryCount = 1;
    const valueDataOffset = 2 + exifEntryCount * 12 + 4;
    const dateTimeOriginalOffset = exifIfdOffset + valueDataOffset;
    const exif = new Uint8Array(valueDataOffset + dateTimeOriginalBytes.length);

    writeUint16BE(exif, 0, exifEntryCount);
    writeIfdEntry(exif, 2, 0x9003, 2, dateTimeOriginalBytes.length, dateTimeOriginalOffset);
    writeUint32BE(exif, 2 + exifEntryCount * 12, 0);
    exif.set(dateTimeOriginalBytes, valueDataOffset);

    return exif;
}

function buildSampleGpsIfd(gpsOffset) {
    const gpsEntryCount = 4;
    const valueDataOffset = 2 + gpsEntryCount * 12 + 4;
    const latValuesOffset = gpsOffset + valueDataOffset;
    const lonValuesOffset = latValuesOffset + 24;
    const gps = new Uint8Array(valueDataOffset + 48);

    writeUint16BE(gps, 0, gpsEntryCount);
    let entryOffset = 2;
    writeIfdEntry(gps, entryOffset, 0x0001, 2, 2, Uint8Array.of(0x4e, 0, 0, 0));
    entryOffset += 12;
    writeIfdEntry(gps, entryOffset, 0x0002, 5, 3, latValuesOffset);
    entryOffset += 12;
    writeIfdEntry(gps, entryOffset, 0x0003, 2, 2, Uint8Array.of(0x57, 0, 0, 0));
    entryOffset += 12;
    writeIfdEntry(gps, entryOffset, 0x0004, 5, 3, lonValuesOffset);
    writeUint32BE(gps, 2 + gpsEntryCount * 12, 0);

    let valueOffset = valueDataOffset;
    for (const [numerator, denominator] of [[37, 1], [46, 1], [741, 25]]) {
        writeRational(gps, valueOffset, numerator, denominator);
        valueOffset += 8;
    }
    for (const [numerator, denominator] of [[122, 1], [25, 1], [246, 25]]) {
        writeRational(gps, valueOffset, numerator, denominator);
        valueOffset += 8;
    }

    return gps;
}

function writeIfdEntry(target, offset, tag, format, count, value) {
    writeUint16BE(target, offset, tag);
    writeUint16BE(target, offset + 2, format);
    writeUint32BE(target, offset + 4, count);
    target.fill(0, offset + 8, offset + 12);
    if (value instanceof Uint8Array) {
        target.set(value.subarray(0, 4), offset + 8);
    } else {
        writeUint32BE(target, offset + 8, value);
    }
}

function writeRational(target, offset, numerator, denominator) {
    writeUint32BE(target, offset, numerator);
    writeUint32BE(target, offset + 4, denominator);
}

function writeUint16BE(target, offset, value) {
    target[offset] = (value >>> 8) & 0xff;
    target[offset + 1] = value & 0xff;
}

function writeUint32BE(target, offset, value) {
    target[offset] = (value >>> 24) & 0xff;
    target[offset + 1] = (value >>> 16) & 0xff;
    target[offset + 2] = (value >>> 8) & 0xff;
    target[offset + 3] = value & 0xff;
}

function asciiBytes(value) {
    return new TextEncoder().encode(value);
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
