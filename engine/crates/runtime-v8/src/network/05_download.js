import { core, primordials } from "ext:core/mod.js";
import { Header } from "ext:host_v8_network/01_header.js";
import { NetworkTask } from "ext:host_v8_network/03_task.js";
import { createListenerGroup } from "ext:host_v8_base/02_async.js";
import {
    DownloadResponse, DownloadErrorResponse, Exception, abortedNetworkError,
} from "ext:host_v8_network/02_response.js";
import {
    op_fetch, op_fetch_send,
    op_open_file, op_write_file, op_close_file,
    op_rename, op_unlink,
} from "ext:core/ops";

const { TypeError } = primordials;

// -- DownloadTask --

class DownloadTask extends NetworkTask {
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

    _triggerProgress(progress, totalBytesWritten, totalBytesExpectedToWrite) {
        if (this._aborted) {
            return;
        }
        this._progressListeners.trigger({ progress, totalBytesWritten, totalBytesExpectedToWrite });
    }
}

// -- Helpers --

let _dlCounter = 0;

function generateTempFilePath(targetPath) {
    const slash = targetPath.lastIndexOf('/');
    const parent = slash >= 0 ? targetPath.substring(0, slash + 1) : '';
    const name = slash >= 0 ? targetPath.substring(slash + 1) : targetPath;
    const id = (++_dlCounter).toString(36);
    return parent + '.' + name + '.migo-download-' + id + '.part';
}

function makeError(errno, msg) {
    return new DownloadErrorResponse(errno, new Exception(errno, msg, 0));
}

// Progress throttle interval (ms)
const PROGRESS_INTERVAL = 100;
// Read chunk size
const CHUNK_SIZE = 64 * 1024;

// -- downloadFile() --

function downloadFile(options = {}) {
    const {
        url, filePath, header = {}, timeout = 60000,
        enableHttp2 = false,
        success = () => {}, fail = () => {}, complete = () => {}
    } = options;

    // Validate URL
    if (!url || typeof url !== 'string') {
        const error = makeError(0, "downloadFile:fail invalid url");
        queueMicrotask(() => { fail(error); complete(error); });
        return new DownloadTask(null);
    }

    const targetPath = filePath || generateTempFilePath('/tmp/dl');
    // Every task owns an exclusive temporary inode in the destination
    // directory. A fixed `<target>.part` lets concurrent tasks truncate,
    // interleave, and unlink one another's work.
    const tmpPath = generateTempFilePath(targetPath);
    const headers = Object.entries(header).map(([key, value]) => [key, String(value)]);

    // Create fetch request (GET, no body)
    let requestRid, cancelHandleRid;
    try {
        const result = op_fetch("GET", url, headers, null, false, null, null, timeout, enableHttp2, false);
        requestRid = result.requestRid;
        cancelHandleRid = result.cancelHandleRid;
    } catch (err) {
        const error = makeError(0, "downloadFile:fail " + err.message);
        queueMicrotask(() => { fail(error); complete(error); });
        return new DownloadTask(null);
    }

    const cancellation = {
        aborted: false,
        responseRid: null,
        abort() {
            this.aborted = true;
            if (cancelHandleRid !== null) core.tryClose(cancelHandleRid);
            if (this.responseRid !== null) core.tryClose(this.responseRid);
        }
    };

    const downloadTask = new DownloadTask(cancellation);

    (async () => {
        let fd = null;
        try {
            const resp = await op_fetch_send(requestRid);
            // Register the body resource before checking cancellation: abort
            // may run in the handoff window after send resolves but before
            // this continuation resumes.
            cancellation.responseRid = resp?.responseRid || null;
            if (cancellation.aborted) throw "aborted";

            if (resp?.error) {
                const error = makeError(resp.status, resp.error);
                fail(error);
                complete(error);
                return;
            }

            const statusCode = resp.status;
            const respHeader = new Header(resp.headers, statusCode);
            downloadTask._triggerHeadersReceived(respHeader);
            if (cancellation.aborted) throw "aborted";

            const totalBytes = resp.contentLength || 0;
            fd = await op_open_file(tmpPath, "wx");
            if (cancellation.aborted) throw "aborted";

            let bytesWritten = 0;
            let lastProgressTime = 0;
            const buffer = new Uint8Array(CHUNK_SIZE);
            while (true) {
                if (cancellation.aborted) throw "aborted";
                const bytesRead = await core.read(resp.responseRid, buffer);
                if (cancellation.aborted) throw "aborted";
                if (bytesRead === 0) break;

                const chunk = buffer.subarray(0, bytesRead);
                await op_write_file(fd, chunk, null, null, null);
                if (cancellation.aborted) throw "aborted";

                bytesWritten += bytesRead;
                const now = Date.now();
                if (now - lastProgressTime >= PROGRESS_INTERVAL) {
                    const progress = totalBytes > 0
                        ? Math.min(100, Math.round((bytesWritten / totalBytes) * 100))
                        : 0;
                    downloadTask._triggerProgress(progress, bytesWritten, totalBytes);
                    lastProgressTime = now;
                }
            }

            core.tryClose(resp.responseRid);
            cancellation.responseRid = null;
            await op_close_file(fd);
            if (cancellation.aborted) throw "aborted";
            fd = null;

            await op_rename(tmpPath, targetPath);
            if (cancellation.aborted) throw "aborted";

            const finalTotal = totalBytes || bytesWritten;
            downloadTask._triggerProgress(100, bytesWritten, finalTotal);
            const result = new DownloadResponse(
                filePath ? undefined : targetPath,
                filePath || undefined,
                statusCode
            );
            success(result);
            complete(result);
        } catch (err) {
            if (fd !== null) {
                try { await op_close_file(fd); } catch (_) {}
            }
            try { await op_unlink(tmpPath); } catch (_) {}

            if (cancellation.aborted || err === "aborted") {
                const error = abortedNetworkError();
                fail(error);
                complete(error);
            } else {
                const error = makeError(500, "downloadFile:fail " + (err.message || err));
                fail(error);
                complete(error);
            }
        } finally {
            if (cancellation.responseRid !== null) {
                core.tryClose(cancellation.responseRid);
                cancellation.responseRid = null;
            }
            if (cancelHandleRid !== null) core.tryClose(cancelHandleRid);
        }
    })();

    return downloadTask;
}

export { downloadFile };
