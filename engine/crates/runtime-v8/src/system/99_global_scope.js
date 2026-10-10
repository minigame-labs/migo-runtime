// Global scope registration for host_v8_system APIs (api-connectivity feature gate).

import * as bluetooth from 'ext:host_v8_system/01_bluetooth.js';
import * as authorize from 'ext:host_v8_system/02_authorize.js';
import * as windowInfo from 'ext:host_v8_system/03_window_info.js';
import * as systemSetting from 'ext:host_v8_system/04_system_settings.js';
import * as deviceInfo from 'ext:host_v8_system/05_device_info.js';
import * as benchmarkLevel from 'ext:host_v8_system/06_benchmark_level.js';
import * as appInfo from 'ext:host_v8_system/07_app_info.js';
import * as authorizeSetting from 'ext:host_v8_system/08_authorize_setting.js';
import * as gameLog from 'ext:host_v8_system/09_game_log.js';
import * as systemInfo from 'ext:host_v8_system/10_system_info.js';
import * as openDataContext from 'ext:host_v8_system/11_open_data_context.js';
import * as windowResize from 'ext:host_v8_system/12_window_resize.js';
import * as loginApi from 'ext:host_v8_system/13_login.js';
import * as settingApi from 'ext:host_v8_system/14_setting.js';
import * as navigateApi from 'ext:host_v8_system/15_navigate.js';
import * as analyticsApi from 'ext:host_v8_system/17_analytics.js';
import * as cryptoApi from 'ext:host_v8_system/18_crypto.js';
import * as logManagerApi from 'ext:host_v8_system/19_log_manager.js';
import * as ecosystem from 'ext:host_v8_system/20_ecosystem.js';
import * as ecosystemObjects from 'ext:host_v8_system/21_ecosystem_objects.js';

import { primordials, core } from "ext:core/mod.js";
const { ObjectDefineProperties } = primordials;

