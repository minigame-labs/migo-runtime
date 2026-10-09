use std::{cmp::min, ffi::CStr, mem::size_of, os::raw::c_char, ptr};

use crate::{
    MIGO_ABI_VERSION_CURRENT, MIGO_ERROR_INVALID_ARGUMENT, MIGO_ERROR_UNSUPPORTED_ABI, MIGO_OK,
    MigoResult,
};

/// Fixed prefix shared by every versioned public structure.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VersionedHeader {
    pub struct_size: u32,
    pub abi_version: u32,
}

/// A zero-valid `#[repr(C)]` record used at the public ABI boundary.
///
/// # Safety
/// Implementors must be valid when every byte is zero, must not contain Rust
/// references or niche-bearing enums, and must use their actual stable C
/// layout. `MINIMUM_SIZE` must cover every field required by the oldest
/// supported prefix and may never be reduced after release.
pub unsafe trait AbiStruct: Sized {
    const MINIMUM_SIZE: usize = size_of::<Self>();
}

/// Version handling for a public output record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputVersionPolicy {
    /// The caller must use the current ABI and may not announce unknown fields.
    CurrentAbi,
    /// Capability discovery may answer a newer caller's known prefix.
    CapabilityNegotiation,
}

/// Copy a caller-supplied versioned input, zero-extending a supported prefix.
///
/// # Safety
/// `header` must be null or suitably aligned and readable for the exact number
/// of bytes announced in its fixed header. Implementing [`AbiStruct`] promises
/// that zero is a valid representation for every absent tail field.
pub unsafe fn copy_versioned<T: AbiStruct>(
    header: *const VersionedHeader,
) -> Result<T, MigoResult> {
    // SAFETY: the function contract requires the fixed header to be readable.
    let Some(head) = (unsafe { header.as_ref() }) else {
        return Err(MIGO_ERROR_INVALID_ARGUMENT);
    };
    if head.abi_version != MIGO_ABI_VERSION_CURRENT {
        return Err(MIGO_ERROR_UNSUPPORTED_ABI);
    }

    let caller_size = head.struct_size as usize;
    if caller_size > size_of::<T>() {
        return Err(MIGO_ERROR_UNSUPPORTED_ABI);
    }
    if caller_size < T::MINIMUM_SIZE {
        return Err(MIGO_ERROR_INVALID_ARGUMENT);
    }

    // SAFETY: AbiStruct requires an all-zero representation to be valid. The
    // caller promises exactly caller_size readable bytes and the bounds above
    // prove the destination is large enough.
    let mut value: T = unsafe { std::mem::zeroed() };
    unsafe {
        ptr::copy_nonoverlapping(
            header.cast::<u8>(),
            (&mut value as *mut T).cast::<u8>(),
            caller_size,
        );
    }
    Ok(value)
}

/// Validate a versioned structure that does not support a shorter prefix.
///
/// # Safety
/// `header` must be null or point to a suitably aligned, readable
/// [`VersionedHeader`].
pub unsafe fn validate_header(
    header: *const VersionedHeader,
    expected_size: usize,
) -> Result<(), MigoResult> {
    // SAFETY: the function contract requires the fixed header to be readable.
    let Some(header) = (unsafe { header.as_ref() }) else {
        return Err(MIGO_ERROR_INVALID_ARGUMENT);
    };
    if header.abi_version != MIGO_ABI_VERSION_CURRENT {
        return Err(MIGO_ERROR_UNSUPPORTED_ABI);
    }

    let caller_size = header.struct_size as usize;
    if caller_size > expected_size {
        return Err(MIGO_ERROR_UNSUPPORTED_ABI);
    }
    if caller_size != expected_size {
        return Err(MIGO_ERROR_INVALID_ARGUMENT);
    }
    Ok(())
}

/// Copy a borrowed, NUL-terminated UTF-8 string into owned storage.
///
/// # Safety
/// `text` must be null or remain readable through its first NUL byte for the
/// duration of this call.
pub unsafe fn copy_utf8(text: *const c_char) -> Result<String, MigoResult> {
    if text.is_null() {
        return Err(MIGO_ERROR_INVALID_ARGUMENT);
    }

    // SAFETY: the caller guarantees a readable NUL-terminated string.
    unsafe { CStr::from_ptr(text) }
        .to_str()
        .map(str::to_owned)
        .map_err(|_| MIGO_ERROR_INVALID_ARGUMENT)
}

/// Copy an exact length-delimited UTF-8 string.
///
/// A null pointer is accepted only for an empty range, in which case it is not
/// dereferenced. Embedded NUL bytes are data rather than terminators.
///
/// # Safety
/// `value` must be null with `length == 0`, or readable for `length` bytes.
pub unsafe fn copy_utf8_with_length(
    value: *const c_char,
    length: u32,
) -> Result<String, MigoResult> {
    if length == 0 {
        return Ok(String::new());
    }
    if value.is_null() {
        return Err(MIGO_ERROR_INVALID_ARGUMENT);
    }
    // SAFETY: the caller guarantees `length` readable bytes at `value`.
    let bytes = unsafe { std::slice::from_raw_parts(value.cast::<u8>(), length as usize) };
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|_| MIGO_ERROR_INVALID_ARGUMENT)
}

/// Write only the output prefix understood by both caller and library.
///
/// The caller-owned header is an input to negotiation and is preserved on a
/// successful write. No output byte is modified when validation fails.
///
/// # Safety
/// `out` must be null or suitably aligned and writable for the `struct_size`
/// bytes announced by its readable [`VersionedHeader`]. `value` must not
/// overlap that allocation. The bytes copied from `value` must be safe to
/// expose across the C boundary; callers should construct ABI outputs from a
/// zeroed record so padding cannot disclose unrelated memory.
pub unsafe fn write_versioned_output<T: AbiStruct>(
    out: *mut T,
    value: &T,
    policy: OutputVersionPolicy,
) -> MigoResult {
    if out.is_null() {
        return MIGO_ERROR_INVALID_ARGUMENT;
    }

    // SAFETY: the function contract requires the fixed header to be readable.
    let caller = unsafe { ptr::read(out.cast::<VersionedHeader>()) };
    let caller_size = caller.struct_size as usize;
    if caller_size < T::MINIMUM_SIZE {
        return MIGO_ERROR_INVALID_ARGUMENT;
    }
    if policy == OutputVersionPolicy::CurrentAbi
        && (caller.abi_version != MIGO_ABI_VERSION_CURRENT || caller_size > size_of::<T>())
    {
        return MIGO_ERROR_UNSUPPORTED_ABI;
    }

    let bytes = min(caller_size, size_of::<T>());
    // SAFETY: validation established the caller's writable prefix and the
    // function contract excludes overlap with the local source record.
    unsafe {
        ptr::copy_nonoverlapping((value as *const T).cast::<u8>(), out.cast::<u8>(), bytes);
        ptr::write(out.cast::<VersionedHeader>(), caller);
    }
    MIGO_OK
}
