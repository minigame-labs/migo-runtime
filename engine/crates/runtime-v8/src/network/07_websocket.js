import { core } from "ext:core/mod.js";
import {
    op_ws_create, op_ws_next_event, op_ws_send, op_ws_close,
} from "ext:core/ops";
import { createListenerGroup, failMessage } from "ext:host_v8_base/02_async.js";
import { toExactArrayBuffer } from "ext:host_v8_network/00_binary.js";

// -- SocketTask --

class SocketTask {
    constructor(rid) {
        this._rid = rid;
        this._closed = false;
        this._openListeners = createListenerGroup('SocketTask onOpen');
        this._messageListeners = createListenerGroup('SocketTask onMessage');
        this._errorListeners = createListenerGroup('SocketTask onError');
        this._closeListeners = createListenerGroup('SocketTask onClose');
    }

    send(options = {}) {
        const { data, success, fail, complete } = options;
        if (this._closed) {
            const err = { errMsg: "sendSocketMessage:fail WebSocket is closed" };
            if (typeof fail === 'function') fail(err);
            if (typeof complete === 'function') complete(err);
            return;
        }

        let dataStr = undefined;
        let dataBuf = undefined;

        if (typeof data === 'string') {
            dataStr = data;
        } else if (data instanceof ArrayBuffer) {
            dataBuf = new Uint8Array(data);
        } else if (ArrayBuffer.isView(data)) {
            dataBuf = new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
        } else {
            const err = { errMsg: "sendSocketMessage:fail invalid data type" };
            if (typeof fail === 'function') fail(err);
            if (typeof complete === 'function') complete(err);
            return;
        }

        op_ws_send(this._rid, dataStr, dataBuf).then(() => {
            const res = { errMsg: "sendSocketMessage:ok" };
            if (typeof success === 'function') success(res);
            if (typeof complete === 'function') complete(res);
        }).catch((err) => {
            const res = { errMsg: failMessage("sendSocketMessage", err) };
            if (typeof fail === 'function') fail(res);
            if (typeof complete === 'function') complete(res);
        });
    }

    close(options = {}) {
        const { code = 1000, reason = "", success, fail, complete } = options;
        if (this._closed) {
            const res = { errMsg: "closeSocket:ok" };
            if (typeof success === 'function') success(res);
            if (typeof complete === 'function') complete(res);
            return;
        }

        if (this._rid < 0) {
            // Handshake has not produced a rid yet. Mark closed so the
            // pending connect (connectSocket) tears down the socket it is
            // about to create, instead of leaving a live ghost connection.
            this._closed = true;
            const res = { errMsg: "closeSocket:ok" };
            if (typeof success === 'function') success(res);
            if (typeof complete === 'function') complete(res);
            return;
        }

        op_ws_close(this._rid, code, reason).then(() => {
            const res = { errMsg: "closeSocket:ok" };
            if (typeof success === 'function') success(res);
            if (typeof complete === 'function') complete(res);
        }).catch((err) => {
            const res = { errMsg: failMessage("closeSocket", err) };
            if (typeof fail === 'function') fail(res);
            if (typeof complete === 'function') complete(res);
        });
    }

    onOpen(callback) {
        this._openListeners.on(callback);
    }

    onMessage(callback) {
        this._messageListeners.on(callback);
    }

    onError(callback) {
        this._errorListeners.on(callback);
    }

    onClose(callback) {
        this._closeListeners.on(callback);
    }

    // -- Internal event dispatch --

    _fireOpen(header) {
        this._openListeners.trigger({ header });
    }

    _fireMessage(data) {
        this._messageListeners.trigger({ data });
    }

    _fireError(errMsg) {
        this._errorListeners.trigger({ errMsg });
    }

    _fireClose(code, reason) {
        this._closed = true;
        core.tryClose(this._rid);
        this._closeListeners.trigger({ code, reason });
    }
}

// -- Event polling loop --

async function _pollEvents(task) {
    while (!task._closed) {
        let event;
        try {
            event = await op_ws_next_event(task._rid);
        } catch (err) {
            if (!task._closed) {
                task._fireError(err.message || "connection lost");
                task._fireClose(1006, "");
            }
            return;
        }

        switch (event.type) {
            case "message":
                if (event.isBinary) {
                    // event.dataBin is an exact-length Uint8Array (external
                    // backing); hand its buffer to the callback without a copy.
                    const buf = toExactArrayBuffer(event.dataBin);
                    task._fireMessage(buf);
                } else {
                    task._fireMessage(event.dataStr);
                }
                break;

            case "error":
                task._fireError(event.errMsg);
                break;

            case "close":
                task._fireClose(event.code, event.reason);
                return;
        }
    }
}

// -- Global socket state --

let _globalSocket = null;
const _globalOpenListeners = createListenerGroup('onSocketOpen');
const _globalMessageListeners = createListenerGroup('onSocketMessage');
const _globalErrorListeners = createListenerGroup('onSocketError');
const _globalCloseListeners = createListenerGroup('onSocketClose');

// -- connectSocket --

