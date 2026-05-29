// Detect base path for GitHub Pages (handles /repo-name/ subpath).
const scriptUrl = import.meta.url;
const basePath = scriptUrl.substring(0, scriptUrl.lastIndexOf('/js/'));
const MAX_FILE_BYTES = 100 * 1024 * 1024;
const SUPPORTED_EXTENSIONS = new Set(['jpg', 'jpeg', 'png', 'webp', 'gif', 'pdf']);
const SUPPORTED_FORMATS_LABEL = 'JPEG, PNG, WebP, GIF, PDF';

let wasmReady = false;
let requestId = 0;
const files = new Map();
const pendingRequests = new Map();

// DOM Elements
const dropZone = document.getElementById('drop-zone');
const dropFeedback = document.getElementById('drop-feedback');
const fileInput = document.getElementById('file-input');
const fileList = document.getElementById('file-list');
const actions = document.getElementById('actions');
const processBtn = document.getElementById('process-btn');
const downloadAllBtn = document.getElementById('download-all-btn');
const clearBtn = document.getElementById('clear-btn');
const modal = document.getElementById('modal');
const modalBody = document.getElementById('modal-body');
const modalClose = document.querySelector('.modal-close');
const worker = new Worker(new URL('./worker.js', import.meta.url), { type: 'module' });
let lastFocusedElement = null;

worker.addEventListener('message', handleWorkerMessage);
worker.addEventListener('error', (event) => {
    console.error('Worker failed:', event.message);
    showLoadError('Failed to start the file processor. Please refresh or try a different browser.');
});

// Event Listeners
dropZone.addEventListener('click', (e) => {
    if (e.target.tagName !== 'LABEL') fileInput.click();
});
dropZone.addEventListener('dragover', handleDragOver);
dropZone.addEventListener('dragleave', handleDragLeave);
dropZone.addEventListener('drop', handleDrop);
fileInput.addEventListener('change', handleFileSelect);
processBtn.addEventListener('click', processAllFiles);
downloadAllBtn.addEventListener('click', downloadAllFiles);
clearBtn.addEventListener('click', clearAllFiles);
modalClose.addEventListener('click', closeModal);
modal.addEventListener('click', (e) => {
    if (e.target === modal) closeModal();
});
document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') closeModal();
    if (e.key === 'Tab' && !modal.classList.contains('hidden')) trapModalFocus(e);
});
dropZone.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' || e.key === ' ') {
        e.preventDefault();
        fileInput.click();
    }
});

function handleWorkerMessage(event) {
    const message = event.data;

    if (message.type === 'ready') {
        wasmReady = true;
        dropZone.classList.add('ready');
        registerServiceWorker();
        updateActions();
        return;
    }

    if (message.type === 'fatal') {
        showLoadError(message.error);
        return;
    }

    if (!message.requestId || !pendingRequests.has(message.requestId)) {
        return;
    }

    const { resolve, reject } = pendingRequests.get(message.requestId);
    pendingRequests.delete(message.requestId);

    if (message.type === 'failed') {
        reject(new Error(message.error || 'Processing failed'));
    } else {
        resolve(message);
    }
}

function sendWorkerMessage(type, payload = {}) {
    return new Promise((resolve, reject) => {
        const id = ++requestId;
        pendingRequests.set(id, { resolve, reject });
        worker.postMessage({ type, requestId: id, ...payload });
    });
}

function showLoadError(message) {
    wasmReady = false;
    dropZone.innerHTML = `
        <div class="drop-zone-content error">
            <p>${escapeHtml(message)}</p>
        </div>
    `;
}

function handleDragOver(e) {
    e.preventDefault();
    dropZone.classList.add('drag-over');
}

function handleDragLeave(e) {
    e.preventDefault();
    dropZone.classList.remove('drag-over');
}

function handleDrop(e) {
    e.preventDefault();
    dropZone.classList.remove('drag-over');
    addFiles(Array.from(e.dataTransfer.files));
}

function handleFileSelect(e) {
    addFiles(Array.from(e.target.files));
    fileInput.value = '';
}

