#![allow(non_snake_case)]

use shared::js_escape::{hook_args_one, hook_args_three, hook_args_two};
// The generation Java captured when the manager was built, not one read here.
use shared::protocol::host_cmd::captured_generation;

use std::borrow::Cow;
use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use jni::objects::{JByteBuffer, JClass, JObject, JString};

/// Parse the internal subPackagesJson field into a Vec of (name, root) pairs.
fn parse_sub_packages(json: Option<String>) -> Vec<(String, String)> {
    let json = match json {
        Some(s) if !s.is_empty() => s,
        _ => return Vec::new(),
    };
    #[derive(serde::Deserialize)]
    struct Entry {
        name: String,
        root: String,
    }
    serde_json::from_str::<Vec<Entry>>(&json)
        .map(|v| v.into_iter().map(|e| (e.name, e.root)).collect())
        .unwrap_or_default()
}

/// Parse the internal preludeScriptsJson field into a Vec of (name, source)
/// pairs. The Java side encodes prelude entries as
/// `[{"name":"...","source":"..."}, ...]`; we deserialize back into the
/// owned-string pairs `InitOptions::with_prelude_scripts` expects.
///
/// Returns an empty vec on null/empty/malformed input — a malformed prelude
/// list shouldn't kill init; the launch will simply proceed without
/// adapter injection (and the misconfigured client should notice that
/// browser-style globals are missing and read this code path's logs).
fn parse_prelude_scripts(json: Option<String>) -> Vec<(String, String)> {
    let json = match json {
        Some(s) if !s.is_empty() => s,
        _ => return Vec::new(),
    };
    #[derive(serde::Deserialize)]
    struct Entry {
        name: String,
        source: String,
    }
    match serde_json::from_str::<Vec<Entry>>(&json) {
        Ok(v) => v.into_iter().map(|e| (e.name, e.source)).collect(),
        Err(e) => {
            tracing::warn!("invalid preludeScriptsJson, ignoring: {}", e);
            Vec::new()
        }
    }
}

/// Convert a JNI string to `Cow<'static, str>`.
/// Returns a borrowed `&'static str` for known constant values,
/// avoiding heap allocation in the common case.
fn jni_string_to_cow(
    env: &mut JNIEnv,
    jstr: &JString,
    known: &[&'static str],
) -> Cow<'static, str> {
    let s: String = env.get_string(jstr).map(|s| s.into()).unwrap_or_default();
    for &k in known {
        if s == k {
            return Cow::Borrowed(k);
        }
    }
    Cow::Owned(s)
}

// ---------------------------------------------------------------------------
// Helper: forward a JSON result string from JNI to JS via EvalScript.
// Used by all "Mode C" callbacks that receive a JSON result from Java and
// need to invoke a global `_internalOn*('escaped_json')` function in V8.
// ---------------------------------------------------------------------------

fn read_json_result(env: &mut JNIEnv, result_json: &JString, fallback_json: &str) -> String {
    env.get_string(result_json)
        .map(|s| s.into())
        .unwrap_or_else(|_| fallback_json.to_string())
}

fn send_json_result_to_js(host_id: jint, json: &str, js_callback: &'static str) {
    // The hook is handed the JSON *string* and parses it itself, which is what
    // it has always received. Decoding here would change the argument every
    // one of these callbacks sees.
    let cmd = HostCommand::InvokeHostHook {
        hook: js_callback,
        args_json: hook_args_one(json),
    };
    let _ = send_reliable_command_to_host(host_id, cmd);
}

fn forward_json_result_to_js(
    env: &mut JNIEnv,
    host_id: jint,
    result_json: &JString,
    js_callback: &'static str,
    fallback_json: &str,
) {
    let json = read_json_result(env, result_json, fallback_json);
    send_json_result_to_js(host_id, &json, js_callback);
}

/// Generate a JNI `extern "system"` callback that forwards a JSON string
/// result to JS via `forward_json_result_to_js`.
macro_rules! jni_json_callback {
    ($fn_name:ident, $js_callback:literal) => {
        jni_json_callback!(
            $fn_name,
            $js_callback,
            r#"{"error":"failed to read result"}"#
        );
    };
    ($fn_name:ident, $js_callback:literal, $fallback:expr) => {
        pub(crate) extern "system" fn $fn_name<'local>(
            mut env: JNIEnv<'local>,
            _class: JClass<'local>,
            host_id: jint,
            result_json: JString<'local>,
        ) {
            jni_safe!(stringify!($fn_name), {
                forward_json_result_to_js(&mut env, host_id, &result_json, $js_callback, $fallback);
            });
        }
    };
}
use jni::sys::{JNI_FALSE, JNI_TRUE, jboolean, jdouble, jfloat, jint, jlong, jobject, jstring};
use jni::{JNIEnv, JavaVM};

use tracing::{error, info, warn};

use migo_core::{
    HostIngress, HostIngressSendError, host_ingress, lease_surface, retire_surface,
    send_command_to_host, send_critical_command_to_host, send_reliable_command_to_host,
    spawn_host_thread,
};
use shared::protocol::camera_frame::{
    CameraFrameEntry, CameraFramePush, PlaneWindow, pack_yuv_planes, publish_camera_frame,
    take_camera_frame, validate_camera_frame_dimensions, validate_camera_frame_payload_lengths,
    with_camera_frame_admission,
};
use shared::protocol::host_cmd::{HostCommand, TouchData, TouchPoint, TouchType};
use shared::protocol::recorder_frame::with_recorder_frame_credit;
use shared::surface::{PixelRatio, SurfaceRef};

use shared::config::InitOptions;
use shared::error::ErrorCode;

use crate::android::jni::init_jni_env;
use crate::android::jni::jni_safe;
use crate::android::logging;
use crate::android::platform::AndroidPlatform;
use crate::android::surface::{
    ANativeWindow_fromSurface, ANativeWindow_getHeight, ANativeWindow_getWidth,
    ANativeWindow_release, ANativeWindow_setBuffersGeometry, AndroidSurfaceWrapper,
};
use crate::host_owners::HostOwners;

static HOST_OWNERS: OnceLock<HostOwners> = OnceLock::new();

fn host_owners() -> &'static HostOwners {
    HOST_OWNERS.get_or_init(HostOwners::new)
}

thread_local! {
    /// Android touch and Choreographer callbacks normally share the UI thread.
    /// Cache immutable per-Host ingress there so steady-state delivery takes
    /// neither the global Host registry lock nor a payload allocation.
    static HOT_INGRESS: RefCell<Option<HostIngress>> = const { RefCell::new(None) };
}

fn with_hot_ingress<R>(host_id: jint, operation: impl FnOnce(&HostIngress) -> R) -> Option<R> {
    HOT_INGRESS.with(|slot| {
        let needs_refresh = slot
            .borrow()
            .as_ref()
            .is_none_or(|ingress| ingress.host_id() != host_id);
        if needs_refresh {
            let ingress = match host_ingress(host_id) {
                Ok(ingress) => ingress,
                Err(error) => {
                    tracing::debug!("Host {host_id} hot ingress unavailable: {error}");
                    return None;
                }
            };
            *slot.borrow_mut() = Some(ingress);
        }
        let ingress = slot.borrow();
        Some(operation(
            ingress.as_ref().expect("hot ingress was just installed"),
        ))
    })
}

fn invalidate_hot_ingress(host_id: jint) {
    HOT_INGRESS.with(|slot| {
        if slot
            .borrow()
            .as_ref()
            .is_some_and(|ingress| ingress.host_id() == host_id)
        {
            *slot.borrow_mut() = None;
        }
    });
}

/// Body of the Android `JNI_OnLoad` entry point.
///
/// The exported `JNI_OnLoad` symbol itself lives in the `android-jni` cdylib
/// crate, which exists only to be `libmigo.so`. Keeping the logic here — and
/// the one exported symbol there — is what lets `platform` stay a reusable
/// rlib while every target owns a separate, audited delivery boundary.
pub fn on_load(vm: JavaVM) -> jint {
    // Initialize logging/tracing once.
    logging::init_logging();

    // Initialize ndk-context for cpal/oboe audio backend.
    // This must be done before any audio operations.
    unsafe {
        ndk_context::initialize_android_context(
            vm.get_java_vm_pointer().cast(),
            std::ptr::null_mut(), // activity can be null for audio-only usage
        );
    }

    // Initialize the global JNI environment helper.
    // This also registers Java exports + native exports.
    init_jni_env(vm).expect("Failed to initialize JNI environment");

    jni::sys::JNI_VERSION_1_6
}

