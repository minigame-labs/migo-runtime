//! A service call's arguments, as the host takes them out of a request.
//!
//! The producer has already converted each argument by the rule deno_core
//! applies to its op's parameter (`op-args.mjs`), so what arrives is the
//! value the Rust op body would have received, tagged with its type. These
//! take each out as that type, and a value of another type is the producer
//! calling the op wrongly: a `TypeError` naming the op and the argument, which
//! is what deno_core throws for an argument it cannot convert.

use frame_wire::value::OwnedValue;
use migo_services::ServiceError;

use super::service_ops;

/// The class a malformed call is thrown as, matching what `deno_core` throws
/// when an op is handed an argument of the wrong type.
pub(crate) const CLASS_TYPE_ERROR: &str = "TypeError";

/// The error for an op this host does not run in that shape.
pub(crate) fn not_a(op: u32, shape: &str) -> ServiceError {
    ServiceError::classed(
        CLASS_TYPE_ERROR,
        match service_ops::name_of(op) {
            Some(name) => format!("{name} is not a {shape} service op"),
            None => format!("service op {op} is not one this host knows"),
        },
    )
}

pub(crate) fn exactly<const N: usize>(
    op: u32,
    args: Vec<OwnedValue>,
) -> Result<[OwnedValue; N], ServiceError> {
    let count = args.len();
    args.try_into().map_err(|_| {
        ServiceError::classed(
            CLASS_TYPE_ERROR,
            format!(
                "{} takes {N} argument(s), not {count}",
                service_ops::name_of(op).unwrap_or("this op")
            ),
        )
    })
}

pub(crate) fn wrong_type(op: u32, index: usize, wanted: &str, got: &OwnedValue) -> ServiceError {
    ServiceError::classed(
        CLASS_TYPE_ERROR,
        format!(
            "argument {index} of {} is a {wanted}, not a {}",
            service_ops::name_of(op).unwrap_or("this op"),
            got.kind_name()
        ),
    )
}

pub(crate) fn string(op: u32, index: usize, value: OwnedValue) -> Result<String, ServiceError> {
    match value {
        OwnedValue::Str(text) => Ok(text),
        other => Err(wrong_type(op, index, "string", &other)),
    }
}

pub(crate) fn u32_of(op: u32, index: usize, value: OwnedValue) -> Result<u32, ServiceError> {
    match value {
        OwnedValue::U32(value) => Ok(value),
        other => Err(wrong_type(op, index, "u32", &other)),
    }
}

pub(crate) fn i32_of(op: u32, index: usize, value: OwnedValue) -> Result<i32, ServiceError> {
    match value {
        OwnedValue::I32(value) => Ok(value),
        other => Err(wrong_type(op, index, "i32", &other)),
    }
}

pub(crate) fn strings(
    op: u32,
    index: usize,
    value: OwnedValue,
) -> Result<Vec<String>, ServiceError> {
    match value {
        OwnedValue::Array(values) => values
            .into_iter()
            .map(|value| string(op, index, value))
            .collect(),
        other => Err(wrong_type(op, index, "array of strings", &other)),
    }
}

pub(crate) fn bytes(op: u32, index: usize, value: OwnedValue) -> Result<Vec<u8>, ServiceError> {
    match value {
        OwnedValue::Bytes(bytes) => Ok(bytes),
        other => Err(wrong_type(op, index, "bytes", &other)),
    }
}

pub(crate) fn boolean(op: u32, index: usize, value: OwnedValue) -> Result<bool, ServiceError> {
    match value {
        OwnedValue::Bool(value) => Ok(value),
        other => Err(wrong_type(op, index, "bool", &other)),
    }
}

pub(crate) fn u64_of(op: u32, index: usize, value: OwnedValue) -> Result<u64, ServiceError> {
    match value {
        OwnedValue::U64(value) => Ok(value),
        other => Err(wrong_type(op, index, "u64", &other)),
    }
}

/// `Option<u64>`: null is None.
pub(crate) fn optional_u64(
    op: u32,
    index: usize,
    value: OwnedValue,
) -> Result<Option<u64>, ServiceError> {
    match value {
        OwnedValue::Null => Ok(None),
        other => u64_of(op, index, other).map(Some),
    }
}

/// `Option<String>`: null is None.
pub(crate) fn optional_string(
    op: u32,
    index: usize,
    value: OwnedValue,
) -> Result<Option<String>, ServiceError> {
    match value {
        OwnedValue::Null => Ok(None),
        other => string(op, index, other).map(Some),
    }
}

/// `Option<&[u8]>`: null is None.
pub(crate) fn optional_bytes(
    op: u32,
    index: usize,
    value: OwnedValue,
) -> Result<Option<Vec<u8>>, ServiceError> {
    match value {
        OwnedValue::Null => Ok(None),
        other => bytes(op, index, other).map(Some),
    }
}

/// `f64`: a Number, as the producer converted it.
pub(crate) fn f64_of(op: u32, index: usize, value: OwnedValue) -> Result<f64, ServiceError> {
    match value {
        OwnedValue::F64(value) => Ok(value),
        other => Err(wrong_type(op, index, "f64", &other)),
    }
}

/// `f32`: its bits, which the producer rounded once (`f32BitsOf`) exactly as
/// `as f32` rounds -- carried as bits so no second rounding can happen.
pub(crate) fn f32_bits_of(op: u32, index: usize, value: OwnedValue) -> Result<f32, ServiceError> {
    match value {
        OwnedValue::U32(bits) => Ok(f32::from_bits(bits)),
        other => Err(wrong_type(op, index, "f32 bits", &other)),
    }
}

/// `#[serde] Vec<f64>`: an array of Numbers.
pub(crate) fn f64s(op: u32, index: usize, value: OwnedValue) -> Result<Vec<f64>, ServiceError> {
    match value {
        OwnedValue::Array(values) => values
            .into_iter()
            .map(|value| f64_of(op, index, value))
            .collect(),
        other => Err(wrong_type(op, index, "array of numbers", &other)),
    }
}
