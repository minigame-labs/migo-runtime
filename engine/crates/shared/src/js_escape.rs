use std::borrow::Cow;

use serde::Serialize;

/// Escape a string for safe interpolation into a JSON double-quoted string.
///
/// Handles: backslash, double quote, newlines, carriage return, and tab.
pub fn escape_for_json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

/// Arguments for a host hook delivered through the retained dispatcher.
///
/// The dispatcher does `JSON.parse(args)` on what it is handed and spreads the
/// result, so these build a JSON array.
///
/// The encoding lives here rather than at each call site for one reason: an
/// array that does not parse is a callback the dispatcher **drops**, and a
/// dropped callback is a promise that never settles. A hand-rolled quoter that
/// forgets one control character loses the login result of whichever user
/// happens to have it in their nickname. `serde_json` is the encoder; nothing
/// below writes a quote by hand.
pub const HOOK_ARGS_NONE: &str = "[]";

/// One argument.
///
/// Pass a `&str` for the hooks that parse their own JSON, a
/// [`serde_json::Value`] for the ones that take an object, a number for the
/// rest -- the shapes are the hooks', and this does not reinterpret them.
pub fn hook_args_one<T: Serialize>(value: T) -> Cow<'static, str> {
    encode_hook_args(&(value,))
}

fn encode_hook_args<T: Serialize>(value: &T) -> Cow<'static, str> {
    match serde_json::to_string(value) {
        Ok(encoded) => Cow::Owned(encoded),
        // Unreachable for the types above: serde_json fails only on non-string
        // map keys and non-finite floats, and `Value` can hold neither.
        //
        // The fallback is deliberately *not* `HOOK_ARGS_NONE`: that would call
        // the hook with `undefined` where it expects a result, settling a
        // promise with nonsense. An empty string does not parse, so the
        // dispatcher drops the call -- the same treatment any other malformed
        // payload gets.
        Err(e) => {
            tracing::error!("host hook arguments could not be encoded: {e}");
            Cow::Borrowed("")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The dispatcher spreads what it decodes, so a one-argument call has to
    /// arrive as a one-element array -- not as a bare value.
    #[test]
    fn one_argument_is_a_one_element_array() {
        assert_eq!(hook_args_one("payload"), r#"["payload"]"#);
    }

    /// The hooks that take a JSON result are handed the *string*, exactly as
    /// they were by the channel this replaces. Encoding it as an object here
    /// would silently change the argument every one of them receives.
    #[test]
    fn a_json_payload_stays_a_string() {
        let encoded = hook_args_one(r#"{"code":"abc"}"#);
        assert_eq!(encoded, r#"["{\"code\":\"abc\"}"]"#);
    }

    /// An object argument -- `onShow` launch options -- travels as a value.
    #[test]
    fn an_object_argument_travels_as_a_value() {
        let value: serde_json::Value = serde_json::json!({"scene": 1001});
        assert_eq!(hook_args_one(value), r#"[{"scene":1001}]"#);
    }

    /// The characters that used to need escaping for JS source, and the ones a
    /// hand-rolled JSON quoter forgets.
    ///
    /// U+2028/U+2029 are the interesting pair: valid inside a JSON string, but
    /// line terminators in JS source. They needed escaping only because the
    /// payload was pasted into source, and nothing here is source -- so they
    /// may pass through unescaped, and the result must still parse.
    #[test]
    fn control_characters_survive_the_round_trip() {
        for payload in [
            "it's a `$100` note",
            "line\nbreak\r\ttab",
            "null\u{0}byte",
            "unit\u{1}separator",
            "sep\u{2028}and\u{2029}para",
            "quote\"and\\slash",
            "",
        ] {
            let encoded = hook_args_one(payload);
            let decoded: Vec<String> =
                serde_json::from_str(&encoded).expect("encoded arguments must parse");
            assert_eq!(decoded, vec![payload.to_string()], "payload {payload:?}");
        }
    }

    /// Zero arguments is a literal, not an encoding, so pin that it is the
    /// array the dispatcher expects rather than something like `null`.
    #[test]
    fn no_arguments_is_an_empty_array() {
        let decoded: Vec<String> =
            serde_json::from_str(HOOK_ARGS_NONE).expect("HOOK_ARGS_NONE must parse");
        assert!(decoded.is_empty());
    }
}
