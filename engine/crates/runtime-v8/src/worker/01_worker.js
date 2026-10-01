import { op_worker_create,
    op_worker_post_message,
    op_worker_recv_message,
    op_worker_recv_error,
    op_worker_terminate } from "ext:core/ops";
import { createListenerGroup } from "ext:host_v8_base/02_async.js";
import { core, primordials } from "ext:core/mod.js";

// The Rust side declares this error's class as "WorkerError" (see worker/mod.rs). deno_core
// can only construct it if a JS constructor is registered under that exact name;
// without one the throw arrives in JS as literal `undefined`, and every handler
// that reads `.message` off it fails instead of reporting the error. Registered
// here for the same reason `IOError` is registered in 02_file_manager.js.
const { Error: _PrimError } = primordials;
class WorkerError extends _PrimError {
  constructor(msg) {
    super(msg);
    this.name = "WorkerError";
  }
}
core.registerErrorClass("WorkerError", WorkerError);

const {
    ArrayPrototypePush,
    Error,
    JSONStringify,
    RangeError,
    StringPrototypeCharCodeAt,
} = primordials;

const MAX_PENDING_MESSAGES = 4096;
const MAX_PENDING_MESSAGE_BYTES = 64 * 1024 * 1024;
const MAX_WORKER_MESSAGE_BYTES = 16 * 1024 * 1024;

function utf8ByteLength(value) {
    let bytes = 0;
    for (let index = 0; index < value.length; index++) {
        const code = StringPrototypeCharCodeAt(value, index);
        if (code <= 0x7f) {
            bytes++;
        } else if (code <= 0x7ff) {
            bytes += 2;
        } else if (
            code >= 0xd800 && code <= 0xdbff &&
            index + 1 < value.length
        ) {
            const next = StringPrototypeCharCodeAt(value, index + 1);
            if (next >= 0xdc00 && next <= 0xdfff) {
                bytes += 4;
                index++;
            } else {
                bytes += 3;
            }
        } else {
            bytes += 3;
        }
    }
    return bytes;
}

let currentWorker = null;
class WorkerInstance {
    #messageListeners = createListenerGroup("[Main-Worker] onMessage");
    #errorListeners = createListenerGroup("Worker onError");
    #processKilledListeners = createListenerGroup("Worker onProcessKilled");
    #terminated = false;
    #ready = false;
    #pendingMessages = [];
    #pendingMessageBytes = 0;
    #env;

    constructor(scriptPath, options) {
        this.#env = Object.freeze({
            USER_DATA_PATH: globalThis.__USER_DATA_PATH || "",
        });
        this.#init(scriptPath);
    }

    async #init(scriptPath) {
        try {
            console.log("[Main-Worker] creating worker for:", scriptPath);
            await op_worker_create(scriptPath);
            if (this.#terminated) {
                try {
                    op_worker_terminate();
                } catch (_) {}
                this.#pendingMessages.length = 0;
                this.#pendingMessageBytes = 0;
                return;
            }
            console.log("[Main-Worker] worker created, flushing pending msgs:", this.#pendingMessages.length);

            // Flush any messages queued before the worker was ready
            for (let index = 0; index < this.#pendingMessages.length; index++) {
                const msg = this.#pendingMessages[index];
                console.log("[Main-Worker] flushing pending message:", msg.length, "bytes");
                op_worker_post_message(msg);
            }
            this.#pendingMessages.length = 0;
            this.#pendingMessageBytes = 0;
            this.#ready = true;

            // Start error pump first so worker errors are caught immediately
            console.log("[Main-Worker] starting error and message pumps");
            this.#pumpErrors();
            this.#pumpMessages();
        } catch (e) {
            this.#ready = false;
            this.#pendingMessages.length = 0;
            this.#pendingMessageBytes = 0;
            if (!this.#terminated) {
                this.#terminated = true;
                try {
                    op_worker_terminate();
                } catch (_) {}
            }
            this.#errorListeners.trigger({ error: e });
            // Clean up on creation failure
            currentWorker = null;
        }
    }

    async #pumpMessages() {
        console.log("[Main-Worker] pumpMessages started, listeners:", this.#messageListeners.size());
        while (!this.#terminated) {
            let json;
            try {
                json = await op_worker_recv_message();
            } catch (e) {
                console.error("[Main-Worker] pumpMessages recv error:", e);
                break;
            }
            if (json === null || json === undefined) {
                console.log("[Main-Worker] pumpMessages received null, exiting");
                break;
            }

            console.log("[Main-Worker] received from worker:", json.length, "bytes, listeners:", this.#messageListeners.size());

            let message;
            try {
                message = JSON.parse(json);
            } catch (_) {
                message = json;
            }

            this.#messageListeners.trigger({ message });
        }
    }

    async #pumpErrors() {
        while (!this.#terminated) {
            let json;
            try {
                json = await op_worker_recv_error();
            } catch (_) {
                break;
            }
            if (json === null || json === undefined) break;

            let error;
            try {
                error = JSON.parse(json);
            } catch (_) {
                error = { message: json };
            }

            this.#errorListeners.trigger({ error });
        }
    }

    get env() {
        return this.#env;
    }

    postMessage(message) {
        if (this.#terminated) {
            throw new Error("Worker has been terminated");
        }
        const json = JSONStringify(message);
        if (!this.#ready) {
            const jsonBytes = utf8ByteLength(json);
            console.log("[Main-Worker] queueing pre-ready message:", jsonBytes, "bytes");
            if (jsonBytes > MAX_WORKER_MESSAGE_BYTES) {
                throw new RangeError(
                    `Worker message too large: ${jsonBytes} bytes (max ${MAX_WORKER_MESSAGE_BYTES} bytes)`,
                );
            }
            if (this.#pendingMessages.length >= MAX_PENDING_MESSAGES) {
                throw new Error("Worker message queue full");
            }
            if (
                jsonBytes >
                MAX_PENDING_MESSAGE_BYTES - this.#pendingMessageBytes
            ) {
                throw new Error("Worker message queue byte limit exceeded");
            }
            // Queue until worker thread is ready
            console.log("[Main-Worker] worker not ready, queueing message");
            ArrayPrototypePush(this.#pendingMessages, json);
            this.#pendingMessageBytes += jsonBytes;
            return;
        }
        console.log("[Main-Worker] postMessage to worker:", json.length, "UTF-16 code units");
        op_worker_post_message(json);
    }

    onMessage(listener) {
        if (typeof listener !== "function") {
            throw new TypeError("listener must be a function");
        }
        this.#messageListeners.on(listener);
    }

    onError(listener) {
        if (typeof listener !== "function") {
            throw new TypeError("listener must be a function");
        }
        this.#errorListeners.on(listener);
    }

    onProcessKilled(listener) {
        if (typeof listener !== "function") {
            throw new TypeError("listener must be a function");
        }
        this.#processKilledListeners.on(listener);
    }

    terminate() {
        if (this.#terminated) return;
        this.#terminated = true;
        this.#pendingMessages.length = 0;
        this.#pendingMessageBytes = 0;

        try {
            op_worker_terminate();
        } catch (_) {}

        currentWorker = null;
    }
}

function createWorker(scriptPath, options) {
    if (currentWorker !== null) {
        throw new Error(
            "Only one worker can exist at a time. Call terminate() first."
        );
    }

    const worker = new WorkerInstance(scriptPath, options);
    currentWorker = worker;
    return worker;
}

export { createWorker };
