use std::sync::{Arc, OnceLock};

use jni::{
    JNIEnv,
    objects::JStaticMethodID,
    signature::{Primitive, ReturnType},
    sys::jvalue,
};
use shared::{
    device::{Orientation, SystemSettings},
    error::{EngineError, ErrorCode},
    protocol::io_cmd::NormalizedImage,
    surface::{SafeArea, WindowInfo},
};

use crate::android::jni::{JAVA_METHOD_CACHE, JavaMethodCache, with_env};
use crate::jni_method_id::once_get_or_try_init;

// Binary protocol convention: all integer fields are little-endian (LE).
// Java side must use ByteBuffer.order(ByteOrder.LITTLE_ENDIAN).
// See also: shared/stats.rs (same convention).

/// Threshold for logging slow JNI calls (potential ANR risk).
/// Android triggers ANR after 5 seconds; we warn well before that.
const SLOW_JNI_CALL_SECS: u64 = 3;

/// Clear any pending JNI exception, logging it if present.
#[inline]
fn clear_jni_exception(env: &mut JNIEnv, context: &str) {
    if env.exception_check().unwrap_or(false) {
        env.exception_describe().ok();
        tracing::error!("JNI exception in {context}");
        env.exception_clear().ok();
    }
}

fn call_static_method<R, F>(
    method_name: &str,
    ret: ReturnType,
    handle: F,
    args: &[jvalue],
) -> Result<R, String>
where
    F: FnOnce(&mut JNIEnv, jni::objects::JValueOwned) -> Result<R, String>,
{
    with_env(|env| {
        let cache = JAVA_METHOD_CACHE
            .get()
            .ok_or("NativeExports class cache not initialized")?;
        let method_id = *cache
            .get_method_id(method_name)
            .ok_or("Method ID not found")?;
        invoke_static_method(env, cache, method_id, method_name, ret, handle, args)
    })
}

/// Invoke a resolved static method: the pre-call exception clear, the slow-call
/// timer and the error formatting every outbound call shares. Split out of
/// `call_static_method` so a per-frame call site can resolve its method id once
/// and still take the identical path for everything after that.
fn invoke_static_method<R, F>(
    env: &mut JNIEnv,
    cache: &JavaMethodCache,
    method_id: JStaticMethodID,
    method_name: &str,
    ret: ReturnType,
    handle: F,
    args: &[jvalue],
) -> Result<R, String>
where
    F: FnOnce(&mut JNIEnv, jni::objects::JValueOwned) -> Result<R, String>,
{
    // Clear any pre-existing JNI exception so a stale exception from a prior
    // call does not cause cascading failures (see notify_error).
    clear_jni_exception(env, method_name);

    let start = std::time::Instant::now();
    let result = unsafe { env.call_static_method_unchecked(cache.class(), method_id, ret, args) };
    let elapsed = start.elapsed();
    if elapsed.as_secs() >= SLOW_JNI_CALL_SECS {
        tracing::warn!(
            "Slow JNI call '{}': {:.1}s (potential ANR risk)",
            method_name,
            elapsed.as_secs_f64()
        );
    }

    match result {
        Ok(val) => handle(env, val),
        Err(e) => {
            clear_jni_exception(env, method_name);
            Err(format!("Failed to call method '{method_name}': {e}"))
        }
    }
}

/// Internal helper for void JNI calls with pre-constructed arguments.
/// Takes `&mut JNIEnv` directly to avoid nested `with_env` calls.
fn call_void_impl(env: &mut JNIEnv, method_name: &str, args: &[jvalue]) -> Result<(), String> {
    clear_jni_exception(env, method_name);

    let cache = JAVA_METHOD_CACHE
        .get()
        .ok_or("NativeExports class cache not initialized")?;
    let method_id = cache
        .get_method_id(method_name)
        .ok_or("Method ID not found")?;
    let class = cache.class();

    let start = std::time::Instant::now();
    let result = unsafe {
        env.call_static_method_unchecked(
            class,
            *method_id,
            ReturnType::Primitive(Primitive::Void),
            args,
        )
    };
    let elapsed = start.elapsed();
    if elapsed.as_secs() >= SLOW_JNI_CALL_SECS {
        tracing::warn!(
            "Slow JNI call '{}': {:.1}s (potential ANR risk)",
            method_name,
            elapsed.as_secs_f64()
        );
    }

    match result {
        Ok(_) => Ok(()),
        Err(e) => {
            clear_jni_exception(env, method_name);
            Err(format!("Failed to call method '{method_name}': {e}"))
        }
    }
}

/// Generate a void JNI call with only host_id parameter.
macro_rules! jni_void {
    ($fn_name:ident, $method:expr) => {
        pub fn $fn_name(host_id: i32) -> Result<(), String> {
            call_static_method(
                $method,
                ReturnType::Primitive(Primitive::Void),
                |_env, _| Ok(()),
                &[jvalue { i: host_id }],
            )
        }
    };
}

/// Generate a void JNI call with host_id + JSON string parameter.
macro_rules! jni_void_json {
    ($fn_name:ident, $method:expr) => {
        pub fn $fn_name(host_id: i32, options_json: &str) -> Result<(), String> {
            call_void_with_string($method, host_id, options_json)
        }
    };
}

/// Generate a JNI call that returns JSON string, with host_id + JSON string parameter.
macro_rules! jni_json {
    ($fn_name:ident, $method:expr) => {
        pub fn $fn_name(host_id: i32, options_json: &str) -> Result<String, String> {
            call_json_method($method, host_id, options_json)
        }
    };
}

/// Generate a void JNI call with host_id + one int parameter.
macro_rules! jni_void_int {
    ($fn_name:ident, $method:expr) => {
        pub fn $fn_name(host_id: i32, value: i32) -> Result<(), String> {
            call_static_method(
                $method,
                ReturnType::Primitive(Primitive::Void),
                |_env, _| Ok(()),
                &[jvalue { i: host_id }, jvalue { i: value }],
            )
        }
    };
}

// Both carry the request id the result must echo. Java cannot invent one, and a
// result with no id settles the oldest pending request rather than its own.
/// Generate a void JNI call with host_id + one long parameter.
macro_rules! jni_void_long {
    ($fn_name:ident, $method:expr) => {
        pub fn $fn_name(host_id: i32, value: i64) -> Result<(), String> {
            call_static_method(
                $method,
                ReturnType::Primitive(Primitive::Void),
                |_env, _| Ok(()),
                &[jvalue { i: host_id }, jvalue { j: value }],
            )
        }
    };
}

