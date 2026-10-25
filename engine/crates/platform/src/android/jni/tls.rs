//! Initialize the upstream verifier with an application Context. The raw JNI
//! boundary keeps the SDK's existing JNI wrappers out of the verifier's API.
use jni::JNIEnv;

pub(super) fn init(env: &mut JNIEnv<'_>) -> Result<(), String> {
    let result = (|| {
        let context = env
            .call_static_method(
                "com/migo/runtime/internal/AppContext",
                "getOrNull",
                "()Landroid/content/Context;",
                &[],
            )
            .and_then(|value| value.l())
            .map_err(|error| format!("failed to obtain platform TLS context: {error}"))?;
        if !context.is_null() {
            // Safety: these handles are live on this thread; upstream copies
            // the application Context/class loader into owned global refs.
            unsafe {
                migo_services::network::tls::init_android(
                    env.get_native_interface().cast(),
                    context.as_raw().cast(),
                )
            }?;
        }
        // Headless Java/native C ABI users with no Context explicitly choose
        // tlsTrust=mozilla. Platform configuration then fails before connecting.
        Ok(())
    })();
    if result.is_err() && env.exception_check().unwrap_or(false) {
        let _ = env.exception_clear();
    }
    result
}