/// The engine version `NativeBridge.version()` hands back to the Java SDK.
///
/// Taken from `CARGO_PKG_VERSION`, which is `[workspace.package] version` --
/// the mirror `scripts/test-release-version-contract.sh` holds equal to
/// `release/VERSION`. It was a hardcoded literal for a long time, which made
/// `MigoRuntime.getNativeVersion()` report a version no build had carried since
/// the 0.1.x line and left `MigoRuntime`'s SDK-vs-native skew check comparing
/// that constant against an identically frozen Java one, so it could never fire.
pub(crate) extern "system" fn version<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> jstring {
    jni_safe!("version", std::ptr::null_mut(), {
        match env.new_string(env!("CARGO_PKG_VERSION")) {
            Ok(s) => s.into_raw(),
            Err(_) => std::ptr::null_mut(),
        }
    })
}

/// Minimum Android API level the native engine was compiled for.
///
/// Sourced from the single authority (`scripts/build-android-so.sh`
/// - currently API 26, matching skia-bindings 0.93's Android NDK
/// preset).  The Java SDK uses this in `isDeviceSupported()` so
/// there is exactly ONE place to change when the floor moves; no
/// silent mismatch between the build script's ANDROID_API and a
/// hard-coded Java constant is possible any more.
pub(crate) extern "system" fn getMinApiLevel<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> jint {
    // Must be kept in sync with scripts/build-android-so.sh
    // (`ANDROID_API`) and platforms/android/library/build.gradle
    // (`minSdk`).  The test in `platform/android/tests` pins the
    // trio together.
    26
}

/// Bridge for `NativeBridge.initIcuData(path)`.
///
/// Two build-time paths:
///
/// * With the default (`graphics` crate's `embed_icudtl` feature):
///   Skia links `icudtl.dat` into `libmigo.so` at build time.  No
///   runtime load is needed; the Java wrapper may still call this
///   for uniform handling, and we return `true` immediately.
///
/// * When the selected graphics artifact does not embed ICU data:
///   Skia needs `SkLoadICU(path)` invoked once before the first text
///   layout op. The wiring of that Skia entry point is pending a
///   profile-validated cutover; until then this path
///   returns `false` so callers surface a clear "ICU bootstrap
///   incomplete" signal instead of crashing inside SkParagraph.
pub(crate) extern "system" fn initIcuData<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    icu_path: JString<'local>,
) -> jni::sys::jboolean {
    jni_safe!("initIcuData", jni::sys::JNI_FALSE, {
        // Fast path: embedded data.  We still read the string so
        // Java doesn't leak the local ref but ignore the value.
        if graphics::EMBEDS_ICU_DATA {
            let _ = env.get_string(&icu_path);
            return jni::sys::JNI_TRUE;
        }
        // External path: placeholder until SkLoadICU is linked.
        // Read the string to consume the arg, then log + return
        // false so the caller knows the bootstrap didn't run.
        let path: String = env
            .get_string(&icu_path)
            .map(|s| s.into())
            .unwrap_or_default();
        tracing::warn!(
            "initIcuData: external ICU mode not yet wired (path={})",
            path
        );
        jni::sys::JNI_FALSE
    })
}

pub(crate) extern "system" fn init(
    mut env: JNIEnv,
    _class: JClass,
    surface: jobject,
    options: JObject<'_>,
) -> jint {
    jni_safe!("init", -1, {
        // A null Surface is a warm start, not a failure: the caller is creating
        // the session before its window exists so that GPU bring-up overlaps the
        // host application's own layout. `updateSurface` delivers the Surface
        // later, down the same path a recreate takes.
        let android_surface = if surface.is_null() {
            info!("init: warm start (no Surface yet); awaiting updateSurface");
            None
        } else {
            // Convert Java Surface to ANativeWindow*.
            let window = unsafe { ANativeWindow_fromSurface(env.get_native_interface(), surface) };
            if window.is_null() {
                error!("init failed: ANativeWindow_fromSurface returned null");
                return -1;
            }

            let (raw_w, raw_h) = unsafe {
                (
                    ANativeWindow_getWidth(window),
                    ANativeWindow_getHeight(window),
                )
            };
            if raw_w <= 0 || raw_h <= 0 {
                error!(
                    "init failed: ANativeWindow_getWidth/Height returned invalid size: {}x{}",
                    raw_w, raw_h
                );
                unsafe { ANativeWindow_release(window) };
                return -1;
            }
            let (w, h) = (raw_w as u32, raw_h as u32);

            // Take ownership of the ANativeWindow ref (acquired by
            // ANativeWindow_fromSurface) via RAII now, so every early return below
            // releases it. Previously each error path between here and host spawn
            // leaked the ref.
            let android_surface =
                match unsafe { AndroidSurfaceWrapper::from_surface_owned(window, w, h) } {
                    Ok(s) => s,
                    Err(e) => {
                        error!("init failed: create AndroidSurfaceWrapper error: {}", e);
                        unsafe { ANativeWindow_release(window) };
                        return -1;
                    }
                };

            // Normalize native window buffer geometry to the observed dimensions.
            // This helps avoid stale rotated geometry during startup transitions.
            let set_geo_rc = unsafe {
                ANativeWindow_setBuffersGeometry(android_surface.native_handle(), raw_w, raw_h, 0)
            };
            if set_geo_rc != 0 {
                tracing::warn!(
                    "init: ANativeWindow_setBuffersGeometry({}x{}) failed: {}",
                    raw_w,
                    raw_h,
                    set_geo_rc
                );
            }
            Some(android_surface)
        };

        // Read required fields from RuntimeConfig
        let cache_dir = match super::get_string_field(&mut env, "cacheDir", &options) {
            Ok(s) => s,
            Err(e) => {
                error!("init failed: read cacheDir error: {}", e);
                return -1;
            }
        };

        let files_dir = match super::get_string_field(&mut env, "filesDir", &options) {
            Ok(s) => s,
            Err(e) => {
                error!("init failed: read filesDir error: {}", e);
                return -1;
            }
        };

        let display_density = match super::get_f32(&mut env, "displayDensity", &options) {
            Ok(v) => v,
            Err(e) => {
                error!("init failed: read displayDensity error: {}", e);
                return -1;
            }
        };

        // Read optional fields with defaults.
        //
        // `or_default` and not `.unwrap_or(...)`: these are Java primitives and
        // an enum reference, which always have a value, so a failed read is a
        // Java/native mismatch and not an unset option. See its own comment for
        // what that difference cost.
        let code_cache_dir = super::get_string_field(&mut env, "codeCacheDir", &options)
            .unwrap_or_else(|_| cache_dir.clone());

        let target_fps = super::or_default(
            super::get_i32(&mut env, "targetFps", &options),
            shared::frame_rate::DEFAULT_FPS as i32,
        );

        let debug_enabled =
            super::or_default(super::get_bool(&mut env, "debugEnabled", &options), false);

        // Three outcomes, and only one of them is the host speaking.
        //
        // Falling back to the process default rather than to a literal `Warn`:
        // the default is `Warn` in a release build and `Debug` in a debug one,
        // and hardcoding the release answer silenced the build whose purpose is
        // to talk. `from_ordinal` rather than `From<i32>` because an ordinal the
        // two enums do not share is a fact worth saying, not a silent `Warn`.
        let log_level = match super::get_enum_ordinal(
            &mut env,
            "logLevel",
            "Lcom/migo/runtime/RuntimeConfig$LogLevel;",
            &options,
        ) {
            Ok(Some(ordinal)) => match shared::config::LogLevel::from_ordinal(ordinal) {
                Some(level) => level,
                None => {
                    let fallback = shared::log_level::default_level();
                    warn!(
                        "RuntimeConfig.logLevel is ordinal {ordinal}, which this library does \
                         not know. The host's SDK declares a level this engine does not; \
                         running at the process default ({fallback:?}) instead."
                    );
                    fallback
                }
            },
            Ok(None) => {
                let fallback = shared::log_level::default_level();
                warn!(
                    "RuntimeConfig.logLevel is null; running at the process default \
                     ({fallback:?}). A host that wants a level must set one."
                );
                fallback
            }
            Err(reason) => {
                let fallback = shared::log_level::default_level();
                warn!(
                    "{reason}. The field is declared on RuntimeConfig, so this is a \
                     Java/native mismatch: whatever level the host asked for is not in \
                     effect and the process default ({fallback:?}) is."
                );
                fallback
            }
        };

        let watchdog_enabled =
            super::or_default(super::get_bool(&mut env, "watchdogEnabled", &options), true);
        let watchdog_timeout_secs = super::or_default(
            super::get_i32(&mut env, "watchdogTimeoutSecs", &options),
            10,
        );
        let code_signing_enabled = super::or_default(
            super::get_bool(&mut env, "codeSigningEnabled", &options),
            true,
        );

        // Read optional code signing public key (hex-encoded Ed25519, 64 chars).
        // Returns None if the field is null, empty, or not present.
        let code_signing_pubkey =
            super::get_optional_string_field(&mut env, "codeSigningPubkey", &options);

        // Read optional game config fields
        let sub_packages = parse_sub_packages(super::get_optional_string_field(
            &mut env,
            "subPackagesJson",
            &options,
        ));
        let workers_path = super::get_optional_string_field(&mut env, "workersPath", &options);

        // Boot prelude scripts (BOM/DOM adapter injection, etc.).
        // Optional — empty list when the host app doesn't configure any.
        let prelude_scripts = parse_prelude_scripts(super::get_optional_string_field(
            &mut env,
            "preludeScriptsJson",
            &options,
        ));

        let init_options = InitOptions::new()
            .with_pixel_ratio(display_density)
            .with_cache_dir(PathBuf::from(cache_dir))
            .with_files_dir(PathBuf::from(files_dir))
            .with_code_cache_dir(PathBuf::from(code_cache_dir))
            .with_target_fps(target_fps)
            .with_debug_enabled(debug_enabled)
            .with_log_level(log_level)
            .with_watchdog_enabled(watchdog_enabled)
            .with_watchdog_timeout_secs(watchdog_timeout_secs)
            .with_code_signing_enabled(code_signing_enabled)
            .with_code_signing_pubkey(code_signing_pubkey)
            .with_sub_packages(sub_packages)
            .with_workers_path(workers_path)
            .with_prelude_scripts(prelude_scripts);

        // The session's level is published by its Host's registration, which has
        // not happened yet: this session does not exist, so it has no level to speak
        // with, and setting one process-wide here is exactly the last-writer-wins
        // that let a new session silence a live one. The line below is emitted at
        // the process default, or at whatever a live session already asked for.
        info!(
            "init: density={}, target_fps={}, debug={}, log_level={:?}",
            display_density,
            target_fps,
            debug_enabled,
            init_options.log_level()
        );

        let platform = Arc::new(AndroidPlatform::new());
        let graphics_platform = match crate::android::presenter::android_graphics_platform() {
            Ok(platform) => platform,
            Err(error) => {
                error!("Android graphics platform initialization failed: {error}");
                return -1;
            }
        };

        // `android_surface` already owns the ANativeWindow ref (wrapped above so
        // early returns release it); hand it to the host.
        let surface_ref: Option<SurfaceRef> =
            android_surface.map(|surface| Arc::new(surface) as SurfaceRef);

        let host = spawn_host_thread(surface_ref, graphics_platform, platform, init_options);
        match host {
            Ok(host) => match host_owners().insert(host) {
                Ok(host_id) => host_id,
                Err(mut host) => {
                    error!(
                        "duplicate Android Host ID {}; refusing ownership",
                        host.id()
                    );
                    if let Err(join_error) = host.shutdown_and_join() {
                        error!("failed to join rejected Android Host: {join_error}");
                    }
                    -1
                }
            },
            Err(e) => {
                error!("Host initialized failed: err={e}");
                -1
            }
        }
    })
}

