use jni::{Env, JavaVM};
use std::sync::OnceLock;

use crate::android::jni::{cache::JavaMethodCache, register_java_exports, register_native_exports};

static JVM: OnceLock<JavaVM> = OnceLock::new();

pub(crate) static JAVA_METHOD_CACHE: OnceLock<JavaMethodCache> = OnceLock::new();

pub fn init_jni_env(jvm: JavaVM) -> Result<(), String> {
    JVM.set(jvm).map_err(|_| "Failed to set JVM".to_string())?;

    with_env(register_java_exports)?;
    with_env(register_native_exports)?;

    // Register Android BitmapFactory as the platform image decoder.
    // This replaces Rust image/zune-image decoders on Android, saving ~2-4 MB in the binary.
    migo_io::register_platform_decoder(|data| {
        crate::android::jni::outbound::decode_image_rgba_jni(data)
    });

    // Register the API-26 Rust/Skia decoder. It writes directly into a
    // CPU-locked AHardwareBuffer, synchronously unlocks, then the render thread
    // imports it through EGLImage. Unsupported formats, EXIF transforms, and
    // any allocation/decode failure fall back per image to BitmapFactory.
    // `decode_image_to_any` picks this path preferentially and
    // transparently falls back to `PLATFORM_DECODER` per-image if AHB
    // decode fails (e.g. vendor driver quirk).
    migo_io::register_platform_ahb_decoder(graphics::image_decode_ahb::decode_image_to_ahb);

    Ok(())
}

/// Run a closure in a JNI local-reference frame on the current thread.
///
/// jni-rs reuses an existing attachment, cleans up the local frame and any
/// exception raised by the closure, and detaches owned threads at thread exit.
/// The closure can only return values independent of the local frame.
pub fn with_env<F, R, E>(f: F) -> Result<R, E>
where
    F: FnOnce(&mut Env) -> Result<R, E>,
    E: From<String>,
{
    let jvm = JVM
        .get()
        .ok_or_else(|| E::from("JVM not initialized".to_string()))?;
    // Preserve callers' error types while letting jni-rs report attachment or
    // pending-exception failures through its own error type.
    jvm.attach_current_thread(|env| Ok::<_, jni::errors::Error>(f(env)))
        .map_err(|error| E::from(format!("JNI attachment failed: {error}")))?
}
