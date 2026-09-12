
import {
  op_camera_create,
  op_camera_destroy,
  op_camera_take_photo,
  op_camera_start_record,
  op_camera_stop_record,
  op_camera_set_zoom,
  op_camera_listen_frame_change,
  op_camera_close_frame_change,
} from "ext:core/ops";
import { allocateHostCallbackId, wrapAsync, createListenerGroup, invokeCallback, errorMessage } from "ext:host_v8_base/02_async.js";

// Camera instance registry: cameraId -> Camera
const _cameras = new Map();

// Auto-incrementing camera ID counter

/**
 * Camera - Provides access to device camera for photo capture,
 * video recording, and real-time frame streaming.
 *
 * Created via createCamera(). Each instance has a unique ID
 * used to route native events back to the correct instance.
 */
class Camera {
  #id;
  #destroyed = false;
  #pendingPhotos = new Map();
  // Multi-listener arrays for event callbacks
  #listeners = {
    cameraFrame: createListenerGroup("Camera cameraFrame"),
    stop: createListenerGroup("Camera stop"),
    authCancel: createListenerGroup("Camera authCancel"),
    error: createListenerGroup("Camera error"),
  };

  constructor(id) {
    this.#id = id;
  }

  // ==================== Frame Streaming ====================

  /**
   * Start listening for camera frame changes.
   * Begins high-frequency frame data streaming from the native camera.
   *
   * @param {Worker} [worker] - Optional Worker for iOS ExperimentalWorker frame data
   */
  listenFrameChange(worker) {
    try {
      op_camera_listen_frame_change(this.#id);
    } catch (e) {
      this.#fireListeners("error", { errMsg: errorMessage(e) });
    }
  }

  /**
   * Stop listening for camera frame changes.
   */
  closeFrameChange() {
    try {
      op_camera_close_frame_change(this.#id);
    } catch (e) {
      this.#fireListeners("error", { errMsg: errorMessage(e) });
    }
  }

  /**
   * Register a callback for camera frame data.
   * Unlike listenFrameChange, this only registers the callback
   * without starting native frame streaming.
   *
   * @param {Function} callback - Receives {data: ArrayBuffer, width: number, height: number}
   */
  onCameraFrame(callback) {
    this.#listeners.cameraFrame.on(callback);
  }

  /**
   * Register a callback for camera stop events.
   * @param {Function} callback
   */
  onStop(callback) {
    this.#listeners.stop.on(callback);
  }

  /**
   * Register a callback for authorization cancellation.
   * @param {Function} callback
   */
  onAuthCancel(callback) {
    this.#listeners.authCancel.on(callback);
  }

  // ==================== Photo Capture ====================

  /**
   * Take a photo.
   *
   * @param {Object} [options]
   * @param {string} [options.quality] - "high", "normal", or "low"
   * @returns {Promise<{tempImagePath: string, width: number, height: number}>}
   */
  takePhoto(options = {}) {
    const opts = JSON.stringify({
      cameraId: this.#id,
      quality: options.quality ?? "normal",
    });
    const requestId = op_camera_take_photo(opts);
    const photo = new Promise((resolve, reject) => {
      this.#pendingPhotos.set(requestId, { resolve, reject });
    });
    return wrapAsync('camera.takePhoto', () => photo, options);
  }

  // ==================== Video Recording ====================

  /**
   * Start video recording.
   *
   * @param {Object} [options]
   * @returns {Promise<void>}
   */
  startRecord(options = {}) {
    const opts = JSON.stringify({ cameraId: this.#id });
    return wrapAsync('camera.startRecord', () => { op_camera_start_record(opts); }, options);
  }

  /**
   * Stop video recording.
   *
   * @param {Object} [options]
   * @param {boolean} [options.compressed] - Whether to compress the video
   * @returns {Promise<{tempThumbPath: string, tempVideoPath: string}>}
   */
  stopRecord(options = {}) {
    const opts = JSON.stringify({
      cameraId: this.#id,
      compressed: options.compressed ?? false,
    });
    return wrapAsync('camera.stopRecord', () => JSON.parse(op_camera_stop_record(opts)), options);
  }

  // ==================== Zoom ====================

  /**
   * Set camera zoom level.
   *
   * @param {Object} [options]
   * @param {number} options.zoom - Zoom level, range [1, maxZoom]
   * @returns {Promise<{zoom: number}>} Actual zoom level applied
   */
  setZoom(options = {}) {
    const opts = JSON.stringify({
      cameraId: this.#id,
      zoom: options.zoom,
    });
    return wrapAsync('camera.setZoom', () => JSON.parse(op_camera_set_zoom(opts)), options);
  }

  // ==================== Lifecycle ====================

  /**
   * Destroy the camera instance and release all resources.
   */
  destroy() {
    if (this.#destroyed) return;
    for (const pending of this.#pendingPhotos.values()) {
      pending.reject({ errMsg: "camera.takePhoto:fail camera destroyed" });
    }
    this.#pendingPhotos.clear();
    this.#destroyed = true;
    _cameras.delete(this.#id);

    try {
      op_camera_destroy(this.#id);
    } catch (e) {
      // Ignore errors during cleanup
    }

    // Clear all listeners
    for (const key of Object.keys(this.#listeners)) {
      this.#listeners[key].off();
    }
  }

  // ==================== Internal ====================

  /** Fire all listeners for the given event type. */
  #fireListeners(type, arg) {
    const group = this.#listeners[type];
    if (!group) return;
    group.trigger(arg);
  }

  /**
   * Internal: handle event from native layer.
   * @param {string} eventType - "stop", "authCancel", "error"
   * @param {string} jsonPayload - JSON data
   */
  _handleEvent(eventType, jsonPayload) {
    if (eventType === "takePhotoResult") {
      let result;
      try {
        result = JSON.parse(jsonPayload);
      } catch (_) {
        return;
      }
      const requestId = result && result.requestId;
      const pending = this.#pendingPhotos.get(requestId);
      if (!pending) return;
      this.#pendingPhotos.delete(requestId);
      if (result._error) {
        pending.reject(result._error);
      } else {
        pending.resolve(result);
      }
      return;
    }

    switch (eventType) {
      case "stop":
        this.#fireListeners("stop");
        break;
      case "authCancel":
        this.#fireListeners("authCancel");
        break;
      case "error": {
        let errObj = { errMsg: "camera.error:fail unknown error" };
        try {
          errObj = JSON.parse(jsonPayload);
        } catch (_) {}
        this.#fireListeners("error", errObj);
        break;
      }
    }
  }

  /**
   * Internal: handle frame data from native layer.
   * @param {ArrayBuffer} data - Raw pixel data (RGBA)
   * @param {number} width - Frame width in pixels
   * @param {number} height - Frame height in pixels
   */
  _handleFrameData(data, width, height) {
    this.#fireListeners("cameraFrame", { data, width, height });
  }
}

/**
 * Create a Camera instance.
 *
 * @param {Object} [options={}]
 * @param {number} [options.x=0] - Left x coordinate
 * @param {number} [options.y=0] - Top y coordinate
 * @param {number} [options.width=300] - Camera width
 * @param {number} [options.height=150] - Camera height
 * @param {string} [options.devicePosition="back"] - Camera position: "front" or "back"
 * @param {string} [options.flash="auto"] - Flash mode: "auto", "on", "off"
 * @param {string} [options.size="small"] - Frame data image size: "small", "medium", "large"
 * @param {Function} [options.success] - Success callback
 * @param {Function} [options.fail] - Failure callback
 * @param {Function} [options.complete] - Complete callback (always called)
 * @returns {Camera}
 */
function createCamera(options = {}) {
  let cameraId;
  let camera;
  try {
    // From the Host's id space rather than a module counter: the platform keeps
    // the camera slot in a table that outlives this isolate, so a counter
    // restarting at 1 aims the retired runtime's frames at the replacement's
    // camera. Inside this `try` on purpose -- an exhausted id space then takes
    // the same `fail`/`complete` path everything else here takes, and needs no
    // failure convention of its own.
    cameraId = allocateHostCallbackId();
    const opts = JSON.stringify({
      cameraId,
      x: options.x ?? 0,
      y: options.y ?? 0,
      width: options.width ?? 300,
      height: options.height ?? 150,
      devicePosition: options.devicePosition ?? "back",
      flash: options.flash ?? "auto",
      size: options.size ?? "small",
    });

    op_camera_create(opts);
    camera = new Camera(cameraId);
    _cameras.set(cameraId, camera);

    invokeCallback("createCamera", "success", options.success, { camera });
  } catch (e) {
    const errMsg = "createCamera:fail " + (errorMessage(e));
    invokeCallback("createCamera", "fail", options.fail, { errMsg });
    // The host refused, but the id is this runtime's: content still gets a
    // handle so its later calls fail cleanly instead of on `undefined`. With no
    // id there is no handle to give -- a Camera under an id nobody allocated
    // would route somebody else's frames.
    if (!camera && cameraId !== undefined) {
      camera = new Camera(cameraId);
      _cameras.set(cameraId, camera);
    }
  } finally {
    invokeCallback("createCamera", "complete", options.complete, undefined);
  }

  return camera;
}

/**
 * Internal: called from native to dispatch camera events.
 * @param {number} cameraId - Camera instance ID
 * @param {string} eventType - Event type
 * @param {string} jsonPayload - JSON payload
 */
function _internalOnCameraEvent(cameraId, eventType, jsonPayload) {
  const camera = _cameras.get(cameraId);
  if (camera) {
    camera._handleEvent(eventType, jsonPayload);
  }
}

/**
 * Internal: called from native to dispatch camera frame data.
 * @param {number} cameraId - Camera instance ID
 * @param {ArrayBuffer} data - Raw pixel data
 * @param {number} width - Frame width
 * @param {number} height - Frame height
 */
function _internalOnCameraFrameData(cameraId, data, width, height) {
  const camera = _cameras.get(cameraId);
  if (camera) {
    camera._handleFrameData(data, width, height);
  }
}

export {
  Camera,
  createCamera,
  _internalOnCameraEvent,
  _internalOnCameraFrameData,
};
