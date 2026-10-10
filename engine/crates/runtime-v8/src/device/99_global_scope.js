// Global scope registration for host_v8_device APIs (api-sensors feature gate).
// When the api-sensors feature is disabled the entire extension (including
// this file) is excluded and the APIs are simply absent from globalThis.

import * as deviceMotion from 'ext:host_v8_device/01_device_motion.js';
import * as gyroscope from 'ext:host_v8_device/02_gyroscope.js';
import * as orientation from 'ext:host_v8_device/03_orientation.js';
import * as compass from 'ext:host_v8_device/04_compass.js';
import * as accelerometer from 'ext:host_v8_device/05_accelerometer.js';
import * as battery from 'ext:host_v8_device/06_battery.js';
import * as clipboard from 'ext:host_v8_device/07_clipboard.js';
import * as vibrate from 'ext:host_v8_device/08_vibrate.js';
import * as screen from 'ext:host_v8_device/09_screen.js';
import * as network from 'ext:host_v8_device/10_network.js';
import * as memory from 'ext:host_v8_device/11_memory.js';
import * as locationApi from 'ext:host_v8_device/12_location.js';
import * as scanCodeApi from 'ext:host_v8_device/13_scan_code.js';

import { primordials, core } from "ext:core/mod.js";
const { ObjectDefineProperties } = primordials;

