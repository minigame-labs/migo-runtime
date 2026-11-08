use jni::{
    Env, jni_sig, jni_str,
    objects::{JObject, JString},
    signature::FieldSignature,
    strings::JNIStr,
};

/// Get an optional String field from a Java object.
/// Returns `None` if the field is null, empty, or cannot be read.
pub(crate) fn get_optional_string_field(
    env: &mut Env,
    field_name: &JNIStr,
    obj: &JObject,
) -> Option<String> {
    let val = match env.get_field(obj, field_name, jni_sig!("Ljava/lang/String;")) {
        Ok(v) => v,
        Err(_) => {
            env.exception_clear();
            return None;
        }
    };
    let jobj = val.l().ok()?;
    if jobj.is_null() {
        return None;
    }
    let jstr = JString::cast_local(env, jobj).ok()?;
    let s: String = jstr.mutf8_chars(env).ok().map(|s| s.into())?;
    if s.is_empty() { None } else { Some(s) }
}

pub(crate) fn get_string_field(
    env: &mut Env,
    field_name: &JNIStr,
    obj: &JObject,
) -> Result<String, String> {
    let val = match env.get_field(obj, field_name, jni_sig!("Ljava/lang/String;")) {
        Ok(v) => v,
        Err(e) => {
            env.exception_clear();
            return Err(format!("Failed to get field '{field_name}': {e}"));
        }
    };

    let jobj = val
        .l()
        .map_err(|e| format!("Field '{field_name}' is not an object: {e}"))?;

    if jobj.is_null() {
        return Err(format!("Field '{}' is null", field_name));
    }

    let jstr = JString::cast_local(env, jobj)
        .map_err(|error| format!("Field '{field_name}' is not a String: {error}"))?;

    jstr.mutf8_chars(env)
        .map_err(|e| format!("Failed to convert '{field_name}' to Rust String: {e}"))
        .map(|s| s.into())
}

pub(crate) fn get_f32(env: &mut Env, field_name: &JNIStr, obj: &JObject) -> Result<f32, String> {
    let val = match env.get_field(obj, field_name, jni_sig!("F")) {
        Ok(v) => v,
        Err(e) => {
            env.exception_clear();
            return Err(format!("Failed to get field '{field_name}': {e}"));
        }
    };

    val.f()
        .map_err(|e| format!("Failed to convert field '{field_name}' to f32: {e}"))
}

pub(crate) fn get_i32(env: &mut Env, field_name: &JNIStr, obj: &JObject) -> Result<i32, String> {
    let val = match env.get_field(obj, field_name, jni_sig!("I")) {
        Ok(v) => v,
        Err(e) => {
            env.exception_clear();
            return Err(format!("Failed to get field '{field_name}': {e}"));
        }
    };

    val.i()
        .map_err(|e| format!("Failed to convert field '{field_name}' to i32: {e}"))
}

pub(crate) fn get_bool(env: &mut Env, field_name: &JNIStr, obj: &JObject) -> Result<bool, String> {
    let val = match env.get_field(obj, field_name, jni_sig!("Z")) {
        Ok(v) => v,
        Err(e) => {
            env.exception_clear();
            return Err(format!("Failed to get field '{field_name}': {e}"));
        }
    };

    val.z()
        .map_err(|e| format!("Failed to convert field '{field_name}' to bool: {e}"))
}

/// A field whose read cannot fail because the host left it unset.
///
/// THE DRIFT THIS EXISTS TO CATCH shipped and stayed invisible. Every reader
/// below returns a `Result` carrying the field name and the reason, and every
/// caller wrote `.unwrap_or(some_default)` — which is correct-looking and
/// throws the reason away. But a Java `int`, `boolean`, `float` or enum
/// reference *always has a value*, so `get_field` cannot fail because the host
/// declined to set one. It fails when the field is not on the class this code
/// names: a host built against a different SDK than the library it loaded.
///
/// Absorbed into a default, that is indistinguishable from a host that chose
/// the default. `RuntimeConfig.logLevel` could be set to INFO by a host and read
/// as WARN by the engine, and the only evidence either way was the absence of
/// logs the host had asked for — which is also what a working INFO run looks
/// like before the first frame.
///
/// `warn!` and not `info!`: the release default level is WARN, so this is
/// audible in exactly the builds where the mismatch would otherwise be silent.
pub(crate) fn or_default<T>(read: Result<T, String>, fallback: T) -> T {
    match read {
        Ok(value) => value,
        Err(reason) => {
            tracing::warn!(
                "{reason}. A primitive field always has a value, so this is a \
                 Java/native mismatch rather than an unset option: the host's \
                 value for it is not in effect and the built-in default is."
            );
            fallback
        }
    }
}

/// The ordinal of an enum field, or `None` when the field is null.
///
/// Three outcomes and not two, because the third used to be a decision hidden
/// in here: a null field returned `Ok(0)`, described as "default to first enum
/// value". The first variant is a different thing for every enum — for
/// `RuntimeConfig.LogLevel` it is `Trace`, the most verbose level there is,
/// which is a surprising answer to a field nobody set. Which default is right
/// belongs to the caller that knows what the field means.
pub(crate) fn get_enum_ordinal(
    env: &mut Env,
    field_name: &JNIStr,
    field_sig: FieldSignature<'_>,
    obj: &JObject,
) -> Result<Option<i32>, String> {
    let val = match env.get_field(obj, field_name, field_sig) {
        Ok(v) => v,
        Err(e) => {
            env.exception_clear();
            return Err(format!("Failed to get enum field '{field_name}': {e}"));
        }
    };

    let enum_obj = val
        .l()
        .map_err(|e| format!("Enum field '{field_name}' is not an object: {e}"))?;

    if enum_obj.is_null() {
        return Ok(None);
    }

    let ordinal = match env.call_method(&enum_obj, jni_str!("ordinal"), jni_sig!("()I"), &[]) {
        Ok(v) => v,
        Err(e) => {
            env.exception_clear();
            return Err(format!("Failed to call ordinal() on '{field_name}': {e}"));
        }
    };

    ordinal
        .i()
        .map(Some)
        .map_err(|e| format!("Failed to get ordinal value for '{field_name}': {e}"))
}
