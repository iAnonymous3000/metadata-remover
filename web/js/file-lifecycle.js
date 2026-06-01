const noop = () => {};

export function forgetFileMessage(id) {
    return { type: 'forget', id };
}

export function applyAnalysisFailure(file, error) {
    if (error?.fileType && error.fileType !== 'unknown') {
        file.type = error.fileType;
    }
    file.sourceFile = null;
    file.visualSourceFile = null;
    file.visualSourceDimensions = null;
    file.cleanedVisualDimensions = null;
    file.status = 'error';
    file.errorMessage = error?.message;
    return forgetFileMessage(file.id);
}

export function applyProcessSuccess(file, result, isVisualVerificationType) {
    file.cleanedData = new Uint8Array(result.cleanedBuffer);
    file.cleanedSize = file.cleanedData.byteLength;
    file.verification = result.verification;
    file.metadata = result.verification;
    file.visualSourceFile = isVisualVerificationType(file.type) ? file.sourceFile : null;
    file.visualSourceDimensions = null;
    file.cleanedVisualDimensions = null;
    file.sourceFile = null;
    file.status = result.verification.metadata_found.length === 0 ? 'done' : 'warning';
}

export function applyProcessFailure(file, error) {
    file.errorMessage = error?.message;
    file.sourceFile = null;
    file.visualSourceFile = null;
    file.visualSourceDimensions = null;
    file.cleanedVisualDimensions = null;
    file.status = 'error';
    return forgetFileMessage(file.id);
}

export function clearFileCollection(files, {
    revokeVisualProof = noop,
    postWorkerControl = noop,
    clearDropFeedback = noop,
    resetMetadataModal = noop,
    renderFileList = noop,
    updateActions = noop
} = {}) {
    for (const file of files.values()) {
        revokeVisualProof(file);
    }
    files.clear();
    postWorkerControl({ type: 'clear' });
    clearDropFeedback();
    resetMetadataModal({ restoreFocus: false });
    renderFileList();
    updateActions();
}

export function removeFileRecord(files, id, {
    revokeVisualProof = noop,
    postWorkerControl = noop,
    fileRowElement = () => null,
    shouldResetMetadataModal = () => false,
    resetMetadataModal = noop,
    updateActions = noop
} = {}) {
    const file = files.get(id);
    if (file) revokeVisualProof(file);
    files.delete(id);
    postWorkerControl(forgetFileMessage(id));
    fileRowElement(id)?.remove();
    if (shouldResetMetadataModal(id)) {
        resetMetadataModal({ restoreFocus: false });
    }
    updateActions();
}
