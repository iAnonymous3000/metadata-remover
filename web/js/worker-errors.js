export function errorToString(error) {
    if (error instanceof Error && error.message) {
        return error.message;
    }
    if (error && typeof error === 'object') {
        if (typeof error.error === 'string') return error.error;
        if (typeof error.message === 'string') return error.message;
    }
    return String(error || 'Unknown error');
}

export function friendlyFileError(fileType, error) {
    const message = errorToString(error);
    const lower = message.toLowerCase();
    const typeLabel = fileTypeLabel(fileType);

    if (
        lower.includes('legacy binary office')
    ) {
        return message;
    }

    if (lower.includes('unsupported zip compression')) {
        return `${typeLabel} uses unsupported ZIP compression.`;
    }

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

export function fileError(fileType, error) {
    const friendly = new Error(friendlyFileError(fileType, error));
    friendly.fileType = fileType;
    return friendly;
}

export function isFatalWasmError(error) {
    if (typeof WebAssembly !== 'undefined' && error instanceof WebAssembly.RuntimeError) {
        return true;
    }

    const message = errorToString(error).toLowerCase();
    return message.includes('unreachable') || message.includes('memory access out of bounds');
}

function fileTypeLabel(fileType) {
    const labels = {
        jpeg: 'JPEG',
        png: 'PNG',
        webp: 'WebP',
        avif: 'AVIF',
        gif: 'GIF',
        heic: 'HEIC',
        heif: 'HEIF',
        tiff: 'TIFF',
        svg: 'SVG',
        mp4: 'MP4',
        mov: 'MOV',
        mp3: 'MP3',
        flac: 'FLAC',
        wav: 'WAV',
        ogg: 'OGG',
        pdf: 'PDF',
        docx: 'DOCX',
        xlsx: 'XLSX',
        pptx: 'PPTX',
        odt: 'ODT',
        ods: 'ODS',
        odp: 'ODP',
        epub: 'EPUB',
        'office-legacy': 'legacy Office'
    };
    return labels[fileType] || 'file';
}