async function addFiles(newFiles) {
    if (!wasmReady) {
        alert('Please wait for the application to finish loading.');
        return;
    }

    const supportedFiles = [];
    const unsupportedFiles = [];

    for (const file of newFiles) {
        const ext = extensionFor(file.name);
        if (SUPPORTED_EXTENSIONS.has(ext)) {
            supportedFiles.push(file);
        } else {
            unsupportedFiles.push(file);
        }
    }

    if (unsupportedFiles.length > 0) {
        showUnsupportedFiles(unsupportedFiles);
    } else {
        clearDropFeedback();
    }

    for (const file of supportedFiles) {
        const id = crypto.randomUUID();
        const record = {
            id,
            name: file.name,
            size: file.size,
            type: 'unknown',
            metadata: emptyMetadata('unknown'),
            originalMetadata: emptyMetadata('unknown'),
            verification: null,
            status: 'loading',
            cleanedData: null,
            cleanedSize: null,
            errorMessage: null
        };
        files.set(id, record);
        renderFileList();
        updateActions();

        if (file.size > MAX_FILE_BYTES) {
            record.status = 'error';
            record.errorMessage = `File exceeds the ${formatSize(MAX_FILE_BYTES)} limit.`;
            renderFileList();
            updateActions();
            continue;
        }

        try {
            const result = await sendWorkerMessage('analyze', { id, file });
            const current = files.get(id);
            if (!current) continue;
            current.type = result.fileType;
            current.metadata = result.metadata;
            current.originalMetadata = result.metadata;
            current.status = 'pending';
        } catch (e) {
            const current = files.get(id);
            if (!current) continue;
            current.status = 'error';
            current.errorMessage = e.message;
        }

        renderFileList();
        updateActions();
    }
}

function showUnsupportedFiles(unsupportedFiles) {
    const shownNames = unsupportedFiles
        .slice(0, 3)
        .map((file) => file.name || 'unnamed file');
    const extraCount = unsupportedFiles.length - shownNames.length;
    const extraText = extraCount > 0 ? ` and ${extraCount} more` : '';
    const fileText = unsupportedFiles.length === 1 ? 'file' : 'files';
    dropFeedback.textContent = `Skipped ${unsupportedFiles.length} unsupported ${fileText}: ${shownNames.join(', ')}${extraText}. Supported formats: ${SUPPORTED_FORMATS_LABEL}.`;
    dropFeedback.classList.add('visible');
}

function clearDropFeedback() {
    dropFeedback.textContent = '';
    dropFeedback.classList.remove('visible');
}

function renderFileList() {
    fileList.innerHTML = '';

    for (const [id, file] of files) {
        const item = document.createElement('div');
        item.className = 'file-item';
        item.dataset.id = id;

        const typeLabel = file.type === 'unknown' ? extensionFor(file.name) || 'file' : file.type;
        const metaText = renderMetaText(file);
        const canDownload = Boolean(file.cleanedData);

        item.innerHTML = `
            <div class="file-icon ${escapeHtml(typeLabel)}">${escapeHtml(typeLabel)}</div>
            <div class="file-info">
                <div class="file-name">${escapeHtml(file.name)}</div>
                <div class="file-meta">${metaText}</div>
            </div>
            <div class="file-status">${renderStatus(file.status)}</div>
            <div class="file-actions">
                <button class="btn-icon" title="View metadata" aria-label="View metadata for ${escapeAttribute(file.name)}" data-action="view" data-id="${id}">
                    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                        <path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z"/>
                        <circle cx="12" cy="12" r="3"/>
                    </svg>
                </button>
                ${canDownload ? `
                <button class="btn-icon" title="Download" aria-label="Download cleaned ${escapeAttribute(file.name)}" data-action="download" data-id="${id}">
                    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                        <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/>
                        <polyline points="7 10 12 15 17 10"/>
                        <line x1="12" y1="15" x2="12" y2="3"/>
                    </svg>
                </button>
                ` : ''}
                <button class="btn-icon" title="Remove" aria-label="Remove ${escapeAttribute(file.name)} from the list" data-action="remove" data-id="${id}">
                    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                        <line x1="18" y1="6" x2="6" y2="18"/>
                        <line x1="6" y1="6" x2="18" y2="18"/>
                    </svg>
                </button>
            </div>
        `;
        fileList.appendChild(item);
    }
}

