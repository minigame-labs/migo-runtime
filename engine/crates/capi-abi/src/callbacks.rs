//! Host callback wire records and their allocation-free semantic validation.

use std::{
    ffi::c_void,
    mem::{offset_of, size_of},
    os::raw::c_char,
};

use crate::{
    AbiStruct, MIGO_ABI_VERSION_CURRENT, MIGO_ERROR_INVALID_ARGUMENT, MigoResult, VersionedHeader,
    copy_versioned,
};

pub type MigoTaskFn = unsafe extern "C" fn(*mut c_void);
pub type MigoDispatchFn = unsafe extern "C" fn(*mut c_void, MigoTaskFn, *mut c_void) -> MigoResult;
pub type MigoOnReadyFn = unsafe extern "C" fn(*mut c_void, *mut c_void);
pub type MigoOnErrorFn = unsafe extern "C" fn(*mut c_void, *mut c_void, *const MigoError);
pub type MigoOnExitRequestedFn = unsafe extern "C" fn(*mut c_void, *mut c_void);
pub type MigoOnSurfaceLostFn = unsafe extern "C" fn(*mut c_void, *mut c_void, u64, u32);
pub type MigoOnSurfaceReleasedFn = unsafe extern "C" fn(*mut c_void, *mut c_void, u64);
pub type MigoOnRequestFrameFn = unsafe extern "C" fn(*mut c_void, *mut c_void);
pub type MigoOnShowKeyboardFn =
    unsafe extern "C" fn(*mut c_void, *mut c_void, *const MigoKeyboardShowOptions);
pub type MigoOnHideKeyboardFn = unsafe extern "C" fn(*mut c_void, *mut c_void);
pub type MigoOnUpdateKeyboardFn =
    unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_char, u32);
pub type MigoOnVibrateFn = unsafe extern "C" fn(*mut c_void, *mut c_void, u32);
pub type MigoOnKeepScreenOnFn = unsafe extern "C" fn(*mut c_void, *mut c_void, u8);
pub type MigoOnGameLogFn = unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_char, u32);