pub(crate) extern "system" fn updateSurface<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
    host_id: jint,
    surface: JObject<'local>,
    width: jint,
    height: jint,
    density: jfloat,
) {
    jni_safe!("updateSurface", {
        let Some(pixel_ratio) = PixelRatio::new(density) else {
            error!("updateSurface failed: invalid display density {density}");
            return;
        };
        // NOTE: ANativeWindow_fromSurface creates a new ANativeWindow reference.
        let raw_surface = surface.into_raw();
        let window = unsafe { ANativeWindow_fromSurface(env.get_native_interface(), raw_surface) };

        if window.is_null() {
            error!("updateSurface failed: ANativeWindow_fromSurface returned null");
            return;
        }

        let (raw_w, raw_h) = unsafe {
            (
                ANativeWindow_getWidth(window),
                ANativeWindow_getHeight(window),
            )
        };
        if raw_w <= 0 || raw_h <= 0 {
            error!(
                "updateSurface failed: ANativeWindow_getWidth/Height returned invalid size: {}x{}",
                raw_w, raw_h
            );
            // Release the strong ref acquired by ANativeWindow_fromSurface before
            // bailing; only the success path transfers it to AndroidSurfaceWrapper.
            unsafe { ANativeWindow_release(window) };
            return;
        }

        let mut w = raw_w as u32;
        let mut h = raw_h as u32;
        if width > 0 && height > 0 {
            let provided_w = width as u32;
            let provided_h = height as u32;
            if provided_w != w || provided_h != h {
                tracing::warn!(
                    "updateSurface size mismatch: provided={}x{}, native={}x{}; using provided size",
                    provided_w,
                    provided_h,
                    w,
                    h
                );
            }
            w = provided_w;
            h = provided_h;
        }

        let set_geo_rc = unsafe { ANativeWindow_setBuffersGeometry(window, w as i32, h as i32, 0) };
        if set_geo_rc != 0 {
            tracing::warn!(
                "updateSurface: ANativeWindow_setBuffersGeometry({}x{}) failed: {}",
                w,
                h,
                set_geo_rc
            );
        }

        let android_surface =
            match unsafe { AndroidSurfaceWrapper::from_surface_owned(window, w, h) } {
                Ok(s) => s,
                Err(e) => {
                    error!(
                        "updateSurface failed: create AndroidSurfaceWrapper error: {}",
                        e
                    );
                    return;
                }
            };

        let surface_ref: SurfaceRef = Arc::new(android_surface);
        let lease = match lease_surface(host_id, surface_ref) {
            Ok(lease) => lease,
            Err(e) => {
                // `lease_surface` consumes and drops the candidate SurfaceRef on
                // every failure, releasing the ANativeWindow strong reference.
                error!("Failed to lease Surface for host {host_id}: {e}");
                return;
            }
        };

        // The generation gate is the queue-independent liveness authority. The
        // render thread validates the lease before native-handle extraction and
        // commits it only after EGL recreation succeeds.
        if let Err(e) = send_critical_command_to_host(
            host_id,
            HostCommand::UpdateSurface {
                lease,
                pixel_ratio: Some(pixel_ratio),
            },
        ) {
            error!("Failed to send UpdateSurface for host {host_id}: {e}");
        }

        info!("Host {} updated surface: {}x{}", host_id, w, h);
    });
}

pub(crate) extern "system" fn onSurfaceDestroyed(_env: JNIEnv, _class: JClass, host_id: jint) {
    jni_safe!("onSurfaceDestroyed", {
        // Retire the exact generation BEFORE this callback returns (Android
        // abandons the BufferQueue on return). Duplicate destroy callbacks are
        // idempotent and emit no command.
        match retire_surface(host_id) {
            Ok(Some(generation)) => {
                if let Err(e) = send_critical_command_to_host(
                    host_id,
                    HostCommand::SurfaceDestroyed { generation },
                ) {
                    error!("Failed to send SurfaceDestroyed for host {host_id}: {e}");
                }
            }
            Ok(None) => {
                tracing::debug!("Host {host_id} Surface already retired");
            }
            Err(e) => {
                error!("Failed to retire Surface for host {host_id}: {e}");
            }
        }
    });
}