function renderMetaText(file) {
    if (file.status === 'error') {
        return escapeHtml(file.errorMessage || 'Unable to process this file.');
    }

    if (file.status === 'done') {
        return `
            <span>${formatSize(file.size)} -> ${formatSize(file.cleanedSize)}</span>
            <span class="metadata-count clean">Verified clean</span>
        `;
    }

    if (file.status === 'warning') {
        const remaining = file.verification?.metadata_found.length ?? 0;
        return `
            <span>${formatSize(file.size)} -> ${formatSize(file.cleanedSize)}</span>
            <span class="metadata-count">${remaining} metadata ${remaining === 1 ? 'entry' : 'entries'} remain</span>
        `;
    }

    const metaCount = file.metadata.metadata_found.length;
    const isClean = metaCount === 0;
    return `
        <span>${formatSize(file.size)}</span>
        <span class="metadata-count ${isClean ? 'clean' : ''}">
            ${metaCount} metadata ${metaCount === 1 ? 'entry' : 'entries'}
        </span>
    `;
}

// Event delegation for file actions
fileList.addEventListener('click', (e) => {
    const btn = e.target.closest('[data-action]');
    if (!btn) return;

    const { action, id } = btn.dataset;
    if (action === 'view') showMetadata(id);
    else if (action === 'download') downloadFile(id);
    else if (action === 'remove') removeFile(id);
});

function renderStatus(status) {
    const statuses = {
        loading: '<span class="status-loading"><span class="spinner"></span> Reading</span>',
        pending: '<span class="status-pending">Pending</span>',
        processing: '<span class="status-processing"><span class="spinner"></span> Processing</span>',
        done: '<span class="status-done">Verified</span>',
        warning: '<span class="status-warning">Review</span>',
        error: '<span class="status-error">Error</span>'
    };
    return statuses[status] || '';
}

function updateActions() {
    const hasFiles = files.size > 0;
    actions.classList.toggle('hidden', !hasFiles);

    const hasPending = [...files.values()].some((file) => file.status === 'pending');
    const hasCleaned = [...files.values()].some((file) => file.cleanedData);
    const busy = [...files.values()].some((file) => file.status === 'loading' || file.status === 'processing');
    processBtn.disabled = !hasFiles || !wasmReady || !hasPending || busy;
    downloadAllBtn.disabled = !hasFiles || !hasCleaned || busy;
}

async function processAllFiles() {
    processBtn.disabled = true;

    for (const [id, file] of files) {
        if (file.status !== 'pending') continue;

        file.status = 'processing';
        renderFileList();
        updateActions();

        try {
            const result = await sendWorkerMessage('process', { id });
            const current = files.get(id);
            if (!current) continue;
            current.cleanedData = new Uint8Array(result.cleanedBuffer);
            current.cleanedSize = current.cleanedData.byteLength;
            current.verification = result.verification;
            current.metadata = result.verification;
            current.status = result.verification.metadata_found.length === 0 ? 'done' : 'warning';
        } catch (e) {
            const current = files.get(id);
            if (!current) continue;
            current.errorMessage = e.message;
            current.status = 'error';
        }

        renderFileList();
        updateActions();
    }
}

function clearAllFiles() {
    files.clear();
    worker.postMessage({ type: 'clear' });
    clearDropFeedback();
    renderFileList();
    updateActions();
}

function removeFile(id) {
    files.delete(id);
    worker.postMessage({ type: 'forget', id });
    renderFileList();
    updateActions();
}

function showMetadata(id) {
    const file = files.get(id);
    if (!file) return;

    const { metadata } = file;

    if (file.status === 'error') {
        modalBody.innerHTML = `<div class="no-metadata">${escapeHtml(file.errorMessage || 'Unable to process this file.')}</div>`;
    } else if (metadata.metadata_found.length === 0) {
        modalBody.innerHTML = '<div class="no-metadata">No metadata found in this file.</div>';
    } else {
        const grouped = metadata.metadata_found.reduce((acc, entry) => {
            (acc[entry.category] ??= []).push(entry);
            return acc;
        }, {});

        let html = '';
        for (const [category, entries] of Object.entries(grouped)) {
            html += `
                <div class="metadata-section">
                    <h3>${escapeHtml(category)}</h3>
                    <table class="metadata-table">
                        ${entries.map((entry) => `
                            <tr>
                                <td>${escapeHtml(entry.name)}</td>
                                <td>${escapeHtml(entry.value)}</td>
                            </tr>
                        `).join('')}
                    </table>
                </div>
            `;
        }

        html += `
            <div class="metadata-section">
                <h3>Summary</h3>
                <table class="metadata-table">
                    <tr>
                        <td>Total metadata bytes</td>
                        <td>${formatSize(metadata.total_metadata_bytes)}</td>
                    </tr>
                </table>
            </div>
        `;

        modalBody.innerHTML = html;
    }

    modal.classList.remove('hidden');
    lastFocusedElement = document.activeElement;
    modalClose.focus();
}

