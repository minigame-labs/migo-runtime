use deno_core::{OpState, ToJsBuffer, op2};
use deno_error::JsErrorBox;
use shared::codec;
use shared::op_state::HostOpState;

#[inline]
fn normalize_coding<'a>(coding: &'a str) -> &'a str {
    if coding.is_empty() { "utf8" } else { coding }
}

#[inline]
fn is_gbk(coding: &str) -> bool {
    coding.eq_ignore_ascii_case("gbk")
}

#[op2]
#[serde]
pub(super) fn op_encode_multi_formats(
    state: &mut OpState,
    #[string] original: &str,
    #[string] coding: &str,
) -> Result<ToJsBuffer, JsErrorBox> {
    let coding = normalize_coding(coding);

    // GBK: try platform CodecService first, then fall back to shared::codec
    if is_gbk(coding) {
        let host = state.borrow::<HostOpState>();
        if let Some(ref services) = host.device_services {
            if let Some(codec_svc) = services.codec() {
                return codec_svc
                    .encode_gbk(original)
                    .map(ToJsBuffer::from)
                    .map_err(JsErrorBox::generic);
            }
        }
    }

    codec::encode_string(original, coding)
        .map(ToJsBuffer::from)
        .map_err(JsErrorBox::generic)
}

#[op2]
#[string]
pub(super) fn op_decode_multi_formats(
    state: &mut OpState,
    #[buffer] buf: &[u8],
    #[string] coding: &str,
) -> Result<String, JsErrorBox> {
    let coding = normalize_coding(coding);

    // GBK: try platform CodecService first, then fall back to shared::codec
    if is_gbk(coding) {
        let host = state.borrow::<HostOpState>();
        if let Some(ref services) = host.device_services {
            if let Some(codec_svc) = services.codec() {
                return codec_svc.decode_gbk(buf).map_err(JsErrorBox::generic);
            }
        }
    }

    // For UTF-8 specifically, match browser/Node semantics: invalid byte
    // sequences become U+FFFD replacement characters rather than throwing.
    // Strict decoding caused real-world breakage when games called
    // `readFile({encoding: 'utf8'})` on files that turned out to be binary:
    // the rejection triggered adapter retry loops. Other encodings keep going
    // through `decode_bytes`, which the unzip path relies on for its
    // strict-to-base64 fallback.
    if coding.eq_ignore_ascii_case("utf8") || coding.eq_ignore_ascii_case("utf-8") {
        return Ok(String::from_utf8_lossy(buf).into_owned());
    }

    codec::decode_bytes(buf, coding).map_err(JsErrorBox::generic)
}