pub(crate) extern "system" fn onOpenSystemBluetoothSetting<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    host_id: jint,
    request_id: jint,
    enabled: jint,
) {
    jni_safe!("onOpenSystemBluetoothSetting", {
        // Absent stays absent: a non-positive id means the request carried
        // none, and writing `0` would make the runtime discard the reply as
        // *present and not an id* -- strictly worse than the fallback it would
        // otherwise take.
        let correlation = if request_id > 0 {
            format!(r#","requestId":{}"#, request_id)
        } else {
            String::new()
        };
        let json = if enabled >= 0 {
            format!(
                r#"{{"errMsg":"openBluetoothAdapterSetting:ok","code":{}{}}}"#,
                enabled, correlation
            )
        } else {
            format!(
                r#"{{"errMsg":"openBluetoothAdapterSetting:fail","code":{}{}}}"#,
                enabled, correlation
            )
        };
        let cmd = HostCommand::InvokeHostHook {
            hook: "_internalOnOpenBluetoothSettingResult",
            args_json: hook_args_one(json.as_str()),
        };
        let _ = send_reliable_command_to_host(host_id, cmd);
    });
}

pub(crate) extern "system" fn onOpenAppAuthorizeSetting<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    host_id: jint,
    request_id: jint,
    code: jint,
) {
    jni_safe!("onOpenAppAuthorizeSetting", {
        let cmd = HostCommand::InvokeHostHook {
            hook: "_internalOnOpenAppAuthorizeSettingFinished",
            args_json: hook_args_two(request_id, code),
        };
        let _ = send_reliable_command_to_host(host_id, cmd);
    });
}

pub(crate) extern "system" fn onTouch(
    env: JNIEnv,
    _cls: JClass,
    host_id: jint,
    action: jint,
    time: jlong,
    count: jint,
    buffer: JObject,
) -> jboolean {
    jni_safe!("onTouch", JNI_FALSE, {
        if count <= 0 || count > 10 {
            return JNI_FALSE;
        }

        let buf = JByteBuffer::from(buffer);

        let addr = match env.get_direct_buffer_address(&buf) {
            Ok(p) => p,
            Err(e) => {
                error!("onTouch failed: get_direct_buffer_address error: {:?}", e);
                return JNI_FALSE;
            }
        };

        let n = count as usize;
        let expected_size = n * std::mem::size_of::<TouchPoint>();

        // Validate buffer capacity before reading
        let capacity = match env.get_direct_buffer_capacity(&buf) {
            Ok(cap) => cap,
            Err(e) => {
                error!("onTouch failed: get_direct_buffer_capacity error: {:?}", e);
                return JNI_FALSE;
            }
        };

        if expected_size > capacity {
            error!(
                "onTouch failed: buffer underflow - expected {} bytes, got {} bytes",
                expected_size, capacity
            );
            return JNI_FALSE;
        }

        // Single memcpy from DirectByteBuffer into fixed inline array — no heap allocation.
        // SAFETY: addr is valid (from get_direct_buffer_address), capacity verified,
        // TouchPoint is repr(C) matching the Java-side packing.
        let mut points = [TouchPoint::default(); 10];
        unsafe {
            std::ptr::copy_nonoverlapping(addr as *const TouchPoint, points.as_mut_ptr(), n);
        }

        let touch_type = match action {
            0 | 5 => TouchType::Start,
            1 | 6 => TouchType::End,
            2 => TouchType::Move,
            3 => TouchType::Cancel,
            _ => {
                tracing::warn!("onTouch rejected unsupported action {action}");
                return JNI_FALSE;
            }
        };

        let touch = TouchData {
            touch_type,
            count: n as u8,
            points,
            timestamp_ms: time as i64,
        };

        let result = with_hot_ingress(host_id, |ingress| {
            let result = ingress.try_send_touch(touch);
            let notify = matches!(result, Err(HostIngressSendError::Full))
                && ingress.claim_input_saturation_notification();
            (result, notify)
        });
        match result {
            Some((Ok(_), _)) => JNI_TRUE,
            Some((Err(HostIngressSendError::Full), notify)) => {
                tracing::debug!("Host {host_id} touch ingress is saturated");
                if notify {
                    if let Err(error) = crate::android::jni::notify_error(
                        host_id,
                        ErrorCode::InputSaturated.as_u16(),
                        ErrorCode::InputSaturated.default_message(),
                        "bounded touch transport refused an event; reduce host sampling rate",
                    ) {
                        tracing::debug!(
                            "Host {host_id} input saturation notification failed: {error}"
                        );
                    }
                }
                JNI_FALSE
            }
            Some((Err(HostIngressSendError::Closed), _)) | None => {
                invalidate_hot_ingress(host_id);
                JNI_FALSE
            }
        }
    })
}

pub(crate) extern "system" fn executeScript<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    host_id: jint,
    script: JString<'local>,
) -> jint {
    jni_safe!("executeScript", -1, {
        let script_str: String = match env.get_string(&script) {
            Ok(s) => s.into(),
            Err(e) => {
                error!("executeScript failed: convert JString error: {:?}", e);
                return -1;
            }
        };

        match send_command_to_host(host_id, HostCommand::EvalScript { source: script_str }) {
            Ok(_) => 0,
            Err(e) => {
                error!("executeScript failed: {}", e);
                -1
            }
        }
    })
}

pub(crate) extern "system" fn mod_main<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    host_id: jint,
    game_id: JString<'local>,
    entry: JString<'local>,
) -> jint {
    jni_safe!("mod_main", -1, {
        let game_id: String = match env.get_string(&game_id) {
            Ok(s) => s.into(),
            Err(e) => {
                error!("modMain failed: convert game_id JString error: {:?}", e);
                return -1;
            }
        };

        let entry: String = match env.get_string(&entry) {
            Ok(s) => s.into(),
            Err(e) => {
                error!("modMain failed: convert entry JString error: {:?}", e);
                return -1;
            }
        };

        info!("modMain: game_id={}, entry={}", game_id, entry);

        match send_command_to_host(host_id, HostCommand::EvaluateModule { game_id, entry }) {
            Ok(_) => 0,
            Err(e) => {
                error!("modMain failed: {}", e);
                -1
            }
        }
    })
}

pub(crate) extern "system" fn shutdown<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    host_id: jint,
) -> jboolean {
    jni_safe!("shutdown", JNI_FALSE, {
        invalidate_hot_ingress(host_id);
        match host_owners().shutdown_with(host_id, |host| host.shutdown_and_join()) {
            Ok(had_owner) => {
                crate::android::services::clear_permissions(host_id);
                if had_owner {
                    info!("Host {} shut down and joined successfully", host_id);
                } else {
                    info!("Host {} is already shut down", host_id);
                }
                JNI_TRUE
            }
            Err(error) => {
                error!(
                    "shutdown failed and remains retryable: host_id={}, error={}",
                    host_id, error
                );
                JNI_FALSE
            }
        }
    })
}

pub(crate) extern "system" fn onShow<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    host_id: jint,
    options_json: JString<'local>,
) {
    jni_safe!("onShow", {
        let options_json: Option<String> = if options_json.is_null() {
            None
        } else {
            env.get_string(&options_json).ok().map(|s| s.into())
        };

        if let Some(json) = options_json.as_ref() {
            info!(
                "Host {} onShow received (options_json_bytes={})",
                host_id,
                json.len()
            );
        } else {
            info!("Host {} onShow received (options_json=<none>)", host_id);
        }

        if let Err(e) = send_critical_command_to_host(host_id, HostCommand::OnShow { options_json })
        {
            error!("Failed to send OnShow for host {host_id}: {e}");
        }
    });
}

pub(crate) extern "system" fn onHide(_env: JNIEnv, _class: JClass, host_id: jint) {
    jni_safe!("onHide", {
        info!("Host {} onHide received", host_id);
        if let Err(e) = send_critical_command_to_host(host_id, HostCommand::OnHide) {
            error!("Failed to send OnHide for host {host_id}: {e}");
        }
    });
}

pub(crate) extern "system" fn onRestart(_env: JNIEnv, _class: JClass, host_id: jint) {
    jni_safe!("onRestart", {
        if let Err(e) = send_command_to_host(host_id, HostCommand::Restart) {
            error!("Failed to send Restart for host {host_id}: {e}");
        }
    });
}

pub(crate) extern "system" fn onAudioInterruptionBegin(
    _env: JNIEnv,
    _class: JClass,
    host_id: jint,
) {
    jni_safe!("onAudioInterruptionBegin", {
        let _ = send_command_to_host(host_id, HostCommand::OnAudioInterruptionBegin);
    });
}

pub(crate) extern "system" fn onAudioInterruptionEnd(_env: JNIEnv, _class: JClass, host_id: jint) {
    jni_safe!("onAudioInterruptionEnd", {
        let _ = send_command_to_host(host_id, HostCommand::OnAudioInterruptionEnd);
    });
}