ObjectDefineProperties(globalThis, {
    // Device Motion
    onDeviceMotionChange: core.propNonEnumerable(deviceMotion.onDeviceMotionChange),
    offDeviceMotionChange: core.propNonEnumerable(deviceMotion.offDeviceMotionChange),
    _internalTriggerDeviceMotionChange: core.propNonEnumerable(deviceMotion._internalTriggerDeviceMotionChange),
    startDeviceMotionListening: core.propNonEnumerable(deviceMotion.startDeviceMotionListening),
    stopDeviceMotionListening: core.propNonEnumerable(deviceMotion.stopDeviceMotionListening),
    _internalOnStartDeviceMotionListeningResult: core.propNonEnumerable(deviceMotion._internalOnStartDeviceMotionListeningResult),

    // Gyroscope
    onGyroscopeChange: core.propNonEnumerable(gyroscope.onGyroscopeChange),
    offGyroscopeChange: core.propNonEnumerable(gyroscope.offGyroscopeChange),
    _internalTriggerGyroscopeChange: core.propNonEnumerable(gyroscope._internalTriggerGyroscopeChange),
    startGyroscope: core.propNonEnumerable(gyroscope.startGyroscope),
    stopGyroscope: core.propNonEnumerable(gyroscope.stopGyroscope),
    _internalOnStartGyroscopeResult: core.propNonEnumerable(gyroscope._internalOnStartGyroscopeResult),

    // Device Orientation
    onDeviceOrientationChange: core.propNonEnumerable(orientation.onDeviceOrientationChange),
    offDeviceOrientationChange: core.propNonEnumerable(orientation.offDeviceOrientationChange),
    _internalTriggerDeviceOrientationChange: core.propNonEnumerable(orientation._internalTriggerDeviceOrientationChange),
    _internalOnDeviceOrientationEvent: core.propNonEnumerable(orientation._internalOnDeviceOrientationEvent),

    // Compass
    onCompassChange: core.propNonEnumerable(compass.onCompassChange),
    offCompassChange: core.propNonEnumerable(compass.offCompassChange),
    _internalTriggerCompassChange: core.propNonEnumerable(compass._internalTriggerCompassChange),
    startCompass: core.propNonEnumerable(compass.startCompass),
    stopCompass: core.propNonEnumerable(compass.stopCompass),
    _internalOnStartCompassResult: core.propNonEnumerable(compass._internalOnStartCompassResult),

    // Accelerometer
    onAccelerometerChange: core.propNonEnumerable(accelerometer.onAccelerometerChange),
    offAccelerometerChange: core.propNonEnumerable(accelerometer.offAccelerometerChange),
    _internalTriggerAccelerometerChange: core.propNonEnumerable(accelerometer._internalTriggerAccelerometerChange),
    startAccelerometer: core.propNonEnumerable(accelerometer.startAccelerometer),
    stopAccelerometer: core.propNonEnumerable(accelerometer.stopAccelerometer),
    _internalOnStartAccelerometerResult: core.propNonEnumerable(accelerometer._internalOnStartAccelerometerResult),

    // Battery
    getBatteryInfo: core.propNonEnumerable(battery.getBatteryInfo),
    getBatteryInfoSync: core.propNonEnumerable(battery.getBatteryInfoSync),

    // Clipboard
    setClipboardData: core.propNonEnumerable(clipboard.setClipboardData),
    getClipboardData: core.propNonEnumerable(clipboard.getClipboardData),
    _internalOnSetClipboardDataResult: core.propNonEnumerable(clipboard._internalOnSetClipboardDataResult),
    _internalOnGetClipboardDataResult: core.propNonEnumerable(clipboard._internalOnGetClipboardDataResult),

    // Vibration
    vibrateShort: core.propNonEnumerable(vibrate.vibrateShort),
    vibrateLong: core.propNonEnumerable(vibrate.vibrateLong),

    // Screen
    getScreenBrightness: core.propNonEnumerable(screen.getScreenBrightness),
    _internalOnGetScreenBrightnessResult: core.propNonEnumerable(screen._internalOnGetScreenBrightnessResult),
    setScreenBrightness: core.propNonEnumerable(screen.setScreenBrightness),
    _internalOnSetScreenBrightnessResult: core.propNonEnumerable(screen._internalOnSetScreenBrightnessResult),
    setKeepScreenOn: core.propNonEnumerable(screen.setKeepScreenOn),
    setDeviceOrientation: core.propNonEnumerable(screen.setDeviceOrientation),
    _internalOnSetDeviceOrientationResult: core.propNonEnumerable(screen._internalOnSetDeviceOrientationResult),
    getScreenRecordingState: core.propNonEnumerable(screen.getScreenRecordingState),
    _internalOnGetScreenRecordingStateResult: core.propNonEnumerable(screen._internalOnGetScreenRecordingStateResult),
    onScreenRecordingStateChanged: core.propNonEnumerable(screen.onScreenRecordingStateChanged),
    offScreenRecordingStateChanged: core.propNonEnumerable(screen.offScreenRecordingStateChanged),
    _internalOnScreenRecordingStateEvent: core.propNonEnumerable(screen._internalOnScreenRecordingStateEvent),
    setVisualEffectOnCapture: core.propNonEnumerable(screen.setVisualEffectOnCapture),
    _internalOnSetVisualEffectOnCaptureResult: core.propNonEnumerable(screen._internalOnSetVisualEffectOnCaptureResult),
    setEnableDebug: core.propNonEnumerable(screen.setEnableDebug),
    onUserCaptureScreen: core.propNonEnumerable(screen.onUserCaptureScreen),
    offUserCaptureScreen: core.propNonEnumerable(screen.offUserCaptureScreen),
    _internalTriggerUserCaptureScreen: core.propNonEnumerable(screen._internalTriggerUserCaptureScreen),

    // Network
    onNetworkStatusChange: core.propNonEnumerable(network.onNetworkStatusChange),
    offNetworkStatusChange: core.propNonEnumerable(network.offNetworkStatusChange),
    onNetworkWeakChange: core.propNonEnumerable(network.onNetworkWeakChange),
    offNetworkWeakChange: core.propNonEnumerable(network.offNetworkWeakChange),
    _internalTriggerNetworkStatusChange: core.propNonEnumerable(network._internalTriggerNetworkStatusChange),
    getNetworkType: core.propNonEnumerable(network.getNetworkType),
    getLocalIPAddress: core.propNonEnumerable(network.getLocalIPAddress),

    // Memory
    onMemoryWarning: core.propNonEnumerable(memory.onMemoryWarning),
    offMemoryWarning: core.propNonEnumerable(memory.offMemoryWarning),
    _internalTriggerMemoryWarning: core.propNonEnumerable(memory._internalTriggerMemoryWarning),

    // Location
    getLocation: core.propNonEnumerable(locationApi.getLocation),
    getFuzzyLocation: core.propNonEnumerable(locationApi.getFuzzyLocation),
    _internalOnLocationResult: core.propNonEnumerable(locationApi._internalOnLocationResult),
    _internalOnFuzzyLocationResult: core.propNonEnumerable(locationApi._internalOnFuzzyLocationResult),

    // Scan Code
    scanCode: core.propNonEnumerable(scanCodeApi.scanCode),
    _internalOnScanCodeResult: core.propNonEnumerable(scanCodeApi._internalOnScanCodeResult),
});
