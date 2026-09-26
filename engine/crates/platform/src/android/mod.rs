pub mod jni;
pub(crate) mod logging;

/// Send engine diagnostics to logcat at `level`.
///
/// The JNI path reaches this through its own session setup; a C host has no
/// JNI, so this is how an embedder that never touches Java asks for the same
/// channel. Kept as one function rather than a module re-export so what is
/// public stays the capability, not the implementation.
pub fn install_logcat_diagnostics(level: shared::config::LogLevel) {
    logging::init_logging();
    logging::update_log_level(level);
}

/// Give the engine image decoders that need no Java.
///
/// The JNI path registers BitmapFactory plus the Skia AHB decoder in
/// `jni::on_load`. A C host never runs that, so without this every image failed
/// to decode -- and a WebGL game, whose images are all CPU-backed, got no pixels
/// at all. The C ABI build also carries the Rust decoders (the `rust-image-decode`
/// feature), which `io::decode_image_fast` tries first; Skia, as linked, decodes
/// PNG but not JPEG. So: Skia's AHB decoder gives PNG the zero-copy path the Java
/// SDK has, and its RGBA decoder is the fallback for what the Rust decoders do
/// not read. Only the first call takes effect; an engine created again in the
/// same process finds them registered.
pub fn install_skia_image_decoders() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        migo_io::register_platform_ahb_decoder(graphics::image_decode_ahb::decode_image_to_ahb);
        migo_io::register_platform_decoder(graphics::image_decode_ahb::decode_image_rgba);
    });
}

/// Register this process's `JavaVM` and an Activity/Context `jobject` with
/// `ndk-context`, which the Android audio backend (cpal's oboe/AAudio path)
/// requires before it can open an output stream.
///
/// The JNI path initializes this as part of `jni::on_load`; a C host has no
/// JNI, so this is how an embedder that never touches Java supplies the same
/// context. `ndk-context` aborts the process if initialized twice, and an
/// embedder's activity can legitimately be recreated within one process, so
/// only the first call takes effect -- later calls are a deliberate no-op
/// rather than a second, panicking write.
///
/// # Safety
/// `vm` must be a valid `JavaVM*` for the process; `activity` must be a valid
/// `jobject` (Activity or Context) or null.
pub unsafe fn init_context(vm: *mut std::ffi::c_void, activity: *mut std::ffi::c_void) {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| unsafe {
        ndk_context::initialize_android_context(vm, activity);
    });
}

pub mod platform;
pub mod presenter;
pub mod services;
pub mod surface;
