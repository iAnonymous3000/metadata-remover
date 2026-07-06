import {
    applyAnalysisFailure,
    applyProcessFailure,
    applyProcessSuccess,
    clearFileCollection,
    removeFileRecord
} from './file-lifecycle.js';
import { cleanedFilename, uniqueFilename } from './download-names.js';
import {
    groupMetadataEntriesByCategory,
    metadataSummary
} from './metadata-summary.js';
import {
    imageDimensionsFromBlob,
    imageDimensionsFromBytes,
    retainedFileBytesForBudget,
    sourceFileBytesForBudget,
    VISUAL_PIXEL_COMPARE_MAX,
    VISUAL_PROOF_BUDGET_OPTIONS,
    VISUAL_THUMBNAIL_MAX_HEIGHT,
    VISUAL_THUMBNAIL_MAX_WIDTH,
    visualVerificationTransientBytesForBudget
} from './memory-budget.js';
import { renderMetadataModalInto, resetMetadataModalElement } from './metadata-modal.js';
import { createSampleJpegFile } from './sample-jpeg.js';
import {
    budgetSkippedVisualProof,
    createVisualVerificationQueue,
    isBudgetSkippedVisualProof,
    isUnavailableVisualProof,
    unavailableVisualProof
} from './visual-verification-queue.js';

// Detect base path for GitHub Pages (handles /repo-name/ subpath).
const scriptUrl = import.meta.url;
const basePath = scriptUrl.substring(0, scriptUrl.lastIndexOf('/js/'));
// Best-effort direct-open guard for the GitHub Pages build. A real
// clickjacking block requires a frame-ancestors response header.
const isFramed = window.self !== window.top;
const MAX_FILE_BYTES = 100 * 1024 * 1024;
const BASE_MEMORY_BUDGET_BYTES = 512 * 1024 * 1024;
const MIN_MEMORY_BUDGET_BYTES = 128 * 1024 * 1024;
const VISUAL_VERIFICATION_CONCURRENCY = 1;
const VISUAL_UNAVAILABLE_RETRY_LIMIT = 1;
const ACTIVE_WORKER_SOURCE_COPY_FACTOR = 2;
const VISUAL_RASTER_TYPES = new Set(['jpeg', 'png', 'webp', 'avif', 'gif']);

let wasmReady = false;
let requestId = 0;
const files = new Map();
const pendingRequests = new Map();
let queuedFiles = [];
let addFilesChain = Promise.resolve();
let fileCollectionGeneration = 0;
let worker = null;
let workerReadyPromise = Promise.resolve();
let resolveWorkerReady = null;
let rejectWorkerReady = null;
let workerRestarting = false;
let processorUnavailable = false;
let dragDepth = 0;
let activeVisualVerificationTransientBytes = 0;
let budgetRetryTimer = null;
let zipDownloadInProgress = false;
let activeZipRequestId = null;
const visualVerificationQueue = createVisualVerificationQueue({
    concurrency: VISUAL_VERIFICATION_CONCURRENCY
});

// DOM Elements
const dropZone = document.getElementById('drop-zone');
const dropFeedback = document.getElementById('drop-feedback');
const fileInput = document.getElementById('file-input');
const fileList = document.getElementById('file-list');
const actions = document.getElementById('actions');
const processBtn = document.getElementById('process-btn');
const downloadAllBtn = document.getElementById('download-all-btn');
const clearBtn = document.getElementById('clear-btn');
const sampleBtn = document.getElementById('sample-btn');
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
    processorUnavailable = false;
    fileInput.disabled = false;
    if (sampleBtn) sampleBtn.disabled = false;
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
        workerRestarting = false;
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
        workerRestarting = false;
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
    if (processorUnavailable) return;
    if (e.target.tagName !== 'LABEL') fileInput.click();
});
window.addEventListener('dragenter', handleDragEnter);
window.addEventListener('dragover', handleDragOver);
window.addEventListener('dragleave', handleDragLeave);
window.addEventListener('drop', handleDrop);
fileInput.addEventListener('change', handleFileSelect);
processBtn.addEventListener('click', processAllFiles);
downloadAllBtn.addEventListener('click', downloadAllFiles);
clearBtn.addEventListener('click', clearAllFiles);
sampleBtn?.addEventListener('click', loadSampleFile);
modalClose.addEventListener('click', closeModal);
modal.addEventListener('click', (e) => {
    if (e.target === modal) closeModal();
});
document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') closeModal();
    if (e.key === 'Tab' && !modal.classList.contains('hidden')) trapModalFocus(e);
});
dropZone.addEventListener('keydown', (e) => {
    if (processorUnavailable) return;
    if (e.key === 'Enter' || e.key === ' ') {
        e.preventDefault();
        fileInput.click();
    }
});

