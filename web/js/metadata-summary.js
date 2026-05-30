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

function metadataEntrySummary(entry) {
    const category = String(entry.category || '');
    const name = String(entry.name || '');
    const value = String(entry.value || '');
    const haystack = `${category} ${name}`.toLowerCase();

    if (haystack.includes('gps') || haystack.includes('location')) {
        return { priority: 1, label: value ? `GPS location: ${value}` : 'GPS location' };
    }
    if (name === 'Camera Model') {
        return { priority: 2, label: value ? `camera model: ${value}` : 'camera model' };
    }
    if (haystack.includes('date') || haystack.includes('time')) {
        return { priority: 3, label: value ? `timestamp: ${value}` : 'timestamp' };
    }
    if (name === 'Camera Make') {
        return { priority: 4, label: value ? `camera make: ${value}` : 'camera make' };
    }
    if (haystack.includes('comment') || haystack.includes('review')) {
        return { priority: 5, label: 'comments/review data' };
    }
    if (haystack.includes('author') || haystack.includes('creator') || haystack.includes('producer')) {
        return { priority: 6, label: 'author/creator data' };
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
