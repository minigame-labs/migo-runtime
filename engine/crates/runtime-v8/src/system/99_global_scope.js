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
    requestSubscribeSystemMessage: core.propNonEnumerable(settingApi.requestSubscribeSystemMessage),
    authPrivateMessage: core.propNonEnumerable(settingApi.authPrivateMessage),
    checkUserLocation: core.propNonEnumerable(settingApi.checkUserLocation),
    getWritePhotosAlbum: core.propNonEnumerable(settingApi.getWritePhotosAlbum),
    checkWritePhotosAlbum: core.propNonEnumerable(settingApi.checkWritePhotosAlbum),

    // Navigate / Customer Service
    navigateToMiniProgram: core.propNonEnumerable(navigateApi.navigateToMiniProgram),
    navigateBackMiniProgram: core.propNonEnumerable(navigateApi.navigateBackMiniProgram),
    _internalOnNavigateToMiniProgramResult: core.propNonEnumerable(navigateApi._internalOnNavigateToMiniProgramResult),
    openCustomerServiceConversation: core.propNonEnumerable(navigateApi.openCustomerServiceConversation),
    openBusinessView: core.propNonEnumerable(navigateApi.openBusinessView),

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
    checkIsAddedToMyMiniProgram: core.propNonEnumerable(appInfo.checkIsAddedToMyMiniProgram),
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
    getFriendCloudStorage: core.propNonEnumerable(openDataContext.getFriendCloudStorage),
    setUserCloudStorage: core.propNonEnumerable(openDataContext.setUserCloudStorage),
    removeUserCloudStorage: core.propNonEnumerable(openDataContext.removeUserCloudStorage),
    modifyFriendInteractiveStorage: core.propNonEnumerable(openDataContext.modifyFriendInteractiveStorage),
    getPotentialFriendList: core.propNonEnumerable(openDataContext.getPotentialFriendList),
    getGameClubData: core.propNonEnumerable(openDataContext.getGameClubData),

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

    // JSSDK Lifecycle

    // Privacy
    getPrivacySetting: core.propNonEnumerable(settingApi.getPrivacySetting),
    openPrivacyContract: core.propNonEnumerable(settingApi.openPrivacyContract),
    requirePrivacyAuthorize: core.propNonEnumerable(settingApi.requirePrivacyAuthorize),
    onNeedPrivacyAuthorization: core.propNonEnumerable(settingApi.onNeedPrivacyAuthorization),
    offNeedPrivacyAuthorization: core.propNonEnumerable(settingApi.offNeedPrivacyAuthorization),
    _internalTriggerNeedPrivacyAuthorization: core.propNonEnumerable(settingApi._internalTriggerNeedPrivacyAuthorization),
    requestSubscribeMessage: core.propNonEnumerable(settingApi.requestSubscribeMessage),

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
});
