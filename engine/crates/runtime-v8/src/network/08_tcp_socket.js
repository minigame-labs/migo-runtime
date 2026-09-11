// Architecture: Each TCPSocket instance wraps a Rust TcpSocketResource
// (identified by rid). Events are polled via an async loop calling
// op_tcp_next_event, matching the WebSocket pattern.

import { core } from "ext:core/mod.js";
import {
    op_tcp_connect, op_tcp_next_event, op_tcp_write, op_tcp_close,
} from "ext:core/ops";
import { createListenerGroup } from "ext:host_v8_base/02_async.js";
import { toExactArrayBuffer } from "ext:host_v8_network/00_binary.js";

// -- TCPSocket class --

class TCPSocket {
    constructor(type) {
        this._type = type || 'ipv4';
        this._rid = -1;
        this._closed = false;
        this._connected = false;
        // A connect result may arrive after a newer attempt. The generation
        // keeps stale results from publishing a rid or terminating the winner.
        this._connectGen = 0;

        // Per-instance event listeners
        this._connectListeners = createListenerGroup('TCPSocket onConnect');
        this._closeListeners = createListenerGroup('TCPSocket onClose');
        this._errorListeners = createListenerGroup('TCPSocket onError');
        this._messageListeners = createListenerGroup('TCPSocket onMessage');
        this._bindWifiListeners = createListenerGroup('TCPSocket onBindWifi');
    }

    // -- Control methods --

    connect(options = {}) {
        if (this._connected || this._closed) return;

        const {
            address,
            port,
            timeout = 2,
        } = options;

        if (!address || port === undefined) {
            this._fireError('connect:fail missing address or port');
            return;
        }

        const myGen = ++this._connectGen;
        (async () => {
            try {
                const result = await op_tcp_connect(address, port, timeout);
                if (this._closed || this._connectGen !== myGen) {
                    // The socket was closed or superseded while connect ran.
                    // The result owns a fresh native resource, so close it
                    // before dropping the stale result.
                    try { op_tcp_close(result.rid); } catch (_) { /* ignore */ }
                    return;
                }
                this._rid = result.rid;
                this._connected = true;

                // Fire connect event with address info
                this._fireConnect({
                    remoteInfo: {
                        address: result.remoteAddress,
                        family: result.remoteFamily,
                        port: result.remotePort,
                    },
                    localInfo: {
                        address: result.localAddress,
                        family: result.localFamily,
                        port: result.localPort,
                    },
                });

                // Start the event polling loop
                await this._pollEvents(myGen);

            } catch (err) {
                // A stale failure must not report an error or close the
                // currently winning connection.
                if (this._closed || this._connectGen !== myGen) return;
                this._fireError(err.message || 'connect:fail unknown error');
                this._doClose();
            }
        })();
    }

    write(data) {
        if (!this._connected || this._closed) return;

        let dataStr = undefined;
        let dataBuf = undefined;

        if (typeof data === 'string') {
            dataStr = data;
        } else if (data instanceof ArrayBuffer) {
            dataBuf = new Uint8Array(data);
        } else if (ArrayBuffer.isView(data)) {
            dataBuf = new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
        } else {
            this._fireError('write:fail invalid data type');
            return;
        }

        op_tcp_write(this._rid, dataStr, dataBuf).catch((err) => {
            if (!this._closed) {
                this._fireError('write:fail ' + (err.message || err));
            }
        });
    }

    close() {
        if (this._closed) return;
        this._doClose();
    }

    bindWifi(options = {}) {
        // bindWifi requires Android Network.bindSocket() -- not yet implemented.
        this._fireError('bindWifi:fail not supported');
    }

    // -- Event listener methods --

    onConnect(listener) {
        this._connectListeners.on(listener);
    }

    offConnect(listener) {
        this._connectListeners.off(listener);
    }

    onClose(listener) {
        this._closeListeners.on(listener);
    }

    offClose(listener) {
        this._closeListeners.off(listener);
    }

    onError(listener) {
        this._errorListeners.on(listener);
    }

    offError(listener) {
        this._errorListeners.off(listener);
    }

    onMessage(listener) {
        this._messageListeners.on(listener);
    }

    offMessage(listener) {
        this._messageListeners.off(listener);
    }

    onBindWifi(listener) {
        this._bindWifiListeners.on(listener);
    }

    offBindWifi(listener) {
        this._bindWifiListeners.off(listener);
    }

    // -- Internal event dispatch --

    _fireConnect(res) {
        this._connectListeners.trigger(res);
    }

    _fireClose() {
        this._closeListeners.trigger();
    }

    _fireError(errMsg) {
        this._errorListeners.trigger({ errMsg });
    }

    _fireMessage(message, remoteInfo, localInfo) {
        this._messageListeners.trigger({ message, remoteInfo, localInfo });
    }

    _doClose() {
        if (this._closed) return;
        this._closed = true;
        this._connected = false;
        ++this._connectGen;
        if (this._rid >= 0) {
            try { op_tcp_close(this._rid); } catch (_) { /* ignore */ }
        }
        this._fireClose();
    }

    async _pollEvents(myGen) {
        while (!this._closed && this._connectGen === myGen && this._rid >= 0) {
            let event;
            try {
                event = await op_tcp_next_event(this._rid);
            } catch (err) {
                if (!this._closed && this._connectGen === myGen) {
                    this._fireError(err.message || 'connection lost');
                    this._doClose();
                }
                return;
            }

            // close()/a newer connect may have invalidated the result while
            // the receive was pending. Never dispatch stale data.
            if (this._closed || this._connectGen !== myGen) return;

            switch (event.type) {
                case 'message': {
                    // event.data is an exact-length Uint8Array (external
                    // backing); hand its buffer to the callback without a copy.
                    const buf = toExactArrayBuffer(event.data);
                    this._fireMessage(
                        buf,
                        {
                            address: event.remoteAddress,
                            family: event.remoteFamily,
                            port: event.remotePort,
                        },
                        {
                            address: event.localAddress,
                            family: event.localFamily,
                            port: event.localPort,
                        },
                    );
                    break;
                }
                case 'error':
                    // A read error on a TCP stream is terminal: the
                    // connection is broken, so fire onError then close
                    // (onClose follows) instead of re-polling a dead fd.
                    this._fireError(event.errMsg);
                    this._doClose();
                    return;
                case 'close':
                    this._doClose();
                    return;
            }
        }
    }
}

// -- Factory function --

function createTCPSocket(options = {}) {
    const type = options.type || 'ipv4';
    return new TCPSocket(type);
}

export { createTCPSocket };
