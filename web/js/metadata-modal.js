export const MODAL_LOADING_MESSAGE = 'Reading metadata. Results will appear when analysis completes.';
export const MODAL_PROCESSING_MESSAGE = 'Cleaning and re-scanning this file. Verification will appear when processing completes.';
export const MODAL_ERROR_MESSAGE = 'Unable to process this file.';

export function renderMetadataModalInto(file, modalBody, renderers) {
    modalBody.dataset.fileId = file.id;

    if (file.status === 'error') {
        modalBody.innerHTML = renderers.renderError(file.errorMessage || MODAL_ERROR_MESSAGE);
    } else if (file.status === 'loading') {
        modalBody.innerHTML = renderers.renderPending(MODAL_LOADING_MESSAGE);
    } else if (file.status === 'processing') {
        modalBody.innerHTML = renderers.renderPending(MODAL_PROCESSING_MESSAGE);
    } else if (file.cleanedData) {
        modalBody.innerHTML = renderers.renderCleaned(file);
    } else if ((file.metadata?.metadata_found ?? []).length === 0) {
        modalBody.innerHTML = renderers.renderNoMetadata(file, 'No removable metadata found in this file.');
    } else {
        modalBody.innerHTML = renderers.renderMetadataDetails(
            file,
            file.metadata,
            'Detected metadata',
            'Total metadata bytes'
        );
    }
}

export function resetMetadataModalElement(modal, modalBody, { lastFocusedElement = null, restoreFocus = true } = {}) {
    const wasOpen = !modal.classList.contains('hidden');
    modal.classList.add('hidden');
    delete modalBody.dataset.fileId;
    modalBody.innerHTML = '';

    if (wasOpen && restoreFocus && lastFocusedElement && typeof lastFocusedElement.focus === 'function') {
        lastFocusedElement.focus();
    }
}
