import {
  op_camera_create,
  op_camera_destroy,
  op_camera_set,
  op_camera_take_photo,
  op_camera_start_record,
  op_camera_stop_record,
  op_camera_set_zoom,
  op_camera_listen_frame_change,
  op_camera_close_frame_change,
} from "ext:core/ops";
import { allocateHostCallbackId, createDeferredApi, createListenerGroup, invokeCallback, failMessage } from "ext:host_v8_base/02_async.js";

// The cameras content holds, by id: where the host's events and frames go.
const _cameras = new Map();

// Every request is answered through its hook once what it asked has happened.
// Opening a camera may wait on the system asking the player, and a long video
// takes its time to be written: those two have no timeout.
const _createApi = createDeferredApi("createCamera", 0);
const _takePhotoApi = createDeferredApi("camera.takePhoto");
const _startRecordApi = createDeferredApi("camera.startRecord");
const _stopRecordApi = createDeferredApi("camera.stopRecord", 0);
const _setZoomApi = createDeferredApi("camera.setZoom");

const QUALITIES = ["high", "normal", "low"];
const POSITIONS = ["front", "back"];
const FLASHES = ["auto", "on", "off"];
const SIZES = ["small", "medium", "large"];

// A command the host carries out and does not answer: one it refuses is
// reported, since there is no call to fail.
function command(op, request) {
  try {
    op(JSON.stringify(request));
  } catch (e) {
    console.error(failMessage("camera", e));
  }
}

/**
 * A camera: its preview over the game, its frames, photos, video and zoom.
 * Made by createCamera; its properties may be set, moving the preview, turning
 * to the other camera, changing the flash or the frames' size.
 */
