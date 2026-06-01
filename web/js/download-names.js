const ZIP_ENTRY_FALLBACK_NAME = 'file';
const WINDOWS_RESERVED_BASENAMES = /^(con|prn|aux|nul|com[1-9]|lpt[1-9])$/i;

export function sanitizeZipEntryName(name) {
    const leafName = String(name || '')
        .replaceAll('\\', '/')
        .split('/')
        .filter(Boolean)
        .pop() || ZIP_ENTRY_FALLBACK_NAME;
    const cleaned = leafName
        .replace(/[\x00-\x1f\x7f]/g, '')
        .trim()
        .replace(/^\.+/, '')
        .replace(/\.+$/, '');
    return windowsSafeName(cleaned || ZIP_ENTRY_FALLBACK_NAME);
}

export function cleanedFilename(name) {
    const [base, ext] = splitFilename(sanitizeZipEntryName(name || ZIP_ENTRY_FALLBACK_NAME));
    const cleanBase = base || ZIP_ENTRY_FALLBACK_NAME;
    return ext ? `${cleanBase}_clean.${ext}` : `${cleanBase}_clean`;
}

export function uniqueFilename(name, usedNames) {
    const [base, ext] = splitFilename(sanitizeZipEntryName(name));
    let candidate = ext ? `${base}.${ext}` : base;
    let index = 2;

    while (usedNames.has(candidate.toLowerCase())) {
        candidate = ext ? `${base}-${index}.${ext}` : `${base}-${index}`;
        index += 1;
    }

    usedNames.add(candidate.toLowerCase());
    return candidate;
}

function splitFilename(name) {
    const idx = name.lastIndexOf('.');
    return idx > 0 ? [name.slice(0, idx), name.slice(idx + 1)] : [name, ''];
}

function windowsSafeName(name) {
    const idx = name.lastIndexOf('.');
    const [base, ext] = idx > 0 ? [name.slice(0, idx), name.slice(idx + 1)] : [name, ''];
    const safeBase = WINDOWS_RESERVED_BASENAMES.test(base) ? `${base}_file` : base;
    return ext ? `${safeBase}.${ext}` : safeBase;
}
