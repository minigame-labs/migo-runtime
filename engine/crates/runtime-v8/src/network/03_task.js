import { primordials } from "ext:core/mod.js";
import { createListenerGroup, invokeCallback } from "ext:host_v8_base/02_async.js";
const { TypeError } = primordials;

class NetworkTask {
    constructor(terminator) {
        this._aborted = false;
        this._headersReceivedListeners = createListenerGroup('Error in headers received');
        this._terminator = terminator;
    }

    abort() {
        if (this._aborted) {
            return;
        }
        this._aborted = true;
        this._terminator?.abort();
        this._headersReceivedListeners.off();
        this._onCleanup();
    }

    /** Override in subclasses for additional cleanup on abort. */
    _onCleanup() {}

    onHeadersReceived(listener) {
        if (typeof listener !== 'function') {
            throw new TypeError('Listener must be a function');
        }
        if (this._aborted) {
            return;
        }
        this._headersReceivedListeners.on(listener);
    }

    offHeadersReceived(listener) {
        if (listener !== undefined && typeof listener !== 'function') return;
        this._headersReceivedListeners.off(listener);
    }

    _triggerHeadersReceived(headers) {
        if (this._aborted) {
            return;
        }
        this._headersReceivedListeners.trigger(headers);
    }

    toJSON() {
        return {};
    }
}

/**
 * Deliver the outcome of one operation to the app's callbacks: one of `success` and `fail`, then
 * `complete`, once, each isolated from the others.
 *
 * The callbacks are the app's functions, not ours. A `success` that throws is the app's bug in
 * handling a request that succeeded: it must not be reported to the app's `fail` as though the
 * request had failed (it was, when the delivery sat inside the `try` that reports a failed read), and
 * must not stop `complete`. And an operation is settled once: whatever else goes wrong after the
 * outcome was delivered is not a second outcome.
 */
function createSettler(apiName, options) {
    const { success, fail, complete } = options;
    let settled = false;
    const settle = (kind, handler, res) => {
        if (settled) return;
        settled = true;
        invokeCallback(apiName, kind, handler, res);
        invokeCallback(apiName, 'complete', complete, res);
    };
    return {
        succeed(res) { settle('success', success, res); },
        fail(res) { settle('fail', fail, res); },
    };
}

/** The `[name, value]` pairs of a header object; anything that is not an object has none. */
function headerEntries(header) {
    if (header === null || typeof header !== 'object') return [];
    return Object.entries(header).map(([key, value]) => [key, String(value)]);
}

export { NetworkTask, createSettler, headerEntries };