function handleWorkerMessage(event) {
    const message = event.data;

    if (message.type === 'ready') {
        wasmReady = true;
        processorUnavailable = false;
        workerRestarting = false;
        fileInput.disabled = false;
        if (sampleBtn) sampleBtn.disabled = false;
        dropZone.classList.add('ready');
        resolveWorkerReady?.();
        registerServiceWorker();
        updateActions();
        flushQueuedFiles();
        return;
    }

    if (message.type === 'fatal') {
        const error = new Error(message.error || 'Failed to start the file processor.');
        wasmReady = false;
        workerRestarting = false;
        dropZone.classList.remove('ready');
        rejectWorkerReady?.(error);
        rejectPendingRequests(error);
        showLoadError(error.message);
        return;
    }

    if (!message.requestId || !pendingRequests.has(message.requestId)) {
        return;
    }

    const pending = pendingRequests.get(message.requestId);
    if (message.type === 'zip-progress') {
        pending.onProgress?.(message);
        return;
    }

    const { resolve, reject } = pending;
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

async function sendWorkerMessage(type, payload = {}, {
    onProgress = null,
    onRequestId = null
} = {}) {
    if (!wasmReady) {
        await workerReadyPromise;
    }

    return new Promise((resolve, reject) => {
        const id = ++requestId;
        pendingRequests.set(id, { resolve, reject, onProgress });
        onRequestId?.(id);
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
    processorUnavailable = true;
    queuedFiles = [];
    fileCollectionGeneration += 1;
    fileInput.disabled = true;
    if (sampleBtn) sampleBtn.disabled = true;
    clearDragState();
    dropZone.innerHTML = `
        <div class="drop-zone-content error">
            <p>${escapeHtml(message)}</p>
        </div>
    `;
    updateActions();
}

function handleDragEnter(e) {
    if (processorUnavailable) {
        if (isFileDrag(e)) e.preventDefault();
        return;
    }
    if (!isFileDrag(e)) return;
    e.preventDefault();
    dragDepth += 1;
    document.body.classList.add('file-drag-active');
    dropZone.classList.add('drag-over');
}

function handleDragOver(e) {
    if (processorUnavailable) {
        if (isFileDrag(e)) {
            e.preventDefault();
            if (e.dataTransfer) e.dataTransfer.dropEffect = 'none';
        }
        return;
    }
    if (!isFileDrag(e)) return;
    e.preventDefault();
    if (e.dataTransfer) {
        e.dataTransfer.dropEffect = 'copy';
    }
    document.body.classList.add('file-drag-active');
    dropZone.classList.add('drag-over');
}

function handleDragLeave(e) {
    if (!isFileDrag(e)) return;
    e.preventDefault();
    dragDepth = Math.max(0, dragDepth - 1);
    if (dragDepth === 0 || didDragLeaveWindow(e)) clearDragState();
}

function handleDrop(e) {
    if (processorUnavailable) {
        if (isFileDrag(e)) {
            e.preventDefault();
            clearDragState();
            showDropFeedback('The local file processor is unavailable. Refresh the page to try again.');
        }
        return;
    }
    if (!isFileDrag(e)) return;
    e.preventDefault();
    const droppedFiles = Array.from(e.dataTransfer?.files || []);
    clearDragState();
    if (droppedFiles.length > 0) addFiles(droppedFiles);
}

function isFileDrag(e) {
    return Array.from(e.dataTransfer?.types || []).includes('Files');
}

function didDragLeaveWindow(e) {
    return e.clientX <= 0
        || e.clientY <= 0
        || e.clientX >= window.innerWidth
        || e.clientY >= window.innerHeight;
}

function clearDragState() {
    dragDepth = 0;
    document.body.classList.remove('file-drag-active');
    dropZone.classList.remove('drag-over');
}

function handleFileSelect(e) {
    if (processorUnavailable) {
        fileInput.value = '';
        return;
    }
    addFiles(Array.from(e.target.files));
    fileInput.value = '';
}

async function loadSampleFile() {
    if (!sampleBtn || processorUnavailable) return;

    sampleBtn.disabled = true;
    try {
        clearDropFeedback();
        const sampleFile = await createSampleJpegFile();
        await addFiles([sampleFile]);
    } catch (e) {
        showDropFeedback(e.message || 'Unable to create the sample JPEG.');
    } finally {
        sampleBtn.disabled = processorUnavailable;
    }
}

function addFiles(newFiles) {
    const filesToAdd = Array.from(newFiles);
    if (processorUnavailable) {
        showDropFeedback('The local file processor is unavailable. Refresh the page to try again.');
        return Promise.resolve();
    }
    if (!wasmReady) {
        queueFiles(filesToAdd);
        return Promise.resolve();
    }

    const generation = fileCollectionGeneration;
    addFilesChain = addFilesChain.then(
        () => addFilesNow(filesToAdd, generation),
        () => addFilesNow(filesToAdd, generation)
    );
    return addFilesChain;
}

async function addFilesNow(newFiles, generation) {
    if (generation !== fileCollectionGeneration) {
        return;
    }

    const filesToCheck = Array.from(newFiles);
    const memoryFiltered = filterByMemoryBudget(filesToCheck, candidateSourceFileBytes);
    const filesToAnalyze = memoryFiltered.accepted;

    const feedbackMessages = [];
    if (memoryFiltered.rejected.length > 0) {
        feedbackMessages.push(memoryLimitMessage(memoryFiltered.rejected, memoryFiltered.budget));
    }

    if (feedbackMessages.length > 0) {
        showDropFeedback(feedbackMessages.join(' '));
    } else {
        clearDropFeedback();
    }

    for (const file of filesToAnalyze) {
        if (generation !== fileCollectionGeneration) {
            break;
        }

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
            visualProof: null,
            visualProofPromise: null,
            visualProofGeneration: 0,
            visualProofAbortController: null,
            visualUnavailableRetryCount: 0,
            visualSourceFile: null,
            visualSourceDimensions: null,
            cleanedVisualDimensions: null,
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
            if (generation !== fileCollectionGeneration) {
                postWorkerControl({ type: 'forget', id });
                continue;
            }
            const current = files.get(id);
            if (!current) {
                postWorkerControl({ type: 'forget', id });
                continue;
            }
            current.type = result.fileType;
            current.metadata = result.metadata;
            current.originalMetadata = result.metadata;
            current.status = 'pending';
        } catch (e) {
            if (generation !== fileCollectionGeneration) {
                postWorkerControl({ type: 'forget', id });
                continue;
            }
            const current = files.get(id);
            if (!current) {
                postWorkerControl({ type: 'forget', id });
                continue;
            }
            postWorkerControl(applyAnalysisFailure(current, e));
        }

        upsertFileRow(id);
        refreshMetadataModal(id);
        updateActions();
    }

    if (generation === fileCollectionGeneration) {
        announce(addedSummary(filesToAnalyze.length));
    }
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
    let total = activeVisualVerificationTransientBytes
        + queuedFiles.reduce((sum, file) => sum + queuedSourceFileBytes(file), 0);
    for (const file of files.values()) {
        total += retainedFileBytesForBudget(file, VISUAL_PROOF_BUDGET_OPTIONS);
    }
    return total;
}

function queuedSourceFileBytes(file) {
    if (file.size > MAX_FILE_BYTES) {
        return 0;
    }
    return file.size;
}

function candidateSourceFileBytes(file) {
    return file.size > MAX_FILE_BYTES ? 0 : file.size * ACTIVE_WORKER_SOURCE_COPY_FACTOR;
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

function showZipProgress({
    completedEntries = 0,
    totalEntries = 0,
    processedBytes = 0,
    totalBytes = 0
} = {}) {
    const fileText = totalEntries === 1 ? 'file' : 'files';
    const boundedCompleted = Math.min(completedEntries, totalEntries);
    const fileProgress = totalEntries > 0
        ? `${boundedCompleted} of ${totalEntries} ${fileText}`
        : 'files';
    const byteProgress = totalBytes > 0
        ? `, ${formatSize(Math.min(processedBytes, totalBytes))} of ${formatSize(totalBytes)}`
        : '';
    showDropFeedback(`Preparing ZIP download (${fileProgress}${byteProgress})...`);
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

    if (file.status === 'loading') {
        return `
            <span>${formatSize(file.size)}</span>
            <span>Reading metadata</span>
        `;
    }

    if (file.status === 'done') {
        return `
            <span>${formatSize(file.size)} -> ${formatSize(file.cleanedSize)}</span>
            <span class="metadata-count clean">Verified clean</span>
            ${renderVisualProofMetaText(file)}
        `;
    }

    if (file.status === 'warning') {
        const remainingEntries = file.verification?.metadata_found ?? [];
        return `
            <span>${formatSize(file.size)} -> ${formatSize(file.cleanedSize)}</span>
            <span class="metadata-count">${escapeHtml(reviewSummary(remainingEntries))}</span>
            ${renderVisualProofMetaText(file)}
        `;
    }

    const metaCount = file.metadata.metadata_found.length;
    const isClean = metaCount === 0;
    return `
        <span>${formatSize(file.size)}</span>
        <span class="metadata-count ${isClean ? 'clean' : ''}">
            ${escapeHtml(isClean ? 'No removable metadata found' : metadataSummary(file.metadata.metadata_found))}
        </span>
    `;
}

function renderVisualProofMetaText(file) {
    if (!isVisualVerificationType(file.type) || !file.cleanedData) {
        return '';
    }
    if (file.visualProofPromise) {
        return '<span class="metadata-count visual-pending">Visual check pending</span>';
    }
    if (isBudgetSkippedVisualProof(file.visualProof)) {
        return file.visualSourceFile
            ? '<span class="metadata-count visual-warning">Visual check waiting for memory</span>'
            : '<span class="metadata-count visual-warning">Visual check skipped</span>';
    }
    if (!file.visualProof) {
        return '';
    }
    if (['matched', 'cleaned-only', 'preview-only'].includes(file.visualProof.status)) {
        return '<span class="metadata-count clean">Visual check complete</span>';
    }
    return '<span class="metadata-count visual-warning">Visual check review</span>';
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
    processBtn.disabled = !hasFiles || !wasmReady || !hasPending || busy || zipDownloadInProgress;
    downloadAllBtn.disabled = !hasFiles || !hasCleaned || busy || zipDownloadInProgress;
}

async function processAllFiles() {
    processBtn.disabled = true;

    for (const [id, file] of files) {
        if (file.status !== 'pending') continue;

        file.status = 'processing';
        upsertFileRow(id);
        refreshMetadataModal(id);
        updateActions();

        try {
            const result = await sendWorkerMessage('process', { id, file: file.sourceFile });
            const current = files.get(id);
            if (!current) continue;
            applyProcessSuccess(current, result, isVisualVerificationType);
            scheduleVisualVerification(current, { requireHeadroom: true });
        } catch (e) {
            const current = files.get(id);
            if (!current) continue;
            postWorkerControl(applyProcessFailure(current, e));
        }

        upsertFileRow(id);
        refreshMetadataModal(id);
        updateActions();
    }

    announce(processedSummary());
}

function clearAllFiles() {
    fileCollectionGeneration += 1;
    cancelActiveZipDownload();
    queuedFiles = [];
    clearBudgetRetryTimer();
    clearFileCollection(files, {
        revokeVisualProof,
        postWorkerControl,
        clearDropFeedback,
        resetMetadataModal,
        renderFileList,
        updateActions
    });
}

function removeFile(id) {
    removeFileRecord(files, id, {
        revokeVisualProof,
        postWorkerControl,
        fileRowElement,
        shouldResetMetadataModal: (fileId) => modalBody.dataset.fileId === fileId,
        resetMetadataModal,
        updateActions
    });
    retryBudgetSkippedVisualVerificationsSoon();
}

function cancelActiveZipDownload() {
    if (activeZipRequestId !== null) {
        postWorkerControl({ type: 'cancel-zip', requestId: activeZipRequestId });
    }
}

function showMetadata(id) {
    const file = files.get(id);
    if (!file) return;

    if (file.cleanedData) {
        scheduleVisualVerification(file, { force: true, requireHeadroom: true });
    }
    renderMetadataModal(file);

    modal.classList.remove('hidden');
    lastFocusedElement = document.activeElement;
    modalClose.focus();
}

function renderMetadataModal(file) {
    renderMetadataModalInto(file, modalBody, {
        renderError: renderModalMessage,
        renderPending: renderPendingMetadata,
        renderCleaned: renderCleanedMetadataDetails,
        renderNoMetadata,
        renderMetadataDetails
    });
}

function refreshMetadataModal(id) {
    if (modal.classList.contains('hidden') || modalBody.dataset.fileId !== id) {
        return;
    }
    const file = files.get(id);
    if (file) renderMetadataModal(file);
}

function renderCleanedMetadataDetails(file) {
    const originalEntries = file.originalMetadata?.metadata_found ?? [];
    const remainingEntries = file.verification?.metadata_found ?? [];
    const remainingKeys = new Set(remainingEntries.map(metadataEntryKey));
    const removedEntries = originalEntries.filter((entry) => !remainingKeys.has(metadataEntryKey(entry)));
    const fileType = file.type || file.metadata.file_type || file.originalMetadata?.file_type;
    const remainingTitle = hasLimitedVerification(remainingEntries)
        ? 'Review after cleaning'
        : 'Still present after cleaning';

    if (originalEntries.length === 0 && remainingEntries.length === 0) {
        return renderNoMetadata(file, 'No removable metadata was found before or after cleaning.');
    }

    return `
        ${renderVisualVerification(file)}
        ${renderPreservedMetadataNote(fileType)}
        ${renderEntrySection('Removed', removedEntries, 'No metadata entries were removed.')}
        ${renderEntrySection(remainingTitle, remainingEntries, 'No removable metadata remains after cleaning.')}
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
        ${file.cleanedData ? renderVisualVerification(file) : ''}
        <div class="no-metadata">${escapeHtml(message)}</div>
        ${renderPreservedMetadataNote(file.type || file.metadata.file_type)}
    `;
}

function renderModalMessage(message) {
    return `<div class="no-metadata">${escapeHtml(message)}</div>`;
}

function renderPendingMetadata(message) {
    return renderModalMessage(message);
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

    return Array.from(groupMetadataEntriesByCategory(entries), ([category, categoryEntries]) => `
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

function isLimitedVerificationEntry(entry) {
    return String(entry.category || '').toLowerCase() === 'limited verification';
}

function hasLimitedVerification(entries) {
    return entries.some(isLimitedVerificationEntry);
}

function reviewSummary(entries) {
    const metadataEntries = entries.filter((entry) => !isLimitedVerificationEntry(entry));
    const hasLimited = metadataEntries.length !== entries.length;

    if (metadataEntries.length === 0) {
        return hasLimited ? 'Limited verification' : 'Review recommended';
    }

    const summary = `${metadataSummary(metadataEntries)} still present`;
    return hasLimited ? `${summary} / limited verification` : summary;
}

function scheduleVisualVerification(file, { force = false, requireHeadroom = false } = {}) {
    const visualPromise = ensureVisualVerification(file, { force, requireHeadroom });
    if (visualPromise) {
        upsertFileRow(file.id);
    }
    visualPromise?.finally(() => {
        upsertFileRow(file.id);
        refreshMetadataModal(file.id);
        retryBudgetSkippedVisualVerificationsSoon();
    }).catch(() => {});
    return visualPromise;
}

function ensureVisualVerification(file, { force = false, requireHeadroom = false } = {}) {
    if (!isVisualVerificationType(file.type) || !file.cleanedData) {
        return null;
    }

    const retryBudgetSkip = force && isBudgetSkippedVisualProof(file.visualProof) && !!file.visualSourceFile;
    const retryUnavailable = canRetryUnavailableVisualProof(file, force);
    if (file.visualProofPromise && !retryBudgetSkip && !retryUnavailable) {
        return file.visualProofPromise;
    }

    if (file.visualProof) {
        if (retryBudgetSkip) {
            revokeVisualProof(file, { keepVisualSource: true });
        } else if (retryUnavailable) {
            file.visualUnavailableRetryCount += 1;
            revokeVisualProof(file, { keepVisualSource: true });
        } else {
            return null;
        }
    }

    const fileId = file.id;
    const generation = (file.visualProofGeneration ?? 0) + 1;
    file.visualProofGeneration = generation;
    file.visualProofAbortController?.abort();
    const abortController = new AbortController();
    file.visualProofAbortController = abortController;
    let visualProofPromise;
    visualProofPromise = visualVerificationQueue.enqueue(({ signal }) => (
        runVisualVerification(fileId, { requireHeadroom, generation, signal })
    ), { signal: abortController.signal })
        .finally(() => {
            const current = files.get(fileId);
            if (
                current?.visualProofPromise === visualProofPromise
                && current.visualProofGeneration === generation
            ) {
                current.visualProofPromise = null;
                if (current.visualProofAbortController === abortController) {
                    current.visualProofAbortController = null;
                }
                if (!shouldRetainVisualSourceAfterProof(current)) {
                    current.visualSourceFile = null;
                    current.visualSourceDimensions = null;
                }
            }
        });
    file.visualProofPromise = visualProofPromise;
    return file.visualProofPromise;
}

function canRetryUnavailableVisualProof(file, force) {
    return !!force
        && isUnavailableVisualProof(file.visualProof)
        && !!file.visualSourceFile
        && (file.visualUnavailableRetryCount ?? 0) < VISUAL_UNAVAILABLE_RETRY_LIMIT;
}

function shouldRetainVisualSourceAfterProof(file) {
    if (isBudgetSkippedVisualProof(file.visualProof)) {
        return !!file.visualSourceFile;
    }
    return isUnavailableVisualProof(file.visualProof)
        && !!file.visualSourceFile
        && (file.visualUnavailableRetryCount ?? 0) < VISUAL_UNAVAILABLE_RETRY_LIMIT;
}

async function runVisualVerification(fileId, { requireHeadroom = false, generation = 0, signal = null } = {}) {
    const file = files.get(fileId);
    if (signal?.aborted || !isCurrentVisualProofJob(file, generation) || file.visualProof || !isVisualVerificationType(file.type) || !file.cleanedData) {
        return;
    }

    const dimensionsReady = await refreshVisualVerificationDimensions(file, generation);
    if (signal?.aborted || !dimensionsReady) {
        return;
    }

    if (requireHeadroom && !hasVisualVerificationHeadroom(file)) {
        skipVisualVerificationForBudget(file);
        return;
    }

    const transientBytes = visualVerificationTransientBytesForBudget(file, VISUAL_PROOF_BUDGET_OPTIONS);
    activeVisualVerificationTransientBytes += transientBytes;
    let proof = null;
    let assigned = false;
    try {
        proof = await prepareVisualVerification(file);
        if (signal?.aborted || !proof) {
            revokeVisualProofObject(proof);
            return;
        }

        const current = files.get(fileId);
        if (
            !isCurrentVisualProofJob(current, generation)
            || current.visualProof
            || !isVisualVerificationType(current.type)
            || !current.cleanedData
        ) {
            revokeVisualProofObject(proof);
            return;
        }

        current.visualProof = proof;
        assigned = true;
    } finally {
        activeVisualVerificationTransientBytes = Math.max(
            0,
            activeVisualVerificationTransientBytes - transientBytes
        );
        if (!files.has(fileId) && !assigned) {
            revokeVisualProofObject(proof);
        }
    }
}

function isCurrentVisualProofJob(file, generation) {
    return !!file && file.visualProofGeneration === generation;
}

function hasVisualVerificationHeadroom(file) {
    const transientBytes = visualVerificationTransientBytesForBudget(file, VISUAL_PROOF_BUDGET_OPTIONS);
    return currentMemoryBytes() + transientBytes <= memoryBudgetBytes();
}

async function refreshVisualVerificationDimensions(file, generation) {
    if (!VISUAL_RASTER_TYPES.has(file.type)) {
        return isCurrentVisualProofJob(files.get(file.id), generation);
    }

    const sourceFile = file.visualSourceFile || file.sourceFile;
    const [sourceDimensions, cleanedDimensions] = await Promise.all([
        safeImageDimensionsFromBlob(file.type, sourceFile),
        safeImageDimensionsFromBytes(file.type, file.cleanedData)
    ]);
    const current = files.get(file.id);
    if (!isCurrentVisualProofJob(current, generation)) {
        return false;
    }

    current.visualSourceDimensions = sourceDimensions || current.visualSourceDimensions;
    current.cleanedVisualDimensions = cleanedDimensions || current.cleanedVisualDimensions;
    return true;
}

async function safeImageDimensionsFromBlob(fileType, blob) {
    try {
        return await imageDimensionsFromBlob(fileType, blob);
    } catch {
        return null;
    }
}

function safeImageDimensionsFromBytes(fileType, data) {
    try {
        return imageDimensionsFromBytes(fileType, data);
    } catch {
        return null;
    }
}

function skipVisualVerificationForBudget(file) {
    revokeVisualProof(file, { keepVisualSource: true });
    file.visualProof = budgetSkippedVisualProof();
}

function retryBudgetSkippedVisualVerificationsSoon() {
    if (budgetRetryTimer !== null) {
        return;
    }
    budgetRetryTimer = setTimeout(() => {
        budgetRetryTimer = null;
        retryBudgetSkippedVisualVerifications();
    }, 0);
}

function clearBudgetRetryTimer() {
    if (budgetRetryTimer === null) {
        return;
    }
    clearTimeout(budgetRetryTimer);
    budgetRetryTimer = null;
}

function retryBudgetSkippedVisualVerifications() {
    if (visualVerificationQueue.activeCount > 0 || visualVerificationQueue.pendingCount > 0) {
        return;
    }

    const skippedFiles = Array.from(files.values()).filter((file) => (
        isBudgetSkippedVisualProof(file.visualProof)
        && file.visualSourceFile
        && !file.visualProofPromise
    ));

    for (const file of skippedFiles) {
        if (hasVisualVerificationHeadroom(file)) {
            scheduleVisualVerification(file, { force: true, requireHeadroom: true });
            return;
        }
    }

    const candidate = skippedFiles[0];
    if (!candidate) {
        return;
    }

    for (const file of skippedFiles.slice(1)) {
        releaseBudgetSkippedVisualSource(file);
        upsertFileRow(file.id);
        refreshMetadataModal(file.id);
        if (hasVisualVerificationHeadroom(candidate)) {
            scheduleVisualVerification(candidate, { force: true, requireHeadroom: true });
            return;
        }
    }
}

function releaseBudgetSkippedVisualSource(file) {
    file.visualSourceFile = null;
    file.visualSourceDimensions = null;
}

async function prepareVisualVerification(file) {
    if (!isVisualVerificationType(file.type) || !file.cleanedData) {
        return null;
    }

    try {
        if (file.type === 'svg') {
            const cleanedSnapshot = await imageSnapshotFromBlob(
                new Blob([file.cleanedData], { type: getMimeType(file.type) })
            );
            delete cleanedSnapshot.image;
            return {
                status: 'cleaned-only',
                summary: 'Cleaned SVG preview rendered',
                detail: 'Raw SVG preview is skipped; the cleaned SVG is rendered locally after active content and external references are removed.',
                cleanedSnapshot
            };
        }

        const originalFile = file.visualSourceFile || file.sourceFile;
        if (!originalFile) {
            throw new Error('Original file data is no longer available for visual verification.');
        }

        let originalSnapshot;
        let cleanedSnapshot;
        try {
            originalSnapshot = await imageSnapshotFromBlob(originalFile);
            cleanedSnapshot = await imageSnapshotFromBlob(
                new Blob([file.cleanedData], { type: getMimeType(file.type) })
            );
            const proof = compareImageSnapshots(originalSnapshot, cleanedSnapshot, file.type);
            delete originalSnapshot.image;
            delete cleanedSnapshot.image;
            return {
                ...proof,
                originalSnapshot,
                cleanedSnapshot
            };
        } catch (error) {
            revokeVisualSnapshot(originalSnapshot);
            revokeVisualSnapshot(cleanedSnapshot);
            throw error;
        }
    } catch (error) {
        return unavailableVisualProof(visualVerificationError(file.type, error));
    }
}

function isVisualVerificationType(fileType) {
    return VISUAL_RASTER_TYPES.has(fileType) || fileType === 'svg';
}

async function imageSnapshotFromBlob(blob) {
    const url = URL.createObjectURL(blob);
    try {
        const image = await loadImage(url);
        const width = image.naturalWidth || image.width;
        const height = image.naturalHeight || image.height;
        if (!width || !height) {
            throw new Error('Browser decoded an empty image.');
        }
        return {
            width,
            height,
            thumbnailUrl: await createThumbnailUrl(image, width, height),
            image
        };
    } finally {
        URL.revokeObjectURL(url);
    }
}

function loadImage(url) {
    return new Promise((resolve, reject) => {
        const image = new Image();
        image.decoding = 'async';
        image.onload = () => resolve(image);
        image.onerror = () => reject(new Error('Browser could not render the image preview.'));
        image.src = url;
    });
}

async function createThumbnailUrl(image, sourceWidth, sourceHeight) {
    const scale = Math.min(
        1,
        VISUAL_THUMBNAIL_MAX_WIDTH / sourceWidth,
        VISUAL_THUMBNAIL_MAX_HEIGHT / sourceHeight
    );
    const width = Math.max(1, Math.round(sourceWidth * scale));
    const height = Math.max(1, Math.round(sourceHeight * scale));
    const canvas = document.createElement('canvas');
    canvas.width = width;
    canvas.height = height;
    const ctx = canvas.getContext('2d', { alpha: true });
    if (!ctx) {
        throw new Error('Browser could not create a preview canvas.');
    }
    ctx.drawImage(image, 0, 0, width, height);
    return canvasToObjectUrl(canvas);
}

function canvasToObjectUrl(canvas) {
    return new Promise((resolve, reject) => {
        canvas.toBlob((blob) => {
            if (!blob) {
                reject(new Error('Browser could not create a preview image.'));
                return;
            }
            resolve(URL.createObjectURL(blob));
        }, 'image/png');
    });
}

function compareImageSnapshots(originalSnapshot, cleanedSnapshot, fileType) {
    if (
        originalSnapshot.width !== cleanedSnapshot.width
        || originalSnapshot.height !== cleanedSnapshot.height
    ) {
        return {
            status: 'changed',
            summary: 'Preview dimensions changed',
            detail: `Original decoded at ${formatDimensions(originalSnapshot)}; cleaned decoded at ${formatDimensions(cleanedSnapshot)}.`
        };
    }

    if (fileType === 'gif') {
        return {
            status: 'preview-only',
            summary: 'First-frame previews match in size',
            detail: 'Animated GIF frames are preserved by the cleaner but not pixel-compared in the browser preview.'
        };
    }

    const pixelCount = originalSnapshot.width * originalSnapshot.height;
    if (pixelCount > VISUAL_PIXEL_COMPARE_MAX) {
        return {
            status: 'preview-only',
            summary: 'Previews match in size',
            detail: `Full-pixel comparison was skipped for this ${formatDimensions(originalSnapshot)} image to keep local memory use bounded.`
        };
    }

    try {
        const matches = decodedPixelsMatch(
            originalSnapshot.image,
            cleanedSnapshot.image,
            originalSnapshot.width,
            originalSnapshot.height
        );
        return matches ? {
            status: 'matched',
            summary: 'Decoded pixels match',
            detail: `Original and cleaned ${formatDimensions(originalSnapshot)} previews decode identically in this browser.`
        } : {
            status: 'changed',
            summary: 'Visual difference detected',
            detail: 'The cleaned image rendered differently from the original in this browser preview.'
        };
    } catch (error) {
        return {
            status: 'preview-only',
            summary: 'Previews loaded',
            detail: `Pixel comparison was unavailable in this browser: ${error.message || 'preview canvas failed'}.`
        };
    }
}

function decodedPixelsMatch(originalImage, cleanedImage, width, height) {
    const canvas = document.createElement('canvas');
    canvas.width = width;
    canvas.height = height;
    const ctx = canvas.getContext('2d', { willReadFrequently: true });
    if (!ctx) {
        throw new Error('Browser could not create a comparison canvas.');
    }

    ctx.drawImage(originalImage, 0, 0, width, height);
    const originalPixels = ctx.getImageData(0, 0, width, height).data;
    const originalCopy = new Uint8ClampedArray(originalPixels);

    ctx.clearRect(0, 0, width, height);
    ctx.drawImage(cleanedImage, 0, 0, width, height);
    const cleanedPixels = ctx.getImageData(0, 0, width, height).data;

    if (originalCopy.length !== cleanedPixels.length) {
        return false;
    }

    for (let i = 0; i < originalCopy.length; i++) {
        if (originalCopy[i] !== cleanedPixels[i]) {
            return false;
        }
    }
    return true;
}

function renderVisualVerification(file) {
    if (!isVisualVerificationType(file.type) || !file.cleanedData) {
        return '';
    }

    const proof = file.visualProof;
    if (!proof) {
        if (!file.visualProofPromise) {
            return '';
        }

        return `
            <div class="metadata-section visual-proof">
                <h3>Visual verification</h3>
                <div class="visual-proof-summary visual-proof-pending">
                    <strong>Preparing local preview</strong>
                    <span>Metadata cleaning and re-scan are complete; the local visual comparison is still running.</span>
                </div>
            </div>
        `;
    }

    const figures = [
        proof.originalSnapshot ? renderVisualFigure('Original', proof.originalSnapshot, file.name) : '',
        proof.cleanedSnapshot ? renderVisualFigure('Cleaned', proof.cleanedSnapshot, cleanedFilename(file.name)) : ''
    ].filter(Boolean).join('');
    const grid = figures ? `<div class="visual-proof-grid">${figures}</div>` : '';

    return `
        <div class="metadata-section visual-proof">
            <h3>Visual verification</h3>
            <div class="visual-proof-summary ${visualProofClass(proof.status)}">
                <strong>${escapeHtml(proof.summary)}</strong>
                <span>${escapeHtml(proof.detail)}</span>
            </div>
            ${grid}
        </div>
    `;
}

function renderVisualFigure(label, snapshot, name) {
    return `
        <figure class="visual-proof-figure">
            <div class="visual-proof-frame">
                <img src="${escapeAttribute(snapshot.thumbnailUrl)}" alt="${escapeAttribute(`${label} preview of ${name}`)}" decoding="async">
            </div>
            <figcaption>
                <span>${escapeHtml(label)}</span>
                <small>${escapeHtml(formatDimensions(snapshot))}</small>
            </figcaption>
        </figure>
    `;
}

function visualProofClass(status) {
    return `visual-proof-${safeClassName(status || 'unknown')}`;
}

function formatDimensions(snapshot) {
    return `${snapshot.width} x ${snapshot.height}px`;
}

function visualVerificationError(fileType, error) {
    const typeLabel = (fileType || 'file').toUpperCase();
    const message = error?.message ? ` ${error.message}` : '';
    return `This browser could not render a local ${typeLabel} preview. Metadata cleaning and re-scan still completed.${message}`;
}

function revokeVisualProof(file, { keepVisualSource = false } = {}) {
    if (!file) {
        return;
    }
    abortVisualVerification(file);
    if (!keepVisualSource) {
        file.visualSourceFile = null;
        file.visualSourceDimensions = null;
    }
    if (!file.visualProof) return;

    revokeVisualProofObject(file.visualProof);
    file.visualProof = null;
}

function abortVisualVerification(file) {
    file.visualProofAbortController?.abort();
    file.visualProofAbortController = null;
}

function revokeVisualProofObject(proof) {
    if (!proof) {
        return;
    }
    for (const snapshot of [proof.originalSnapshot, proof.cleanedSnapshot]) {
        revokeVisualSnapshot(snapshot);
    }
}

function revokeVisualSnapshot(snapshot) {
    if (snapshot?.thumbnailUrl) {
        URL.revokeObjectURL(snapshot.thumbnailUrl);
    }
}

function renderPreservedMetadataNote(fileType) {
    const preserved = {
        jpeg: 'JPEG orientation and color-profile/color-transform data may be kept so photos do not rotate sideways or shift colors.',
        png: 'PNG transparency and color-management chunks are kept so images render the same after cleaning.',
        webp: 'WebP image, alpha, animation, and color-profile chunks are kept so the file still displays correctly.',
        avif: 'AVIF image item data is kept; EXIF/XMP metadata items and Content Credentials boxes are neutralized where the browser parser can identify them.',
        gif: 'GIF frames, transparency controls, plain-text image blocks, and animation loops are kept so animation and appearance are preserved.',
        tiff: 'TIFF image dimensions, orientation, strip/tile offsets, and image data are kept so the raster content remains readable.',
        heic: 'HEIC/HEIF image item data is kept; EXIF/XMP metadata items and Content Credentials boxes are neutralized where the browser parser can identify them.',
        heif: 'HEIC/HEIF image item data is kept; EXIF/XMP metadata items and Content Credentials boxes are neutralized where the browser parser can identify them.',
        mp4: 'MP4 track and media payload boxes are kept; metadata and Content Credentials boxes are replaced with inert free-space boxes to preserve offsets.',
        mov: 'MOV track and media payload boxes are kept; metadata and Content Credentials boxes are replaced with inert free-space boxes to preserve offsets.',
        mp3: 'MP3 audio frames are kept; ID3v1/ID3v2, APEv2, and Lyrics3 tags, comments, private frames, and embedded artwork are removed.',
        flac: 'FLAC audio frames, stream info, seek tables, and padding are kept; Vorbis comments, embedded artwork, application blocks, cue sheets, and ID3/APE tags are removed.',
        wav: 'WAV format and sample data are kept; LIST/INFO tags, Broadcast Wave bext, iXML/XMP, ID3 chunks, cue labels, and other non-audio chunks are removed.',
        ogg: 'OGG audio packets and codec headers are kept; the Vorbis comment header (tags, vendor string, and embedded artwork comments) is replaced with an empty one and pages are renumbered.',
        epub: 'EPUB book content, styles, and required package structure are kept; author/publisher/calibre metadata is removed, required identifiers/timestamps are normalized, and supported embedded images are recursively cleaned. Unreadable embedded files stay in review.',
        svg: 'SVG drawing elements are kept while metadata elements, title/description text, XML comments, processing instructions, active content, event handlers, external references, and editor namespace attributes are removed. Base64 data URIs holding supported raster images are recursively cleaned; other data URI content is preserved and kept in review.'
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

    if (fileType === 'pdf') {
        const note = 'Cleaning PDF review annotations removes author names, timestamps, popup links, and comment text from markup annotations. Visible annotation notes may change.';
        return `
        <div class="metadata-note">
            <strong>May change annotations:</strong> ${escapeHtml(note)}
        </div>
    `;
    }

    return '';
}

function closeModal() {
    resetMetadataModal();
}

function resetMetadataModal({ restoreFocus = true } = {}) {
    resetMetadataModalElement(modal, modalBody, { lastFocusedElement, restoreFocus });
    lastFocusedElement = null;
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

async function downloadAllFiles() {
    if (zipDownloadInProgress) return;

    const cleanedFiles = [...files.values()].filter((file) => file.cleanedData);
    if (cleanedFiles.length === 0) return;

    const usedNames = new Set();
    const entries = cleanedFiles.map((file) => ({
        name: uniqueFilename(cleanedFilename(file.name), usedNames),
        data: new Blob([file.cleanedData])
    }));
    const generation = fileCollectionGeneration;
    zipDownloadInProgress = true;
    showZipProgress({
        completedEntries: 0,
        totalEntries: entries.length,
        processedBytes: 0,
        totalBytes: entries.reduce((sum, entry) => sum + entry.data.size, 0)
    });
    updateActions();

    try {
        const result = await sendWorkerMessage('zip', { entries }, {
            onProgress: showZipProgress,
            onRequestId(id) {
                activeZipRequestId = id;
            }
        });
        if (generation !== fileCollectionGeneration) {
            return;
        }
        const url = URL.createObjectURL(result.blob);
        triggerDownload(url, 'metadata-cleaned.zip');
        clearDropFeedback();
    } catch (e) {
        if (generation === fileCollectionGeneration) {
            showDropFeedback(e.message || 'Unable to create ZIP file.');
        }
    } finally {
        activeZipRequestId = null;
        zipDownloadInProgress = false;
        updateActions();
    }
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
    avif: 'image/avif',
    gif: 'image/gif',
    heic: 'image/heic',
    heif: 'image/heif',
    tiff: 'image/tiff',
    svg: 'image/svg+xml',
    mp4: 'video/mp4',
    mov: 'video/quicktime',
    mp3: 'audio/mpeg',
    flac: 'audio/flac',
    wav: 'audio/wav',
    ogg: 'audio/ogg',
    pdf: 'application/pdf',
    docx: 'application/vnd.openxmlformats-officedocument.wordprocessingml.document',
    xlsx: 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet',
    pptx: 'application/vnd.openxmlformats-officedocument.presentationml.presentation',
    odt: 'application/vnd.oasis.opendocument.text',
    ods: 'application/vnd.oasis.opendocument.spreadsheet',
    odp: 'application/vnd.oasis.opendocument.presentation',
    epub: 'application/epub+zip'
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
