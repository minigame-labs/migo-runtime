import { op_get_battery_info } from "ext:core/ops";
import { failMessage, promisify } from "ext:host_v8_base/02_async.js";

// What the host reports, or its failure: a device with no battery -- a desktop,
// a host that reports none -- answers "not supported" rather than a level
// nobody measured.
function getBatteryInfoSync() {
    try {
        return JSON.parse(op_get_battery_info());
    } catch (e) {
        throw new Error(failMessage('getBatteryInfoSync', e));
    }
}

const getBatteryInfo = promisify('getBatteryInfo', () => JSON.parse(op_get_battery_info()));

export { getBatteryInfo, getBatteryInfoSync };
