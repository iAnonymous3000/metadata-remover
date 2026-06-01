const BUDGET_SKIP_RETRY_REASON = 'budget';
const UNAVAILABLE_RETRY_REASON = 'unavailable';

export function createVisualVerificationQueue({ concurrency = 1 } = {}) {
    const limit = Math.max(1, Math.floor(Number(concurrency)) || 1);
    const pending = [];
    let activeCount = 0;

    function enqueue(task, { signal = null } = {}) {
        return new Promise((resolve, reject) => {
            if (signal?.aborted) {
                resolve();
                return;
            }

            const job = { task, resolve, reject, signal, abortListener: null };
            job.abortListener = () => {
                const index = pending.indexOf(job);
                if (index === -1) {
                    return;
                }
                pending.splice(index, 1);
                cleanupJob(job);
                resolve();
            };
            signal?.addEventListener('abort', job.abortListener, { once: true });
            pending.push(job);
            drain();
        });
    }

    function drain() {
        while (activeCount < limit && pending.length > 0) {
            const job = pending.shift();
            if (job.signal?.aborted) {
                cleanupJob(job);
                job.resolve();
                continue;
            }

            activeCount += 1;
            Promise.resolve()
                .then(() => (job.signal?.aborted ? undefined : job.task({ signal: job.signal })))
                .then(job.resolve, job.reject)
                .finally(() => {
                    cleanupJob(job);
                    activeCount -= 1;
                    drain();
                });
        }
    }

    function cleanupJob(job) {
        if (job.abortListener) {
            job.signal?.removeEventListener('abort', job.abortListener);
            job.abortListener = null;
        }
    }

    return {
        enqueue,
        get activeCount() {
            return activeCount;
        },
        get concurrency() {
            return limit;
        },
        get pendingCount() {
            return pending.length;
        }
    };
}

export function budgetSkippedVisualProof() {
    return {
        status: 'unavailable',
        summary: 'Visual check skipped',
        detail: 'Automatic visual comparison was skipped to keep local memory use bounded. Metadata cleaning and re-scan still completed.',
        retryReason: BUDGET_SKIP_RETRY_REASON
    };
}

export function isBudgetSkippedVisualProof(proof) {
    return proof?.retryReason === BUDGET_SKIP_RETRY_REASON;
}

export function unavailableVisualProof(detail) {
    return {
        status: 'unavailable',
        summary: 'Visual check unavailable',
        detail,
        retryReason: UNAVAILABLE_RETRY_REASON
    };
}

export function isUnavailableVisualProof(proof) {
    return proof?.retryReason === UNAVAILABLE_RETRY_REASON;
}