function connectSocket(options = {}) {
    const {
        url,
        header = {},
        protocols = [],
        tcpNoDelay = false,
        perMessageDeflate = false,
        timeout = 60000,
        success,
        fail,
        complete,
    } = options;

    if (!url || typeof url !== 'string') {
        const err = { errMsg: "connectSocket:fail invalid url" };
        if (typeof fail === 'function') queueMicrotask(() => fail(err));
        if (typeof complete === 'function') queueMicrotask(() => complete(err));
        return new SocketTask(-1);
    }

    const headerList = [];
    for (const key of Object.keys(header)) {
        headerList.push([key, String(header[key])]);
    }

    const task = new SocketTask(-1);

    // Set as global socket
    _globalSocket = task;

    // Bridge global listeners to this task
    task.onOpen((res) => {
        _globalOpenListeners.trigger(res);
    });
    task.onMessage((res) => {
        _globalMessageListeners.trigger(res);
    });
    task.onError((res) => {
        _globalErrorListeners.trigger(res);
    });
    task.onClose((res) => {
        _globalCloseListeners.trigger(res);
        if (_globalSocket === task) {
            _globalSocket = null;
        }
    });

    // Async connection
    (async () => {
        try {
            const result = await op_ws_create(url, protocols, headerList, timeout);
            task._rid = result.rid;

            // close() may have run while the handshake was in flight; if so,
            // tear down the freshly-created socket rather than leaving a
            // live ghost connection the caller believes is closed.
            if (task._closed) {
                core.tryClose(result.rid);
                return;
            }

            const res = { errMsg: "connectSocket:ok" };
            if (typeof success === 'function') success(res);
            if (typeof complete === 'function') complete(res);

            // Fire onOpen with response headers
            task._fireOpen({
                "Sec-WebSocket-Protocol": result.protocol,
                "Sec-WebSocket-Extensions": result.extensions,
            });

            // Start event polling loop
            await _pollEvents(task);

        } catch (err) {
            // If close() already ran (e.g. connect failed after the caller
            // closed), stay silent: no post-close fail/onError/onClose.
            if (task._closed) return;
            const res = { errMsg: failMessage("connectSocket", err) };
            if (typeof fail === 'function') fail(res);
            if (typeof complete === 'function') complete(res);
            task._fireError(res.errMsg);
            task._fireClose(1006, "");
        }
    })();

    return task;
}

// -- Global convenience APIs --

function sendSocketMessage(options = {}) {
    if (!_globalSocket || _globalSocket._closed) {
        const err = { errMsg: "sendSocketMessage:fail WebSocket is not connected" };
        if (typeof options.fail === 'function') queueMicrotask(() => options.fail(err));
        if (typeof options.complete === 'function') queueMicrotask(() => options.complete(err));
        return Promise.reject(err);
    }
    const { data, success, fail, complete } = options;
    let dataStr = undefined;
    let dataBuf = undefined;
    if (typeof data === 'string') {
        dataStr = data;
    } else if (data instanceof ArrayBuffer) {
        dataBuf = new Uint8Array(data);
    } else if (ArrayBuffer.isView(data)) {
        dataBuf = new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
    } else {
        const err = { errMsg: "sendSocketMessage:fail invalid data type" };
        if (typeof fail === 'function') queueMicrotask(() => fail(err));
        if (typeof complete === 'function') queueMicrotask(() => complete(err));
        return Promise.reject(err);
    }
    return op_ws_send(_globalSocket._rid, dataStr, dataBuf).then(() => {
        const res = { errMsg: "sendSocketMessage:ok" };
        if (typeof success === 'function') success(res);
        if (typeof complete === 'function') complete(res);
        return res;
    }).catch((err) => {
        const res = { errMsg: failMessage("sendSocketMessage", err) };
        if (typeof fail === 'function') fail(res);
        if (typeof complete === 'function') complete(res);
        throw res;
    });
}

function closeSocket(options = {}) {
    if (!_globalSocket || _globalSocket._closed) {
        const err = { errMsg: "closeSocket:fail WebSocket is not connected" };
        if (typeof options.fail === 'function') queueMicrotask(() => options.fail(err));
        if (typeof options.complete === 'function') queueMicrotask(() => options.complete(err));
        return Promise.reject(err);
    }
    const { code = 1000, reason = "", success, fail, complete } = options;
    return op_ws_close(_globalSocket._rid, code, reason).then(() => {
        const res = { errMsg: "closeSocket:ok" };
        if (typeof success === 'function') success(res);
        if (typeof complete === 'function') complete(res);
        return res;
    }).catch((err) => {
        const res = { errMsg: failMessage("closeSocket", err) };
        if (typeof fail === 'function') fail(res);
        if (typeof complete === 'function') complete(res);
        throw res;
    });
}

function onSocketOpen(callback) {
    _globalOpenListeners.on(callback);
}

function offSocketOpen(callback) {
    _globalOpenListeners.off(callback);
}

function onSocketMessage(callback) {
    _globalMessageListeners.on(callback);
}

function offSocketMessage(callback) {
    _globalMessageListeners.off(callback);
}

function onSocketError(callback) {
    _globalErrorListeners.on(callback);
}

function offSocketError(callback) {
    _globalErrorListeners.off(callback);
}

function onSocketClose(callback) {
    _globalCloseListeners.on(callback);
}

function offSocketClose(callback) {
    _globalCloseListeners.off(callback);
}

export {
    connectSocket,
    sendSocketMessage,
    closeSocket,
    onSocketOpen,
    offSocketOpen,
    onSocketMessage,
    offSocketMessage,
    onSocketError,
    offSocketError,
    onSocketClose,
    offSocketClose,
};
