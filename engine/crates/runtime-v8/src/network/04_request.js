import { core, primordials } from "ext:core/mod.js";
import { Header } from "ext:host_v8_network/01_header.js";
import { NetworkTask, createSettler, headerEntries } from "ext:host_v8_network/03_task.js";
import { ReadableStream } from "ext:host_v8_web/06_stream.js";
import { createListenerGroup, errorMessage } from "ext:host_v8_base/02_async.js";
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
    const upper = typeof method === "string" ? method.toUpperCase() : "";
    if (!KNOWN_METHODS.has(upper)) {
        throw new TypeError(`Unsupported HTTP method: ${String(method)}`);
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
    const headerList = headerEntries(headers);

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
 * Turn the bytes of a response body (a Uint8Array) into what the caller asked for, with JSON parse
 * resilience: a body that is not JSON is the text it is.
 *
 * An empty body is an empty body of the kind asked for -- "" or an empty ArrayBuffer -- not `null`:
 * `res.data.length` and `res.data.byteLength` are read without a check.
 */
function fromBodyBuffer(bytes, dataType, responseType) {
    if (responseType === 'arraybuffer') {
        return TypedArrayPrototypeGetBuffer(bytes);
    }
    const text = TypedArrayPrototypeGetByteLength(bytes) === 0 ? '' : core.decode(bytes);
    if (dataType === 'json') {
        try {
            return JSONParse(text);
        } catch (_) {
            // Return raw string if JSON parse fails
            return text;
        }
    }
    return text;
}

const EMPTY_BODY = new Uint8Array(0);

// -- request() --

function request(options) {
    // `null` and non-objects are a call that names no url, not a throw out of the caller's helper.
    const opts = (options !== null && typeof options === 'object') ? options : {};
    const {
        url, data, header, timeout = 60000, method = 'GET',
        dataType = 'json', responseType = 'text',
        enableHttp2 = false,
        enableCache = false,
        enableChunked = false,
    } = opts;
    const settler = createSettler('request', opts);

    // A call that cannot make a request is a `fail`, delivered after this returns as every other
    // outcome is, and not a throw: a game's request helper does not wrap each call in a try.
    const failBeforeSending = (detail) => {
        const error = new ErrorResponse(0, new Exception(0, "request:fail " + detail, 0));
        queueMicrotask(() => settler.fail(error));
        return new RequestTask(null);
    };

    if (!url || typeof url !== 'string') {
        return failBeforeSending("invalid url");
    }

    let methodNormalized, requestRid, cancelHandleRid;
    try {
        methodNormalized = normalizeMethod(method);

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
        const result = op_fetch(
            methodNormalized,
            finalUrl,
            headers,
            null,   // clientRid
            reqBody !== null,
            reqBody,
            null,   // reqRid
            timeout,
            enableHttp2,
            enableCache
        );
        requestRid = result.requestRid;
        cancelHandleRid = result.cancelHandleRid;
    } catch (err) {
        return failBeforeSending(errorMessage(err));
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
                settler.fail(new ErrorResponse(resp.status, new Exception(resp.status, resp.error, 0)));
                return;
            }


            const respHeader = new Header(resp.headers, resp.status);
            requestTask._triggerHeadersReceived(respHeader);

            const cbResp = new Response(respHeader);

            if (nullBodyStatus(resp.status)
                || methodNormalized === "HEAD" || methodNormalized === "CONNECT") {
                core.close(resp.responseRid);
                cbResp.data = fromBodyBuffer(EMPTY_BODY, dataType, responseType);
            } else {
                let rds = null;
                try {
                    rds = new ReadableStream(resp.responseRid);

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
                        let bodyBytes = EMPTY_BODY;
                        if (chunks.length === 1) {
                            // Most bodies (JSON, a small config) fit one chunk, which is already
                            // a copy out of the shared read buffer: no second one.
                            bodyBytes = chunks[0];
                        } else if (chunks.length > 1) {
                            bodyBytes = new Uint8Array(totalBytes);
                            let offset = 0;
                            for (const chunk of chunks) {
                                bodyBytes.set(chunk, offset);
                                offset += chunk.byteLength;
                            }
                        }
                        cbResp.data = fromBodyBuffer(bodyBytes, dataType, responseType);
                    }
                } catch (err) {
                    if (rds !== null) rds.cancel();
                    // A read cancelled by abort() surfaces here; report it
                    // as an abort (via the outer catch), not a 500.
                    if (cancellation.aborted) throw "aborted";
                    settler.fail(new ErrorResponse(500, new Exception(500, "read data failed: " + err, 0)));
                    return;
                }
            }

            if (cancellation.aborted) throw "aborted";

            settler.succeed(cbResp);
        } catch (err) {
            settler.fail(cancellation.aborted
                ? abortedNetworkError()
                : new ErrorResponse(500, new Exception(500, errorMessage(err), 0)));
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