class Camera {
  #id;
  #destroyed = false;
  #x;
  #y;
  #width;
  #height;
  #devicePosition;
  #flash;
  #size;
  #listeners = {
    cameraFrame: createListenerGroup("Camera cameraFrame"),
    stop: createListenerGroup("Camera stop"),
    authCancel: createListenerGroup("Camera authCancel"),
  };

  constructor(id, options) {
    this.#id = id;
    this.#x = options.x;
    this.#y = options.y;
    this.#width = options.width;
    this.#height = options.height;
    this.#devicePosition = options.devicePosition;
    this.#flash = options.flash;
    this.#size = options.size;
  }

  get x() { return this.#x; }
  set x(value) { this.#change("x", value, finite); }
  get y() { return this.#y; }
  set y(value) { this.#change("y", value, finite); }
  get width() { return this.#width; }
  set width(value) { this.#change("width", value, positive); }
  get height() { return this.#height; }
  set height(value) { this.#change("height", value, positive); }
  get devicePosition() { return this.#devicePosition; }
  set devicePosition(value) { this.#change("devicePosition", value, oneOf(POSITIONS)); }
  get flash() { return this.#flash; }
  set flash(value) { this.#change("flash", value, oneOf(FLASHES)); }
  get size() { return this.#size; }
  set size(value) { this.#change("size", value, oneOf(SIZES)); }

  /**
   * Hand content the frames, each to the onCameraFrame callbacks.
   * @param {Worker} [worker] - Accepted for the reference's iOS ExperimentalWorker; frames reach this isolate.
   */
  listenFrameChange(worker) {
    if (this.#destroyed) return;
    command(op_camera_listen_frame_change, { cameraId: this.#id });
  }

  /** Stop handing content the frames. */
  closeFrameChange() {
    if (this.#destroyed) return;
    command(op_camera_close_frame_change, { cameraId: this.#id });
  }

  /**
   * @param {Function} callback - Receives {width, height, data}: data an ArrayBuffer of RGBA, four bytes a pixel.
   */
  onCameraFrame(callback) {
    this.#listeners.cameraFrame.on(callback);
  }

  /** @param {Function} callback - The camera stopped on its own: the app went to the background, say. */
  onStop(callback) {
    this.#listeners.stop.on(callback);
  }

  /** @param {Function} callback - The player refused the system's permission to the camera. */
  onAuthCancel(callback) {
    this.#listeners.authCancel.on(callback);
  }

  /**
   * @param {string} [quality] - "high", "normal" (the default) or "low"
   * @returns {Promise<{tempImagePath: string, width: number, height: number}>}
   */
  takePhoto(quality) {
    return this.#call(_takePhotoApi, "camera.takePhoto", op_camera_take_photo, {
      quality: QUALITIES.includes(quality) ? quality : "normal",
    });
  }

  /** @returns {Promise} Settled once the camera records. */
  startRecord() {
    return this.#call(_startRecordApi, "camera.startRecord", op_camera_start_record, {});
  }

  /**
   * @param {boolean} [compressed] - Whether to compress the video
   * @returns {Promise<{tempThumbPath: string, tempVideoPath: string}>}
   */
  stopRecord(compressed) {
    return this.#call(_stopRecordApi, "camera.stopRecord", op_camera_stop_record, {
      compressed: compressed === true,
    });
  }

  /**
   * @param {Object} args
   * @param {number} args.zoom - From 1 to the camera's maximum, to a tenth
   * @returns {Promise<{zoom: number}>} The zoom the camera took
   */
  setZoom(args) {
    const zoom = args !== null && typeof args === "object" ? Number(args.zoom) : NaN;
    if (!Number.isFinite(zoom) || zoom < 1) {
      return Promise.reject({ errMsg: "camera.setZoom:fail zoom must be a number from 1" });
    }
    return this.#call(_setZoomApi, "camera.setZoom", op_camera_set_zoom, { zoom: Math.round(zoom * 10) / 10 });
  }

  /** Close the camera and take its preview away. */
  destroy() {
    if (this.#destroyed) return;
    this.#destroyed = true;
    _cameras.delete(this.#id);
    command(op_camera_destroy, { cameraId: this.#id });
    for (const key of Object.keys(this.#listeners)) {
      this.#listeners[key].off();
    }
  }

  // One request of this camera, `name`, answered through `api`'s hook.
  #call(api, name, op, request) {
    if (this.#destroyed) {
      return Promise.reject({ errMsg: failMessage(name, "camera destroyed") });
    }
    request.cameraId = this.#id;
    return api.invoke({}, (opts, requestId) => {
      request.requestId = requestId;
      op(JSON.stringify(request));
    });
  }

  // A property content set: kept, and the host told, when it is one the
  // reference allows; anything else is ignored, as a browser ignores it.
  #change(name, value, valid) {
    if (this.#destroyed || !valid(value)) return;
    switch (name) {
      case "x": this.#x = value; break;
      case "y": this.#y = value; break;
      case "width": this.#width = value; break;
      case "height": this.#height = value; break;
      case "devicePosition": this.#devicePosition = value; break;
      case "flash": this.#flash = value; break;
      case "size": this.#size = value; break;
      default: return;
    }
    command(op_camera_set, { cameraId: this.#id, [name]: value });
  }

  _handleEvent(type) {
    this.#listeners[type].trigger();
  }

  _handleFrameData(data, width, height) {
    this.#listeners.cameraFrame.trigger({ data, width, height });
  }
}

function finite(value) {
  return typeof value === "number" && Number.isFinite(value);
}

function positive(value) {
  return finite(value) && value > 0;
}

function oneOf(values) {
  return (value) => values.includes(value);
}

// An option content gave, or the reference's default when it gave none that
// is valid.
function option(value, valid, fallback) {
  return valid(value) ? value : fallback;
}

/**
 * Open a camera and show its preview over the game.
 *
 * @param {Object} [options={}]
 * @param {number} [options.x=0] - Left, in the game's pixels
 * @param {number} [options.y=0] - Top
 * @param {number} [options.width=300]
 * @param {number} [options.height=150]
 * @param {string} [options.devicePosition="back"] - "front" or "back"
 * @param {string} [options.flash="auto"] - "auto", "on" or "off"
 * @param {string} [options.size="small"] - The frames' size: "small", "medium" or "large"
 * @param {Function} [options.success] - Called once the preview shows
 * @param {Function} [options.fail]
 * @param {Function} [options.complete]
 * @returns {Camera}
 */
function createCamera(options = {}) {
  const settings = {
    x: option(options.x, finite, 0),
    y: option(options.y, finite, 0),
    width: option(options.width, positive, 300),
    height: option(options.height, positive, 150),
    devicePosition: option(options.devicePosition, oneOf(POSITIONS), "back"),
    flash: option(options.flash, oneOf(FLASHES), "auto"),
    size: option(options.size, oneOf(SIZES), "small"),
  };
  let cameraId;
  try {
    // From the Host's id space rather than a module counter: a camera the
    // platform still holds for a runtime that was replaced must not be taken
    // for the replacement's, and an exhausted id space fails the call rather
    // than handing out a camera under an id nobody allocated.
    cameraId = allocateHostCallbackId();
  } catch (e) {
    const res = { errMsg: failMessage("createCamera", e) };
    invokeCallback("createCamera", "fail", options.fail, res);
    invokeCallback("createCamera", "complete", options.complete, res);
    return undefined;
  }
  // The handle is content's whatever the host answers, so its later calls fail
  // cleanly instead of on `undefined`.
  const camera = new Camera(cameraId, settings);
  _cameras.set(cameraId, camera);
  // Not a promise API: content hears through its callbacks, and a camera that
  // did not open is gone -- destroyed, so the host lets go of whatever it had
  // opened, and nothing more reaches it.
  _createApi.invoke(
    {
      success: options.success,
      fail: (res) => {
        camera.destroy();
        invokeCallback("createCamera", "fail", options.fail, res);
      },
      complete: options.complete,
    },
    (opts, requestId) => {
      op_camera_create(JSON.stringify(Object.assign({ requestId, cameraId }, settings)));
    },
  );
  return camera;
}

function _internalOnCreateCameraResult(resultJson) {
  _createApi.settle(resultJson);
}

function _internalOnCameraTakePhotoResult(resultJson) {
  _takePhotoApi.settle(resultJson);
}

function _internalOnCameraStartRecordResult(resultJson) {
  _startRecordApi.settle(resultJson);
}

function _internalOnCameraStopRecordResult(resultJson) {
  _stopRecordApi.settle(resultJson);
}

function _internalOnCameraSetZoomResult(resultJson) {
  _setZoomApi.settle(resultJson);
}

// The camera an event names, or none.
function cameraOf(eventJson) {
  try {
    const event = JSON.parse(eventJson);
    return event !== null && typeof event === "object" ? _cameras.get(event.cameraId) : undefined;
  } catch (_) {
    return undefined;
  }
}

function _internalOnCameraStopEvent(eventJson) {
  const camera = cameraOf(eventJson);
  if (camera) camera._handleEvent("stop");
}

function _internalOnCameraAuthCancelEvent(eventJson) {
  const camera = cameraOf(eventJson);
  if (camera) camera._handleEvent("authCancel");
}

/**
 * Called from native with one frame.
 * @param {number} cameraId
 * @param {ArrayBuffer} data - RGBA, four bytes a pixel, rows top first
 * @param {number} width
 * @param {number} height
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
  _internalOnCreateCameraResult,
  _internalOnCameraTakePhotoResult,
  _internalOnCameraStartRecordResult,
  _internalOnCameraStopRecordResult,
  _internalOnCameraSetZoomResult,
  _internalOnCameraStopEvent,
  _internalOnCameraAuthCancelEvent,
  _internalOnCameraFrameData,
};
