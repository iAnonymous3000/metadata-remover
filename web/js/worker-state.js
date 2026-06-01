export function createWorkerFileState() {
    return {
        fileData: new Map(),
        forgottenIds: new Set(),
        pendingFileRequests: new Map(),
        generation: 0
    };
}

export function handleWorkerControlMessage(state, message) {
    if (!message || typeof message !== 'object') {
        return true;
    }

    if (message.type === 'clear') {
        state.fileData.clear();
        state.forgottenIds.clear();
        state.pendingFileRequests.clear();
        state.generation += 1;
        return true;
    }

    if (message.type === 'forget') {
        if (message.id) {
            state.fileData.delete(message.id);
            if (state.pendingFileRequests.has(message.id)) {
                state.forgottenIds.add(message.id);
            }
        }
        return true;
    }

    return false;
}

export function beginFileMessage(state, message) {
    if (message?.id && isFileDataRequest(message)) {
        state.pendingFileRequests.set(
            message.id,
            (state.pendingFileRequests.get(message.id) || 0) + 1
        );
    }
    return state.generation;
}

export function finishFileMessage(state, message) {
    if (!message?.id || !isFileDataRequest(message)) {
        return;
    }

    const pendingCount = state.pendingFileRequests.get(message.id) || 0;
    if (pendingCount <= 1) {
        state.pendingFileRequests.delete(message.id);
        state.forgottenIds.delete(message.id);
    } else {
        state.pendingFileRequests.set(message.id, pendingCount - 1);
    }
}

export function fileMessageGeneration(state) {
    return state.generation;
}

export function shouldSkipFileMessage(state, message, generation) {
    if (!message?.id || !isFileDataRequest(message)) {
        return false;
    }
    return generation !== state.generation || state.forgottenIds.has(message.id);
}

export function getStoredFileData(state, id) {
    return state.fileData.get(id);
}

export function storeFileData(state, id, data, generation) {
    if (generation !== state.generation || state.forgottenIds.has(id)) {
        return false;
    }
    state.fileData.set(id, data);
    return true;
}

export function deleteStoredFileData(state, id) {
    state.fileData.delete(id);
}

export function clearStoredFileData(state) {
    state.fileData.clear();
}

export function forgetFailedRequestData(state, message) {
    if (message?.id && isFileDataRequest(message)) {
        state.fileData.delete(message.id);
    }
}

function isFileDataRequest(message) {
    return message.type === 'analyze' || message.type === 'process';
}