pub(crate) extern "system" fn onUserCaptureScreen(
    _env: JNIEnv,
    _class: JClass,
    host_id: jint,
    generation: jlong,
) {
    jni_safe!("onUserCaptureScreen", {
        let _ = send_command_to_host(
            host_id,
            HostCommand::OnUserCaptureScreen {
                runtime_generation: captured_generation(generation),
            },
        );
    });
}

pub(crate) extern "system" fn onModalResult<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    host_id: jint,
    request_id: jint,
    confirm: jint,
    cancel: jint,
) {
    jni_safe!("onModalResult", {
        let cmd = HostCommand::InvokeHostHook {
            hook: "_internalOnModalResult",
            args_json: hook_args_three(request_id, confirm, cancel),
        };
        let _ = send_reliable_command_to_host(host_id, cmd);
    });
}

pub(crate) extern "system" fn onActionSheetResult<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    host_id: jint,
    request_id: jint,
    tap_index: jint,
) {
    jni_safe!("onActionSheetResult", {
        let cmd = HostCommand::InvokeHostHook {
            hook: "_internalOnActionSheetResult",
            args_json: hook_args_two(request_id, tap_index),
        };
        let _ = send_reliable_command_to_host(host_id, cmd);
    });
}

// ==================== Device Sensor ====================

pub(crate) extern "system" fn onDeviceMotionChange(
    _env: JNIEnv,
    _class: JClass,
    host_id: jint,
    generation: jlong,
    alpha: jdouble,
    beta: jdouble,
    gamma: jdouble,
) {
    jni_safe!("onDeviceMotionChange", {
        let _ = send_command_to_host(
            host_id,
            HostCommand::OnDeviceMotionChange {
                alpha: alpha as f64,
                beta: beta as f64,
                gamma: gamma as f64,
                runtime_generation: captured_generation(generation),
            },
        );
    });
}

pub(crate) extern "system" fn onGyroscopeChange(
    _env: JNIEnv,
    _class: JClass,
    host_id: jint,
    generation: jlong,
    x: jdouble,
    y: jdouble,
    z: jdouble,
) {
    jni_safe!("onGyroscopeChange", {
        let _ = send_command_to_host(
            host_id,
            HostCommand::OnGyroscopeChange {
                x: x as f64,
                y: y as f64,
                z: z as f64,
                runtime_generation: captured_generation(generation),
            },
        );
    });
}

pub(crate) extern "system" fn onDeviceOrientationChange<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    host_id: jint,
    value: JString<'local>,
) {
    jni_safe!("onDeviceOrientationChange", {
        static KNOWN: &[&str] = &["portrait", "landscape", "landscapeReverse"];
        let val = jni_string_to_cow(&mut env, &value, KNOWN);
        let _ = send_command_to_host(
            host_id,
            HostCommand::OnDeviceOrientationChange { value: val },
        );
    });
}

pub(crate) extern "system" fn onCompassChange<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    host_id: jint,
    generation: jlong,
    direction: jdouble,
    accuracy: JString<'local>,
) {
    jni_safe!("onCompassChange", {
        static KNOWN: &[&str] = &["high", "medium", "low", "no-contact", "unreliable"];
        let acc = jni_string_to_cow(&mut env, &accuracy, KNOWN);
        let _ = send_command_to_host(
            host_id,
            HostCommand::OnCompassChange {
                direction: direction as f64,
                accuracy: acc,
                runtime_generation: captured_generation(generation),
            },
        );
    });
}

pub(crate) extern "system" fn onAccelerometerChange(
    _env: JNIEnv,
    _class: JClass,
    host_id: jint,
    generation: jlong,
    x: jdouble,
    y: jdouble,
    z: jdouble,
) {
    jni_safe!("onAccelerometerChange", {
        let _ = send_command_to_host(
            host_id,
            HostCommand::OnAccelerometerChange {
                x: x as f64,
                y: y as f64,
                z: z as f64,
                runtime_generation: captured_generation(generation),
            },
        );
    });
}

pub(crate) extern "system" fn onNetworkStatusChange<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    host_id: jint,
    is_connected: jni::sys::jboolean,
    network_type: JString<'local>,
) {
    jni_safe!("onNetworkStatusChange", {
        static KNOWN: &[&str] = &["wifi", "2g", "3g", "4g", "5g", "unknown", "none"];
        let net = jni_string_to_cow(&mut env, &network_type, KNOWN);
        let _ = send_command_to_host(
            host_id,
            HostCommand::OnNetworkStatusChange {
                is_connected: is_connected != 0,
                network_type: net,
            },
        );
    });
}

// ==================== Recorder Events ====================

pub(crate) extern "system" fn onRecorderEvent<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    host_id: jint,
    generation: jlong,
    event_type: JString<'local>,
    json_payload: JString<'local>,
) {
    jni_safe!("onRecorderEvent", {
        let evt: String = env
            .get_string(&event_type)
            .map(|s| s.into())
            .unwrap_or_default();
        let payload: String = env
            .get_string(&json_payload)
            .map(|s| s.into())
            .unwrap_or_else(|_| "{}".to_string());
        // Recorder lifecycle events are must-deliver: a full data queue may
        // drop a PCM frame, but it must never swallow stop/error state.
        let _ = send_reliable_command_to_host(
            host_id,
            HostCommand::RecorderEvent {
                event_type: evt,
                json_payload: payload,
                runtime_generation: captured_generation(generation),
            },
        );
    });
}

pub(crate) extern "system" fn onRecorderFrameData(
    mut env: JNIEnv,
    _class: JClass,
    host_id: jint,
    generation: jlong,
    frame_data: jni::sys::jbyteArray,
    frame_length: jint,
    is_last_frame: jni::sys::jboolean,
) {
    jni_safe!("onRecorderFrameData", {
        let frame_data = unsafe { jni::objects::JByteArray::from_raw(frame_data) };
        let array_len = match env.get_array_length(&frame_data) {
            Ok(length) => usize::try_from(length).unwrap_or(0),
            Err(error) => {
                error!(
                    "onRecorderFrameData: failed to inspect byte array: {:?}",
                    error
                );
                return;
            }
        };
        let frame_length = match usize::try_from(frame_length) {
            Ok(length) if length <= array_len => length,
            _ => {
                error!("onRecorderFrameData: invalid frame length {}", frame_length);
                return;
            }
        };
        let is_last_frame = is_last_frame != 0;
        let Some((credit, copy_result)) = with_recorder_frame_credit(
            host_id,
            if is_last_frame { 0 } else { frame_length },
            || {
                let mut raw = vec![0i8; frame_length];
                env.get_byte_array_region(&frame_data, 0, &mut raw)
                    .map(|()| raw)
            },
        ) else {
            tracing::debug!(
                "onRecorderFrameData: byte budget full, dropped {} bytes",
                frame_length
            );
            return;
        };
        let mut raw = match copy_result {
            Ok(raw) => raw,
            Err(error) => {
                error!(
                    "onRecorderFrameData: failed to read byte array: {:?}",
                    error
                );
                drop(credit);
                return;
            }
        };
        let ptr = raw.as_mut_ptr().cast::<u8>();
        let len = raw.len();
        let capacity = raw.capacity();
        std::mem::forget(raw);
        let data = unsafe { Vec::from_raw_parts(ptr, len, capacity) };
        let command = HostCommand::RecorderFrameData {
            data,
            is_last_frame,
            credit,
            runtime_generation: captured_generation(generation),
        };
        if is_last_frame {
            let _ = send_reliable_command_to_host(host_id, command);
        } else {
            let _ = send_command_to_host(host_id, command);
        }
    });
}

// ==================== Camera ====================

pub(crate) extern "system" fn onCameraEvent<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    host_id: jint,
    generation: jlong,
    camera_id: jint,
    event_type: JString<'local>,
    json_payload: JString<'local>,
) {
    jni_safe!("onCameraEvent", {
        let evt: String = env
            .get_string(&event_type)
            .map(|s| s.into())
            .unwrap_or_default();
        let payload: String = env
            .get_string(&json_payload)
            .map(|s| s.into())
            .unwrap_or_else(|_| "{}".to_string());
        let _ = send_command_to_host(
            host_id,
            HostCommand::CameraEvent {
                camera_id: camera_id as u32,
                event_type: evt,
                json_payload: payload,
                runtime_generation: captured_generation(generation),
            },
        );
    });
}

