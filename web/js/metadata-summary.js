export function metadataSummary(entries) {
    if (entries.length === 0) {
        return 'No removable metadata';
    }

    const labels = [];
    const seen = new Set();
    const summaries = entries
        .map(metadataEntrySummary)
        .filter(Boolean)
        .sort((a, b) => a.priority - b.priority);

    for (const summary of summaries) {
        const key = summary.label.toLowerCase();
        if (seen.has(key)) continue;
        seen.add(key);
        labels.push(summary.label);
    }

    if (labels.length === 0) {
        const entryText = entries.length === 1 ? 'entry' : 'entries';
        return `${entries.length} metadata ${entryText}`;
    }

    const visible = labels.slice(0, 3);
    const hidden = labels.length - visible.length;
    if (hidden > 0) {
        visible.push(`${hidden} more`);
    }

    return visible.join(' / ');
}

export function groupMetadataEntriesByCategory(entries) {
    const grouped = new Map();
    for (const entry of entries) {
        const category = String(entry.category);
        const categoryEntries = grouped.get(category);
        if (categoryEntries) {
            categoryEntries.push(entry);
        } else {
            grouped.set(category, [entry]);
        }
    }
    return grouped;
}

function metadataEntrySummary(entry) {
    const category = String(entry.category || '');
    const name = String(entry.name || '');
    const value = String(entry.value || '');
    const haystack = `${category} ${name}`.toLowerCase();

    if (haystack.includes('gps') || haystack.includes('location')) {
        return { priority: 1, label: value ? `GPS location: ${value}` : 'GPS location' };
    }
    if (haystack.includes('content credentials') || haystack.includes('c2pa') || haystack.includes('jumb')) {
        return { priority: 2, label: 'Content Credentials' };
    }
    if (name === 'Camera Model') {
        return { priority: 3, label: value ? `camera model: ${value}` : 'camera model' };
    }
    if (name === 'DateTimeOriginal') {
        return { priority: 4, label: value ? `timestamp: ${value}` : 'timestamp' };
    }
    if (haystack.includes('date') || haystack.includes('time')) {
        return { priority: 5, label: value ? `timestamp: ${value}` : 'timestamp' };
    }
    if (name === 'Camera Make') {
        return { priority: 6, label: value ? `camera make: ${value}` : 'camera make' };
    }
    if (haystack.includes('comment') || haystack.includes('review')) {
        return { priority: 7, label: 'comments/review data' };
    }
    if (haystack.includes('author') || haystack.includes('creator') || haystack.includes('producer')) {
        return { priority: 8, label: 'author/creator data' };
    }
    if (haystack.includes('embedded artwork') || haystack.includes('attached picture')) {
        return { priority: 9, label: 'embedded audio artwork' };
    }
    if (haystack.includes('apev2') || haystack.includes('lyrics3') || haystack.includes('unsupported audio metadata')) {
        return { priority: 10, label: 'audio tail metadata' };
    }
    if (haystack.includes('audio metadata') || haystack.includes('id3')) {
        return { priority: 11, label: 'ID3/audio metadata' };
    }
    if (haystack.includes('embedded image metadata')) {
        return { priority: 12, label: 'embedded image metadata' };
    }
    if (haystack.includes('epub metadata')) {
        return { priority: 13, label: 'EPUB package metadata' };
    }
    if (haystack.includes('limited verification')) {
        return { priority: 30, label: 'limited verification' };
    }
    if (haystack.includes('exif')) {
        return { priority: 20, label: 'EXIF data' };
    }
    if (haystack.includes('xmp')) {
        return { priority: 21, label: 'XMP data' };
    }
    if (haystack.includes('jfif') || haystack.includes('app')) {
        return { priority: 22, label: 'JPEG app metadata' };
    }
    if (haystack.includes('trailing')) {
        return { priority: 23, label: 'trailing data' };
    }

    return { priority: 50, label: name || category || 'metadata' };
}
