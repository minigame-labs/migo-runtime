use std::ffi::c_void;

use jni::{JNIEnv, NativeMethod};
use tracing::info;

use crate::{
    android::jni::{
        JAVA_METHOD_CACHE, JavaMethodCache, executeScript, getConsoleLogs, getDebugStats,
        getMinApiLevel, init, initIcuData, mod_main, nativeAhbPointerFromHardwareBuffer,
        onAccelerometerChange, onActionSheetResult, onAdEvent, onAudioInterruptionBegin,
        onAudioInterruptionEnd, onAuthorizeResult, onBLECharacteristicValueChange,
        onBLEConnectionStateChange, onBLEMTUChange, onBeaconServiceChange, onBeaconUpdate,
        onBluetoothAdapterStateChange, onBluetoothDeviceFound, onBluetoothResult, onCameraEvent,
        onCameraFrameData, onCheckSessionResult, onChooseImageResult, onChooseMediaResult,
        onChooseMessageFileResult, onCompassChange, onCompressImageResult, onDeviceMotionChange,
        onDeviceOrientationChange, onEcosystemEvent, onEcosystemResult, onFuzzyLocationResult,
        onGetPhoneNumberResult, onGetUserInfoResult, onGyroscopeChange, onHide, onKeyboardComplete,
        onKeyboardConfirm, onKeyboardHeightChange, onKeyboardInput, onLocationResult,
        onLoginResult, onMemoryWarning, onMidasPaymentGameItemResult, onMidasPaymentResult,
        onModalResult, onNavigateToMiniProgramResult, onNetworkStatusChange,
        onOpenAppAuthorizeSetting, onOpenSettingResult, onOpenSystemBluetoothSetting,
        onPointerEvent, onPointerLockChanged, onPreviewImageResult, onPreviewMediaResult,
        onRecorderEvent, onRecorderFrameData, onRestart, onSaveImageToPhotosAlbumResult,
        onScanCodeResult, onScreenRecordingStateChanged, onShareAppMessageResult, onShareMenuEvent,
        onShareMessageToFriendResult, onShow, onShowShareImageMenuResult, onSubpackageProgress,
        onSubpackageResult, onSurfaceDestroyed, onThermalStatusChanged, onTouch,
        onUserCaptureScreen, onVideoEvent, onVsync, onWheelEvent, onWindowStateChanged, shutdown,
        updatePermission, updateSurface, version,
    },
    jni_profile_contract::{self, JniMethod, MethodDirection},
};