pub(crate) extern "system" fn onCameraFrameData<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
    host_id: jint,
    generation: jlong,
    camera_id: jint,
    y_buf: JByteBuffer<'local>,
    y_off: jint,
    y_len: jint,
    u_buf: JByteBuffer<'local>,
    u_off: jint,
    u_len: jint,
    v_buf: JByteBuffer<'local>,
    v_off: jint,
    v_len: jint,
    width: jint,
    height: jint,
) {
    jni_safe!("onCameraFrameData", {
        // Reject oversized dimensions and byte windows before converting signed
        // values or borrowing a Java direct-buffer address. `pack_yuv_planes`
        // repeats the byte cap after capacity-window validation, so both JNI
        // ingress and the shared copy boundary are independently bounded.
        let (width, height) = match validate_camera_frame_dimensions(width, height) {
            Ok(dimensions) => dimensions,
            Err(error) => {
                tracing::warn!("onCameraFrameData: invalid dimensions: {:?}", error);
                return;
            }
        };
        if let Err(error) = validate_camera_frame_payload_lengths([y_len, u_len, v_len]) {
            tracing::warn!("onCameraFrameData: invalid payload lengths: {:?}", error);
            return;
        }
        let Some((admission, packed)) =
            with_camera_frame_admission(host_id, camera_id as u32, || {
                // Resolve each direct plane buffer's base address + capacity. The jni
                // wrapper rejects null / non-direct buffers and a -1 capacity, so a
                // malformed buffer is dropped rather than mis-read.
                let resolve = |buf: &JByteBuffer, plane: &str| -> Option<(*mut u8, usize)> {
                    match (
                        env.get_direct_buffer_address(buf),
                        env.get_direct_buffer_capacity(buf),
                    ) {
                        (Ok(addr), Ok(cap)) => Some((addr, cap)),
                        _ => {
                            tracing::warn!(
                                "onCameraFrameData: {} plane buffer not direct/usable",
                                plane
                            );
                            None
                        }
                    }
                };
                let (Some((y_addr, y_cap)), Some((u_addr, u_cap)), Some((v_addr, v_cap))) = (
                    resolve(&y_buf, "Y"),
                    resolve(&u_buf, "U"),
                    resolve(&v_buf, "V"),
                ) else {
                    return None;
                };

                // SAFETY: each (addr, cap) comes from a live, direct ByteBuffer whose
                // backing Image is held open by the synchronous Java caller for the
                // duration of this call. `u8` has alignment 1 and `addr` is non-null
                // (the jni wrapper rejects null). These capacity slices are used only
                // to pack into an owned `Vec` below; no slice, raw address, or
                // `JByteBuffer` escapes this call or crosses the host channel.
                let y_slice = unsafe { std::slice::from_raw_parts(y_addr as *const u8, y_cap) };
                let u_slice = unsafe { std::slice::from_raw_parts(u_addr as *const u8, u_cap) };
                let v_slice = unsafe { std::slice::from_raw_parts(v_addr as *const u8, v_cap) };

                // The single copy: validate each `[offset, offset+len)` window against
                // its capacity and concatenate Y/U/V into one owned Vec.
                Some(
                    match pack_yuv_planes([
                        PlaneWindow {
                            buffer: y_slice,
                            offset: y_off,
                            len: y_len,
                        },
                        PlaneWindow {
                            buffer: u_slice,
                            offset: u_off,
                            len: u_len,
                        },
                        PlaneWindow {
                            buffer: v_slice,
                            offset: v_off,
                            len: v_len,
                        },
                    ]) {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::warn!("onCameraFrameData: invalid plane window: {:?}", e);
                            return None;
                        }
                    },
                )
            })
        else {
            return;
        };

        match publish_camera_frame(
            admission,
            CameraFrameEntry {
                data: packed,
                width,
                height,
            },
        ) {
            CameraFramePush::Notify(credit) => {
                let result = send_command_to_host(
                    host_id,
                    HostCommand::CameraFrameData {
                        camera_id: camera_id as u32,
                        data: Vec::new(),
                        width: 0,
                        height: 0,
                        credit,
                        runtime_generation: captured_generation(generation),
                    },
                );
                if result.is_err() {
                    let _ = take_camera_frame(host_id, camera_id as u32);
                }
            }
            CameraFramePush::Superseded => {}
        }
    });
}

// ==================== Bluetooth Callbacks ====================

pub(crate) extern "system" fn onBluetoothAdapterStateChange(
    _env: JNIEnv,
    _class: JClass,
    host_id: jint,
    available: jni::sys::jboolean,
    discovering: jni::sys::jboolean,
) {
    jni_safe!("onBluetoothAdapterStateChange", {
        let _ = send_command_to_host(
            host_id,
            HostCommand::OnBluetoothAdapterStateChange {
                available: available != 0,
                discovering: discovering != 0,
            },
        );
    });
}

pub(crate) extern "system" fn onBluetoothDeviceFound<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    host_id: jint,
    devices_json: JString<'local>,
) {
    jni_safe!("onBluetoothDeviceFound", {
        let json: String = env
            .get_string(&devices_json)
            .map(|s| s.into())
            .unwrap_or_else(|_| "[]".to_string());
        let _ = send_command_to_host(
            host_id,
            HostCommand::OnBluetoothDeviceFound { devices_json: json },
        );
    });
}

pub(crate) extern "system" fn onBeaconUpdate<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    host_id: jint,
    beacons_json: JString<'local>,
) {
    jni_safe!("onBeaconUpdate", {
        let json: String = env
            .get_string(&beacons_json)
            .map(|s| s.into())
            .unwrap_or_else(|_| "[]".to_string());
        let _ = send_command_to_host(host_id, HostCommand::OnBeaconUpdate { beacons_json: json });
    });
}

/// Record the host's decision for one scope.
///
/// Written into this side's cache rather than routed to JS: `require_scope`
/// runs on every gated op and reads it there, and JS holds no permission state
/// at all -- a permission answer content can reach is a permission answer
/// content can change.
pub(crate) extern "system" fn updatePermission<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    host_id: jint,
    scope: JString<'local>,
    granted: jni::sys::jboolean,
) -> jboolean {
    jni_safe!("updatePermission", JNI_FALSE, {
        if host_ingress(host_id).is_err() {
            tracing::debug!(host_id, "ignoring permission update for ended session");
            return JNI_FALSE;
        }
        let scope: String = match env.get_string(&scope) {
            Ok(s) => s.into(),
            Err(_) => return JNI_FALSE,
        };
        let Some(scope) = shared::services::Scope::from_minigame_str(&scope) else {
            tracing::warn!(host_id, "ignoring unknown permission scope");
            return JNI_FALSE;
        };
        match crate::android::services::update_permission(host_id, scope, granted != 0, || {
            crate::android::jni::permission_revoke_resources(host_id, scope.as_minigame_str())
        }) {
            Ok(()) => JNI_TRUE,
            Err(crate::android_permission_gate::UpdateError::Closed) => {
                tracing::debug!(host_id, "ignoring permission update for closing session");
                JNI_FALSE
            }
            Err(crate::android_permission_gate::UpdateError::Cleanup(error)) => {
                tracing::error!(
                    host_id,
                    scope = scope.as_minigame_str(),
                    %error,
                    "permission revocation cleanup failed"
                );
                if let Err(notify_error) = crate::android::jni::notify_error(
                    host_id,
                    ErrorCode::Internal.as_u16(),
                    "permission revocation cleanup failed",
                    &error,
                ) {
                    tracing::error!(host_id, %notify_error, "cleanup failure callback failed");
                }
                JNI_FALSE
            }
        }
    })
}

pub(crate) extern "system" fn onBeaconServiceChange(
    _env: JNIEnv,
    _class: JClass,
    host_id: jint,
    available: jni::sys::jboolean,
    discovering: jni::sys::jboolean,
) {
    jni_safe!("onBeaconServiceChange", {
        let _ = send_command_to_host(
            host_id,
            HostCommand::OnBeaconServiceChange {
                available: available != 0,
                discovering: discovering != 0,
            },
        );
    });
}

// ==================== BLE GATT Callbacks ====================

