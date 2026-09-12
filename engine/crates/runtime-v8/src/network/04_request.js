import { core, primordials } from "ext:core/mod.js";
import { Header } from "ext:host_v8_network/01_header.js";
import { NetworkTask } from "ext:host_v8_network/03_task.js";
import { ReadableStream } from "ext:host_v8_web/06_stream.js";
import { createListenerGroup } from "ext:host_v8_base/02_async.js";
import {
    abortedNetworkError, Response, ErrorResponse,
    nullBodyStatus, Exception,
} from "ext:host_v8_network/02_response.js";
import { op_fetch, op_fetch_send } from "ext:core/ops";

const {
    TypeError, JSONParse, TypedArrayPrototypeGetBuffer,
    TypedArrayPrototypeGetByteLength,
} = primordials;

// -- Constants --

const KNOWN_METHODS = new Set(["DELETE", "GET", "HEAD", "OPTIONS", "PATCH", "POST", "PUT", "TRACE", "CONNECT"]);
const NO_BODY_METHODS = new Set(["GET", "HEAD", "TRACE", "CONNECT"]);
// Buffered responses are policy-bounded. Chunked mode below remains streaming
// and does not accumulate body bytes.
const MAX_BUFFERED_BODY_BYTES = 32 * 1024 * 1024;
// -- RequestTask --

class RequestTask extends NetworkTask {
    constructor(terminator) {
        super(terminator);
        this._chunkReceivedListeners = createListenerGroup('Error in chunk received');
    }

    _onCleanup() {
        this._chunkReceivedListeners.off();
    }

    onChunkReceived(listener) {
        if (typeof listener !== 'function') {
            throw new TypeError('Listener must be a function');
        }
        if (this._aborted) {
            return;
        }
        this._chunkReceivedListeners.on(listener);
    }

    offChunkReceived(listener) {
        if (listener !== undefined && typeof listener !== 'function') return;
        this._chunkReceivedListeners.off(listener);
    }

    _triggerChunkReceived(chunk) {
        if (this._aborted) {
            return;
        }
        this._chunkReceivedListeners.trigger(chunk);
    }
}

// -- Helpers --

function normalizeMethod(method = "GET") {
    const upper = method.toUpperCase();
    if (!KNOWN_METHODS.has(upper)) {
        throw new TypeError(`Unsupported HTTP method: ${method}`);
    }
    return upper;
}

/**
 * Append object/string data as query parameters for GET/HEAD requests.
 */
function appendQueryParams(url, data) {
    if (data == null) return url;

    let qs;
    if (typeof data === 'string') {
        qs = data;
    } else if (typeof data === 'object' && !(data instanceof ArrayBuffer) && !ArrayBuffer.isView(data)) {
        const parts = [];
        const entries = Object.entries(data);
        for (let i = 0; i < entries.length; i++) {
            const [key, value] = entries[i];
            parts.push(encodeURIComponent(key) + '=' + encodeURIComponent(String(value)));
        }
        if (parts.length === 0) return url;
        qs = parts.join('&');
    } else {
        return url;
    }

    if (!qs) return url;
    const sep = url.includes('?') ? '&' : '?';
    return url + sep + qs;
}

/**
 * Build header list with smart Content-Type auto-detection.
 */
function fillHeaders(headers, method, data, hasBody) {
    const headerList = Object.entries(headers).map(([key, value]) => [key, String(value)]);

    // Check if Content-Type already provided (case-insensitive)
    let hasContentType = false;
    for (let i = 0; i < headerList.length; i++) {
        if (headerList[i][0].toLowerCase() === 'content-type') {
            hasContentType = true;
            break;
        }
    }

    if (!hasContentType && hasBody) {
        if (typeof data === 'object' && data !== null
            && !(data instanceof ArrayBuffer) && !ArrayBuffer.isView(data)) {
            headerList.push(["Content-Type", "application/json"]);
        } else if (typeof data === 'string') {
            headerList.push(["Content-Type", "text/plain"]);
        } else {
            headerList.push(["Content-Type", "application/octet-stream"]);
        }
    }

    return headerList;
}

function chunkToU8(chunk) {
    return typeof chunk === "string" ? core.encode(chunk) : chunk;
}

function chunkToString(chunk) {
    return typeof chunk === "string" ? chunk : core.decode(chunk);
}

/**
 * Serialize request body to Uint8Array.
 */
function toBodyBuffer(body) {
    if (body == null) {
        return null;
    }
    if (body instanceof ArrayBuffer) {
        return new Uint8Array(body);
    }
    if (ArrayBuffer.isView(body)) {
        return new Uint8Array(body.buffer, body.byteOffset, body.byteLength);
    }
    if (typeof body === "string") {
        return core.encode(body);
    }
    if (typeof body === "object") {
        return core.encode(JSON.stringify(body));
    }
    throw new TypeError("Unsupported body type");
}

/**
 * Deserialize response body with JSON parse resilience.
 */
function fromBodyBuffer(buffer, dataType, responseType) {
    if (responseType === 'arraybuffer') {
        return TypedArrayPrototypeGetBuffer(chunkToU8(buffer));
    }
    if (dataType === 'json') {
        const text = chunkToString(buffer);
        try {
            return JSONParse(text);
        } catch (_) {
            // Return raw string if JSON parse fails
            return text;
        }
    }
    return chunkToString(buffer);
}

// -- request() --