// The Java mirror of the engine's runtime generation. It is told, never asked:
// the engine owns the numbering, and a Java side that read it back would be a
// second authority.
pub fn begin_runtime_restart(host_id: i32, retired: i64, next: i64) -> Result<(), String> {
    call_static_method(
        "beginRuntimeRestart",
        ReturnType::Primitive(Primitive::Void),
        |_env, _| Ok(()),
        &[
            jvalue { i: host_id },
            jvalue { j: retired },
            jvalue { j: next },
        ],
    )
}

jni_void_long!(complete_runtime_restart, "completeRuntimeRestart");

jni_void_int!(open_bluetooth_settings, "openSystemBluetoothSetting");

jni_void_int!(open_app_authorize_setting, "openAppAuthorizeSetting");

// R1: request one Choreographer frame callback (Rust render/host -> Java).
// `NativeExports.requestVsync` posts to the main thread and rechecks the live
// GameSession, so a late call after teardown is harmless.
//
// The render thread issues this once per animating frame -- unlike every call
// above it, which a user action paces -- so it resolves its method id once
// rather than hashing "requestVsync" out of the name-keyed cache on every
// frame. Everything after the lookup is `invoke_static_method`, the same path
// the generic helper takes.
pub fn request_vsync(host_id: i32) -> Result<(), String> {
    static METHOD_ID: OnceLock<JStaticMethodID> = OnceLock::new();

    with_env(|env| {
        let cache = JAVA_METHOD_CACHE
            .get()
            .ok_or("NativeExports class cache not initialized")?;
        let method_id = *once_get_or_try_init(&METHOD_ID, || {
            cache
                .get_method_id("requestVsync")
                .copied()
                .ok_or_else(|| "Method ID not found for requestVsync".to_string())
        })?;
        invoke_static_method(
            env,
            cache,
            method_id,
            "requestVsync",
            ReturnType::Primitive(Primitive::Void),
            |_env, _| Ok(()),
            &[jvalue { i: host_id }],
        )
    })
}

pub fn get_window_info(host_id: i32) -> Result<WindowInfo, String> {
    call_static_method(
        "getWindowInfoBytes",
        ReturnType::Object,
        |env, result| {
            let byte_array = result.l().map_err(|_| "Null byte array from Java")?;
            let bytes = env
                .convert_byte_array(jni::objects::JByteArray::from(byte_array))
                .map_err(|e| format!("Failed to convert byte array: {}", e))?;

            if bytes.len() < 52 {
                return Err("Insufficient data in byte array".to_string());
            }

            let mut window_width =
                i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f32;
            let mut window_height =
                i32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as f32;
            let mut screen_width =
                i32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as f32;
            let mut screen_height =
                i32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]) as f32;
            let mut status_bar_height =
                i32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]) as f32;

            let pixel_ratio_int = i32::from_le_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
            let pixel_ratio = pixel_ratio_int as f32 / 1000.0;

            let mut screen_top =
                i32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]) as f32;

            // bytes[28..36]: reserved (2 x i32), skipped

            let mut safe_area_left =
                i32::from_le_bytes([bytes[36], bytes[37], bytes[38], bytes[39]]) as f32;
            let mut safe_area_top =
                i32::from_le_bytes([bytes[40], bytes[41], bytes[42], bytes[43]]) as f32;
            let mut safe_area_right =
                i32::from_le_bytes([bytes[44], bytes[45], bytes[46], bytes[47]]) as f32;
            let mut safe_area_bottom =
                i32::from_le_bytes([bytes[48], bytes[49], bytes[50], bytes[51]]) as f32;

            if pixel_ratio != 0.0 && (pixel_ratio - 1.0).abs() > f32::EPSILON {
                window_width /= pixel_ratio;
                window_height /= pixel_ratio;
                screen_width /= pixel_ratio;
                screen_height /= pixel_ratio;
                status_bar_height /= pixel_ratio;
                screen_top /= pixel_ratio;

                safe_area_left /= pixel_ratio;
                safe_area_top /= pixel_ratio;
                safe_area_right /= pixel_ratio;
                safe_area_bottom /= pixel_ratio;
            }

            Ok(WindowInfo {
                pixel_ratio,
                screen_width,
                screen_height,
                window_width,
                window_height,
                status_bar_height,
                screen_top,
                safe_area: SafeArea {
                    left: safe_area_left,
                    top: safe_area_top,
                    right: safe_area_right,
                    bottom: safe_area_bottom,
                },
            })
        },
        &[jvalue { i: host_id }],
    )
}

pub fn get_system_settings(host_id: i32) -> Result<SystemSettings, String> {
    call_static_method(
        "getSystemSettingInfoBytes",
        ReturnType::Object,
        |env, result| {
            let byte_array = result.l().map_err(|_| "Null byte array from Java")?;
            let bytes = env
                .convert_byte_array(jni::objects::JByteArray::from(byte_array))
                .map_err(|e| format!("Failed to convert byte array: {}", e))?;

            if bytes.len() < 4 {
                return Err("Insufficient data in system setting byte array".to_string());
            }

            Ok(SystemSettings {
                bluetooth_enabled: bytes[0] != 0,
                location_enabled: bytes[1] != 0,
                wifi_enabled: bytes[2] != 0,
                orientation: match bytes[3] {
                    1 => Orientation::Portrait,
                    2 => Orientation::Landscape,
                    _ => Orientation::Unknown,
                },
            })
        },
        &[jvalue { i: host_id }],
    )
}

pub fn get_device_info_json() -> Result<String, String> {
    call_static_method(
        "getDeviceInfoJson",
        ReturnType::Object,
        |env, result| {
            let jstring = result.l().map_err(|_| "Null string from Java")?;
            let json_str = env
                .get_string(&jni::objects::JString::from(jstring))
                .map_err(|e| format!("Failed to convert JSON string: {}", e))?
                .into();
            Ok(json_str)
        },
        &[],
    )
}

pub fn get_app_authorization_setting_json(host_id: i32) -> Result<String, String> {
    call_static_method(
        "getAppAuthorizationSettingJson",
        ReturnType::Object,
        |env, result| {
            let jstring = result.l().map_err(|_| "Null string from Java")?;
            let json_str = env
                .get_string(&jni::objects::JString::from(jstring))
                .map_err(|e| format!("Failed to convert authorization JSON string: {}", e))?
                .into();
            Ok(json_str)
        },
        &[jvalue { i: host_id }],
    )
}