pub(crate) extern "system" fn onBLEConnectionStateChange<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    host_id: jint,
    device_id: JString<'local>,
    connected: jni::sys::jboolean,
) {
    jni_safe!("onBLEConnectionStateChange", {
        let dev: String = env
            .get_string(&device_id)
            .map(|s| s.into())
            .unwrap_or_default();
        let _ = send_command_to_host(
            host_id,
            HostCommand::OnBLEConnectionStateChange {
                device_id: dev,
                connected: connected != 0,
            },
        );
    });
}

/// Bytes of characteristic value read without touching the heap.
///
/// The ATT specification caps an attribute value at 512 bytes, and a
/// notification is further capped by the negotiated MTU, so this covers every
/// value a conforming peripheral can send. Larger ones are still delivered
/// correctly — [`read_characteristic_value`] falls back to a heap buffer rather
/// than truncating, because a silently shortened value is a payload the content
/// would misread.
const BLE_INLINE_VALUE_BYTES: usize = 512;

/// Copy a JVM byte array into `inline`, spilling to the heap only if it is
/// larger than any conforming notification.
///
/// `Err` means the JVM raised — a pending exception must reach Java rather than
/// be papered over with an empty value.
fn read_characteristic_value<'a>(
    env: &JNIEnv<'_>,
    value: &jni::objects::JByteArray<'_>,
    inline: &'a mut [i8; BLE_INLINE_VALUE_BYTES],
    spill: &'a mut Vec<i8>,
) -> Result<&'a [u8], jni::errors::Error> {
    let len = env.get_array_length(value)?.max(0) as usize;
    let buffer: &mut [i8] = if len <= BLE_INLINE_VALUE_BYTES {
        &mut inline[..len]
    } else {
        spill.resize(len, 0);
        spill.as_mut_slice()
    };
    env.get_byte_array_region(value, 0, buffer)?;
    // SAFETY: `i8` and `u8` have the same size, alignment and validity, so a
    // shared reinterpretation of an initialised `[i8]` is sound. JNI types the
    // region as `jbyte`; every consumer above this boundary wants bytes.
    Ok(unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast::<u8>(), buffer.len()) })
}

/// Read a Java string without the class check `get_string` performs.
///
/// # Safety
/// `obj` must be a `java.lang.String`. Every caller here is a parameter of a
/// `native` method declared with a `String` parameter in `NativeBridge`, so the
/// JVM has already type-checked it at the call site; the check `get_string`
/// repeats is a `FindClass` plus an assignability test, three times per
/// notification, on a path a peripheral drives at its own rate.
unsafe fn borrow_java_string<'obj_ref, 'local: 'obj_ref>(
    env: &JNIEnv<'local>,
    obj: &'obj_ref JString<'local>,
) -> Option<jni::strings::JavaStr<'local, 'local, 'obj_ref>> {
    unsafe { env.get_string_unchecked(obj) }.ok()
}

pub(crate) extern "system" fn onBLECharacteristicValueChange<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
    host_id: jint,
    device_id: JString<'local>,
    service_id: JString<'local>,
    characteristic_id: JString<'local>,
    value: jni::objects::JByteArray<'local>,
) {
    jni_safe!("onBLECharacteristicValueChange", {
        // Section 7.3: a peripheral chooses this path's rate, so nothing here
        // may reach the heap or a lock shared beyond this Session. The three
        // identifiers stay borrowed from the JVM, the value lands in a stack
        // buffer, and the Session's own recycled slot is the only destination.
        // `HostIngress::try_send_ble_characteristic_value` is the measured half;
        // see its gates in `core::runtime::registry`.
        let mut inline = [0i8; BLE_INLINE_VALUE_BYTES];
        let mut spill: Vec<i8> = Vec::new();
        let Ok(bytes) = read_characteristic_value(&env, &value, &mut inline, &mut spill) else {
            // Cleared, not propagated. The only way to get here is a JVM
            // exception, and letting one stay pending as this returns would
            // deliver it to whichever framework thread runs the GATT callback --
            // an app-visible crash raised from inside the Bluetooth stack, for a
            // notification that could simply be dropped. The bounds come from the
            // array itself, so nothing is expected to raise here at all.
            let _ = env.exception_clear();
            tracing::debug!("Host {host_id} dropped a BLE notification: value unreadable");
            return;
        };

        // SAFETY: all three are `String` parameters of a `native` method.
        let device = unsafe { borrow_java_string(&env, &device_id) };
        let service = unsafe { borrow_java_string(&env, &service_id) };
        let characteristic = unsafe { borrow_java_string(&env, &characteristic_id) };
        // Borrowed for ASCII, which every UUID and device address is; the owned
        // arm exists so a non-conforming identifier is delivered rather than
        // dropped, and it is the only allocation this path can make.
        let device = device.as_deref().map(Cow::from).unwrap_or_default();
        let service = service.as_deref().map(Cow::from).unwrap_or_default();
        let characteristic = characteristic.as_deref().map(Cow::from).unwrap_or_default();

        let _ = with_hot_ingress(host_id, |ingress| {
            ingress.try_send_ble_characteristic_value(&device, &service, &characteristic, bytes)
        });
    });
}

pub(crate) extern "system" fn onBLEMTUChange<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    host_id: jint,
    device_id: JString<'local>,
    mtu: jint,
) {
    jni_safe!("onBLEMTUChange", {
        let dev: String = env
            .get_string(&device_id)
            .map(|s| s.into())
            .unwrap_or_default();
        let _ = send_command_to_host(
            host_id,
            HostCommand::OnBLEMTUChange {
                device_id: dev,
                mtu: mtu as u32,
            },
        );
    });
}

// ==================== Keyboard Callbacks ====================

pub(crate) extern "system" fn onKeyboardInput<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    host_id: jint,
    generation: jlong,
    value: JString<'local>,
) {
    jni_safe!("onKeyboardInput", {
        let val: String = env.get_string(&value).map(|s| s.into()).unwrap_or_default();
        let _ = send_command_to_host(
            host_id,
            HostCommand::OnKeyboardInput {
                value: val,
                runtime_generation: captured_generation(generation),
            },
        );
    });
}

pub(crate) extern "system" fn onKeyboardConfirm<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    host_id: jint,
    generation: jlong,
    value: JString<'local>,
) {
    jni_safe!("onKeyboardConfirm", {
        let val: String = env.get_string(&value).map(|s| s.into()).unwrap_or_default();
        let _ = send_command_to_host(
            host_id,
            HostCommand::OnKeyboardConfirm {
                value: val,
                runtime_generation: captured_generation(generation),
            },
        );
    });
}

pub(crate) extern "system" fn onKeyboardComplete<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    host_id: jint,
    generation: jlong,
    value: JString<'local>,
) {
    jni_safe!("onKeyboardComplete", {
        let val: String = env.get_string(&value).map(|s| s.into()).unwrap_or_default();
        let _ = send_command_to_host(
            host_id,
            HostCommand::OnKeyboardComplete {
                value: val,
                runtime_generation: captured_generation(generation),
            },
        );
    });
}

pub(crate) extern "system" fn onKeyboardHeightChange(
    _env: JNIEnv,
    _class: JClass,
    host_id: jint,
    generation: jlong,
    height: jdouble,
) {
    jni_safe!("onKeyboardHeightChange", {
        let _ = send_command_to_host(
            host_id,
            HostCommand::OnKeyboardHeightChange {
                height: height as f64,
                runtime_generation: captured_generation(generation),
            },
        );
    });
}

// ==================== Memory Warning Callbacks ====================

pub(crate) extern "system" fn onMemoryWarning(
    _env: JNIEnv,
    _class: JClass,
    host_id: jint,
    level: jint,
) {
    jni_safe!("onMemoryWarning", {
        let _ = send_command_to_host(
            host_id,
            HostCommand::OnMemoryWarning {
                level: level as i32,
            },
        );
    });
}

// ==================== ADPF Thermal Callbacks ====================

pub(crate) extern "system" fn onThermalStatusChanged(
    _env: JNIEnv,
    _class: JClass,
    host_id: jint,
    status: jint,
) {
    jni_safe!("onThermalStatusChanged", {
        let _ = send_command_to_host(
            host_id,
            HostCommand::OnThermalStatusChanged {
                status: status as i32,
            },
        );
    });
}

// ==================== Image API Callbacks ====================

jni_json_callback!(onCompressImageResult, "_internalOnCompressImageResult");
jni_json_callback!(onChooseImageResult, "_internalOnChooseImageResult");
jni_json_callback!(
    onChooseMessageFileResult,
    "_internalOnChooseMessageFileResult"
);