function request(options = {}) {
    const {
        url, data, header = {}, timeout = 60000, method = 'GET',
        dataType = 'json', responseType = 'text',
        enableHttp2 = false, enableQuic = false,
        enableCache = false, enableHttpDNS = false,
        enableChunked = false,
        success = () => {}, fail = () => {}, complete = () => {}
    } = options;

    // Validate URL
    if (!url || typeof url !== 'string') {
        const error = new ErrorResponse(0, new Exception(0, "request:fail invalid url", 0));
        queueMicrotask(() => { fail(error); complete(error); });
        return new RequestTask(null);
    }

    const methodNormalized = normalizeMethod(method);

    // For GET/HEAD: serialize object data as query parameters
    let finalUrl = url;
    let reqBody = null;
    if (data != null) {
        if (NO_BODY_METHODS.has(methodNormalized)) {
            finalUrl = appendQueryParams(url, data);
        } else {
            reqBody = toBodyBuffer(data);
        }
    }

    const headers = fillHeaders(header, methodNormalized, data, reqBody !== null);
    let reqRid = null;

    let requestRid, cancelHandleRid;
    try {
        const result = op_fetch(
            methodNormalized,
            finalUrl,
            headers,
            null,   // clientRid
            reqBody !== null || reqRid !== null,
            reqBody,
            reqRid,
            timeout,
            enableHttp2,
            enableCache
        );
        requestRid = result.requestRid;
        cancelHandleRid = result.cancelHandleRid;
    } catch (err) {
        const error = new ErrorResponse(0, new Exception(0, "request:fail " + err.message, 0));
        queueMicrotask(() => { fail(error); complete(error); });
        return new RequestTask(null);
    }

    const cancellation = {
        aborted: false,
        responseRid: null,
        abort() {
            this.aborted = true;
            if (cancelHandleRid !== null) core.tryClose(cancelHandleRid);
            // The request cancel handle only covers the send phase. Once
            // headers are in, abort() must also close the body resource so
            // an in-flight (possibly large) download stops immediately
            // instead of running to completion and firing success().
            if (this.responseRid !== null) core.tryClose(this.responseRid);
        }
    };

    const requestTask = new RequestTask(cancellation);

    (async () => {
        try {
            const resp = await op_fetch_send(requestRid);
            // Take ownership before the abort check; the send continuation
            // can race an abort after the Rust response resource exists.
            if (resp?.responseRid) cancellation.responseRid = resp.responseRid;
            if (cancellation.aborted) throw abortedNetworkError();
            if (resp?.error) {
                const error = new ErrorResponse(resp.status, new Exception(resp.status, resp.error, 0));
                fail(error);
                complete(error);
                return;
            }


            const respHeader = new Header(resp.headers, resp.status);
            requestTask._triggerHeadersReceived(respHeader);

            const cbResp = new Response(respHeader);

            if (nullBodyStatus(resp.status)) {
                core.close(resp.responseRid);
            } else if (methodNormalized === "HEAD" || methodNormalized === "CONNECT") {
                core.close(resp.responseRid);
            } else {
                let rds = null;
                try {
                    rds = new ReadableStream(resp.responseRid);
                    let bodyBytes;

                    if (enableChunked) {
                        // Truly streaming: chunks are handed to the user's
                        // listener as they arrive and then released.
                        const buffer = new Uint8Array(64 * 1024);
                        await rds.pull(buffer, (chunk) => {
                            if (chunk === undefined) return;
                            const chunkCopy = new Uint8Array(chunk);
                            requestTask._triggerChunkReceived({ data: chunkCopy.buffer });
                        });
                    } else {
                        // Pull bounded chunks so a chunked response cannot
                        // make readAll allocate an unbounded body. A callback
                        // error is followed by rds.cancel() in the catch.
                        const chunks = [];
                        let totalBytes = 0;
                        const buffer = new Uint8Array(64 * 1024);
                        await rds.pull(buffer, (chunk) => {
                            if (chunk === undefined) return;
                            const length = TypedArrayPrototypeGetByteLength(chunk);
                            totalBytes += length;
                            if (totalBytes > MAX_BUFFERED_BODY_BYTES) {
                                throw new Error("response body exceeds buffered response limit");
                            }
                            chunks.push(new Uint8Array(chunk));
                        });
                        if (totalBytes > 0) {
                            const merged = new Uint8Array(totalBytes);
                            let offset = 0;
                            for (const chunk of chunks) {
                                merged.set(chunk, offset);
                                offset += chunk.byteLength;
                            }
                            bodyBytes = merged.buffer;
                        }
                    }

                    if (bodyBytes != null) {
                        cbResp.data = fromBodyBuffer(bodyBytes, dataType, responseType);
                    }
                } catch (err) {
                    if (rds !== null) rds.cancel();
                    // A read cancelled by abort() surfaces here; report it
                    // as an abort (via the outer catch), not a 500.
                    if (cancellation.aborted) throw "aborted";
                    const error = new ErrorResponse(500, new Exception(500, "read data failed: " + err, 0));
                    fail(error);
                    complete(error);
                    return;
                }
            }

            if (cancellation.aborted) throw "aborted";

            success(cbResp);
            complete(cbResp);
        } catch (err) {
            const error = cancellation.aborted
                ? abortedNetworkError()
                : new ErrorResponse(500, new Exception(500, err.message, 0));
            fail(error);
            complete(error);
        } finally {
            if (cancellation.responseRid !== null) {
                core.tryClose(cancellation.responseRid);
                cancellation.responseRid = null;
            }
            if (cancelHandleRid !== null) core.tryClose(cancelHandleRid);
        }
    })();

    return requestTask;
}

export { request };
