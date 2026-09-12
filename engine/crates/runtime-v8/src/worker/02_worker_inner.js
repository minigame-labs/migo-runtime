import { op_worker_inner_post_message,
    op_worker_inner_recv_message } from "ext:core/ops";
import * as request from "ext:host_v8_network/04_request.js";
import * as download from "ext:host_v8_network/05_download.js";
import * as upload from "ext:host_v8_network/06_upload.js";
import * as websocket from "ext:host_v8_network/07_websocket.js";
import * as innerAudio from "ext:host_v8_audio/02_inner_audio_context.js";
import * as fileManager from "ext:host_v8_file/02_file_manager.js";
import * as envApi from "ext:host_v8_env/00_env.js";
import * as timersInternal from "ext:host_v8_web/02_timers.js";
import { createListenerGroup } from "ext:host_v8_base/02_async.js";

const messageListeners = createListenerGroup("[Worker-JS] onMessage");

async function _startMessagePump() {
    console.log("[Worker-JS] message pump started, listeners:", messageListeners.size());
    while (true) {
        let event;
        try {
            event = await op_worker_inner_recv_message();
        } catch (e) {
            console.error("[Worker-JS] recv error:", e);
            break;
        }
        // null/undefined means Terminate signal received
        if (event === null || event === undefined) {
            console.log("[Worker-JS] received null/terminate, exiting pump");
            break;
        }

        if (event.type === "lifecycle") {
            timersInternal._internalSetTimerBackgrounded(
                event.backgrounded,
                event.elapsedMicros,
            );
            continue;
        }

        const json = event.data;

        console.log("[Worker-JS] received message, listeners:", messageListeners.size());

        let message;
        try {
            message = JSON.parse(json);
        } catch (_) {
            message = json;
        }

        messageListeners.trigger({ message });
    }
}

const worker = {
    postMessage(message) {
        const serialized = JSON.stringify(message);
        console.log("[Worker-JS] postMessage to main:", serialized);
        op_worker_inner_post_message(serialized);
    },

    onMessage(listener) {
        if (typeof listener !== "function") {
            throw new TypeError("listener must be a function");
        }
        messageListeners.on(listener);
        console.log("[Worker-JS] onMessage listener registered, total:", messageListeners.size());
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
