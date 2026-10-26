import { op_worker_inner_recv_message,
    op_worker_inner_poll_lifecycle } from "ext:core/ops";
import * as request from "ext:host_v8_network/04_request.js";
import * as download from "ext:host_v8_network/05_download.js";
import * as upload from "ext:host_v8_network/06_upload.js";
import * as websocket from "ext:host_v8_network/07_websocket.js";
import * as innerAudio from "ext:host_v8_audio/02_inner_audio_context.js";
import * as fileManager from "ext:host_v8_file/02_file_manager.js";
import * as envApi from "ext:host_v8_env/00_env.js";
import * as timersInternal from "ext:host_v8_web/02_timers.js";
import { createListenerGroup } from "ext:host_v8_base/02_async.js";
import { postMessage, decodeMessage } from "ext:host_v8_worker_codec/00_worker_codec.js";
import { primordials } from "ext:core/mod.js";
const { ArrayIsArray } = primordials;

const messageListeners = createListenerGroup("[Worker-JS] onMessage");

function drainTimerLifecycle() {
    const transitions = op_worker_inner_poll_lifecycle();
    if (transitions === null || transitions === undefined) return;
    for (let i = 0; i < transitions.length; i++) {
        const event = transitions[i];
        timersInternal._internalSetTimerBackgrounded(event.backgrounded, event.elapsedMicros);
    }
}

timersInternal._internalSetTimerTaskPrelude(drainTimerLifecycle);

async function _startMessagePump() {
    while (true) {
        let event;
        try {
            event = await op_worker_inner_recv_message();
        } catch (_) {
            break;                  // the main side is gone: nothing more comes
        }
        // null/undefined means Terminate signal received
        if (event === null || event === undefined) break;

        if (!ArrayIsArray(event)) {
            // The independent synchronous cursor is authoritative. Applying
            // this already-resolved async envelope could replay an older edge
            // after a timer prelude has consumed a newer hide/show snapshot.
            drainTimerLifecycle();
            continue;
        }

        const message = decodeMessage(event);
        messageListeners.trigger({ message });
    }
}

const worker = {
    postMessage(message) {
        postMessage(message);
    },

    onMessage(listener) {
        if (typeof listener !== "function") {
            throw new TypeError("listener must be a function");
        }
        messageListeners.on(listener);
    },

    connectSocket: websocket.connectSocket,
    createInnerAudioContext: innerAudio.createInnerAudioContext,
    downloadFile: download.downloadFile,
    env: envApi.env,
    getFileSystemManager() {
        return fileManager.getFileSystemManager();
    },
    request: request.request,
    uploadFile: upload.uploadFile,
};

export { worker, _startMessagePump };