// ==================== UI Interaction ====================

/// Call a void Java static method with signature (int, String).
fn call_void_with_string(method_name: &str, host_id: i32, json: &str) -> Result<(), String> {
    with_env(|env| {
        let jstr = env
            .new_string(json)
            .map_err(|e| format!("Failed to create Java string: {e}"))?;
        // Keep jstr alive until after the call so the local ref remains valid.
        let args = [
            jvalue { i: host_id },
            jvalue {
                l: jstr.as_raw() as *mut _,
            },
        ];
        call_void_impl(env, method_name, &args)
    })
}

/// Call a NativeExports static method with signature `(II)V`.
/// Avoids the String allocation needed by `call_void_with_string` when
/// the second argument is just an integer.
fn call_void_with_int(method_name: &str, host_id: i32, value: i32) -> Result<(), String> {
    with_env(|env| {
        call_void_impl(
            env,
            method_name,
            &[jvalue { i: host_id }, jvalue { i: value }],
        )
    })
}

jni_void_json!(show_toast, "showToast");
jni_void!(hide_toast, "hideToast");
jni_void_json!(show_modal, "showModal");
jni_void_json!(show_loading, "showLoading");
jni_void!(hide_loading, "hideLoading");
jni_void_json!(show_action_sheet, "showActionSheet");

// ==================== Battery ====================

pub fn get_battery_info_json() -> Result<String, String> {
    call_static_method(
        "getBatteryInfoJson",
        ReturnType::Object,
        |env, result| {
            let jstring = result.l().map_err(|_| "Null string from Java")?;
            let json_str = env
                .get_string(&jni::objects::JString::from(jstring))
                .map_err(|e| format!("Failed to convert battery info JSON string: {}", e))?
                .into();
            Ok(json_str)
        },
        &[],
    )
}

// ==================== Vibration ====================

/// Trigger a short vibration with intensity type.
/// Returns 0 on success, -1 if unavailable, -2 if type not supported.
pub fn vibrate_short(vibrate_type: &str) -> Result<i32, String> {
    with_env(|env| {
        let cache = JAVA_METHOD_CACHE
            .get()
            .ok_or("NativeExports class cache not initialized")?;
        let method_id = cache
            .get_method_id("vibrateShort")
            .ok_or("Method ID not found for vibrateShort")?;
        let class = cache.class();

        let jstr = env
            .new_string(vibrate_type)
            .map_err(|e| format!("Failed to create Java string: {e}"))?;

        let result = unsafe {
            env.call_static_method_unchecked(
                class,
                *method_id,
                ReturnType::Primitive(Primitive::Int),
                &[jvalue {
                    l: jstr.as_raw() as *mut _,
                }],
            )
        };

        match result {
            Ok(val) => Ok(val.i().unwrap_or(-1)),
            Err(e) => {
                clear_jni_exception(env, "vibrateShort");
                Err(format!("Failed to call vibrateShort: {e}"))
            }
        }
    })
}

/// Trigger a long vibration.
/// Returns 0 on success, -1 if unavailable.
pub fn vibrate_long() -> Result<i32, String> {
    call_static_method(
        "vibrateLong",
        ReturnType::Primitive(Primitive::Int),
        |_env, val| Ok(val.i().unwrap_or(-1)),
        &[],
    )
}

// ==================== Screen ====================

pub fn get_screen_brightness(host_id: i32) -> Result<f32, String> {
    call_static_method(
        "getScreenBrightness",
        ReturnType::Primitive(Primitive::Float),
        |_env, val| Ok(val.f().unwrap_or(-1.0)),
        &[jvalue { i: host_id }],
    )
}

pub fn set_screen_brightness(host_id: i32, value: f32) -> Result<i32, String> {
    call_static_method(
        "setScreenBrightness",
        ReturnType::Primitive(Primitive::Int),
        |_env, val| Ok(val.i().unwrap_or(-1)),
        &[jvalue { i: host_id }, jvalue { f: value }],
    )
}

pub fn set_keep_screen_on(host_id: i32, keep_on: bool) -> Result<i32, String> {
    call_static_method(
        "setKeepScreenOn",
        ReturnType::Primitive(Primitive::Int),
        |_env, val| Ok(val.i().unwrap_or(-1)),
        &[jvalue { i: host_id }, jvalue { z: keep_on as u8 }],
    )
}

pub fn set_device_orientation(host_id: i32, value: &str) -> Result<i32, String> {
    with_env(|env| {
        let cache = JAVA_METHOD_CACHE
            .get()
            .ok_or("NativeExports class cache not initialized")?;
        let method_id = cache
            .get_method_id("setDeviceOrientation")
            .ok_or("Method ID not found for setDeviceOrientation")?;
        let class = cache.class();

        let jstr = env
            .new_string(value)
            .map_err(|e| format!("Failed to create Java string: {e}"))?;

        let result = unsafe {
            env.call_static_method_unchecked(
                class,
                *method_id,
                ReturnType::Primitive(Primitive::Int),
                &[
                    jvalue { i: host_id },
                    jvalue {
                        l: jstr.as_raw() as *mut _,
                    },
                ],
            )
        };

        match result {
            Ok(val) => Ok(val.i().unwrap_or(-1)),
            Err(e) => {
                clear_jni_exception(env, "setDeviceOrientation");
                Err(format!("Failed to call setDeviceOrientation: {e}"))
            }
        }
    })
}

// ==================== Debug ====================

pub fn set_enable_debug(host_id: i32, enabled: bool) -> Result<i32, String> {
    call_static_method(
        "setEnableDebug",
        ReturnType::Primitive(Primitive::Int),
        |_env, val| Ok(val.i().unwrap_or(-1)),
        &[jvalue { i: host_id }, jvalue { z: enabled as u8 }],
    )
}

// ==================== Device Sensor ====================

jni_void_json!(start_device_motion, "startDeviceMotionListening");
jni_void!(stop_device_motion, "stopDeviceMotionListening");
jni_void_json!(start_gyroscope, "startGyroscope");
jni_void!(stop_gyroscope, "stopGyroscope");

// ==================== Compass ====================

jni_void!(start_compass, "startCompass");
jni_void!(stop_compass, "stopCompass");

// ==================== Accelerometer ====================

