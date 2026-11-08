//! Compatibility entry point for hosts that initialized the former Android
//! audio backend's JNI context. The current native Oboe bridge needs no VM.

use crate::panic_barrier::guard;
use migo_capi_abi::{MIGO_ERROR_INVALID_ARGUMENT, MIGO_OK, MigoResult};

/// Retained for ABI compatibility. Non-null `vm` returns success without
/// retaining or dereferencing either pointer; new hosts need not call this.
///
/// # Safety
/// `vm` must be null or a valid `JavaVM*` for this process for the duration
/// of the call. `activity`, if non-null, must be a valid `jobject`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_android_init_context(
    vm: *mut std::ffi::c_void,
    _activity: *mut std::ffi::c_void,
) -> MigoResult {
    guard("migo_android_init_context", || {
        // Preserve the old argument-validation contract as well as the symbol.
        if vm.is_null() {
            return MIGO_ERROR_INVALID_ARGUMENT;
        }
        MIGO_OK
    })
}
