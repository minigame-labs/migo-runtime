//! Pure product-profile contract for the Android JNI boundary.
//!
//! This module deliberately has no JNI dependency so host tests can prove the
//! method surface selected by each Cargo product before an Android build runs.

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub(crate) struct JniMethod {
    pub(crate) name: &'static str,
    pub(crate) sig: &'static str,
}

macro_rules! methods {
    ($(($name:literal, $sig:literal)),* $(,)?) => {
        &[$(JniMethod { name: $name, sig: $sig }),*]
    };
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProductProfile {
    Full,
    Slim,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MethodDirection {
    JavaToNative,
    NativeToJava,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MethodGroup {
    Core,
    Sensors,
    Media,
    Connectivity,
    Commerce,
    System,
}

impl MethodGroup {
    #[cfg(test)]
    pub(crate) const ALL: [Self; 6] = [
        Self::Core,
        Self::Sensors,
        Self::Media,
        Self::Connectivity,
        Self::Commerce,
        Self::System,
    ];
}

const NATIVE_CORE: &[JniMethod] = methods![
    ("version", "()Ljava/lang/String;"),
    ("getMinApiLevel", "()I"),
    ("initIcuData", "(Ljava/lang/String;)Z"),
    (
        "init",
        "(Ljava/lang/Object;Lcom/migo/runtime/RuntimeConfig;)I"
    ),
    ("shutdown", "(I)Z"),
    ("onShow", "(ILjava/lang/String;)V"),
    ("onHide", "(I)V"),
    ("onRestart", "(I)V"),
    ("updateSurface", "(ILjava/lang/Object;IIF)V"),
    ("onSurfaceDestroyed", "(I)V"),
    ("onTouchEvent", "(IIJILjava/nio/ByteBuffer;)Z"),
    ("modMain", "(ILjava/lang/String;Ljava/lang/String;)I"),
    ("executeScript", "(ILjava/lang/String;)I"),
    ("onVsync", "(IJ)V"),
    ("getDebugStats", "(I)[B"),
    ("getConsoleLogs", "(IJ)Ljava/lang/String;"),
    (
        "nativeAhbPointerFromHardwareBuffer",
        "(Landroid/hardware/HardwareBuffer;)J"
    ),
    ("onKeyboardInput", "(IJLjava/lang/String;)V"),
    ("onKeyboardConfirm", "(IJLjava/lang/String;)V"),
    ("onKeyboardComplete", "(IJLjava/lang/String;)V"),
    ("onKeyboardHeightChange", "(IJD)V"),
    ("onMemoryWarning", "(II)V"),
    ("onThermalStatusChanged", "(II)V"),
    ("onSubpackageProgress", "(ILjava/lang/String;)V"),
    ("onSubpackageResult", "(ILjava/lang/String;)V"),
];

const NATIVE_SENSORS: &[JniMethod] = methods![
    ("onDeviceMotionChange", "(IJDDD)V"),
    ("onGyroscopeChange", "(IJDDD)V"),
    ("onDeviceOrientationChange", "(ILjava/lang/String;)V"),
    ("onCompassChange", "(IJDLjava/lang/String;)V"),
    ("onAccelerometerChange", "(IJDDD)V"),
    ("onNetworkStatusChange", "(IZLjava/lang/String;)V"),
    ("onUserCaptureScreen", "(IJ)V"),
    ("onLocationResult", "(ILjava/lang/String;)V"),
    ("onFuzzyLocationResult", "(ILjava/lang/String;)V"),
    ("onScanCodeResult", "(ILjava/lang/String;)V"),
];

const NATIVE_MEDIA: &[JniMethod] = methods![
    ("onAudioInterruptionBegin", "(I)V"),
    ("onAudioInterruptionEnd", "(I)V"),
    (
        "onRecorderEvent",
        "(IJLjava/lang/String;Ljava/lang/String;)V"
    ),
    ("onRecorderFrameData", "(IJ[BZ)V"),
    (
        "onCameraEvent",
        "(IJILjava/lang/String;Ljava/lang/String;)V"
    ),
    (
        "onCameraFrameData",
        "(IJILjava/nio/ByteBuffer;IILjava/nio/ByteBuffer;IILjava/nio/ByteBuffer;IIII)V"
    ),
    ("onCompressImageResult", "(ILjava/lang/String;)V"),
    ("onChooseImageResult", "(ILjava/lang/String;)V"),
    ("onChooseMessageFileResult", "(ILjava/lang/String;)V"),
    ("onVideoEvent", "(IJILjava/lang/String;Ljava/lang/String;)V"),
];

const NATIVE_CONNECTIVITY: &[JniMethod] = methods![
    ("onOpenSystemBluetoothSetting", "(III)V"),
    ("onOpenAppAuthorizeSetting", "(III)V"),
    ("onBluetoothAdapterStateChange", "(IZZ)V"),
    ("onBluetoothDeviceFound", "(ILjava/lang/String;)V"),
    ("onBeaconUpdate", "(ILjava/lang/String;)V"),
    ("onBeaconServiceChange", "(IZZ)V"),
    ("onBLEConnectionStateChange", "(ILjava/lang/String;Z)V"),
    (
        "onBLECharacteristicValueChange",
        "(ILjava/lang/String;Ljava/lang/String;Ljava/lang/String;[B)V"
    ),
    ("onBLEMTUChange", "(ILjava/lang/String;I)V"),
    ("onLoginResult", "(ILjava/lang/String;)V"),
    ("onCheckSessionResult", "(ILjava/lang/String;)V"),
    ("onGetUserInfoResult", "(ILjava/lang/String;)V"),
    ("onGetPhoneNumberResult", "(ILjava/lang/String;)V"),
    ("onOpenSettingResult", "(ILjava/lang/String;)V"),
    ("onNavigateToMiniProgramResult", "(ILjava/lang/String;)V"),
];

const NATIVE_COMMERCE: &[JniMethod] = methods![
    ("onShareAppMessageResult", "(ILjava/lang/String;)V"),
    ("onAdEvent", "(ILjava/lang/String;)V"),
    ("onMidasPaymentResult", "(ILjava/lang/String;)V"),
    ("onMidasPaymentGameItemResult", "(ILjava/lang/String;)V"),
];

const NATIVE_SYSTEM: &[JniMethod] = methods![
    ("onAuthorizeResult", "(ILjava/lang/String;)V"),
    ("updatePermission", "(ILjava/lang/String;Z)Z"),
    ("onModalResult", "(IIII)V"),
    ("onActionSheetResult", "(III)V"),
];

const JAVA_CORE: &[JniMethod] = methods![
    // Core, not an optional group: every profile restarts its runtime, and a
    // Slim session that could not be told would leave its Java generation
    // boundary stuck in RESTARTING -- refusing every acquisition for the rest
    // of the session.
    ("beginRuntimeRestart", "(IJJ)V"),
    ("completeRuntimeRestart", "(IJ)V"),
    (
        "unzipFile",
        "(Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;"
    ),
    ("encodeGbk", "(Ljava/lang/String;)[B"),
    ("decodeGbk", "([B)Ljava/lang/String;"),
    ("decodeImageAhb", "([B)[B"),
    ("decodeImageRgba", "([B)[B"),
    ("keyboardShow", "(ILjava/lang/String;)V"),
    ("keyboardHide", "(I)V"),
    ("keyboardUpdate", "(ILjava/lang/String;)V"),
    ("subpackageDownload", "(ILjava/lang/String;)V"),
    ("onGameReady", "(I)V"),
    ("requestVsync", "(I)V"),
    ("onError", "(IILjava/lang/String;Ljava/lang/String;)V"),
    ("onExit", "(I)V"),
    ("onHostMessage", "(ILjava/lang/String;)V"),
    // Core for the same reason `beginRuntimeRestart` is: every profile presents, and
    // a Slim session whose Surface died at swap time would otherwise never be told --
    // the render worker stays alive, so no channel closes and no reply arrives.
    ("onSurfaceLost", "(IJI)V"),
];

const JAVA_SENSORS: &[JniMethod] = methods![
    ("getBatteryInfoJson", "()Ljava/lang/String;"),
    ("vibrateShort", "(Ljava/lang/String;)I"),
    ("vibrateLong", "()I"),
    ("getScreenBrightness", "(I)F"),
    ("setScreenBrightness", "(IF)I"),
    ("setKeepScreenOn", "(IZ)I"),
    ("setDeviceOrientation", "(ILjava/lang/String;)I"),
    ("startCaptureScreen", "(I)V"),
    ("stopCaptureScreen", "(I)V"),
    ("setEnableDebug", "(IZ)I"),
    ("startDeviceMotionListening", "(ILjava/lang/String;)V"),
    ("stopDeviceMotionListening", "(I)V"),
    ("startGyroscope", "(ILjava/lang/String;)V"),
    ("stopGyroscope", "(I)V"),
    ("startCompass", "(I)V"),
    ("stopCompass", "(I)V"),
    ("startAccelerometer", "(ILjava/lang/String;)V"),
    ("stopAccelerometer", "(I)V"),
    ("startNetworkMonitoring", "(I)V"),
    ("stopNetworkMonitoring", "(I)V"),
    ("getNetworkTypeJson", "(I)Ljava/lang/String;"),
    ("getLocalIPAddressJson", "()Ljava/lang/String;"),
    ("setClipboardData", "(ILjava/lang/String;)I"),
    ("getClipboardData", "(I)Ljava/lang/String;"),
    ("getLocation", "(ILjava/lang/String;)V"),
    ("getFuzzyLocation", "(ILjava/lang/String;)V"),
    ("scanCode", "(ILjava/lang/String;)V"),
];

const JAVA_MEDIA: &[JniMethod] = methods![
    ("setInnerAudioOption", "(IZZZ)V"),
    ("getAvailableAudioSources", "(I)Ljava/lang/String;"),
    ("recorderStart", "(ILjava/lang/String;)V"),
    ("recorderPause", "(I)V"),
    ("recorderResume", "(I)V"),
    ("recorderStop", "(I)V"),
    ("cameraCreate", "(ILjava/lang/String;)Ljava/lang/String;"),
    ("cameraDestroy", "(II)V"),
    ("cameraTakePhotoAsync", "(IILjava/lang/String;)V"),
    (
        "cameraStartRecord",
        "(ILjava/lang/String;)Ljava/lang/String;"
    ),
    (
        "cameraStopRecord",
        "(ILjava/lang/String;)Ljava/lang/String;"
    ),
    ("cameraSetZoom", "(ILjava/lang/String;)Ljava/lang/String;"),
    ("cameraListenFrameChange", "(II)V"),
    ("cameraCloseFrameChange", "(II)V"),
    ("imageSaveToPhotosAlbum", "(ILjava/lang/String;)V"),
    ("imagePreviewMedia", "(ILjava/lang/String;)V"),
    ("imagePreviewImage", "(ILjava/lang/String;)V"),
    ("imageCompress", "(ILjava/lang/String;)V"),
    ("imageChooseMessageFile", "(ILjava/lang/String;)V"),
    ("imageChooseImage", "(ILjava/lang/String;)V"),
    ("videoCreate", "(ILjava/lang/String;)Ljava/lang/String;"),
    ("videoPlay", "(II)V"),
    ("videoPause", "(II)V"),
    ("videoStop", "(II)V"),
    ("videoSeek", "(ILjava/lang/String;)V"),
    ("videoRequestFullscreen", "(ILjava/lang/String;)V"),
    ("videoExitFullscreen", "(II)V"),
    ("videoSetProperty", "(ILjava/lang/String;)V"),
    ("videoDestroy", "(II)V"),
];

const JAVA_CONNECTIVITY: &[JniMethod] = methods![
    ("openSystemBluetoothSetting", "(II)V"),
    ("openAppAuthorizeSetting", "(II)V"),
    ("getWindowInfoBytes", "(I)[B"),
    ("getSystemSettingInfoBytes", "(I)[B"),
    ("getDeviceInfoJson", "()Ljava/lang/String;"),
    ("getAppAuthorizationSettingJson", "(I)Ljava/lang/String;"),
    ("bluetoothOpenAdapter", "(ILjava/lang/String;)V"),
    ("bluetoothCloseAdapter", "(I)V"),
    ("bluetoothGetAdapterState", "(I)Ljava/lang/String;"),
    ("bluetoothStartDevicesDiscovery", "(ILjava/lang/String;)V"),
    ("bluetoothStopDevicesDiscovery", "(I)V"),
    ("bluetoothGetDevices", "(I)Ljava/lang/String;"),
    (
        "bluetoothGetConnectedDevices",
        "(ILjava/lang/String;)Ljava/lang/String;"
    ),
    ("bluetoothMakePair", "(ILjava/lang/String;)V"),
    ("bluetoothIsDevicePaired", "(ILjava/lang/String;)V"),
    ("bluetoothStartBeaconDiscovery", "(ILjava/lang/String;)V"),
    ("bluetoothStopBeaconDiscovery", "(I)V"),
    ("bluetoothGetBeacons", "(I)Ljava/lang/String;"),
    ("bleCreateConnection", "(ILjava/lang/String;)V"),
    ("bleCloseConnection", "(ILjava/lang/String;)V"),
    (
        "bleGetDeviceServices",
        "(ILjava/lang/String;)Ljava/lang/String;"
    ),
    (
        "bleGetDeviceCharacteristics",
        "(ILjava/lang/String;)Ljava/lang/String;"
    ),
    ("bleReadCharacteristicValue", "(ILjava/lang/String;)V"),
    ("bleWriteCharacteristicValue", "(ILjava/lang/String;)V"),
    (
        "bleNotifyCharacteristicValueChange",
        "(ILjava/lang/String;)V"
    ),
    (
        "bleGetDeviceRSSI",
        "(ILjava/lang/String;)Ljava/lang/String;"
    ),
    ("bleSetMTU", "(ILjava/lang/String;)V"),
    ("bleGetMTU", "(ILjava/lang/String;)Ljava/lang/String;"),
    ("gameLogReport", "(ILjava/lang/String;)V"),
    ("authLogin", "(ILjava/lang/String;)V"),
    ("authCheckSession", "(ILjava/lang/String;)V"),
    ("authGetUserInfo", "(ILjava/lang/String;)V"),
    ("authGetPhoneNumber", "(ILjava/lang/String;)V"),
    ("openSetting", "(ILjava/lang/String;)V"),
    ("navigateToMiniProgram", "(ILjava/lang/String;)V"),
    ("openCustomerServiceConversation", "(ILjava/lang/String;)V"),
];

const JAVA_COMMERCE: &[JniMethod] = methods![
    ("shareAppMessage", "(ILjava/lang/String;)V"),
    (
        "checkIsSupportMidasPayment",
        "(ILjava/lang/String;)Ljava/lang/String;"
    ),
    ("requestMidasPayment", "(ILjava/lang/String;)V"),
    ("requestMidasPaymentGameItem", "(ILjava/lang/String;)V"),
    ("adCreate", "(ILjava/lang/String;)V"),
    ("adLoad", "(ILjava/lang/String;)V"),
    ("adShow", "(ILjava/lang/String;)V"),
    ("adHide", "(ILjava/lang/String;)V"),
    ("adUpdateStyle", "(ILjava/lang/String;)V"),
    ("adDestroy", "(ILjava/lang/String;)V"),
];

const JAVA_SYSTEM: &[JniMethod] = methods![
    ("permissionRequest", "(ILjava/lang/String;)V"),
    ("revokePermissionResources", "(ILjava/lang/String;)V"),
    ("showToast", "(ILjava/lang/String;)V"),
    ("hideToast", "(I)V"),
    ("showModal", "(ILjava/lang/String;)V"),
    ("showLoading", "(ILjava/lang/String;)V"),
    ("hideLoading", "(I)V"),
    ("showActionSheet", "(ILjava/lang/String;)V"),
];

pub(crate) fn group_methods(
    group: MethodGroup,
    direction: MethodDirection,
) -> &'static [JniMethod] {
    match (direction, group) {
        (MethodDirection::JavaToNative, MethodGroup::Core) => NATIVE_CORE,
        (MethodDirection::JavaToNative, MethodGroup::Sensors) => NATIVE_SENSORS,
        (MethodDirection::JavaToNative, MethodGroup::Media) => NATIVE_MEDIA,
        (MethodDirection::JavaToNative, MethodGroup::Connectivity) => NATIVE_CONNECTIVITY,
        (MethodDirection::JavaToNative, MethodGroup::Commerce) => NATIVE_COMMERCE,
        (MethodDirection::JavaToNative, MethodGroup::System) => NATIVE_SYSTEM,
        (MethodDirection::NativeToJava, MethodGroup::Core) => JAVA_CORE,
        (MethodDirection::NativeToJava, MethodGroup::Sensors) => JAVA_SENSORS,
        (MethodDirection::NativeToJava, MethodGroup::Media) => JAVA_MEDIA,
        (MethodDirection::NativeToJava, MethodGroup::Connectivity) => JAVA_CONNECTIVITY,
        (MethodDirection::NativeToJava, MethodGroup::Commerce) => JAVA_COMMERCE,
        (MethodDirection::NativeToJava, MethodGroup::System) => JAVA_SYSTEM,
    }
}

#[cfg(test)]
pub(crate) fn methods_for(profile: ProductProfile, direction: MethodDirection) -> Vec<JniMethod> {
    let groups: &[MethodGroup] = match profile {
        ProductProfile::Full => &MethodGroup::ALL,
        ProductProfile::Slim => &[MethodGroup::Core],
    };
    let capacity = groups
        .iter()
        .map(|group| group_methods(*group, direction).len())
        .sum();
    let mut methods = Vec::with_capacity(capacity);
    for group in groups {
        methods.extend_from_slice(group_methods(*group, direction));
    }
    methods
}

pub(crate) fn active_methods(direction: MethodDirection) -> Vec<JniMethod> {
    let mut methods = group_methods(MethodGroup::Core, direction).to_vec();
    #[cfg(feature = "api-sensors")]
    methods.extend_from_slice(group_methods(MethodGroup::Sensors, direction));
    #[cfg(feature = "api-media")]
    methods.extend_from_slice(group_methods(MethodGroup::Media, direction));
    #[cfg(feature = "api-connectivity")]
    methods.extend_from_slice(group_methods(MethodGroup::Connectivity, direction));
    #[cfg(feature = "api-commerce")]
    methods.extend_from_slice(group_methods(MethodGroup::Commerce, direction));
    #[cfg(feature = "api-system")]
    methods.extend_from_slice(group_methods(MethodGroup::System, direction));
    methods
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn assert_unique(methods: &[JniMethod]) {
        let mut seen = HashSet::new();
        for method in methods {
            assert!(
                seen.insert((method.name, method.sig)),
                "duplicate JNI method: {} {}",
                method.name,
                method.sig
            );
        }
    }

    fn kept_member_names<'a>(rules: &'a str, class_name: &str) -> HashSet<&'a str> {
        let mut in_class = false;
        let mut names = HashSet::new();
        for line in rules.lines().map(str::trim) {
            if line.starts_with("-keepclassmembers") {
                in_class = line.contains(class_name);
                continue;
            }
            if !in_class {
                continue;
            }
            if line == "}" {
                in_class = false;
                continue;
            }
            let Some(open_paren) = line.find('(') else {
                continue;
            };
            if let Some(name) = line[..open_paren].split_whitespace().last() {
                names.insert(name);
            }
        }
        names
    }

    #[test]
    fn full_surface_preserves_the_existing_jni_contract() {
        let native = methods_for(ProductProfile::Full, MethodDirection::JavaToNative);
        let java = methods_for(ProductProfile::Full, MethodDirection::NativeToJava);

        // Counts are pinned so that growing the JNI surface is a deliberate
        // edit rather than a side effect. Last moved by the host-authoritative
        // ad bridge: +1 native (`onAdEvent`, the single inbound ad channel)
        // and +6 Java (`adCreate`/`adLoad`/`adShow`/`adHide`/`adUpdateStyle`/
        // `adDestroy`). Then by host-decided permissions: +2 native
        // (`onAuthorizeResult`, `updatePermission`) and +1 Java
        // (`permissionRequest`). Permission revocation adds +1 Java
        // (`revokePermissionResources`) for synchronous targeted teardown.
        // Deriving the display period from the vsync timestamps the scheduler
        // already receives removes -1 native (`setDisplayRefreshRate`), which
        // reported a rate once per session and never again when the display
        // changed mode.
        assert_eq!(native.len(), 68, "full NativeBridge surface changed");
        // Runtime-generation fencing adds +2 Java (`beginRuntimeRestart`,
        // `completeRuntimeRestart`), both Core: every profile restarts.
        // Concurrent-session correctness removes -1 Java (`getCacheDirPath`,
        // which nothing in the engine called and which resolved an Activity
        // through whichever session came first). Surface-loss delivery adds
        // +1 Java (`onSurfaceLost`), Core: it is the only signal that reaches
        // an Android host when presentation fails on a Surface it still holds.
        assert_eq!(java.len(), 127, "full NativeExports surface changed");
        assert_unique(&native);
        assert_unique(&java);
    }

    #[test]
    fn slim_surface_is_exactly_core() {
        for direction in [MethodDirection::JavaToNative, MethodDirection::NativeToJava] {
            let slim = methods_for(ProductProfile::Slim, direction);
            assert_eq!(slim, group_methods(MethodGroup::Core, direction));
            assert_unique(&slim);
        }
    }

    /// The profile this build compiled, read from the features rather than passed
    /// in as a value.
    const COMPILED_PROFILE: ProductProfile = if cfg!(feature = "profile-slim") {
        ProductProfile::Slim
    } else {
        ProductProfile::Full
    };

    /// One rule, two implementations, and until now only the one that never ships
    /// was tested.
    ///
    /// `methods_for` decides by matching a `ProductProfile` and exists only under
    /// `#[cfg(test)]`. The registration path calls `active_methods`, which decides
    /// with a chain of five `#[cfg(feature)]` attributes. Every other test in this
    /// module asserts over `methods_for`, so deleting any one line of that chain
    /// shipped a build registering fewer JNI methods than its profile declares --
    /// content calling a missing method would get `UnsatisfiedLinkError` at the
    /// moment it was used -- with this whole suite green.
    ///
    /// This is also the assertion that gives the Slim host suite something to
    /// observe: under Full it demands the union of every group, under Slim exactly
    /// Core, and it is the only test whose *result* depends on which profile
    /// compiled it.
    #[test]
    fn the_registered_surface_is_the_one_this_profile_declares() {
        for direction in [MethodDirection::JavaToNative, MethodDirection::NativeToJava] {
            assert_eq!(
                active_methods(direction),
                methods_for(COMPILED_PROFILE, direction),
                "active_methods' cfg chain disagrees with the {COMPILED_PROFILE:?} profile rule",
            );
        }
    }

    #[test]
    fn full_is_the_disjoint_union_of_core_and_optional_groups() {
        for direction in [MethodDirection::JavaToNative, MethodDirection::NativeToJava] {
            let full = methods_for(ProductProfile::Full, direction);
            let mut union = Vec::new();
            for group in MethodGroup::ALL {
                union.extend_from_slice(group_methods(group, direction));
            }
            assert_eq!(full, union);
            assert_unique(&full);
        }
    }

    #[test]
    fn critical_core_and_optional_members_are_classified() {
        let core_java = group_methods(MethodGroup::Core, MethodDirection::NativeToJava);
        for name in [
            "decodeImageAhb",
            "keyboardShow",
            "subpackageDownload",
            "requestVsync",
            "onHostMessage",
        ] {
            assert!(core_java.iter().any(|method| method.name == name), "{name}");
        }

        let slim_native = methods_for(ProductProfile::Slim, MethodDirection::JavaToNative);
        for name in [
            "onAudioInterruptionBegin",
            "onCameraFrameData",
            "onBluetoothAdapterStateChange",
            "onMidasPaymentResult",
            "onModalResult",
        ] {
            assert!(
                !slim_native.iter().any(|method| method.name == name),
                "slim leaked {name}"
            );
        }
    }

    #[test]
    fn slim_r8_rules_are_the_exact_core_jni_name_sets() {
        const PROGUARD: &str =
            include_str!("../../../../../../platforms/android/library/proguard-slim.pro");
        const CONSUMER: &str =
            include_str!("../../../../../../platforms/android/library/consumer-rules-slim.pro");
        let expected_native: HashSet<_> = NATIVE_CORE.iter().map(|method| method.name).collect();
        let expected_java: HashSet<_> = JAVA_CORE.iter().map(|method| method.name).collect();

        for (kind, rules) in [("local", PROGUARD), ("consumer", CONSUMER)] {
            assert_eq!(
                kept_member_names(rules, "NativeBridge"),
                expected_native,
                "{kind} slim NativeBridge roots drifted from the Rust contract"
            );
            assert_eq!(
                kept_member_names(rules, "NativeExports"),
                expected_java,
                "{kind} slim NativeExports roots drifted from the Rust contract"
            );
        }
    }
}