jni_void_json!(start_accelerometer, "startAccelerometer");
jni_void!(stop_accelerometer, "stopAccelerometer");

// ==================== Screen Capture ====================

jni_void!(start_capture_screen, "startCaptureScreen");
jni_void!(stop_capture_screen, "stopCaptureScreen");

// ==================== Network ====================

jni_void!(start_network_monitoring, "startNetworkMonitoring");
jni_void!(stop_network_monitoring, "stopNetworkMonitoring");

pub fn get_network_type_json(host_id: i32) -> Result<String, String> {
    call_static_method(
        "getNetworkTypeJson",
        ReturnType::Object,
        |env, result| {
            let jstring = result.l().map_err(|_| "Null string from Java")?;
            let json_str = env
                .get_string(&jni::objects::JString::from(jstring))
                .map_err(|e| format!("Failed to convert network type JSON string: {}", e))?
                .into();
            Ok(json_str)
        },
        &[jvalue { i: host_id }],
    )
}

pub fn get_local_ip_address_json() -> Result<String, String> {
    call_static_method(
        "getLocalIPAddressJson",
        ReturnType::Object,
        |env, result| {
            let jstring = result.l().map_err(|_| "Null string from Java")?;
            let json_str = env
                .get_string(&jni::objects::JString::from(jstring))
                .map_err(|e| format!("Failed to convert local IP JSON string: {}", e))?
                .into();
            Ok(json_str)
        },
        &[],
    )
}

// ==================== Audio Platform ====================

/// Set inner audio options: mixWithOther, obeyMuteSwitch, speakerOn.
/// Calls Java: NativeExports.setInnerAudioOption(hostId, mixWithOther, obeyMuteSwitch, speakerOn)
pub fn set_inner_audio_option(
    host_id: i32,
    mix_with_other: bool,
    obey_mute_switch: bool,
    speaker_on: bool,
) -> Result<(), String> {
    call_static_method(
        "setInnerAudioOption",
        ReturnType::Primitive(Primitive::Void),
        |_env, _| Ok(()),
        &[
            jvalue { i: host_id },
            jvalue {
                z: mix_with_other as u8,
            },
            jvalue {
                z: obey_mute_switch as u8,
            },
            jvalue {
                z: speaker_on as u8,
            },
        ],
    )
}

/// Get available audio input sources as JSON array string.
/// Calls Java: NativeExports.getAvailableAudioSources(hostId) -> String (JSON array)
pub fn get_available_audio_sources(host_id: i32) -> Result<String, String> {
    call_static_method(
        "getAvailableAudioSources",
        ReturnType::Object,
        |env, result| {
            let jstring = result.l().map_err(|_| "Null string from Java")?;
            let json_str = env
                .get_string(&jni::objects::JString::from(jstring))
                .map_err(|e| format!("Failed to convert audio sources string: {}", e))?
                .into();
            Ok(json_str)
        },
        &[jvalue { i: host_id }],
    )
}

// ==================== Recorder ====================

// Start recording with options JSON.
// Calls Java: NativeExports.recorderStart(hostId, optionsJson)
jni_void_json!(recorder_start, "recorderStart");

// Pause recording. Calls Java: NativeExports.recorderPause(hostId)
jni_void!(recorder_pause, "recorderPause");

// Resume recording. Calls Java: NativeExports.recorderResume(hostId)
jni_void!(recorder_resume, "recorderResume");

// Stop recording. Calls Java: NativeExports.recorderStop(hostId)
jni_void!(recorder_stop, "recorderStop");

// ==================== Charset Encoding (GBK) ====================

/// Encode a string to GBK bytes using Android's java.nio.charset.Charset.
/// Calls Java: NativeExports.encodeGbk(String) -> byte[]
pub fn encode_gbk(data: &str) -> Result<Vec<u8>, String> {
    with_env(|env| {
        let cache = JAVA_METHOD_CACHE
            .get()
            .ok_or("NativeExports class cache not initialized")?;
        let method_id = cache
            .get_method_id("encodeGbk")
            .ok_or("Method ID not found for encodeGbk")?;
        let class = cache.class();

        let j_data = env
            .new_string(data)
            .map_err(|e| format!("Failed to create Java string: {e}"))?;

        let result = unsafe {
            env.call_static_method_unchecked(
                class,
                *method_id,
                ReturnType::Array,
                &[jvalue {
                    l: j_data.as_raw() as *mut _,
                }],
            )
        };

        match result {
            Ok(val) => {
                let obj = val
                    .l()
                    .map_err(|e| format!("encodeGbk: expected object: {e}"))?;
                if obj.is_null() {
                    return Err("GBK encode error".to_string());
                }
                let byte_array = jni::objects::JByteArray::from(obj);
                let bytes = env
                    .convert_byte_array(byte_array)
                    .map_err(|e| format!("encodeGbk: failed to convert byte array: {e}"))?;
                Ok(bytes)
            }
            Err(e) => {
                clear_jni_exception(env, "encodeGbk");
                Err(format!("encodeGbk failed: {e}"))
            }
        }
    })
}

/// Decode GBK bytes to a string using Android's java.nio.charset.Charset.
/// Calls Java: NativeExports.decodeGbk(byte[]) -> String
pub fn decode_gbk(data: &[u8]) -> Result<String, String> {
    with_env(|env| {
        let cache = JAVA_METHOD_CACHE
            .get()
            .ok_or("NativeExports class cache not initialized")?;
        let method_id = cache
            .get_method_id("decodeGbk")
            .ok_or("Method ID not found for decodeGbk")?;
        let class = cache.class();

        let j_bytes = env
            .byte_array_from_slice(data)
            .map_err(|e| format!("Failed to create Java byte array: {e}"))?;

        let result = unsafe {
            env.call_static_method_unchecked(
                class,
                *method_id,
                ReturnType::Object,
                &[jvalue {
                    l: j_bytes.as_raw() as *mut _,
                }],
            )
        };

        match result {
            Ok(val) => {
                let obj = val
                    .l()
                    .map_err(|e| format!("decodeGbk: expected object: {e}"))?;
                if obj.is_null() {
                    return Err("GBK decode error".to_string());
                }
                let j_str = jni::objects::JString::from(obj);
                let s = env
                    .get_string(&j_str)
                    .map_err(|e| format!("decodeGbk: failed to get string: {e}"))?;
                Ok(s.into())
            }
            Err(e) => {
                clear_jni_exception(env, "decodeGbk");
                Err(format!("decodeGbk failed: {e}"))
            }
        }
    })
}

