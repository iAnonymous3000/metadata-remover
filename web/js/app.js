// Detect base path for GitHub Pages (handles /repo-name/ subpath).
const scriptUrl = import.meta.url;
const basePath = scriptUrl.substring(0, scriptUrl.lastIndexOf('/js/'));
const isFramed = window.self !== window.top;
const MAX_FILE_BYTES = 100 * 1024 * 1024;
const BASE_MEMORY_BUDGET_BYTES = 512 * 1024 * 1024;
const MIN_MEMORY_BUDGET_BYTES = 128 * 1024 * 1024;
const ZIP32_MAX = 0xffffffff;
const ZIP16_MAX = 0xffff;
const SUPPORTED_EXTENSIONS = new Set(['jpg', 'jpeg', 'png', 'webp', 'gif', 'pdf', 'docx', 'xlsx', 'pptx']);
const LEGACY_OFFICE_EXTENSIONS = new Set(['doc', 'xls', 'ppt']);
const SUPPORTED_FORMATS_LABEL = 'JPEG, PNG, WebP, GIF, PDF, DOCX, XLSX, PPTX';

let wasmReady = false;
let requestId = 0;
const files = new Map();
const pendingRequests = new Map();
let queuedFiles = [];
let worker = null;
let workerReadyPromise = Promise.resolve();
let resolveWorkerReady = null;
let rejectWorkerReady = null;
let workerRestarting = false;
let dragDepth = 0;

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
const srStatus = document.getElementById('sr-status');
let lastFocusedElement = null;

if (isFramed) {
    bustFrame();
    showLoadError('Open Metadata Remover directly in a new tab to clean files safely.');
} else {
    startWorker();
}

function bustFrame() {
    try {
        window.top.location = window.self.location.href;
    } catch {
        document.documentElement.classList.add('framed');
    }
}

function startWorker() {
    wasmReady = false;
    workerReadyPromise = new Promise((resolve, reject) => {
        resolveWorkerReady = resolve;
        rejectWorkerReady = reject;
    });

    worker = new Worker(new URL('./worker.js', import.meta.url), { type: 'module' });
    worker.addEventListener('message', handleWorkerMessage);
    worker.addEventListener('error', handleWorkerError);
    worker.addEventListener('messageerror', handleWorkerError);
    workerReadyPromise.catch(() => {});
}

function handleWorkerError(event) {
    const message = event.message || 'The file processor stopped unexpectedly.';
    console.error('Worker failed:', message);

    if (wasmReady) {
        restartWorker('The file processor recovered from an unexpected error.');
    } else {
        rejectWorkerReady?.(new Error(message));
        rejectPendingRequests(new Error(message));
        showLoadError('Failed to start the file processor. Please refresh or try a different browser.');
    }
}

function restartWorker(message) {
    if (workerRestarting) {
        return workerReadyPromise;
    }

    workerRestarting = true;
    wasmReady = false;
    dropZone.classList.remove('ready');
    if (message) {
        dropFeedback.textContent = message;
        dropFeedback.classList.add('visible');
    }
    rejectPendingRequests(new Error(message || 'The file processor restarted.'));

    worker?.terminate();
    startWorker();
    workerReadyPromise.catch((error) => {
        showLoadError(error.message || 'Failed to restart the file processor.');
    });
    return workerReadyPromise;
}

function rejectPendingRequests(error) {
    for (const { reject } of pendingRequests.values()) {
        reject(error);
    }
    pendingRequests.clear();
}

function postWorkerControl(message) {
    try {
        worker?.postMessage(message);
    } catch (e) {
        console.warn('Unable to send worker control message:', e);
    }
}