pub const MIGO_VIBRATION_SHORT_LIGHT: u32 = 0;
pub const MIGO_VIBRATION_SHORT_MEDIUM: u32 = 1;
pub const MIGO_VIBRATION_SHORT_HEAVY: u32 = 2;
pub const MIGO_VIBRATION_LONG: u32 = 3;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MigoError {
    pub header: VersionedHeader,
    pub code: MigoResult,
    pub flags: u32,
    pub message_utf8: *const c_char,
    pub message_length: u32,
    pub reserved0: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MigoKeyboardShowOptions {
    pub header: VersionedHeader,
    pub flags: u32,
    pub max_length: u32,
    pub confirm_type: u32,
    pub keyboard_type: u32,
    pub default_value_utf8: *const c_char,
    pub default_value_length: u32,
    pub reserved0: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MigoHostCallbacks {
    pub header: VersionedHeader,
    pub user_data: *mut c_void,
    pub dispatcher_data: *mut c_void,
    pub dispatch: Option<MigoDispatchFn>,
    pub on_ready: Option<MigoOnReadyFn>,
    pub on_error: Option<MigoOnErrorFn>,
    pub on_exit_requested: Option<MigoOnExitRequestedFn>,
    pub on_surface_lost: Option<MigoOnSurfaceLostFn>,
    pub on_request_frame: Option<MigoOnRequestFrameFn>,
    pub on_show_keyboard: Option<MigoOnShowKeyboardFn>,
    pub on_hide_keyboard: Option<MigoOnHideKeyboardFn>,
    pub on_update_keyboard: Option<MigoOnUpdateKeyboardFn>,
    /// Appended after the ABI v1 keyboard prefix. It is a wakeup edge only;
    /// release queries remain authoritative.
    pub on_surface_released: Option<MigoOnSurfaceReleasedFn>,
    /// Appended device capabilities, each optional and independent.
    pub on_vibrate: Option<MigoOnVibrateFn>,
    pub on_keep_screen_on: Option<MigoOnKeepScreenOnFn>,
    pub on_game_log: Option<MigoOnGameLogFn>,
}

// SAFETY: the record consists only of integers, raw pointers and nullable C
// function pointers. Its historical minimum ends after dispatch; all later
// callbacks were append-only optional fields.
unsafe impl AbiStruct for MigoHostCallbacks {
    // Derived, not written down: `on_ready` is the first append-only field, so
    // its offset *is* "ends after dispatch". The literal `32` this replaces was
    // right on LP64 and wrong on ILP32, where the three pointers ahead of
    // `dispatch` are four bytes each and the true minimum is 20 -- so a 32-bit
    // host passing a struct that fully contains `dispatch` was refused with
    // `MIGO_ERROR_INVALID_ARGUMENT`, which reads as a fault in the caller.
    // `MigoKeyEvent` already derives its minimum this way; this impl was the
    // outlier.
    const MINIMUM_SIZE: usize = offset_of!(MigoHostCallbacks, on_ready);
}

// Pins the LP64 value the literal used to state, so the derivation cannot
// silently move the minimum on the platform every existing host is built for.
#[cfg(target_pointer_width = "64")]
const _: () = assert!(<MigoHostCallbacks as AbiStruct>::MINIMUM_SIZE == 32);
// The ILP32 counterpart: an 8-byte header, three 4-byte pointers, then the
// 4-byte `dispatch`.
#[cfg(target_pointer_width = "32")]
const _: () = assert!(<MigoHostCallbacks as AbiStruct>::MINIMUM_SIZE == 20);

/// Callback state retained by a Session after caller memory is released.
#[derive(Clone, Copy, Debug)]
pub struct ValidatedHostCallbacks {
    pub user_data: *mut c_void,
    pub dispatcher_data: *mut c_void,
    pub dispatch: MigoDispatchFn,
    pub on_ready: Option<MigoOnReadyFn>,
    pub on_error: Option<MigoOnErrorFn>,
    pub on_exit_requested: Option<MigoOnExitRequestedFn>,
    pub on_surface_lost: Option<MigoOnSurfaceLostFn>,
    pub on_request_frame: Option<MigoOnRequestFrameFn>,
    pub on_show_keyboard: Option<MigoOnShowKeyboardFn>,
    pub on_hide_keyboard: Option<MigoOnHideKeyboardFn>,
    pub on_update_keyboard: Option<MigoOnUpdateKeyboardFn>,
    pub on_surface_released: Option<MigoOnSurfaceReleasedFn>,
    pub on_vibrate: Option<MigoOnVibrateFn>,
    pub on_keep_screen_on: Option<MigoOnKeepScreenOnFn>,
    pub on_game_log: Option<MigoOnGameLogFn>,
}

// SAFETY: both pointers are opaque host tokens. Migo never dereferences them;
// it only returns them to the host's own dispatcher/callback functions, whose
// lifetime contract covers the configured Session.
unsafe impl Send for ValidatedHostCallbacks {}
// SAFETY: identical to Send; copied tokens are never dereferenced by Rust.
unsafe impl Sync for ValidatedHostCallbacks {}

impl ValidatedHostCallbacks {
    #[inline]
    pub fn supplies_keyboard(&self) -> bool {
        self.on_show_keyboard.is_some()
    }

    #[inline]
    pub fn drives_frames(&self) -> bool {
        self.on_request_frame.is_some()
    }
}

impl MigoHostCallbacks {
    /// A complete current-version record with no configured dispatcher or
    /// callbacks. Useful to hosts that fill selected fields incrementally.
    pub fn empty() -> Self {
        Self {
            header: VersionedHeader {
                struct_size: size_of::<Self>() as u32,
                abi_version: MIGO_ABI_VERSION_CURRENT,
            },
            user_data: std::ptr::null_mut(),
            dispatcher_data: std::ptr::null_mut(),
            dispatch: None,
            on_ready: None,
            on_error: None,
            on_exit_requested: None,
            on_surface_lost: None,
            on_request_frame: None,
            on_show_keyboard: None,
            on_hide_keyboard: None,
            on_update_keyboard: None,
            on_surface_released: None,
            on_vibrate: None,
            on_keep_screen_on: None,
            on_game_log: None,
        }
    }

    /// Copy and validate a caller-owned callback record.
    ///
    /// # Safety
    /// `callbacks` must be null or readable for its announced byte count.
    pub unsafe fn parse(
        callbacks: *const Self,
    ) -> Result<Option<ValidatedHostCallbacks>, MigoResult> {
        // SAFETY: forwarded from the public parser contract.
        let raw = unsafe { copy_versioned::<Self>(callbacks.cast::<VersionedHeader>()) }?;
        raw.validate_fields()
    }

    /// Validate a local callback record, respecting its announced prefix.
    pub fn validate(&self) -> Result<Option<ValidatedHostCallbacks>, MigoResult> {
        // SAFETY: a reference to Self supplies a full local allocation; the
        // announced prefix is checked before it is copied.
        let raw =
            unsafe { copy_versioned::<Self>((self as *const Self).cast::<VersionedHeader>()) }?;
        raw.validate_fields()
    }

    fn validate_fields(self) -> Result<Option<ValidatedHostCallbacks>, MigoResult> {
        let keyboard_verbs = self.on_show_keyboard.is_some() as u8
            + self.on_hide_keyboard.is_some() as u8
            + self.on_update_keyboard.is_some() as u8;
        if keyboard_verbs != 0 && keyboard_verbs != 3 {
            return Err(MIGO_ERROR_INVALID_ARGUMENT);
        }

        let has_callback = self.on_ready.is_some()
            || self.on_error.is_some()
            || self.on_exit_requested.is_some()
            || self.on_surface_lost.is_some()
            || self.on_request_frame.is_some()
            || self.on_show_keyboard.is_some()
            || self.on_hide_keyboard.is_some()
            || self.on_update_keyboard.is_some();
        let has_callback = has_callback
            || self.on_surface_released.is_some()
            || self.on_vibrate.is_some()
            || self.on_keep_screen_on.is_some()
            || self.on_game_log.is_some();
        let Some(dispatch) = self.dispatch else {
            return if has_callback {
                Err(MIGO_ERROR_INVALID_ARGUMENT)
            } else {
                Ok(None)
            };
        };

        Ok(Some(ValidatedHostCallbacks {
            user_data: self.user_data,
            dispatcher_data: self.dispatcher_data,
            dispatch,
            on_ready: self.on_ready,
            on_error: self.on_error,
            on_exit_requested: self.on_exit_requested,
            on_surface_lost: self.on_surface_lost,
            on_request_frame: self.on_request_frame,
            on_show_keyboard: self.on_show_keyboard,
            on_hide_keyboard: self.on_hide_keyboard,
            on_update_keyboard: self.on_update_keyboard,
            on_surface_released: self.on_surface_released,
            on_vibrate: self.on_vibrate,
            on_keep_screen_on: self.on_keep_screen_on,
            on_game_log: self.on_game_log,
        }))
    }
}

const _: () = assert!(offset_of!(MigoError, header) == 0);
const _: () = assert!(offset_of!(MigoKeyboardShowOptions, header) == 0);
const _: () = assert!(offset_of!(MigoHostCallbacks, header) == 0);

#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<MigoError>() == 32);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<MigoKeyboardShowOptions>() == 40);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(offset_of!(MigoKeyboardShowOptions, default_value_utf8) == 24);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<MigoHostCallbacks>() == 128);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(offset_of!(MigoHostCallbacks, dispatch) == 24);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(offset_of!(MigoHostCallbacks, on_request_frame) == 64);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(offset_of!(MigoHostCallbacks, on_update_keyboard) == 88);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(offset_of!(MigoHostCallbacks, on_surface_released) == 96);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(offset_of!(MigoHostCallbacks, on_game_log) == 120);

#[cfg(target_pointer_width = "32")]
const _: () = assert!(size_of::<MigoError>() == 28);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(size_of::<MigoKeyboardShowOptions>() == 36);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(offset_of!(MigoKeyboardShowOptions, default_value_utf8) == 24);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(size_of::<MigoHostCallbacks>() == 68);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(offset_of!(MigoHostCallbacks, dispatch) == 16);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(offset_of!(MigoHostCallbacks, on_request_frame) == 36);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(offset_of!(MigoHostCallbacks, on_update_keyboard) == 48);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(offset_of!(MigoHostCallbacks, on_surface_released) == 52);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(offset_of!(MigoHostCallbacks, on_game_log) == 64);
