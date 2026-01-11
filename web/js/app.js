// Determine base path for GitHub Pages compatibility
const WASM_PATH = new URL('../wasm/metadata_remover.js', import.meta.url).href;

let wasmReady = false;
let wasmModule = null;
const files = new Map();

async function initWasm() {
    try {
        const wasm = await import(WASM_PATH);
        await wasm.default();
        wasmModule = wasm;
        wasmReady = true;
        dropZone.classList.add('ready');
    } catch (e) {
        console.error('Failed to load WASM:', e);
        dropZone.innerHTML = `
            <div class="drop-zone-content error">
                <p>Failed to load. Please refresh or try a different browser.</p>
            </div>
        `;
    }
}

// DOM Elements
const dropZone = document.getElementById('drop-zone');
const fileInput = document.getElementById('file-input');
const fileList = document.getElementById('file-list');
const actions = document.getElementById('actions');
const processBtn = document.getElementById('process-btn');
const clearBtn = document.getElementById('clear-btn');
const modal = document.getElementById('modal');
const modalBody = document.getElementById('modal-body');
const modalClose = document.querySelector('.modal-close');

// Event Listeners
dropZone.addEventListener('click', (e) => {
    if (e.target.tagName !== 'LABEL') fileInput.click();
});
dropZone.addEventListener('dragover', handleDragOver);
dropZone.addEventListener('dragleave', handleDragLeave);
dropZone.addEventListener('drop', handleDrop);
fileInput.addEventListener('change', handleFileSelect);
processBtn.addEventListener('click', processAllFiles);
clearBtn.addEventListener('click', clearAllFiles);
modalClose.addEventListener('click', () => modal.classList.add('hidden'));
modal.addEventListener('click', (e) => {
    if (e.target === modal) modal.classList.add('hidden');
});
document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') modal.classList.add('hidden');
});

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

const SUPPORTED_EXTENSIONS = new Set(['jpg', 'jpeg', 'png', 'pdf']);

async function addFiles(newFiles) {
    if (!wasmReady) {
        alert('Please wait for the application to finish loading.');
        return;
    }

    const validFiles = newFiles.filter(f => {
        const ext = f.name.split('.').pop().toLowerCase();
        return SUPPORTED_EXTENSIONS.has(ext);
    });

    for (const file of validFiles) {
        const id = crypto.randomUUID();
        const data = new Uint8Array(await file.arrayBuffer());

        files.set(id, {
            id,
            name: file.name,
            size: file.size,
            type: wasmModule.detect_file_type(data),
            data,
            metadata: wasmModule.extract_metadata(data),
            status: 'pending',
            cleanedData: null
        });
    }

    renderFileList();
    updateActions();
}

function renderFileList() {
    fileList.innerHTML = '';

    for (const [id, file] of files) {
        const item = document.createElement('div');
        item.className = 'file-item';
        item.dataset.id = id;

        const metaCount = file.metadata.metadata_found.length;
        const isClean = metaCount === 0;

        item.innerHTML = `
            <div class="file-icon ${file.type}">${file.type}</div>
            <div class="file-info">
                <div class="file-name">${escapeHtml(file.name)}</div>
                <div class="file-meta">
                    <span>${formatSize(file.size)}</span>
                    <span class="metadata-count ${isClean ? 'clean' : ''}">
                        ${metaCount} metadata ${metaCount === 1 ? 'entry' : 'entries'}
                    </span>
                </div>
            </div>
            <div class="file-status">${renderStatus(file.status)}</div>
            <div class="file-actions">
                <button class="btn-icon" title="View metadata" data-action="view" data-id="${id}">
                    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                        <path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z"/>
                        <circle cx="12" cy="12" r="3"/>
                    </svg>
                </button>
                ${file.status === 'done' ? `
                <button class="btn-icon" title="Download" data-action="download" data-id="${id}">
                    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                        <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/>
                        <polyline points="7 10 12 15 17 10"/>
                        <line x1="12" y1="15" x2="12" y2="3"/>
                    </svg>
                </button>
                ` : ''}
                <button class="btn-icon" title="Remove" data-action="remove" data-id="${id}">
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
        pending: '<span class="status-pending">Pending</span>',
        processing: '<span class="status-processing"><span class="spinner"></span> Processing</span>',
        done: '<span class="status-done">Cleaned</span>',
        error: '<span class="status-error">Error</span>'
    };
    return statuses[status] || '';
}

function updateActions() {
    const hasFiles = files.size > 0;
    actions.classList.toggle('hidden', !hasFiles);

    if (hasFiles) {
        const hasPending = [...files.values()].some(f => f.status === 'pending');
        processBtn.disabled = !hasPending;
    }
}

async function processAllFiles() {
    processBtn.disabled = true;

    for (const [id, file] of files) {
        if (file.status !== 'pending') continue;

        file.status = 'processing';
        renderFileList();

        // Yield to UI
        await new Promise(r => setTimeout(r, 0));

        try {
            file.cleanedData = wasmModule.remove_metadata(file.data);
            file.status = 'done';
        } catch (e) {
            console.error('Error processing:', file.name, e);
            file.status = 'error';
        }
    }

    renderFileList();
    updateActions();
}

function clearAllFiles() {
    files.clear();
    renderFileList();
    updateActions();
}

function removeFile(id) {
    files.delete(id);
    renderFileList();
    updateActions();
}

function showMetadata(id) {
    const file = files.get(id);
    if (!file) return;

    const { metadata } = file;

    if (metadata.metadata_found.length === 0) {
        modalBody.innerHTML = '<div class="no-metadata">No metadata found in this file.</div>';
    } else {
        // Group by category
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
                        ${entries.map(e => `
                            <tr>
                                <td>${escapeHtml(e.name)}</td>
                                <td>${escapeHtml(e.value)}</td>
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

    URL.revokeObjectURL(url);
}

function splitFilename(name) {
    const idx = name.lastIndexOf('.');
    return idx > 0 ? [name.slice(0, idx), name.slice(idx + 1)] : [name, ''];
}

const MIME_TYPES = {
    jpeg: 'image/jpeg',
    png: 'image/png',
    pdf: 'application/pdf'
};

function getMimeType(type) {
    return MIME_TYPES[type] || 'application/octet-stream';
}

function formatSize(bytes) {
    if (bytes === 0) return '0 B';
    const k = 1024;
    const sizes = ['B', 'KB', 'MB', 'GB'];
    const i = Math.floor(Math.log(bytes) / Math.log(k));
    return `${(bytes / k ** i).toFixed(1)} ${sizes[i]}`;
}

function escapeHtml(text) {
    const div = document.createElement('div');
    div.textContent = text;
    return div.innerHTML;
}

// Initialize
initWasm();
