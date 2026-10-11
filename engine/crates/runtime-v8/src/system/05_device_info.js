import { op_get_device_info } from "ext:core/ops";
import { appInfo } from "ext:host_v8_system/07_app_info.js";

function getDeviceInfo() {
    let device;
    try {
        device = JSON.parse(op_get_device_info());
    } catch (error) {
        console.error("Failed to get device info:", error);
        device = {
            abi: "",
            deviceAbi: "",
            brand: "",
            model: "unknown",
            system: "",
            platform: "",
            cpuType: "",
            memorySize: ""
        };
    }
    // The host's score for the device, the one getDeviceBenchmarkInfo answers.
    device.benchmarkLevel = appInfo().benchmarkLevel;
    return device;
}

export { getDeviceInfo };