// ==================== File Operations ====================

/// Extract a zip file to target directory using Android's built-in java.util.zip.
/// Calls Java: NativeExports.unzipFile(zipPath, destDir) -> String
///
/// Returns Ok(file_count) on success, Err(message) on failure.
/// The Java side returns either a number string (success) or "ERR:..." (error).
pub fn unzip_file(zip_path: &str, dest_dir: &str) -> Result<usize, String> {
    with_env(|env| {
        let cache = JAVA_METHOD_CACHE
            .get()
            .ok_or("NativeExports class cache not initialized")?;
        let method_id = cache
            .get_method_id("unzipFile")
            .ok_or("Method ID not found")?;
        let class = cache.class();

        let j_zip_path = env
            .new_string(zip_path)
            .map_err(|e| format!("Failed to create Java string for zip_path: {e}"))?;
        let j_dest_dir = env
            .new_string(dest_dir)
            .map_err(|e| format!("Failed to create Java string for dest_dir: {e}"))?;

        let result = unsafe {
            env.call_static_method_unchecked(
                class,
                *method_id,
                ReturnType::Object,
                &[
                    jvalue {
                        l: j_zip_path.as_raw() as *mut _,
                    },
                    jvalue {
                        l: j_dest_dir.as_raw() as *mut _,
                    },
                ],
            )
        };

        match result {
            Ok(val) => {
                let jstring = val.l().map_err(|_| "Null string from Java")?;
                let result_str: String = env
                    .get_string(&jni::objects::JString::from(jstring))
                    .map_err(|e| format!("Failed to convert unzip result string: {e}"))?
                    .into();

                if result_str.starts_with("ERR:") {
                    Err(result_str[4..].to_string())
                } else {
                    result_str
                        .parse::<usize>()
                        .map_err(|_| format!("Invalid file count from Java: {result_str}"))
                }
            }
            Err(e) => {
                clear_jni_exception(env, "unzipFile");
                Err(format!("Failed to call unzipFile: {e}"))
            }
        }
    })
}

// ==================== Clipboard ====================

pub fn set_clipboard_data(host_id: i32, data: &str) -> Result<(), String> {
    with_env(|env| {
        let cache = JAVA_METHOD_CACHE
            .get()
            .ok_or("NativeExports class cache not initialized")?;
        let method_id = cache
            .get_method_id("setClipboardData")
            .ok_or("Method ID not found")?;
        let class = cache.class();

        let jstr = env
            .new_string(data)
            .map_err(|e| format!("Failed to create Java string: {e}"))?;

        let result = unsafe {
            env.call_static_method_unchecked(
                class,
                *method_id,
                ReturnType::Primitive(Primitive::Int),
                &[
                    jvalue { i: host_id },
                    jvalue {
                        l: jstr.as_raw() as *mut _,
                    },
                ],
            )
        };

        match result {
            Ok(val) => {
                let code = val.i().unwrap_or(-1);
                if code == 0 {
                    Ok(())
                } else {
                    Err("Clipboard operation failed".to_string())
                }
            }
            Err(e) => {
                clear_jni_exception(env, "setClipboardData");
                Err(format!("Failed to call setClipboardData: {e}"))
            }
        }
    })
}

pub fn get_clipboard_data(host_id: i32) -> Result<String, String> {
    call_static_method(
        "getClipboardData",
        ReturnType::Object,
        |env, result| {
            let jstring = result.l().map_err(|_| "Null string from Java")?;
            let data = env
                .get_string(&jni::objects::JString::from(jstring))
                .map_err(|e| format!("Failed to convert clipboard string: {}", e))?
                .into();
            Ok(data)
        },
        &[jvalue { i: host_id }],
    )
}

// ==================== Camera ====================

/// Call a Java static method with signature (int hostId, String json) -> String.
/// Used by camera, image, location, and any other API that takes options JSON and returns a JSON result.
fn call_json_method(method_name: &str, host_id: i32, options_json: &str) -> Result<String, String> {
    with_env(|env| {
        clear_jni_exception(env, method_name);

        let cache = JAVA_METHOD_CACHE
            .get()
            .ok_or("NativeExports class cache not initialized")?;
        let method_id = cache
            .get_method_id(method_name)
            .ok_or("Method ID not found")?;
        let class = cache.class();

        let jstr = env
            .new_string(options_json)
            .map_err(|e| format!("Failed to create Java string: {e}"))?;

        let start = std::time::Instant::now();
        let result = unsafe {
            env.call_static_method_unchecked(
                class,
                *method_id,
                ReturnType::Object,
                &[
                    jvalue { i: host_id },
                    jvalue {
                        l: jstr.as_raw() as *mut _,
                    },
                ],
            )
        };
        let elapsed = start.elapsed();
        if elapsed.as_secs() >= SLOW_JNI_CALL_SECS {
            tracing::warn!(
                "Slow JNI call '{}': {:.1}s (potential ANR risk)",
                method_name,
                elapsed.as_secs_f64()
            );
        }

        match result {
            Ok(val) => {
                let jstring = val.l().map_err(|_| "Null string from Java")?;
                let json_str = env
                    .get_string(&jni::objects::JString::from(jstring))
                    .map_err(|e| format!("Failed to convert JSON string: {e}"))?
                    .into();
                Ok(json_str)
            }
            Err(e) => {
                clear_jni_exception(env, method_name);
                Err(format!("Failed to call method '{method_name}': {e}"))
            }
        }
    })
}

// Create a camera instance.
// Calls Java: NativeExports.cameraCreate(hostId, optionsJson) -> String
jni_json!(camera_create, "cameraCreate");

/// Destroy a camera instance.
/// Calls Java: NativeExports.cameraDestroy(hostId, cameraId)
pub fn camera_destroy(host_id: i32, camera_id: u32) -> Result<(), String> {
    call_static_method(
        "cameraDestroy",
        ReturnType::Primitive(Primitive::Void),
        |_env, _| Ok(()),
        &[
            jvalue { i: host_id },
            jvalue {
                i: camera_id as i32,
            },
        ],
    )
}

// Take a photo. Calls Java: NativeExports.cameraTakePhoto(hostId, optionsJson) -> String
jni_json!(camera_take_photo, "cameraTakePhoto");

// Start video recording. Calls Java: NativeExports.cameraStartRecord(hostId, optionsJson) -> String
jni_json!(camera_start_record, "cameraStartRecord");

