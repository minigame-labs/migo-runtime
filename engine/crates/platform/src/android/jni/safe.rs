/// Extract a human-readable message from a `catch_unwind` panic payload.
pub(crate) fn panic_message(e: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = e.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = e.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

/// Contains Rust panics inside a JNI entry point in unwinding builds.
///
/// Shipping release profiles use `panic = "abort"`: a panic terminates the
/// process before `catch_unwind` can observe it. Recoverable JNI failures must
/// use the function's normal error path. This barrier does not promise recovery
/// from a release panic or from unwinding through an inner C++ callback.
///
/// In an unwinding build, logs the panic and returns the provided fallback.
/// For `void` JNI functions, use `()` as fallback.
macro_rules! jni_safe {
    // Native methods that call JNI acquire the library's scoped environment.
    // Keep the SDK's existing per-method fallback (notably init's -1).
    ($name:expr, $env:ident => $default:expr, $body:block) => {{
        // `return fallback` in the native body must leave only that body,
        // rather than the outer closure whose return type is Result.
        #[allow(clippy::redundant_closure_call)]
        let outcome = $env
            .with_env(|$env| Ok::<_, jni::errors::Error>((|| $body)()))
            .into_outcome();
        match outcome {
            jni::Outcome::Ok(value) => value,
            jni::Outcome::Err(error) => {
                tracing::error!("JNI failure in {}: {}", $name, error);
                $default
            }
            jni::Outcome::Panic(error) => {
                tracing::error!(
                    "JNI panic in {}: {}",
                    $name,
                    $crate::android::jni::safe::panic_message(&error)
                );
                $default
            }
        }
    }};
    // Void variant (no return value)
    ($name:expr, $body:expr) => {{
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| $body));
        match result {
            Ok(val) => val,
            Err(e) => {
                tracing::error!(
                    "JNI panic in {}: {}",
                    $name,
                    $crate::android::jni::safe::panic_message(&e)
                );
            }
        }
    }};
    // Return-value variant (with default on panic)
    ($name:expr, $default:expr, $body:expr) => {{
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| $body));
        match result {
            Ok(val) => val,
            Err(e) => {
                tracing::error!(
                    "JNI panic in {}: {}",
                    $name,
                    $crate::android::jni::safe::panic_message(&e)
                );
                $default
            }
        }
    }};
}

pub(crate) use jni_safe;