// Event Listeners
dropZone.addEventListener('click', (e) => {
    if (e.target.tagName !== 'LABEL') fileInput.click();
});
dropZone.addEventListener('dragenter', handleDragEnter);
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
        workerRestarting = false;
        dropZone.classList.add('ready');
        resolveWorkerReady?.();
        registerServiceWorker();
        updateActions();
        flushQueuedFiles();
        return;
    }

    if (message.type === 'fatal') {
        const error = new Error(message.error || 'Failed to start the file processor.');
        rejectWorkerReady?.(error);
        rejectPendingRequests(error);
        showLoadError(message.error);
        return;
    }

    if (!message.requestId || !pendingRequests.has(message.requestId)) {
        return;
    }

    const { resolve, reject } = pendingRequests.get(message.requestId);
    pendingRequests.delete(message.requestId);

    if (message.type === 'failed') {
        const error = new Error(message.error || 'Processing failed');
        error.fileType = message.fileType;
        reject(error);
        if (message.fatal) {
            restartWorker('The file processor recovered from a fatal file error. Re-add any failed file to try again.');
        }
    } else {
        resolve(message);
    }
}

async function sendWorkerMessage(type, payload = {}) {
    if (!wasmReady) {
        await workerReadyPromise;
    }

    return new Promise((resolve, reject) => {
        const id = ++requestId;
        pendingRequests.set(id, { resolve, reject });
        try {
            worker.postMessage({ type, requestId: id, ...payload });
        } catch (e) {
            pendingRequests.delete(id);
            reject(e);
        }
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

function handleDragEnter(e) {
    e.preventDefault();
    dragDepth += 1;
    dropZone.classList.add('drag-over');
}

function handleDragOver(e) {
    e.preventDefault();
    if (e.dataTransfer) {
        e.dataTransfer.dropEffect = 'copy';
    }
}

function handleDragLeave(e) {
    e.preventDefault();
    dragDepth = Math.max(0, dragDepth - 1);
    if (dragDepth === 0) {
        dropZone.classList.remove('drag-over');
    }
}

function handleDrop(e) {
    e.preventDefault();
    dragDepth = 0;
    dropZone.classList.remove('drag-over');
    addFiles(Array.from(e.dataTransfer.files));
}

function handleFileSelect(e) {
    addFiles(Array.from(e.target.files));
    fileInput.value = '';
}

async function addFiles(newFiles) {
    if (!wasmReady) {
        queueFiles(newFiles);
        return;
    }

    const supportedFiles = [];
    const unsupportedFiles = [];
    const legacyOfficeFiles = [];

    for (const file of newFiles) {
        const ext = extensionFor(file.name);
        if (SUPPORTED_EXTENSIONS.has(ext)) {
            supportedFiles.push(file);
        } else if (LEGACY_OFFICE_EXTENSIONS.has(ext)) {
            legacyOfficeFiles.push(file);
        } else {
            unsupportedFiles.push(file);
        }
    }

    const memoryFiltered = filterByMemoryBudget(supportedFiles, candidateSourceFileBytes);
    const filesToAnalyze = memoryFiltered.accepted;

    const feedbackMessages = [];
    if (legacyOfficeFiles.length > 0) {
        feedbackMessages.push(legacyOfficeMessage(legacyOfficeFiles));
    }
    if (unsupportedFiles.length > 0) {
        feedbackMessages.push(unsupportedFilesMessage(unsupportedFiles));
    }
    if (memoryFiltered.rejected.length > 0) {
        feedbackMessages.push(memoryLimitMessage(memoryFiltered.rejected, memoryFiltered.budget));
    }

    if (feedbackMessages.length > 0) {
        showDropFeedback(feedbackMessages.join(' '));
    } else {
        clearDropFeedback();
    }

    for (const file of filesToAnalyze) {
        const id = crypto.randomUUID();
        const record = {
            id,
            name: file.name,
            size: file.size,
            sourceFile: file,
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
        upsertFileRow(id);
        updateActions();

        if (file.size > MAX_FILE_BYTES) {
            record.status = 'error';
            record.sourceFile = null;
            record.errorMessage = `File exceeds the ${formatSize(MAX_FILE_BYTES)} limit.`;
            upsertFileRow(id);
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
            if (e.fileType && e.fileType !== 'unknown') {
                current.type = e.fileType;
            }
            current.status = 'error';
            current.errorMessage = e.message;
        }

        upsertFileRow(id);
        updateActions();
    }

    announce(addedSummary(filesToAnalyze.length));
}

function queueFiles(newFiles) {
    if (newFiles.length === 0) return;
    const memoryFiltered = filterByMemoryBudget(newFiles, queuedSourceFileBytes);
    queuedFiles.push(...memoryFiltered.accepted);

    const messages = [];
    if (queuedFiles.length > 0) {
        const fileText = queuedFiles.length === 1 ? 'file' : 'files';
        messages.push(`Queued ${queuedFiles.length} ${fileText} until the local processor is ready.`);
    }
    if (memoryFiltered.rejected.length > 0) {
        messages.push(memoryLimitMessage(memoryFiltered.rejected, memoryFiltered.budget));
    }

    if (messages.length > 0) {
        showDropFeedback(messages.join(' '));
    }
}

function flushQueuedFiles() {
    if (!wasmReady || queuedFiles.length === 0) return;
    const filesToAdd = queuedFiles;
    queuedFiles = [];
    addFiles(filesToAdd);
}

function unsupportedFilesMessage(unsupportedFiles) {
    const shownNames = unsupportedFiles
        .slice(0, 3)
        .map((file) => file.name || 'unnamed file');
    const extraCount = unsupportedFiles.length - shownNames.length;
    const extraText = extraCount > 0 ? ` and ${extraCount} more` : '';
    const fileText = unsupportedFiles.length === 1 ? 'file' : 'files';
    return `Skipped ${unsupportedFiles.length} unsupported ${fileText}: ${shownNames.join(', ')}${extraText}. Supported formats: ${SUPPORTED_FORMATS_LABEL}.`;
}

function legacyOfficeMessage(legacyOfficeFiles) {
    const shownNames = legacyOfficeFiles
        .slice(0, 3)
        .map((file) => file.name || 'unnamed file');
    const extraCount = legacyOfficeFiles.length - shownNames.length;
    const extraText = extraCount > 0 ? ` and ${extraCount} more` : '';
    const fileText = legacyOfficeFiles.length === 1 ? 'file' : 'files';
    return `Skipped ${legacyOfficeFiles.length} legacy Office ${fileText}: ${shownNames.join(', ')}${extraText}. Save .doc, .xls, or .ppt files as .docx, .xlsx, or .pptx first.`;
}

function filterByMemoryBudget(candidateFiles, byteCounter = sourceFileBytesForBudget) {
    const budget = memoryBudgetBytes();
    let projectedBytes = currentMemoryBytes();
    const accepted = [];
    const rejected = [];

    for (const file of candidateFiles) {
        const sourceBytes = byteCounter(file);
        if (sourceBytes === 0 || projectedBytes + sourceBytes <= budget) {
            accepted.push(file);
            projectedBytes += sourceBytes;
        } else {
            rejected.push(file);
        }
    }

    return { accepted, rejected, budget };
}

function memoryBudgetBytes() {
    const deviceMemory = Number(navigator.deviceMemory);
    if (!Number.isFinite(deviceMemory) || deviceMemory <= 0) {
        return BASE_MEMORY_BUDGET_BYTES;
    }

    const scaledBudget = Math.floor(deviceMemory * 1024 * 1024 * 1024 * 0.25);
    return Math.min(BASE_MEMORY_BUDGET_BYTES, Math.max(MIN_MEMORY_BUDGET_BYTES, scaledBudget));
}

function currentMemoryBytes() {
    let total = queuedFiles.reduce((sum, file) => sum + queuedSourceFileBytes(file), 0);
    for (const file of files.values()) {
        total += sourceFileBytesForBudget(file.sourceFile);
        total += file.cleanedData?.byteLength ?? 0;
    }
    return total;
}

function queuedSourceFileBytes(file) {
    const ext = extensionFor(file.name);
    if (!SUPPORTED_EXTENSIONS.has(ext) || file.size > MAX_FILE_BYTES) {
        return 0;
    }
    return file.size;
}

function candidateSourceFileBytes(file) {
    return file.size > MAX_FILE_BYTES ? 0 : file.size;
}

function sourceFileBytesForBudget(file) {
    return file?.size ?? 0;
}

function memoryLimitMessage(rejectedFiles, budget) {
    const shownNames = rejectedFiles
        .slice(0, 3)
        .map((file) => file.name || 'unnamed file');
    const extraCount = rejectedFiles.length - shownNames.length;
    const extraText = extraCount > 0 ? ` and ${extraCount} more` : '';
    const fileText = rejectedFiles.length === 1 ? 'file' : 'files';
    return `Skipped ${rejectedFiles.length} ${fileText} because the local memory budget is ${formatSize(budget)}: ${shownNames.join(', ')}${extraText}. Download or clear finished files before adding more.`;
}

function showDropFeedback(message) {
    dropFeedback.textContent = message;
    dropFeedback.classList.add('visible');
}

function clearDropFeedback() {
    dropFeedback.textContent = '';
    dropFeedback.classList.remove('visible');
}

// The file list is no longer an aria-live region (it churned on every row
// update). Instead we announce batch milestones once, through a dedicated
// visually-hidden status region.
function announce(message) {
    if (srStatus) srStatus.textContent = message;
}

function addedSummary(count) {
    if (count === 0) return '';
    return `${count} ${count === 1 ? 'file' : 'files'} added and analyzed.`;
}

function processedSummary() {
    let clean = 0;
    let review = 0;
    let failed = 0;
    for (const file of files.values()) {
        if (file.status === 'done') clean += 1;
        else if (file.status === 'warning') review += 1;
        else if (file.status === 'error') failed += 1;
    }

    const parts = [];
    if (clean > 0) parts.push(`${clean} verified clean`);
    if (review > 0) parts.push(`${review} need review`);
    if (failed > 0) parts.push(`${failed} could not be processed`);
    return parts.length > 0 ? `Cleaning complete: ${parts.join(', ')}.` : '';
}

const VIEW_ICON_SVG = `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z"/><circle cx="12" cy="12" r="3"/></svg>`;
const DOWNLOAD_ICON_SVG = `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="7 10 12 15 17 10"/><line x1="12" y1="15" x2="12" y2="3"/></svg>`;
const REMOVE_ICON_SVG = `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/></svg>`;

function fileTypeLabel(file) {
    return file.type === 'unknown' ? extensionFor(file.name) || 'file' : file.type;
}

function fileRowElement(id) {
    return fileList.querySelector(`.file-item[data-id="${id}"]`);
}

function downloadButtonHtml(file) {
    return `
                <button class="btn-icon" title="Download" aria-label="Download cleaned ${escapeAttribute(file.name)}" data-action="download" data-id="${file.id}">
                    ${DOWNLOAD_ICON_SVG}
                </button>`;
}

function fileRowHtml(file) {
    const typeLabel = fileTypeLabel(file);
    const typeClass = safeClassName(typeLabel);
    return `
            <div class="file-icon ${typeClass}">${escapeHtml(typeLabel)}</div>
            <div class="file-info">
                <div class="file-name" title="${escapeAttribute(file.name)}">${escapeHtml(file.name)}</div>
                <div class="file-meta">${renderMetaText(file)}</div>
            </div>
            <div class="file-status">${renderStatus(file.status)}</div>
            <div class="file-actions">
                <button class="btn-icon" title="View metadata" aria-label="View metadata for ${escapeAttribute(file.name)}" data-action="view" data-id="${file.id}">
                    ${VIEW_ICON_SVG}
                </button>${file.cleanedData ? downloadButtonHtml(file) : ''}
                <button class="btn-icon" title="Remove" aria-label="Remove ${escapeAttribute(file.name)} from the list" data-action="remove" data-id="${file.id}">
                    ${REMOVE_ICON_SVG}
                </button>
            </div>
        `;
}

function createFileRow(file) {
    const item = document.createElement('div');
    item.className = 'file-item';
    item.dataset.id = file.id;
    item.innerHTML = fileRowHtml(file);
    return item;
}

// Full rebuild; used only when the whole list changes at once (e.g. Clear All).
function renderFileList() {
    fileList.innerHTML = '';
    for (const file of files.values()) {
        fileList.appendChild(createFileRow(file));
    }
}

// Patch only the parts of one row that change across its lifecycle. Updating one
// file never re-renders the rest of the list, so a finishing file can't steal
// focus from a button the user is on or trigger list-wide screen-reader churn.
function upsertFileRow(id) {
    const file = files.get(id);
    if (!file) return;

    const row = fileRowElement(id);
    if (!row) {
        fileList.appendChild(createFileRow(file));
        return;
    }

    const icon = row.querySelector('.file-icon');
    const typeLabel = fileTypeLabel(file);
    icon.className = `file-icon ${safeClassName(typeLabel)}`;
    icon.textContent = typeLabel;

    row.querySelector('.file-meta').innerHTML = renderMetaText(file);
    row.querySelector('.file-status').innerHTML = renderStatus(file.status);

    const actionsEl = row.querySelector('.file-actions');
    const downloadBtn = actionsEl.querySelector('[data-action="download"]');
    if (file.cleanedData && !downloadBtn) {
        actionsEl.querySelector('[data-action="view"]')
            .insertAdjacentHTML('afterend', downloadButtonHtml(file));
    } else if (!file.cleanedData && downloadBtn) {
        downloadBtn.remove();
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

    let hasPending = false;
    let hasCleaned = false;
    let busy = false;
    for (const file of files.values()) {
        if (file.status === 'pending') hasPending = true;
        if (file.cleanedData) hasCleaned = true;
        if (file.status === 'loading' || file.status === 'processing') busy = true;
    }
    processBtn.disabled = !hasFiles || !wasmReady || !hasPending || busy;
    downloadAllBtn.disabled = !hasFiles || !hasCleaned || busy;
}

async function processAllFiles() {
    processBtn.disabled = true;

    for (const [id, file] of files) {
        if (file.status !== 'pending') continue;

        file.status = 'processing';
        upsertFileRow(id);
        updateActions();

        try {
            const result = await sendWorkerMessage('process', { id, file: file.sourceFile });
            const current = files.get(id);
            if (!current) continue;
            current.cleanedData = new Uint8Array(result.cleanedBuffer);
            current.cleanedSize = current.cleanedData.byteLength;
            current.verification = result.verification;
            current.metadata = result.verification;
            current.sourceFile = null;
            current.status = result.verification.metadata_found.length === 0 ? 'done' : 'warning';
        } catch (e) {
            const current = files.get(id);
            if (!current) continue;
            current.errorMessage = e.message;
            current.status = 'error';
        }

        upsertFileRow(id);
        updateActions();
    }

    announce(processedSummary());
}

function clearAllFiles() {
    files.clear();
    postWorkerControl({ type: 'clear' });
    clearDropFeedback();
    renderFileList();
    updateActions();
}

function removeFile(id) {
    files.delete(id);
    postWorkerControl({ type: 'forget', id });
    fileRowElement(id)?.remove();
    updateActions();
}

function showMetadata(id) {
    const file = files.get(id);
    if (!file) return;

    if (file.status === 'error') {
        modalBody.innerHTML = `<div class="no-metadata">${escapeHtml(file.errorMessage || 'Unable to process this file.')}</div>`;
    } else if (file.cleanedData) {
        modalBody.innerHTML = renderCleanedMetadataDetails(file);
    } else if (file.metadata.metadata_found.length === 0) {
        modalBody.innerHTML = renderNoMetadata(file, 'No removable metadata found in this file.');
    } else {
        modalBody.innerHTML = renderMetadataDetails(
            file,
            file.metadata,
            'Detected metadata',
            'Total metadata bytes'
        );
    }

    modal.classList.remove('hidden');
    lastFocusedElement = document.activeElement;
    modalClose.focus();
}

function renderCleanedMetadataDetails(file) {
    const originalEntries = file.originalMetadata?.metadata_found ?? [];
    const remainingEntries = file.verification?.metadata_found ?? [];
    const remainingKeys = new Set(remainingEntries.map(metadataEntryKey));
    const removedEntries = originalEntries.filter((entry) => !remainingKeys.has(metadataEntryKey(entry)));
    const fileType = file.type || file.metadata.file_type || file.originalMetadata?.file_type;

    if (originalEntries.length === 0 && remainingEntries.length === 0) {
        return renderNoMetadata(file, 'No removable metadata was found before or after cleaning.');
    }

    return `
        ${renderPreservedMetadataNote(fileType)}
        ${renderEntrySection('Removed', removedEntries, 'No metadata entries were removed.')}
        ${renderEntrySection('Still present after cleaning', remainingEntries, 'No removable metadata remains after cleaning.')}
        <div class="metadata-section">
            <h3>Summary</h3>
            <table class="metadata-table">
                <tr>
                    <td>Original metadata bytes</td>
                    <td>${formatSize(file.originalMetadata?.total_metadata_bytes ?? 0)}</td>
                </tr>
                <tr>
                    <td>Remaining metadata bytes</td>
                    <td>${formatSize(file.verification?.total_metadata_bytes ?? 0)}</td>
                </tr>
            </table>
        </div>
    `;
}

function renderMetadataDetails(file, metadata, title, totalLabel) {
    return `
        ${renderEntrySection(title, metadata.metadata_found, 'No removable metadata found in this file.')}
        ${renderPreservedMetadataNote(file.type || metadata.file_type)}
        <div class="metadata-section">
            <h3>Summary</h3>
            <table class="metadata-table">
                <tr>
                    <td>${escapeHtml(totalLabel)}</td>
                    <td>${formatSize(metadata.total_metadata_bytes)}</td>
                </tr>
            </table>
        </div>
    `;
}

function renderNoMetadata(file, message) {
    return `
        <div class="no-metadata">${escapeHtml(message)}</div>
        ${renderPreservedMetadataNote(file.type || file.metadata.file_type)}
    `;
}

function renderEntrySection(title, entries, emptyMessage) {
    if (entries.length === 0) {
        return `
            <div class="metadata-section">
                <h3>${escapeHtml(title)}</h3>
                <div class="metadata-empty">${escapeHtml(emptyMessage)}</div>
            </div>
        `;
    }

    const grouped = entries.reduce((acc, entry) => {
        (acc[entry.category] ??= []).push(entry);
        return acc;
    }, {});

    return Object.entries(grouped).map(([category, categoryEntries]) => `
        <div class="metadata-section">
            <h3>${escapeHtml(title)}: ${escapeHtml(category)}</h3>
            <table class="metadata-table">
                ${categoryEntries.map((entry) => `
                    <tr>
                        <td>${escapeHtml(entry.name)}</td>
                        <td>${escapeHtml(entry.value)}</td>
                    </tr>
                `).join('')}
            </table>
        </div>
    `).join('');
}

function metadataEntryKey(entry) {
    return `${entry.category}\u0000${entry.name}\u0000${entry.value}`;
}

function renderPreservedMetadataNote(fileType) {
    const preserved = {
        jpeg: 'JPEG orientation and color-profile/color-transform data may be kept so photos do not rotate sideways or shift colors.',
        png: 'PNG transparency and color-management chunks are kept so images render the same after cleaning.',
        webp: 'WebP image, alpha, animation, and color-profile chunks are kept so the file still displays correctly.',
        gif: 'GIF frames, transparency controls, plain-text image blocks, and animation loops are kept so animation and appearance are preserved.'
    };

    if (preserved[fileType]) {
        return `
        <div class="metadata-note">
            <strong>Kept for correct display:</strong> ${escapeHtml(preserved[fileType])}
        </div>
    `;
    }

    if (fileType === 'docx') {
        const note = 'Cleaning a DOCX accepts tracked changes and removes review content: any pending insertions are kept, while tracked deletions, comments, and reviewer names are dropped. This changes the document’s visible review state, not only hidden metadata.';
        return `
        <div class="metadata-note">
            <strong>Changes document content:</strong> ${escapeHtml(note)}
        </div>
    `;
    }

    return '';
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

    triggerDownload(url, cleanedFilename(file.name));
}

function downloadAllFiles() {
    const cleanedFiles = [...files.values()].filter((file) => file.cleanedData);
    if (cleanedFiles.length === 0) return;

    let zip;
    try {
        const usedNames = new Set();
        zip = createZip(cleanedFiles.map((file) => ({
            name: uniqueFilename(cleanedFilename(file.name), usedNames),
            data: file.cleanedData
        })));
    } catch (e) {
        dropFeedback.textContent = e.message || 'Unable to create ZIP file.';
        dropFeedback.classList.add('visible');
        return;
    }

    const url = URL.createObjectURL(zip);
    triggerDownload(url, 'metadata-cleaned.zip');
}

function triggerDownload(url, filename) {
    const a = document.createElement('a');
    a.href = url;
    a.download = filename;
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 60_000);
}

function createZip(entries) {
    if (entries.length > ZIP16_MAX) {
        throw new Error('Too many files for this ZIP download.');
    }

    const localParts = [];
    const centralParts = [];
    let offset = 0;

    for (const entry of entries) {
        const nameBytes = new TextEncoder().encode(entry.name);
        if (nameBytes.byteLength > ZIP16_MAX) {
            throw new Error('A cleaned filename is too long for ZIP download.');
        }
        const data = entry.data instanceof Uint8Array ? entry.data : new Uint8Array(entry.data);
        if (data.byteLength > ZIP32_MAX || offset > ZIP32_MAX) {
            throw new Error('Batch is too large for ZIP download.');
        }

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

function cleanedFilename(name) {
    const [base, ext] = splitFilename(name || 'file');
    const cleanBase = base || 'file';
    return ext ? `${cleanBase}_clean.${ext}` : `${cleanBase}_clean`;
}

function uniqueFilename(name, usedNames) {
    const [base, ext] = splitFilename(name);
    let candidate = name;
    let index = 2;

    while (usedNames.has(candidate.toLowerCase())) {
        candidate = ext ? `${base}-${index}.${ext}` : `${base}-${index}`;
        index += 1;
    }

    usedNames.add(candidate.toLowerCase());
    return candidate;
}

function extensionFor(name) {
    const index = name.lastIndexOf('.');
    if (index <= 0 || index === name.length - 1) {
        return '';
    }
    return name.slice(index + 1).toLowerCase();
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
    pdf: 'application/pdf',
    docx: 'application/vnd.openxmlformats-officedocument.wordprocessingml.document',
    xlsx: 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet',
    pptx: 'application/vnd.openxmlformats-officedocument.presentationml.presentation'
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

function safeClassName(text) {
    return String(text).toLowerCase().replace(/[^a-z0-9_-]/g, '-');
}

function registerServiceWorker() {
    if (!('serviceWorker' in navigator) || location.protocol === 'file:') {
        return;
    }

    navigator.serviceWorker.register(`${basePath}/sw.js`).catch((e) => {
        console.warn('Service worker registration failed:', e);
    });
}