// ==================== Location Callbacks ====================

jni_json_callback!(onLocationResult, "_internalOnLocationResult");
jni_json_callback!(onFuzzyLocationResult, "_internalOnFuzzyLocationResult");

// ==================== Scan Code Callbacks ====================

jni_json_callback!(onScanCodeResult, "_internalOnScanCodeResult");

// ==================== Auth Callbacks ====================

// Every ad event -- load, error, close (with the reward verdict), resize,
// hide -- arrives on this single channel and is routed to an ad object by the
// `adId` inside the payload.
jni_json_callback!(onAdEvent, "_internalOnAdEvent");

jni_json_callback!(onAuthorizeResult, "_internalOnAuthorizeResult");

jni_json_callback!(onLoginResult, "_internalOnLoginResult");
jni_json_callback!(onCheckSessionResult, "_internalOnCheckSessionResult");
jni_json_callback!(onGetUserInfoResult, "_internalOnGetUserInfoResult");
jni_json_callback!(onGetPhoneNumberResult, "_internalOnGetPhoneNumberResult");

// ==================== Subpackage Callbacks ====================

jni_json_callback!(
    onSubpackageProgress,
    "_internalOnSubpackageProgress",
    r#"{"requestId":0}"#
);

/// The one JSON callback that does not forward its payload verbatim: the host
/// reports where it downloaded the subpackage, and that path is kept in the
/// runtime rather than handed to the game. `op_install_subpackage` takes it back
/// by request id, so a game cannot name a file for the installer to ingest.
pub(crate) extern "system" fn onSubpackageResult<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    host_id: jint,
    result_json: JString<'local>,
) {
    jni_safe!("onSubpackageResult", {
        let json = read_json_result(
            &mut env,
            &result_json,
            r#"{"requestId":0,"error":"failed to read result"}"#,
        );
        let for_js = shared::services::intercept_download_result(host_id, &json);
        send_json_result_to_js(host_id, &for_js, "_internalOnSubpackageResult");
    });
}

// ==================== VSync (Choreographer) ====================

pub(crate) extern "system" fn onVsync(
    _env: JNIEnv,
    _class: JClass,
    host_id: jint,
    frame_time_nanos: jlong,
) {
    jni_safe!("onVsync", {
        let frame_time_ms = frame_time_nanos as f64 / 1_000_000.0;
        match with_hot_ingress(host_id, |ingress| ingress.try_send_vsync(frame_time_ms)) {
            Some(Ok(()) | Err(HostIngressSendError::Full)) => {}
            Some(Err(HostIngressSendError::Closed)) | None => {
                invalidate_hot_ingress(host_id);
            }
        }
    });
}

// ==================== Debug Stats ====================

pub(crate) extern "system" fn getDebugStats(
    env: JNIEnv,
    _class: JClass,
    host_id: jint,
) -> jni::sys::jbyteArray {
    jni_safe!("getDebugStats", std::ptr::null_mut(), {
        let stats = match shared::stats::get_stats(host_id) {
            Some(s) => s,
            None => return std::ptr::null_mut(),
        };

        let buf = stats.snapshot();

        match env.byte_array_from_slice(&buf) {
            Ok(arr) => arr.into_raw(),
            Err(_) => std::ptr::null_mut(),
        }
    })
}

// ==================== Setting (Mode C) ====================

jni_json_callback!(onOpenSettingResult, "_internalOnOpenSettingResult");

// ==================== Share (Mode C) ====================

jni_json_callback!(onShareAppMessageResult, "_internalOnShareAppMessageResult");

// ==================== Navigate (Mode C) ====================

jni_json_callback!(
    onNavigateToMiniProgramResult,
    "_internalOnNavigateToMiniProgramResult"
);

// ==================== Payment (Mode C) ====================

jni_json_callback!(onMidasPaymentResult, "_internalOnMidasPaymentResult");
jni_json_callback!(
    onMidasPaymentGameItemResult,
    "_internalOnMidasPaymentGameItemResult"
);

// ==================== Video Callbacks ====================

pub(crate) extern "system" fn onVideoEvent<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    host_id: jint,
    generation: jlong,
    video_id: jint,
    event_type: JString<'local>,
    data_json: JString<'local>,
) {
    jni_safe!("onVideoEvent", {
        let evt: String = env
            .get_string(&event_type)
            .map(|s| s.into())
            .unwrap_or_default();
        let data: String = env
            .get_string(&data_json)
            .map(|s| s.into())
            .unwrap_or_else(|_| "{}".to_string());
        let _ = send_command_to_host(
            host_id,
            HostCommand::OnVideoStateChange {
                video_id: video_id as u32,
                event_type: evt,
                data,
                runtime_generation: captured_generation(generation),
            },
        );
    });
}

// ==================== Console Logs ====================

pub(crate) extern "system" fn getConsoleLogs<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
    host_id: jint,
    since_cursor: jlong,
) -> jstring {
    jni_safe!("getConsoleLogs", std::ptr::null_mut(), {
        let json = shared::console_log::read_console_logs_json(host_id, since_cursor as u64)
            .unwrap_or_else(|| r#"{"logs":[],"cursor":0}"#.to_string());
        match env.new_string(&json) {
            Ok(s) => s.into_raw(),
            Err(_) => std::ptr::null_mut(),
        }
    })
}

// ==================== AHardwareBuffer native helpers ====================

// NDK declarations for the subset of AHB ABI we need on the inbound
// side. Kept separate from `shared::protocol::ahb::sys` so this
// module can compile stand-alone; the duplicate `extern` declaration
// is deduplicated by the linker (the symbol comes from `libandroid`).
#[cfg(target_os = "android")]
unsafe extern "C" {
    /// Native accessor for Java `HardwareBuffer` — NDK API 26+.
    /// Returns a **borrowed** pointer valid for the lifetime of the
    /// Java wrapper; callers that need independent ownership must
    /// call `AHardwareBuffer_acquire` themselves.
    fn AHardwareBuffer_fromHardwareBuffer(
        env: *mut jni::sys::JNIEnv,
        hardware_buffer_obj: jni::sys::jobject,
    ) -> *mut std::ffi::c_void;
    fn AHardwareBuffer_acquire(buffer: *mut std::ffi::c_void);
}

/// Called by `NativeBridge.nativeAhbPointerFromHardwareBuffer`.
///
/// Returns the native `AHardwareBuffer*` as `jlong`. On any failure
/// (null input, NDK error, unexpected JNI state) returns `0`, which
/// [`NativeExports.decodeImageAhb`] treats as "no AHB handle, fall
/// back to the RGBA byte[] path".
///
/// The returned raw pointer owns one extra strong refcount:
/// `AHardwareBuffer_fromHardwareBuffer` yields a borrowed pointer,
/// then this bridge calls `AHardwareBuffer_acquire` before returning
/// to Java. That lets Java close its `HardwareBuffer` wrapper in a
/// `finally` block while Rust later adopts the native handle without
/// another acquire.
#[cfg(target_os = "android")]
pub(crate) extern "system" fn nativeAhbPointerFromHardwareBuffer<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
    hb: JObject<'local>,
) -> jni::sys::jlong {
    jni_safe!("nativeAhbPointerFromHardwareBuffer", 0, {
        if hb.is_null() {
            return 0;
        }
        // SAFETY: `hb` is a non-null reference to a Java
        // HardwareBuffer passed by the VM. The NDK function accepts
        // a `JNIEnv*` + `jobject`; both come directly from the
        // caller's frame.
        let ptr = unsafe { AHardwareBuffer_fromHardwareBuffer(env.get_raw(), hb.as_raw()) };
        if ptr.is_null() {
            return 0;
        }
        // SAFETY: `ptr` came from `AHardwareBuffer_fromHardwareBuffer`
        // for a live Java `HardwareBuffer`; acquire publishes one
        // independent native refcount the Rust side can later own.
        unsafe { AHardwareBuffer_acquire(ptr) };
        ptr as jni::sys::jlong
    })
}

// Non-Android stub. Inbound registrations reference the name on
// every target; the stub keeps the linker happy on desktop dev
// builds where there is no real AHB subsystem.
#[cfg(not(target_os = "android"))]
pub(crate) extern "system" fn nativeAhbPointerFromHardwareBuffer<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    _hb: JObject<'local>,
) -> jni::sys::jlong {
    0
}
