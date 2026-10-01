import { primordials } from "ext:core/mod.js";
const { Uint8Array, Uint32Array, TypedArrayPrototypeGetBuffer } = primordials;
import { op_now, op_now_us } from "ext:core/ops";

const hrU8 = new Uint8Array(8);
const hr = new Uint32Array(TypedArrayPrototypeGetBuffer(hrU8));

class BrowserPerformance {
    #timeOrigin = 0;

    now() {
        op_now(hrU8);
        return hr[0] * 1000 + hr[1] / 1e6;
    }

    // The wall-clock time, in milliseconds since the epoch, at which `now()` was zero:
    // `performance.timeOrigin + performance.now()` is the current epoch time, which is how
    // content turns a `now()` or an animation-frame timestamp into a date. Read from the two
    // clocks once and kept, so it does not drift with later adjustments of the wall clock.
    get timeOrigin() {
        if (this.#timeOrigin === 0) {
            this.#timeOrigin = Date.now() - this.now();
        }
        return this.#timeOrigin;
    }
}

class Performance {
    now() {
        return op_now_us();
    }
}

export const performance = new BrowserPerformance();

const minigamePerformance = new Performance();
export const getPerformance = () => minigamePerformance;