// Stop video recording. Calls Java: NativeExports.cameraStopRecord(hostId, optionsJson) -> String
jni_json!(camera_stop_record, "cameraStopRecord");

// Set camera zoom level. Calls Java: NativeExports.cameraSetZoom(hostId, optionsJson) -> String
jni_json!(camera_set_zoom, "cameraSetZoom");

/// Start listening for camera frame changes.
/// Calls Java: NativeExports.cameraListenFrameChange(hostId, cameraId)
pub fn camera_listen_frame_change(host_id: i32, camera_id: u32) -> Result<(), String> {
    call_static_method(
        "cameraListenFrameChange",
        ReturnType::Primitive(Primitive::Void),
        |_env, _| Ok(()),
        &[
            jvalue { i: host_id },
            jvalue {
                i: camera_id as i32,
            },
        ],
    )
}

/// Stop listening for camera frame changes.
/// Calls Java: NativeExports.cameraCloseFrameChange(hostId, cameraId)
pub fn camera_close_frame_change(host_id: i32, camera_id: u32) -> Result<(), String> {
    call_static_method(
        "cameraCloseFrameChange",
        ReturnType::Primitive(Primitive::Void),
        |_env, _| Ok(()),
        &[
            jvalue { i: host_id },
            jvalue {
                i: camera_id as i32,
            },
        ],
    )
}

// ==================== Bluetooth ====================

/// Call a Java Bluetooth method that takes (hostId) and returns a JSON string.
fn call_bluetooth_json_no_args(method_name: &str, host_id: i32) -> Result<String, String> {
    call_static_method(
        method_name,
        ReturnType::Object,
        |env, result| {
            let jstring = result.l().map_err(|_| "Null string from Java")?;
            let json_str = env
                .get_string(&jni::objects::JString::from(jstring))
                .map_err(|e| format!("Failed to convert bluetooth JSON string: {e}"))?
                .into();
            Ok(json_str)
        },
        &[jvalue { i: host_id }],
    )
}

jni_void_json!(bluetooth_open_adapter, "bluetoothOpenAdapter");
jni_void!(bluetooth_close_adapter, "bluetoothCloseAdapter");

pub fn bluetooth_get_adapter_state(host_id: i32) -> Result<String, String> {
    call_bluetooth_json_no_args("bluetoothGetAdapterState", host_id)
}

jni_void_json!(
    bluetooth_start_devices_discovery,
    "bluetoothStartDevicesDiscovery"
);
jni_void!(
    bluetooth_stop_devices_discovery,
    "bluetoothStopDevicesDiscovery"
);

pub fn bluetooth_get_devices(host_id: i32) -> Result<String, String> {
    call_bluetooth_json_no_args("bluetoothGetDevices", host_id)
}

jni_json!(
    bluetooth_get_connected_devices,
    "bluetoothGetConnectedDevices"
);
jni_void_json!(bluetooth_make_pair, "bluetoothMakePair");
jni_void_json!(bluetooth_is_device_paired, "bluetoothIsDevicePaired");
jni_void_json!(
    bluetooth_start_beacon_discovery,
    "bluetoothStartBeaconDiscovery"
);
jni_void!(
    bluetooth_stop_beacon_discovery,
    "bluetoothStopBeaconDiscovery"
);

pub fn bluetooth_get_beacons(host_id: i32) -> Result<String, String> {
    call_bluetooth_json_no_args("bluetoothGetBeacons", host_id)
}

// ---- BLE GATT ----

jni_void_json!(ble_create_connection, "bleCreateConnection");
jni_void_json!(ble_close_connection, "bleCloseConnection");
jni_json!(ble_get_device_services, "bleGetDeviceServices");
jni_json!(
    ble_get_device_characteristics,
    "bleGetDeviceCharacteristics"
);
jni_void_json!(ble_read_characteristic_value, "bleReadCharacteristicValue");
jni_void_json!(
    ble_write_characteristic_value,
    "bleWriteCharacteristicValue"
);
jni_void_json!(
    ble_notify_characteristic_value_change,
    "bleNotifyCharacteristicValueChange"
);
jni_json!(ble_get_device_rssi, "bleGetDeviceRSSI");
jni_void_json!(ble_set_mtu, "bleSetMTU");
jni_json!(ble_get_mtu, "bleGetMTU");

// ==================== Image API ====================

jni_void_json!(image_save_to_photos_album, "imageSaveToPhotosAlbum");
jni_void_json!(image_preview_media, "imagePreviewMedia");
jni_void_json!(image_preview_image, "imagePreviewImage");
jni_void_json!(image_compress, "imageCompress");
jni_void_json!(image_choose_message_file, "imageChooseMessageFile");
jni_void_json!(image_choose_image, "imageChooseImage");

// ==================== Subpackage ====================

jni_void_json!(subpackage_download, "subpackageDownload");

// ==================== Keyboard ====================

jni_void_json!(keyboard_show, "keyboardShow");
jni_void!(keyboard_hide, "keyboardHide");
jni_void_json!(keyboard_update, "keyboardUpdate");

// ==================== Location ====================

jni_void_json!(get_location, "getLocation");
jni_void_json!(get_fuzzy_location, "getFuzzyLocation");

// ==================== Scan Code ====================

jni_void_json!(scan_code, "scanCode");

// ==================== Game Log ====================

jni_void_json!(game_log_report, "gameLogReport");

// ==================== Ads ====================
//
// Fire-and-forget: every ad result -- including the reward verdict -- comes
// back on the `onAdEvent` channel, never as a return value here.

jni_void_json!(ad_create, "adCreate");
jni_void_json!(ad_load, "adLoad");
jni_void_json!(ad_show, "adShow");
jni_void_json!(ad_hide, "adHide");
jni_void_json!(ad_update_style, "adUpdateStyle");
jni_void_json!(ad_destroy, "adDestroy");

// ==================== Permission ====================
//
// Fire-and-forget: the host may put the question to a user, so the decision
// comes back on the `onAuthorizeResult` channel rather than as a return value.

jni_void_json!(permission_request, "permissionRequest");
jni_void_json!(permission_revoke_resources, "revokePermissionResources");

// ==================== Auth ====================

jni_void_json!(auth_login, "authLogin");
jni_void_json!(auth_check_session, "authCheckSession");
jni_void_json!(auth_get_user_info, "authGetUserInfo");
jni_void_json!(auth_get_phone_number, "authGetPhoneNumber");