function closeModal() {
    if (modal.classList.contains('hidden')) return;
    modal.classList.add('hidden');
    if (lastFocusedElement && typeof lastFocusedElement.focus === 'function') {
        lastFocusedElement.focus();
    }
}

function trapModalFocus(event) {
    const focusable = modal.querySelectorAll('button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])');
    if (focusable.length === 0) return;

    const first = focusable[0];
    const last = focusable[focusable.length - 1];

    if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
    }
}

function downloadFile(id) {
    const file = files.get(id);
    if (!file?.cleanedData) return;

    const blob = new Blob([file.cleanedData], { type: getMimeType(file.type) });
    const url = URL.createObjectURL(blob);

    const a = document.createElement('a');
    a.href = url;
    const [name, ext] = splitFilename(file.name);
    a.download = `${name}_clean.${ext}`;
    a.click();

    setTimeout(() => URL.revokeObjectURL(url), 0);
}

function downloadAllFiles() {
    const cleanedFiles = [...files.values()].filter((file) => file.cleanedData);
    if (cleanedFiles.length === 0) return;

    const zip = createZip(cleanedFiles.map((file) => {
        const [name, ext] = splitFilename(file.name);
        return {
            name: `${name}_clean.${ext}`,
            data: file.cleanedData
        };
    }));
    const url = URL.createObjectURL(zip);
    const a = document.createElement('a');
    a.href = url;
    a.download = 'metadata-cleaned.zip';
    a.click();
    setTimeout(() => URL.revokeObjectURL(url), 0);
}

function createZip(entries) {
    const localParts = [];
    const centralParts = [];
    let offset = 0;

    for (const entry of entries) {
        const nameBytes = new TextEncoder().encode(entry.name);
        const data = entry.data instanceof Uint8Array ? entry.data : new Uint8Array(entry.data);
        const crc = crc32(data);
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
    }

    const centralOffset = offset;
    const centralSize = centralParts.reduce((size, part) => size + part.byteLength, 0);
    const end = zipHeader(22, 0x06054b50);
    end.setUint16(8, entries.length, true);
    end.setUint16(10, entries.length, true);
    end.setUint32(12, centralSize, true);
    end.setUint32(16, centralOffset, true);

    return new Blob([...localParts, ...centralParts, end], { type: 'application/zip' });
}

function zipHeader(size, signature) {
    const bytes = new Uint8Array(size);
    const view = new DataView(bytes.buffer);
    view.setUint32(0, signature, true);
    return view;
}

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

function crc32(data) {
    let crc = 0xffffffff;
    for (const byte of data) {
        crc = CRC32_TABLE[(crc ^ byte) & 0xff] ^ (crc >>> 8);
    }
    return (crc ^ 0xffffffff) >>> 0;
}

function splitFilename(name) {
    const idx = name.lastIndexOf('.');
    return idx > 0 ? [name.slice(0, idx), name.slice(idx + 1)] : [name, ''];
}

function extensionFor(name) {
    const ext = name.split('.').pop();
    return ext ? ext.toLowerCase() : '';
}

function emptyMetadata(fileType) {
    return {
        file_type: fileType,
        metadata_found: [],
        total_metadata_bytes: 0
    };
}

const MIME_TYPES = {
    jpeg: 'image/jpeg',
    png: 'image/png',
    webp: 'image/webp',
    gif: 'image/gif',
    pdf: 'application/pdf'
};

function getMimeType(type) {
    return MIME_TYPES[type] || 'application/octet-stream';
}

function formatSize(bytes) {
    if (bytes === 0) return '0 B';
    const k = 1024;
    const sizes = ['B', 'KB', 'MB', 'GB'];
    const i = Math.min(Math.floor(Math.log(bytes) / Math.log(k)), sizes.length - 1);
    return `${(bytes / k ** i).toFixed(1)} ${sizes[i]}`;
}

function escapeHtml(text) {
    const div = document.createElement('div');
    div.textContent = String(text);
    return div.innerHTML;
}

function escapeAttribute(text) {
    return escapeHtml(text).replaceAll('"', '&quot;').replaceAll("'", '&#39;');
}

function registerServiceWorker() {
    if (!('serviceWorker' in navigator) || location.protocol === 'file:') {
        return;
    }

    navigator.serviceWorker.register(`${basePath}/sw.js`).catch((e) => {
        console.warn('Service worker registration failed:', e);
    });
}
