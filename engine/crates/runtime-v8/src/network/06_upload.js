import { core, primordials } from "ext:core/mod.js";
import { Header } from "ext:host_v8_network/01_header.js";
import { NetworkTask, createSettler, headerEntries } from "ext:host_v8_network/03_task.js";
import { createListenerGroup, failMessage } from "ext:host_v8_base/02_async.js";
import {
    UploadResponse, UploadErrorResponse, Exception, abortedNetworkError,
} from "ext:host_v8_network/02_response.js";
import { op_fetch_upload, op_fetch_upload_cancel_handle } from "ext:core/ops";

const { TypeError } = primordials;

// -- UploadTask --

class UploadTask extends NetworkTask {
    constructor(terminator) {
        super(terminator);
        this._progressListeners = createListenerGroup('Error in progress');
    }

    _onCleanup() {
        this._progressListeners.off();
    }

    onProgressUpdate(listener) {
        if (typeof listener !== 'function') {
            throw new TypeError('Listener must be a function');
        }
        if (this._aborted) {
            return;
        }
        this._progressListeners.on(listener);
    }

    offProgressUpdate(listener) {
        if (listener !== undefined && typeof listener !== 'function') return;
        this._progressListeners.off(listener);
    }

    _triggerProgress(progress, totalBytesSent, totalBytesExpectedToSend) {
        if (this._aborted) {
            return;
        }
        this._progressListeners.trigger({ progress, totalBytesSent, totalBytesExpectedToSend });
    }
}

// -- Helpers --

function makeError(errno, msg) {
    return new UploadErrorResponse(errno, new Exception(errno, msg, 0));
}

function extractFilename(filePath) {
    const sep = filePath.lastIndexOf('/');
    return sep >= 0 ? filePath.substring(sep + 1) : filePath;
}

// -- uploadFile() --

function uploadFile(options) {
    // `null` and non-objects are a call that names no url, not a throw out of the caller's helper.
    const opts = (options !== null && typeof options === 'object') ? options : {};
    const {
        url, filePath, name, header, formData,
        timeout = 60000,
        enableHttp2 = false,
    } = opts;
    const settler = createSettler('uploadFile', opts);

    // Validate required fields
    if (!url || typeof url !== 'string') {
        const error = makeError(0, "uploadFile:fail invalid url");
        queueMicrotask(() => settler.fail(error));
        return new UploadTask(null);
    }
    if (!filePath || typeof filePath !== 'string') {
        const error = makeError(0, "uploadFile:fail invalid filePath");
        queueMicrotask(() => settler.fail(error));
        return new UploadTask(null);
    }
    if (!name || typeof name !== 'string') {
        const error = makeError(0, "uploadFile:fail invalid name");
        queueMicrotask(() => settler.fail(error));
        return new UploadTask(null);
    }

    const headers = headerEntries(header);
    const formEntries = headerEntries(formData);
    const filename = extractFilename(filePath);

    // Create a cancel handle up front so abort() can interrupt the
    // in-flight request: closing this resource cancels the Rust upload
    // future (which wraps send+read in `.or_cancel`). Without it,
    // abort() only flipped a flag and the upload kept running.
    let cancelHandleRid = null;
    try {
        cancelHandleRid = op_fetch_upload_cancel_handle();
    } catch (_) {
        cancelHandleRid = null;
    }

    const cancellation = {
        aborted: false,
        abort() {
            this.aborted = true;
            if (cancelHandleRid !== null) core.tryClose(cancelHandleRid);
        }
    };

    const uploadTask = new UploadTask(cancellation);

    (async () => {
        try {
            // We deliberately skip the old `op_read_file` + full-buffer
            // roundtrip. The Rust op opens the VFS-resolved file and
            // streams it directly into reqwest's multipart encoder,
            // so a 50 MiB upload no longer pins the whole file in
            // JS heap + Rust Vec at the same time.
            uploadTask._triggerProgress(0, 0, 0);

            const result = await op_fetch_upload(
                cancelHandleRid === null ? 0 : cancelHandleRid,
                url,
                filePath,
                name,
                filename,
                headers,
                formEntries,
                { timeout, enableHttp2 },
            );

            if (cancellation.aborted) throw "aborted";

            if (result.error) {
                settler.fail(makeError(result.statusCode || 0, result.error));
                return;
            }

            // Trigger headers received
            const respHeader = new Header(result.headers, result.statusCode);
            uploadTask._triggerHeadersReceived(respHeader);

            // Final progress at 100%. We don't have a pre-read size
            // any more, so we use the totalBytesSent the Rust op
            // reports after the request completes; if that's zero
            // we still emit a progress=100 tick so callers can close
            // out their state machines.
            const sent = result.totalBytesSent || 0;
            uploadTask._triggerProgress(100, sent, sent);

            settler.succeed(new UploadResponse(result.data, result.statusCode));

        } catch (err) {
            if (cancellation.aborted || err === "aborted") {
                settler.fail(abortedNetworkError());
            } else {
                settler.fail(makeError(500, failMessage("uploadFile", err)));
            }
        } finally {
            // Release the cancel handle on every exit path (success,
            // failure, or abort). tryClose is a no-op if abort() already
            // closed it.
            if (cancelHandleRid !== null) core.tryClose(cancelHandleRid);
        }
    })();

    return uploadTask;
}

export { uploadFile };