ObjectDefineProperties(globalThis, {
    // Bluetooth
    openSystemBluetoothSetting: core.propNonEnumerable(bluetooth.openSystemBluetoothSetting),
    _internalOnOpenBluetoothSettingResult: core.propNonEnumerable(bluetooth._internalOnOpenBluetoothSettingResult),
    openBluetoothAdapter: core.propNonEnumerable(bluetooth.openBluetoothAdapter),
    closeBluetoothAdapter: core.propNonEnumerable(bluetooth.closeBluetoothAdapter),
    getBluetoothAdapterState: core.propNonEnumerable(bluetooth.getBluetoothAdapterState),
    startBluetoothDevicesDiscovery: core.propNonEnumerable(bluetooth.startBluetoothDevicesDiscovery),
    stopBluetoothDevicesDiscovery: core.propNonEnumerable(bluetooth.stopBluetoothDevicesDiscovery),
    getBluetoothDevices: core.propNonEnumerable(bluetooth.getBluetoothDevices),
    getConnectedBluetoothDevices: core.propNonEnumerable(bluetooth.getConnectedBluetoothDevices),
    makeBluetoothPair: core.propNonEnumerable(bluetooth.makeBluetoothPair),
    isBluetoothDevicePaired: core.propNonEnumerable(bluetooth.isBluetoothDevicePaired),
    onBluetoothAdapterStateChange: core.propNonEnumerable(bluetooth.onBluetoothAdapterStateChange),
    offBluetoothAdapterStateChange: core.propNonEnumerable(bluetooth.offBluetoothAdapterStateChange),
    onBluetoothDeviceFound: core.propNonEnumerable(bluetooth.onBluetoothDeviceFound),
    offBluetoothDeviceFound: core.propNonEnumerable(bluetooth.offBluetoothDeviceFound),
    _internalTriggerBluetoothAdapterStateChange: core.propNonEnumerable(bluetooth._internalTriggerBluetoothAdapterStateChange),
    _internalTriggerBluetoothDeviceFound: core.propNonEnumerable(bluetooth._internalTriggerBluetoothDeviceFound),
    // Beacon
    startBeaconDiscovery: core.propNonEnumerable(bluetooth.startBeaconDiscovery),
    stopBeaconDiscovery: core.propNonEnumerable(bluetooth.stopBeaconDiscovery),
    getBeacons: core.propNonEnumerable(bluetooth.getBeacons),
    onBeaconUpdate: core.propNonEnumerable(bluetooth.onBeaconUpdate),
    offBeaconUpdate: core.propNonEnumerable(bluetooth.offBeaconUpdate),
    onBeaconServiceChange: core.propNonEnumerable(bluetooth.onBeaconServiceChange),
    offBeaconServiceChange: core.propNonEnumerable(bluetooth.offBeaconServiceChange),
    _internalTriggerBeaconUpdate: core.propNonEnumerable(bluetooth._internalTriggerBeaconUpdate),
    _internalTriggerBeaconServiceChange: core.propNonEnumerable(bluetooth._internalTriggerBeaconServiceChange),
    // BLE GATT
    createBLEConnection: core.propNonEnumerable(bluetooth.createBLEConnection),
    closeBLEConnection: core.propNonEnumerable(bluetooth.closeBLEConnection),
    getBLEDeviceServices: core.propNonEnumerable(bluetooth.getBLEDeviceServices),
    getBLEDeviceCharacteristics: core.propNonEnumerable(bluetooth.getBLEDeviceCharacteristics),
    readBLECharacteristicValue: core.propNonEnumerable(bluetooth.readBLECharacteristicValue),
    writeBLECharacteristicValue: core.propNonEnumerable(bluetooth.writeBLECharacteristicValue),
    notifyBLECharacteristicValueChange: core.propNonEnumerable(bluetooth.notifyBLECharacteristicValueChange),
    getBLEDeviceRSSI: core.propNonEnumerable(bluetooth.getBLEDeviceRSSI),
    setBLEMTU: core.propNonEnumerable(bluetooth.setBLEMTU),
    getBLEMTU: core.propNonEnumerable(bluetooth.getBLEMTU),
    onBLEConnectionStateChange: core.propNonEnumerable(bluetooth.onBLEConnectionStateChange),
    offBLEConnectionStateChange: core.propNonEnumerable(bluetooth.offBLEConnectionStateChange),
    onBLECharacteristicValueChange: core.propNonEnumerable(bluetooth.onBLECharacteristicValueChange),
    offBLECharacteristicValueChange: core.propNonEnumerable(bluetooth.offBLECharacteristicValueChange),
    onBLEMTUChange: core.propNonEnumerable(bluetooth.onBLEMTUChange),
    offBLEMTUChange: core.propNonEnumerable(bluetooth.offBLEMTUChange),
    _internalTriggerBLEConnectionStateChange: core.propNonEnumerable(bluetooth._internalTriggerBLEConnectionStateChange),
    _internalTriggerBLECharacteristicValueChange: core.propNonEnumerable(bluetooth._internalTriggerBLECharacteristicValueChange),
    _internalTriggerBLEMTUChange: core.propNonEnumerable(bluetooth._internalTriggerBLEMTUChange),
    // Bluetooth results and host-service events
    _internalOnOpenBluetoothAdapterResult: core.propNonEnumerable(bluetooth._internalOnOpenBluetoothAdapterResult),
    _internalOnCloseBluetoothAdapterResult: core.propNonEnumerable(bluetooth._internalOnCloseBluetoothAdapterResult),
    _internalOnGetBluetoothAdapterStateResult: core.propNonEnumerable(bluetooth._internalOnGetBluetoothAdapterStateResult),
    _internalOnStartBluetoothDevicesDiscoveryResult: core.propNonEnumerable(bluetooth._internalOnStartBluetoothDevicesDiscoveryResult),
    _internalOnStopBluetoothDevicesDiscoveryResult: core.propNonEnumerable(bluetooth._internalOnStopBluetoothDevicesDiscoveryResult),
    _internalOnGetBluetoothDevicesResult: core.propNonEnumerable(bluetooth._internalOnGetBluetoothDevicesResult),
    _internalOnGetConnectedBluetoothDevicesResult: core.propNonEnumerable(bluetooth._internalOnGetConnectedBluetoothDevicesResult),
    _internalOnMakeBluetoothPairResult: core.propNonEnumerable(bluetooth._internalOnMakeBluetoothPairResult),
    _internalOnIsBluetoothDevicePairedResult: core.propNonEnumerable(bluetooth._internalOnIsBluetoothDevicePairedResult),
    _internalOnStartBeaconDiscoveryResult: core.propNonEnumerable(bluetooth._internalOnStartBeaconDiscoveryResult),
    _internalOnStopBeaconDiscoveryResult: core.propNonEnumerable(bluetooth._internalOnStopBeaconDiscoveryResult),
    _internalOnGetBeaconsResult: core.propNonEnumerable(bluetooth._internalOnGetBeaconsResult),
    _internalOnCreateBLEConnectionResult: core.propNonEnumerable(bluetooth._internalOnCreateBLEConnectionResult),
    _internalOnCloseBLEConnectionResult: core.propNonEnumerable(bluetooth._internalOnCloseBLEConnectionResult),
    _internalOnGetBLEDeviceServicesResult: core.propNonEnumerable(bluetooth._internalOnGetBLEDeviceServicesResult),
    _internalOnGetBLEDeviceCharacteristicsResult: core.propNonEnumerable(bluetooth._internalOnGetBLEDeviceCharacteristicsResult),
    _internalOnReadBLECharacteristicValueResult: core.propNonEnumerable(bluetooth._internalOnReadBLECharacteristicValueResult),
    _internalOnWriteBLECharacteristicValueResult: core.propNonEnumerable(bluetooth._internalOnWriteBLECharacteristicValueResult),
    _internalOnNotifyBLECharacteristicValueChangeResult: core.propNonEnumerable(bluetooth._internalOnNotifyBLECharacteristicValueChangeResult),
    _internalOnGetBLEDeviceRSSIResult: core.propNonEnumerable(bluetooth._internalOnGetBLEDeviceRSSIResult),
    _internalOnSetBLEMTUResult: core.propNonEnumerable(bluetooth._internalOnSetBLEMTUResult),
    _internalOnGetBLEMTUResult: core.propNonEnumerable(bluetooth._internalOnGetBLEMTUResult),
    _internalOnBluetoothAdapterStateEvent: core.propNonEnumerable(bluetooth._internalOnBluetoothAdapterStateEvent),
    _internalOnBluetoothDeviceFoundEvent: core.propNonEnumerable(bluetooth._internalOnBluetoothDeviceFoundEvent),
    _internalOnBLEConnectionStateEvent: core.propNonEnumerable(bluetooth._internalOnBLEConnectionStateEvent),
    _internalOnBLEMTUEvent: core.propNonEnumerable(bluetooth._internalOnBLEMTUEvent),
    _internalOnBeaconUpdateEvent: core.propNonEnumerable(bluetooth._internalOnBeaconUpdateEvent),
    _internalOnBeaconServiceEvent: core.propNonEnumerable(bluetooth._internalOnBeaconServiceEvent),

    // Authorize
    openAppAuthorizeSetting: core.propNonEnumerable(authorize.openAppAuthorizeSetting),
    _internalOnOpenAppAuthorizeSettingFinished: core.propNonEnumerable(authorize._internalOnOpenAppAuthorizeSettingFinished),

    // Setting / Authorize
    getSetting: core.propNonEnumerable(settingApi.getSetting),
    authorize: core.propNonEnumerable(settingApi.authorize),
    openSetting: core.propNonEnumerable(settingApi.openSetting),
    _internalOnOpenSettingResult: core.propNonEnumerable(settingApi._internalOnOpenSettingResult),
    _internalOnAuthorizeResult: core.propNonEnumerable(settingApi._internalOnAuthorizeResult),
    _internalUpdateAuthSetting: core.propNonEnumerable(settingApi._internalUpdateAuthSetting),
    checkUserLocation: core.propNonEnumerable(settingApi.checkUserLocation),
    getWritePhotosAlbum: core.propNonEnumerable(settingApi.getWritePhotosAlbum),
    checkWritePhotosAlbum: core.propNonEnumerable(settingApi.checkWritePhotosAlbum),

    // Navigate / Customer Service
    navigateToMiniProgram: core.propNonEnumerable(navigateApi.navigateToMiniProgram),
    navigateBackMiniProgram: core.propNonEnumerable(navigateApi.navigateBackMiniProgram),
    _internalOnNavigateToMiniProgramResult: core.propNonEnumerable(navigateApi._internalOnNavigateToMiniProgramResult),
    openCustomerServiceConversation: core.propNonEnumerable(navigateApi.openCustomerServiceConversation),

    // Login
    login: core.propNonEnumerable(loginApi.login),
    checkSession: core.propNonEnumerable(loginApi.checkSession),
    getUserInfo: core.propNonEnumerable(loginApi.getUserInfo),
    getUserProfile: core.propNonEnumerable(loginApi.getUserProfile),
    getPhoneNumber: core.propNonEnumerable(loginApi.getPhoneNumber),
    _internalOnLoginResult: core.propNonEnumerable(loginApi._internalOnLoginResult),
    _internalOnCheckSessionResult: core.propNonEnumerable(loginApi._internalOnCheckSessionResult),
    _internalOnGetUserInfoResult: core.propNonEnumerable(loginApi._internalOnGetUserInfoResult),
    _internalOnGetPhoneNumberResult: core.propNonEnumerable(loginApi._internalOnGetPhoneNumberResult),

    // Window Info
    getWindowInfo: core.propNonEnumerable(windowInfo.getWindowInfo),

    // System Setting
    getSystemSetting: core.propNonEnumerable(systemSetting.getSystemSetting),

    // Device Info
    getDeviceInfo: core.propNonEnumerable(deviceInfo.getDeviceInfo),

    // Benchmark Level
    getDeviceBenchmarkInfo: core.propNonEnumerable(benchmarkLevel.getDeviceBenchmarkInfo),

    // App Info
    getAppBaseInfo: core.propNonEnumerable(appInfo.getAppBaseInfo),
    getAccountInfoSync: core.propNonEnumerable(appInfo.getAccountInfoSync),
    _internalSetAppId: core.propNonEnumerable(appInfo._internalSetAppId),

    // Authorize Setting
    getAppAuthorizeSetting: core.propNonEnumerable(authorizeSetting.getAppAuthorizeSetting),

    // System Info (legacy)
    getSystemInfo: core.propNonEnumerable(systemInfo.getSystemInfo),
    getSystemInfoSync: core.propNonEnumerable(systemInfo.getSystemInfoSync),
    getSystemInfoAsync: core.propNonEnumerable(systemInfo.getSystemInfoAsync),

    // Open Data Context
    onMessage: core.propNonEnumerable(openDataContext.onMessage),
    offMessage: core.propNonEnumerable(openDataContext.offMessage),
    postMessage: core.propNonEnumerable(openDataContext.postMessage),
    getOpenDataContext: core.propNonEnumerable(openDataContext.getOpenDataContext),
    getSharedCanvas: core.propNonEnumerable(openDataContext.getSharedCanvas),

    // Window Resize
    //
    // The host hook is not here: `_internalTriggerWindowResize` is registered by
    // 98_global_scope_window.js, because the canvas must follow the surface in
    // every product profile and this extension is one `api-connectivity` away
    // from not existing. What content can subscribe to does belong here.
    onWindowResize: core.propNonEnumerable(windowResize.onWindowResize),
    offWindowResize: core.propNonEnumerable(windowResize.offWindowResize),

    // Game Log
    getGameLogManager: core.propNonEnumerable(gameLog.getGameLogManager),

    // Analytics
    reportEvent: core.propNonEnumerable(analyticsApi.reportEvent),
    reportMonitor: core.propNonEnumerable(analyticsApi.reportMonitor),
    reportScene: core.propNonEnumerable(analyticsApi.reportScene),
    reportPerformance: core.propNonEnumerable(analyticsApi.reportPerformance),

    // UserCryptoManager
    getUserCryptoManager: core.propNonEnumerable(cryptoApi.getUserCryptoManager),

    // LogManager
    getLogManager: core.propNonEnumerable(logManagerApi.getLogManager),
    getRealtimeLogManager: core.propNonEnumerable(logManagerApi.getRealtimeLogManager),

    // Ecosystem: requests, events and getters the host answers (20_ecosystem.js)
    _internalOnEcosystemResult: core.propNonEnumerable(ecosystem._internalOnEcosystemResult),
    _internalOnEcosystemEvent: core.propNonEnumerable(ecosystem._internalOnEcosystemEvent),

    // Ecosystem objects (21_ecosystem_objects.js)
    getGameServerManager: core.propNonEnumerable(ecosystemObjects.getGameServerManager),
    getRankManager: core.propNonEnumerable(ecosystemObjects.getRankManager),
    getMiniReportManager: core.propNonEnumerable(ecosystemObjects.getMiniReportManager),
    getScenePerformanceManager: core.propNonEnumerable(ecosystemObjects.getScenePerformanceManager),
    createStoreGift: core.propNonEnumerable(ecosystemObjects.createStoreGift),
});

// One property per name on the module's closed lists.
const ecosystemProperties = {};
for (const name of Object.keys(ecosystem.ecosystemApis)) {
    ecosystemProperties[name] = core.propNonEnumerable(ecosystem.ecosystemApis[name]);
}
ObjectDefineProperties(globalThis, ecosystemProperties);
