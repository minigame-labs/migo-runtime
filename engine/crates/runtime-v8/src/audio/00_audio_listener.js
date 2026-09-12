import { AudioParam } from "ext:host_v8_audio/00_audio_param.js";
import { warnUnsupportedCapability } from "ext:host_v8_audio/00_audio_node.js";

/**
 * AudioListener: represents the listener in 3D audio space.
 *
 * Used by PannerNode for spatialization calculations.
 * Position and orientation can be set via AudioParam for automation.
 */
function listenerParam(defaultValue, minValue, maxValue) {
  const param = new AudioParam(defaultValue, minValue, maxValue);
  let current = defaultValue;
  const warn = () => warnUnsupportedCapability(
    "listener-position",
    "AudioListener position/orientation is stored in JS but not applied by native panners",
  );
  const set = (value) => {
    const number = Number(value);
    if (!Number.isNaN(number)) {
      current = Math.max(minValue, Math.min(maxValue, number));
    }
    warn();
    return param;
  };
  Object.defineProperties(param, {
    value: {
      configurable: false,
      enumerable: true,
      get: () => current,
      set,
    },
    setValueAtTime: { value: set },
    linearRampToValueAtTime: { value: set },
    exponentialRampToValueAtTime: { value: set },
    setTargetAtTime: { value: set },
    cancelScheduledValues: { value: warn },
  });
  return param;
}

class AudioListener {
  #positionX;
  #positionY;
  #positionZ;
  #forwardX;
  #forwardY;
  #forwardZ;
  #upX;
  #upY;
  #upZ;

  constructor() {
    const MIN = -3.4028235e38;
    const MAX = 3.4028235e38;
    this.#positionX = listenerParam(0, MIN, MAX);
    this.#positionY = listenerParam(0, MIN, MAX);
    this.#positionZ = listenerParam(0, MIN, MAX);
    this.#forwardX = listenerParam(0, MIN, MAX);
    this.#forwardY = listenerParam(0, MIN, MAX);
    this.#forwardZ = listenerParam(-1, MIN, MAX);
    this.#upX = listenerParam(0, MIN, MAX);
    this.#upY = listenerParam(1, MIN, MAX);
    this.#upZ = listenerParam(0, MIN, MAX);
  }

  get positionX() { return this.#positionX; }
  get positionY() { return this.#positionY; }
  get positionZ() { return this.#positionZ; }
  get forwardX() { return this.#forwardX; }
  get forwardY() { return this.#forwardY; }
  get forwardZ() { return this.#forwardZ; }
  get upX() { return this.#upX; }
  get upY() { return this.#upY; }
  get upZ() { return this.#upZ; }

  setPosition(x, y, z) {
    this.#positionX.value = x;
    this.#positionY.value = y;
    this.#positionZ.value = z;
  }

  setOrientation(x, y, z, upX, upY, upZ) {
    this.#forwardX.value = x;
    this.#forwardY.value = y;
    this.#forwardZ.value = z;
    this.#upX.value = upX;
    this.#upY.value = upY;
    this.#upZ.value = upZ;
  }
}

export { AudioListener };