fn native_fn_ptr(name: &str) -> Option<*mut c_void> {
    let pointer = match name {
        "version" => version as *mut c_void,
        "getMinApiLevel" => getMinApiLevel as *mut c_void,
        "initIcuData" => initIcuData as *mut c_void,
        "init" => init as *mut c_void,
        "shutdown" => shutdown as *mut c_void,
        "onShow" => onShow as *mut c_void,
        "onHide" => onHide as *mut c_void,
        "onRestart" => onRestart as *mut c_void,
        "updateSurface" => updateSurface as *mut c_void,
        "onSurfaceDestroyed" => onSurfaceDestroyed as *mut c_void,
        "onTouchEvent" => onTouch as *mut c_void,
        "modMain" => mod_main as *mut c_void,
        "executeScript" => executeScript as *mut c_void,
        "onVsync" => onVsync as *mut c_void,
        "getDebugStats" => getDebugStats as *mut c_void,
        "getConsoleLogs" => getConsoleLogs as *mut c_void,
        "nativeAhbPointerFromHardwareBuffer" => nativeAhbPointerFromHardwareBuffer as *mut c_void,
        "onKeyboardInput" => onKeyboardInput as *mut c_void,
        "onKeyboardConfirm" => onKeyboardConfirm as *mut c_void,
        "onKeyboardComplete" => onKeyboardComplete as *mut c_void,
        "onKeyboardHeightChange" => onKeyboardHeightChange as *mut c_void,
        "onMemoryWarning" => onMemoryWarning as *mut c_void,
        "onThermalStatusChanged" => onThermalStatusChanged as *mut c_void,
        "onSubpackageProgress" => onSubpackageProgress as *mut c_void,
        "onSubpackageResult" => onSubpackageResult as *mut c_void,
        #[cfg(feature = "api-sensors")]
        "onDeviceMotionChange" => onDeviceMotionChange as *mut c_void,
        #[cfg(feature = "api-sensors")]
        "onGyroscopeChange" => onGyroscopeChange as *mut c_void,
        #[cfg(feature = "api-sensors")]
        "onDeviceOrientationChange" => onDeviceOrientationChange as *mut c_void,
        #[cfg(feature = "api-sensors")]
        "onCompassChange" => onCompassChange as *mut c_void,
        #[cfg(feature = "api-sensors")]
        "onAccelerometerChange" => onAccelerometerChange as *mut c_void,
        #[cfg(feature = "api-sensors")]
        "onNetworkStatusChange" => onNetworkStatusChange as *mut c_void,
        #[cfg(feature = "api-sensors")]
        "onUserCaptureScreen" => onUserCaptureScreen as *mut c_void,
        #[cfg(feature = "api-sensors")]
        "onScreenRecordingStateChanged" => onScreenRecordingStateChanged as *mut c_void,
        #[cfg(feature = "api-sensors")]
        "onLocationResult" => onLocationResult as *mut c_void,
        #[cfg(feature = "api-sensors")]
        "onFuzzyLocationResult" => onFuzzyLocationResult as *mut c_void,
        #[cfg(feature = "api-sensors")]
        "onScanCodeResult" => onScanCodeResult as *mut c_void,
        #[cfg(feature = "api-media")]
        "onAudioInterruptionBegin" => onAudioInterruptionBegin as *mut c_void,
        #[cfg(feature = "api-media")]
        "onAudioInterruptionEnd" => onAudioInterruptionEnd as *mut c_void,
        #[cfg(feature = "api-media")]
        "onRecorderEvent" => onRecorderEvent as *mut c_void,
        #[cfg(feature = "api-media")]
        "onRecorderFrameData" => onRecorderFrameData as *mut c_void,
        #[cfg(feature = "api-media")]
        "onCameraEvent" => onCameraEvent as *mut c_void,
        #[cfg(feature = "api-media")]
        "onCameraFrameData" => onCameraFrameData as *mut c_void,
        #[cfg(feature = "api-media")]
        "onSaveImageToPhotosAlbumResult" => onSaveImageToPhotosAlbumResult as *mut c_void,
        #[cfg(feature = "api-media")]
        "onPreviewImageResult" => onPreviewImageResult as *mut c_void,
        #[cfg(feature = "api-media")]
        "onPreviewMediaResult" => onPreviewMediaResult as *mut c_void,
        #[cfg(feature = "api-media")]
        "onCompressImageResult" => onCompressImageResult as *mut c_void,
        #[cfg(feature = "api-media")]
        "onChooseImageResult" => onChooseImageResult as *mut c_void,
        #[cfg(feature = "api-media")]
        "onChooseMessageFileResult" => onChooseMessageFileResult as *mut c_void,
        #[cfg(feature = "api-media")]
        "onChooseMediaResult" => onChooseMediaResult as *mut c_void,
        #[cfg(feature = "api-media")]
        "onVideoEvent" => onVideoEvent as *mut c_void,
        #[cfg(feature = "api-connectivity")]
        "onOpenSystemBluetoothSetting" => onOpenSystemBluetoothSetting as *mut c_void,
        #[cfg(feature = "api-connectivity")]
        "onOpenAppAuthorizeSetting" => onOpenAppAuthorizeSetting as *mut c_void,
        #[cfg(feature = "api-connectivity")]
        "onBluetoothResult" => onBluetoothResult as *mut c_void,
        #[cfg(feature = "api-connectivity")]
        "onBluetoothAdapterStateChange" => onBluetoothAdapterStateChange as *mut c_void,
        #[cfg(feature = "api-connectivity")]
        "onBluetoothDeviceFound" => onBluetoothDeviceFound as *mut c_void,
        #[cfg(feature = "api-connectivity")]
        "onBeaconUpdate" => onBeaconUpdate as *mut c_void,
        #[cfg(feature = "api-connectivity")]
        "onBeaconServiceChange" => onBeaconServiceChange as *mut c_void,
        #[cfg(feature = "api-connectivity")]
        "onBLEConnectionStateChange" => onBLEConnectionStateChange as *mut c_void,
        #[cfg(feature = "api-connectivity")]
        "onBLECharacteristicValueChange" => onBLECharacteristicValueChange as *mut c_void,
        #[cfg(feature = "api-connectivity")]
        "onBLEMTUChange" => onBLEMTUChange as *mut c_void,
        #[cfg(feature = "api-connectivity")]
        "onLoginResult" => onLoginResult as *mut c_void,
        #[cfg(feature = "api-connectivity")]
        "onCheckSessionResult" => onCheckSessionResult as *mut c_void,
        #[cfg(feature = "api-connectivity")]
        "onGetUserInfoResult" => onGetUserInfoResult as *mut c_void,
        #[cfg(feature = "api-connectivity")]
        "onGetPhoneNumberResult" => onGetPhoneNumberResult as *mut c_void,
        #[cfg(feature = "api-connectivity")]
        "onOpenSettingResult" => onOpenSettingResult as *mut c_void,
        #[cfg(feature = "api-connectivity")]
        "onNavigateToMiniProgramResult" => onNavigateToMiniProgramResult as *mut c_void,
        #[cfg(feature = "api-system")]
        "onAuthorizeResult" => onAuthorizeResult as *mut c_void,
        #[cfg(feature = "api-system")]
        "updatePermission" => updatePermission as *mut c_void,
        #[cfg(feature = "api-commerce")]
        "onAdEvent" => onAdEvent as *mut c_void,
        #[cfg(feature = "api-commerce")]
        "onShareAppMessageResult" => onShareAppMessageResult as *mut c_void,
        "onShareMessageToFriendResult" => onShareMessageToFriendResult as *mut c_void,
        "onShowShareImageMenuResult" => onShowShareImageMenuResult as *mut c_void,
        "onShareMenuEvent" => onShareMenuEvent as *mut c_void,
        "onPointerEvent" => onPointerEvent as *mut c_void,
        "onWheelEvent" => onWheelEvent as *mut c_void,
        "onPointerLockChanged" => onPointerLockChanged as *mut c_void,
        "onWindowStateChanged" => onWindowStateChanged as *mut c_void,
        #[cfg(feature = "api-commerce")]
        "onMidasPaymentResult" => onMidasPaymentResult as *mut c_void,
        #[cfg(feature = "api-commerce")]
        "onMidasPaymentGameItemResult" => onMidasPaymentGameItemResult as *mut c_void,
        #[cfg(feature = "api-system")]
        "onModalResult" => onModalResult as *mut c_void,
        #[cfg(feature = "api-system")]
        "onActionSheetResult" => onActionSheetResult as *mut c_void,
        "onEcosystemResult" => onEcosystemResult as *mut c_void,
        "onEcosystemEvent" => onEcosystemEvent as *mut c_void,
        _ => return None,
    };
    Some(pointer)
}

