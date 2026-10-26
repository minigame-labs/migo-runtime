import { op_worker_create,
    op_worker_recv_message,
    op_worker_recv_error,
    op_worker_terminate } from "ext:core/ops";
import { createListenerGroup } from "ext:host_v8_base/02_async.js";
import { primordials } from "ext:core/mod.js";
import { encodeMessage, decodeMessage, wireBytes, postMessage, postEncodedMessage } from "ext:host_v8_worker_codec/00_worker_codec.js";

const {
    ArrayPrototypePush,
    Error,
    MathMin,
} = primordials;

const MAX_PENDING_MESSAGES = 4096;
const MAX_PENDING_MESSAGE_BYTES = 64 * 1024 * 1024;
const MAX_WORKER_MESSAGE_BYTES = 16 * 1024 * 1024;

let currentWorker = null;
class WorkerInstance {
    #messageListeners = createListenerGroup("[Main-Worker] onMessage");
    #errorListeners = createListenerGroup("Worker onError");
    #processKilledListeners = createListenerGroup("Worker onProcessKilled");
    #terminated = false;
    #ready = false;
    #pendingMessages = [];
    #pendingMessageBytes = 0;
    #pendingReservations = 0;
    #env;

    constructor(scriptPath, options) {
        this.#env = Object.freeze({
            USER_DATA_PATH: globalThis.__USER_DATA_PATH || "",
        });
        this.#init(scriptPath);
    }

    async #init(scriptPath) {
        try {
            await op_worker_create(scriptPath);
            if (this.#terminated) {
                // terminate() already addressed this attempt. An old create
                // completion must never terminate a newly created Worker.
                this.#pendingMessages.length = 0;
                this.#pendingMessageBytes = 0;
                return;
            }
            // Flush any messages queued before the worker was ready
            for (let index = 0; index < this.#pendingMessages.length; index++) {
                postEncodedMessage(this.#pendingMessages[index]);
            }
            this.#pendingMessages.length = 0;
            this.#pendingMessageBytes = 0;
            this.#ready = true;

            // Start error pump first so worker errors are caught immediately
            this.#pumpErrors();
            this.#pumpMessages();
        } catch (e) {
            this.#ready = false;
            this.#pendingMessages.length = 0;
            this.#pendingMessageBytes = 0;
            const report = !this.#terminated;
            if (report) {
                this.#terminated = true;
                try {
                    op_worker_terminate();
                } catch (_) {}
            }
            if (report) this.#errorListeners.trigger({ error: e });
            // Clean up on creation failure
            if (currentWorker === this) currentWorker = null;
        }
    }

    async #pumpMessages() {
        while (!this.#terminated) {
            let wire;
            try {
                wire = await op_worker_recv_message();
            } catch (_) {
                break;              // the worker is gone (terminated): nothing more comes
            }
            if (this.#terminated || wire === null || wire === undefined) break;
            const message = decodeMessage(wire);
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
            if (this.#terminated || json === null || json === undefined) break;

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
        if (!this.#ready) {
            if (this.#pendingMessages.length + this.#pendingReservations >= MAX_PENDING_MESSAGES) {
                throw new Error("Worker message queue full");
            }
            const maxBytes = MathMin(MAX_WORKER_MESSAGE_BYTES, MAX_PENDING_MESSAGE_BYTES - this.#pendingMessageBytes);
            if (maxBytes <= 0) {
                throw new Error("Worker message queue byte limit exceeded");
            }
            // Reserve before cloning: getters can synchronously post again or
            // terminate this instance. Never serialize first and check later.
            this.#pendingReservations++;
            this.#pendingMessageBytes += maxBytes;
            let retained = 0;
            try {
                const wire = encodeMessage(message, maxBytes);
                if (this.#terminated) throw new Error("Worker has been terminated");
                ArrayPrototypePush(this.#pendingMessages, wire);
                retained = wireBytes(wire);
            } finally {
                this.#pendingReservations--;
                // terminate() may already have cleared all retained entries.
                if (!this.#terminated) this.#pendingMessageBytes -= maxBytes - retained;
            }
            return;
        }
        postMessage(message);
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

        if (currentWorker === this) currentWorker = null;
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