// ==================== Lifecycle Notification ====================

// Notify the Java layer that the game module has been loaded and is ready.
// Calls `NativeExports.onGameReady(hostId)` so Java can measure startup time.
jni_void!(notify_game_ready, "onGameReady");

// ==================== Error Notification ====================

// Notify the Java layer that the host is exiting normally.
// Calls `NativeExports.onExit(hostId)` so Java can finish the Activity.
// Used when JS calls `exitMiniProgram()`.
jni_void!(notify_exit, "onExit");

// Deliver a JS-to-host message to the Java layer.
// Calls `NativeExports.onHostMessage(hostId, json)` where `json` is a
// `{"type":"...","payload":"..."}` envelope built by the `op_send_to_host` op.
jni_void_json!(notify_host_message, "onHostMessage");

/// Tell the Java layer that a live Surface was retired after a presentation failure.
///
/// Calls `NativeExports.onSurfaceLost(hostId, generation, reason)`. Distinct from
/// `onSurfaceDestroyed`, which travels the other way: that one is Java telling the
/// engine it is taking its Surface back, and this one is the engine telling Java that
/// the Surface Java still believes in is gone. Nothing else carries that direction --
/// the render worker stays alive, no channel closes and no reply arrives -- so without
/// this an Android host had no way to learn it and no reason to attach another.
///
/// The generation is the one that was lost, so a host that has already moved on can
/// recognise a report for a Surface it replaced.
pub fn notify_surface_lost(host_id: i32, generation: u64, reason: u32) -> Result<(), String> {
    call_static_method(
        "onSurfaceLost",
        ReturnType::Primitive(Primitive::Void),
        |_env, _| Ok(()),
        &[
            jvalue { i: host_id },
            jvalue {
                j: generation as i64,
            },
            jvalue { i: reason as i32 },
        ],
    )
}

/// Notify the Java layer about a fatal engine error.
///
/// Calls `NativeExports.onError(hostId, errorCode, message, detail)`.
///
/// This is called from:
/// - `catch_unwind` when the host thread panics
/// - Watchdog when an ANR is detected
/// - V8 heap limit / execution timeout handlers
///
/// **Thread safety:** This function uses `with_env` which attaches the calling
/// thread to the JVM if not already attached.  Safe to call from any thread
/// (host thread, watchdog thread, etc.).
pub fn notify_error(
    host_id: i32,
    error_code: u16,
    message: &str,
    detail: &str,
) -> Result<(), String> {
    with_env(|env| {
        // Defensively clear any pre-existing JNI exception state so that
        // string creation below doesn't fail due to a stale exception from
        // a prior JNI call (e.g., during OOM or crash paths).
        clear_jni_exception(env, "notify_error (pre-call)");

        let cache = JAVA_METHOD_CACHE
            .get()
            .ok_or("NativeExports class cache not initialized")?;
        let method_id = cache
            .get_method_id("onError")
            .ok_or("Method ID not found for onError")?;
        let class = cache.class();

        // OOM-resilient string creation: if new_string fails (e.g., under
        // heavy memory pressure), fall back to empty strings so the error
        // code still reaches Java.
        let j_msg = env
            .new_string(message)
            .or_else(|_| env.new_string(""))
            .map_err(|e| format!("Failed to create Java string for message: {e}"))?;
        let j_detail = env
            .new_string(detail)
            .or_else(|_| env.new_string(""))
            .map_err(|e| format!("Failed to create Java string for detail: {e}"))?;

        let result = unsafe {
            env.call_static_method_unchecked(
                class,
                *method_id,
                ReturnType::Primitive(Primitive::Void),
                &[
                    jvalue { i: host_id },
                    jvalue {
                        i: error_code as i32,
                    },
                    jvalue {
                        l: j_msg.as_raw() as *mut _,
                    },
                    jvalue {
                        l: j_detail.as_raw() as *mut _,
                    },
                ],
            )
        };

        match result {
            Ok(_) => Ok(()),
            Err(e) => {
                clear_jni_exception(env, "onError");
                Err(format!("Failed to call onError: {e}"))
            }
        }
    })
}

// ==================== Image Decoding ====================

/// Decode image bytes to RGBA using Android's BitmapFactory via JNI.
/// Returns NormalizedImage on success.
///
/// The Java side returns a packed byte array: `[width_le32, height_le32, RGBA_pixels...]`.
/// We read width/height from the first 8 bytes, then copy the RGBA payload from `bytes[8..]`.
pub fn decode_image_rgba_jni(data: &[u8]) -> Result<NormalizedImage, EngineError> {
    let result: Result<NormalizedImage, String> = with_env(|env| {
        let cache = JAVA_METHOD_CACHE
            .get()
            .ok_or("NativeExports cache not initialized")?;
        let method_id = cache
            .get_method_id("decodeImageRgba")
            .ok_or("decodeImageRgba method not found")?;
        let class = cache.class();

        // Clear any stale pending exception before JNI allocation.
        clear_jni_exception(env, "decode_image_rgba_jni (pre-call)");

        let j_data = env.byte_array_from_slice(data).map_err(|e| {
            // byte_array_from_slice calls NewByteArray which can throw
            // OutOfMemoryError.  Clear it so ART doesn't abort on the
            // next JNI call.
            clear_jni_exception(env, "decode_image_rgba_jni (byte_array_from_slice)");
            format!("Failed to create byte array ({}B): {e}", data.len())
        })?;

        let result = unsafe {
            env.call_static_method_unchecked(
                class,
                *method_id,
                ReturnType::Object,
                &[jvalue {
                    l: j_data.as_raw() as *mut _,
                }],
            )
        };

        match result {
            Ok(val) => {
                let obj = val.l().map_err(|e| format!("expected object: {e}"))?;
                if obj.is_null() {
                    return Err("BitmapFactory returned null".into());
                }
                let byte_array = jni::objects::JByteArray::from(obj);
                let bytes = env
                    .convert_byte_array(byte_array)
                    .map_err(|e| format!("Failed to read result bytes: {e}"))?;

                if bytes.len() < 8 {
                    return Err("BitmapFactory response too short".into());
                }

                let width = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                let height = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);

                let expected = (width as usize) * (height as usize) * 4;
                if bytes.len() - 8 != expected {
                    return Err(format!(
                        "BitmapFactory: expected {} RGBA bytes, got {}",
                        expected,
                        bytes.len() - 8
                    ));
                }

                // Use offset-based access to avoid O(n) shift from drain(..8).
                let rgba = bytes[8..].to_vec();

                Ok(NormalizedImage {
                    width,
                    height,
                    rgba: Arc::new(rgba),
                })
            }
            Err(e) => {
                clear_jni_exception(env, "decodeImageRgba");
                Err(format!("BitmapFactory JNI call failed: {e}"))
            }
        }
    });

    result.map_err(|e| EngineError::new(ErrorCode::ImageReadError).with_detail(e))
}

