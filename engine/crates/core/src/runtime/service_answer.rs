//! How the host writes an answer's numbers, so that the producer hands content what the embedded runtime does.
//!
//! The wire carries a 64-bit integer as a BigInt, so that nothing rounds in transit. What content sees is decided by
//! the embedded op's return type: a `#[bigint]` op answers a BigInt, and a `u64` inside a `#[serde]` struct is the
//! Number serde_v8 makes of it. A facade that does arithmetic on the field -- a download's percentage divides by the
//! content length -- throws "Invalid mix of BigInt and other type" on the wrong one, which is what failed every
//! download on the phone. The encoding is therefore the host's, written once here, and the producer rebuilds the
//! answer without converting anything.

use frame_wire::value::OwnedValue;

/// The largest integer a JavaScript Number holds exactly: 2^53 - 1.
pub(super) const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;

/// A `u64` inside a `#[serde]` answer, as serde_v8 serializes one: a Number while it is a safe integer, a BigInt past
/// that.
pub(super) fn serde_u64(value: u64) -> OwnedValue {
    if value <= MAX_SAFE_INTEGER {
        OwnedValue::F64(value as f64)
    } else {
        OwnedValue::U64(value)
    }
}

/// An `Option<u64>` inside a `#[serde]` answer: `null` for none.
pub(super) fn serde_optional_u64(value: Option<u64>) -> OwnedValue {
    match value {
        Some(value) => serde_u64(value),
        None => OwnedValue::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_u64_is_a_number_while_it_is_safe_and_a_bigint_past_it() {
        assert_eq!(serde_u64(0), OwnedValue::F64(0.0));
        assert_eq!(
            serde_u64(MAX_SAFE_INTEGER),
            OwnedValue::F64(MAX_SAFE_INTEGER as f64)
        );
        assert_eq!(
            serde_u64(MAX_SAFE_INTEGER + 1),
            OwnedValue::U64(MAX_SAFE_INTEGER + 1)
        );
    }

    #[test]
    fn an_absent_u64_is_null_and_a_present_one_is_written_as_any_other() {
        assert_eq!(serde_optional_u64(None), OwnedValue::Null);
        assert_eq!(serde_optional_u64(Some(11)), OwnedValue::F64(11.0));
        assert_eq!(
            serde_optional_u64(Some(MAX_SAFE_INTEGER + 1)),
            OwnedValue::U64(MAX_SAFE_INTEGER + 1)
        );
    }
}
