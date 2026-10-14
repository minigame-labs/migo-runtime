import * as amdshim from "ext:host_v8_base/01_amdshim.js"
import * as gc from "ext:host_v8_base/03_gc.js"
import * as console from "ext:host_v8_console/01_console.js"
import * as event from "ext:host_v8_event/01_event.js"
import * as timers from "ext:host_v8_web/02_timers.js";
import * as request from "ext:host_v8_network/04_request.js";
import * as download from "ext:host_v8_network/05_download.js";
import * as upload from "ext:host_v8_network/06_upload.js";
import * as websocket from "ext:host_v8_network/07_websocket.js";
import * as prefetchApi from "ext:host_v8_network/10_prefetch.js";
import * as save from "ext:host_v8_file/01_save.js";
import * as codec from "ext:host_v8_utility/01_codec.js";
import * as image from "ext:host_v8_image/01_image.js";
import * as imageData from "ext:host_v8_image/01_image_data.js";

import { core } from "ext:core/mod.js";

const windowOrWorkerGlobalScope = {
    define: core.propNonEnumerable(amdshim.define),
    require: core.propNonEnumerable(amdshim.require),

    console: core.propNonEnumerable(console.console),

    // Event
    onError: core.propWritable(event.onError),
    offError: core.propWritable(event.offError),
    onUnhandledRejection: core.propWritable(event.onUnhandledRejection),
    offUnhandledRejection: core.propWritable(event.offUnhandledRejection),

    // Timers
    setTimeout: core.propWritable(timers.setTimeout),
    clearTimeout: core.propWritable(timers.clearTimeout),
    setInterval: core.propWritable(timers.setInterval),
    clearInterval: core.propWritable(timers.clearInterval),
    setImmediate: core.propWritable(timers.setImmediate),

    // Network
    request: core.propWritable(request.request),
    downloadFile: core.propWritable(download.downloadFile),
    uploadFile: core.propWritable(upload.uploadFile),

    // Prefetch
    prefetchDns: core.propWritable(prefetchApi.prefetchDns),
    prefetchAssets: core.propWritable(prefetchApi.prefetchAssets),

    // WebSocket
    connectSocket: core.propWritable(websocket.connectSocket),
    sendSocketMessage: core.propWritable(websocket.sendSocketMessage),
    closeSocket: core.propWritable(websocket.closeSocket),
    onSocketOpen: core.propWritable(websocket.onSocketOpen),
    offSocketOpen: core.propWritable(websocket.offSocketOpen),
    onSocketMessage: core.propWritable(websocket.onSocketMessage),
    offSocketMessage: core.propWritable(websocket.offSocketMessage),
    onSocketError: core.propWritable(websocket.onSocketError),
    offSocketError: core.propWritable(websocket.offSocketError),
    onSocketClose: core.propWritable(websocket.onSocketClose),
    offSocketClose: core.propWritable(websocket.offSocketClose),

    // File
    saveFileToDisk: core.propNonEnumerable(save.saveFileToDisk),
    _internalOnSaveFileToDiskResult: core.propNonEnumerable(save._internalOnSaveFileToDiskResult),
    saveFile: core.propNonEnumerable(save.saveFile),
    saveFileSync: core.propNonEnumerable(save.saveFileSync),
    getFileSystemManager: core.propNonEnumerable(save.getFileSystemManager),

    // Utility
    encode: core.propNonEnumerable(codec.encode),
    decode: core.propNonEnumerable(codec.decode),

    // Image
    createImage: core.propNonEnumerable(image.createImage),
    createImageData: core.propNonEnumerable(imageData.createImageData),
    // Browser parity: ImageBitmap constructor + createImageBitmap()
    // factory.  The factory is the canonical entry point; exposing
    // the class too lets small-game code do `instanceof ImageBitmap`
    // for polyfill detection without relying on prototype probing.
    ImageBitmap: core.propNonEnumerable(image.ImageBitmap),
    createImageBitmap: core.propNonEnumerable(image.createImageBitmap),

    // GC / Memory
    triggerGC: core.propNonEnumerable(gc.triggerGC),
    getHeapStatistics: core.propNonEnumerable(gc.getHeapStatistics),
};

export {
    windowOrWorkerGlobalScope
}