fn registered_native_method(spec: JniMethod) -> Result<NativeMethod, String> {
    let fn_ptr = native_fn_ptr(spec.name).ok_or_else(|| {
        format!(
            "active NativeBridge method has no compiled Rust callback: {} {}",
            spec.name, spec.sig
        )
    })?;
    Ok(NativeMethod {
        name: spec.name.into(),
        sig: spec.sig.into(),
        fn_ptr,
    })
}

pub(crate) fn register_native_exports(env: &mut JNIEnv) -> Result<(), String> {
    let class = env
        .find_class("com/migo/runtime/internal/NativeBridge")
        .map_err(|error| format!("Failed to find NativeBridge: {error:?}"))?;
    let methods = jni_profile_contract::active_methods(MethodDirection::JavaToNative)
        .into_iter()
        .map(registered_native_method)
        .collect::<Result<Vec<_>, _>>()?;

    env.register_native_methods(&class, &methods)
        .map_err(|error| format!("Failed to register native methods: {error:?}"))?;

    info!(
        method_count = methods.len(),
        "Registered NativeBridge methods"
    );
    Ok(())
}

pub(crate) fn register_java_exports(env: &mut JNIEnv) -> Result<(), String> {
    let local_class = env
        .find_class("com/migo/runtime/internal/NativeExports")
        .map_err(|error| format!("Failed to find class NativeExports: {error}"))?;
    let methods = jni_profile_contract::active_methods(MethodDirection::NativeToJava);
    let global_class = env
        .new_global_ref(local_class)
        .map_err(|error| format!("Failed to create global ref for NativeExports: {error}"))?;
    let mut cache = JavaMethodCache::new(global_class);

    for method in &methods {
        let method_id = env
            .get_static_method_id(
                "com/migo/runtime/internal/NativeExports",
                method.name,
                method.sig,
            )
            .map_err(|error| {
                format!(
                    "Failed to get method id for {} {}: {error}",
                    method.name, method.sig
                )
            })?;
        cache.insert_method_id(method.name, method_id);
    }

    JAVA_METHOD_CACHE
        .set(cache)
        .map_err(|_| "Failed to set JAVA_METHOD_CACHE (already initialized)".to_string())?;

    info!(method_count = methods.len(), "Cached NativeExports methods");
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::jni_profile_contract::{MethodDirection, MethodGroup, group_methods};

    #[test]
    fn request_vsync_descriptor_is_int_to_void() {
        let method = group_methods(MethodGroup::Core, MethodDirection::NativeToJava)
            .iter()
            .find(|method| method.name == "requestVsync")
            .expect("core requestVsync contract");
        assert_eq!(method.sig, "(I)V");
    }

    #[test]
    fn touch_descriptor_returns_acceptance_boolean() {
        let method = group_methods(MethodGroup::Core, MethodDirection::JavaToNative)
            .iter()
            .find(|method| method.name == "onTouchEvent")
            .expect("core touch contract");
        assert_eq!(method.sig, "(IIJILjava/nio/ByteBuffer;)Z");
    }

    #[test]
    fn on_camera_frame_data_descriptor_shape() {
        let method = group_methods(MethodGroup::Media, MethodDirection::JavaToNative)
            .iter()
            .find(|method| method.name == "onCameraFrameData")
            .expect("media camera-frame contract");
        assert_eq!(method.sig.matches("Ljava/nio/ByteBuffer;").count(), 3);
        assert_eq!(method.sig.matches('I').count(), 10);
        assert!(method.sig.ends_with(")V"));
    }
}