/// Zero-memcpy image decode path.
///
/// Calls `NativeExports.decodeImageAhb(byte[]) -> byte[]`. The Java
/// side decodes into a hardware-backed bitmap and returns a packed
/// 16-byte header `[ahb_ptr_i64_le, width_u32_le, height_u32_le]`.
/// A zero pointer means "decode failed or AHB path unavailable; caller
/// must fall back to [`decode_image_rgba_jni`]".
///
/// On success the returned [`AhbImage`] adopts **one strong refcount**
/// on the AHB. The Java bridge acquires that native ref before it
/// closes its `HardwareBuffer` wrapper, so Rust must adopt the raw
/// pointer without another acquire.
pub fn decode_image_ahb_jni(
    data: &[u8],
) -> Result<shared::protocol::io_cmd::AhbImage, EngineError> {
    use shared::protocol::ahb::{AhbDesc, OwnedAhb};
    use shared::protocol::io_cmd::AhbImage;

    let result: Result<AhbImage, String> = with_env(|env| {
        let cache = JAVA_METHOD_CACHE
            .get()
            .ok_or("NativeExports cache not initialized")?;
        let method_id = cache
            .get_method_id("decodeImageAhb")
            .ok_or("decodeImageAhb method not found")?;
        let class = cache.class();

        clear_jni_exception(env, "decode_image_ahb_jni (pre-call)");

        let j_data = env.byte_array_from_slice(data).map_err(|e| {
            clear_jni_exception(env, "decode_image_ahb_jni (byte_array_from_slice)");
            format!("Failed to create byte array ({}B): {e}", data.len())
        })?;

        let call_result = unsafe {
            env.call_static_method_unchecked(
                class,
                *method_id,
                ReturnType::Object,
                &[jvalue {
                    l: j_data.as_raw() as *mut _,
                }],
            )
        };

        match call_result {
            Ok(val) => {
                let obj = val.l().map_err(|e| format!("expected object: {e}"))?;
                if obj.is_null() {
                    return Err("decodeImageAhb returned null".into());
                }
                let byte_array = jni::objects::JByteArray::from(obj);
                let bytes = env
                    .convert_byte_array(byte_array)
                    .map_err(|e| format!("Failed to read AHB header: {e}"))?;

                if bytes.len() != 16 {
                    return Err(format!(
                        "decodeImageAhb header size mismatch: got {}, want 16",
                        bytes.len()
                    ));
                }
                let ahb_ptr = i64::from_le_bytes([
                    bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
                ]);
                let width = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
                let height = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);

                if ahb_ptr == 0 {
                    return Err("decodeImageAhb: null AHB pointer (fallback path)".into());
                }

                // Adopt the Java-bumped refcount. Descriptor width
                // and height come straight from the decoder.
                let desc = AhbDesc::rgba_sampled(width, height);
                // SAFETY: the trusted Java bridge returns a live AHB pointer
                // with one native reference transferred to Rust. This path
                // creates the canonical wrapper; any Rust sharing uses Clone.
                let ahb =
                    unsafe { OwnedAhb::from_raw_owned(ahb_ptr as *mut std::ffi::c_void, desc) }
                        .map_err(|e| format!("AHB adopt: {e}"))?;
                Ok(AhbImage::new(width, height, ahb))
            }
            Err(e) => {
                clear_jni_exception(env, "decodeImageAhb");
                Err(format!("decodeImageAhb call failed: {e}"))
            }
        }
    });

    result.map_err(|e| EngineError::new(ErrorCode::ImageReadError).with_detail(e))
}

// ==================== Setting ====================

jni_void_json!(open_setting, "openSetting");

// ==================== Share ====================

jni_void_json!(share_app_message, "shareAppMessage");

// ==================== Navigate ====================

jni_void_json!(navigate_to_mini_program, "navigateToMiniProgram");
jni_void_json!(
    open_customer_service_conversation,
    "openCustomerServiceConversation"
);

// ==================== Payment ====================

jni_json!(check_is_support_midas_payment, "checkIsSupportMidasPayment");
jni_void_json!(request_midas_payment, "requestMidasPayment");
jni_void_json!(
    request_midas_payment_game_item,
    "requestMidasPaymentGameItem"
);

// ==================== Video ====================

jni_json!(video_create, "videoCreate");

pub fn video_play(host_id: i32, video_id: u32) -> Result<(), String> {
    call_void_with_int("videoPlay", host_id, video_id as i32)
}

pub fn video_pause(host_id: i32, video_id: u32) -> Result<(), String> {
    call_void_with_int("videoPause", host_id, video_id as i32)
}

pub fn video_stop(host_id: i32, video_id: u32) -> Result<(), String> {
    call_void_with_int("videoStop", host_id, video_id as i32)
}

pub fn video_seek(host_id: i32, video_id: u32, position: f64) -> Result<(), String> {
    call_void_with_string(
        "videoSeek",
        host_id,
        &format!("{{\"videoId\":{},\"position\":{}}}", video_id, position),
    )
}

pub fn video_request_fullscreen(host_id: i32, video_id: u32, direction: i32) -> Result<(), String> {
    call_void_with_string(
        "videoRequestFullscreen",
        host_id,
        &format!("{{\"videoId\":{},\"direction\":{}}}", video_id, direction),
    )
}

pub fn video_exit_fullscreen(host_id: i32, video_id: u32) -> Result<(), String> {
    call_void_with_int("videoExitFullscreen", host_id, video_id as i32)
}

pub fn video_set_property(host_id: i32, video_id: u32, property_json: &str) -> Result<(), String> {
    call_void_with_string(
        "videoSetProperty",
        host_id,
        &format!(
            "{{\"videoId\":{},\"properties\":{}}}",
            video_id, property_json
        ),
    )
}

pub fn video_destroy(host_id: i32, video_id: u32) -> Result<(), String> {
    call_void_with_int("videoDestroy", host_id, video_id as i32)
}
