//! The v1 wire shape, pinned.
//!
//! Every struct here crosses the ABI boundary, so its size and the offset of
//! every field are part of the contract — not implementation detail a refactor
//! may change. A host compiled against v1 headers writes these exact bytes; a
//! library that lays them out differently reads one field's bits as another's
//! and reports success while doing it.
//!
//! These are `const` assertions rather than tests: a layout break should fail
//! the build of the crate that caused it, at the moment it is introduced,
//! rather than wait for a test run — and rather than wait for a v1 client to
//! meet the changed library in the field, which is when it would otherwise be
//! discovered.
//!
//! The pointer-bearing size numbers in this module are LP64
//! (`linux-x86_64` and `aarch64-linux-android`, the runtime slices that ship).
//! The dependency-free `migo-capi-abi` crate pins the corresponding Rust ILP32
//! callback records, while `tests/c_abi` compiles the public C/C++ records for
//! SysV ILP32, Windows x86 and LLP64. Pointer-free assertions below hold on
//! every target and are checked wherever this crate builds.

use std::mem::{align_of, offset_of, size_of};

use crate::{
    MigoContentDescriptor, MigoEngineConfig, MigoSessionConfig,
    callbacks::{MigoError, MigoHostCallbacks, MigoKeyboardShowOptions},
    capabilities::MigoCapabilities,
    gamepad::{MigoGamepadButton, MigoGamepadInfo, MigoGamepadStateEvent},
    input::{MigoPointerEvent, MigoTouchEvent, MigoTouchPoint, MigoWheelEvent},
    keyboard::{MigoCompositionEvent, MigoKeyEvent, MigoKeyboardEvent},
    surface::{
        MigoAndroidNativeWindowDescriptor, MigoSurfaceDescriptor, MigoSurfaceMetrics,
        MigoSurfaceReleaseStatus, MigoWaylandSurfaceDescriptor, MigoWin32HwndDescriptor,
        MigoX11WindowDescriptor,
    },
};
use migo_capi_abi::VersionedHeader;

/// Every versioned struct must begin with its header.
///
/// `validate_header` casts a caller's pointer to `*const VersionedHeader` and
/// reads `struct_size` before it trusts anything else. That cast is only sound
/// while the header is the first member: a struct that ever grew a field in
/// front of it would have its own bytes read as a size and a version, and the
/// validation that exists to catch a mismatched caller would be the thing
/// producing one.
macro_rules! header_is_first {
    ($($type:ty),+ $(,)?) => {$(
        const _: () = assert!(offset_of!($type, header) == 0);
        const _: () = assert!(align_of::<$type>() >= align_of::<VersionedHeader>());
    )+};
}

header_is_first!(
    MigoCapabilities,
    MigoEngineConfig,
    MigoSessionConfig,
    MigoContentDescriptor,
    MigoHostCallbacks,
    MigoError,
    MigoSurfaceDescriptor,
    MigoSurfaceMetrics,
    MigoSurfaceReleaseStatus,
    MigoAndroidNativeWindowDescriptor,
    MigoWin32HwndDescriptor,
    MigoX11WindowDescriptor,
    MigoWaylandSurfaceDescriptor,
    MigoTouchEvent,
    MigoPointerEvent,
    MigoWheelEvent,
    MigoKeyboardEvent,
    MigoKeyboardShowOptions,
    MigoKeyEvent,
    MigoGamepadInfo,
    MigoGamepadStateEvent,
    MigoCompositionEvent,
);

// The header itself. Two `u32`s, in this order, on every target: it is the one
// shape a caller must get right before anything else can be negotiated.
const _: () = assert!(size_of::<VersionedHeader>() == 8);
const _: () = assert!(offset_of!(VersionedHeader, struct_size) == 0);
const _: () = assert!(offset_of!(VersionedHeader, abi_version) == 4);

// `MigoTouchPoint` has no header: it is an array element, not a versioned
// struct, and its count comes from the event that points at it. Its layout is
// load-bearing anyway — `to_touch_data` reinterprets an array of these as the
// engine's own point type — so every field is pinned here, on every target,
// since it contains no pointer.
const _: () = assert!(size_of::<MigoTouchPoint>() == 20);
const _: () = assert!(offset_of!(MigoTouchPoint, id) == 0);
const _: () = assert!(offset_of!(MigoTouchPoint, x) == 4);
const _: () = assert!(offset_of!(MigoTouchPoint, y) == 8);
const _: () = assert!(offset_of!(MigoTouchPoint, pressure) == 12);
const _: () = assert!(offset_of!(MigoTouchPoint, flags) == 16);

// `MigoGamepadButton` likewise has no header: it is an array element whose count
// comes from the sample that points at it. Pointer-free, so pinned everywhere.
const _: () = assert!(size_of::<MigoGamepadButton>() == 8);
const _: () = assert!(offset_of!(MigoGamepadButton, flags) == 0);
const _: () = assert!(offset_of!(MigoGamepadButton, value) == 4);

// The negotiation entry point's own shape. It carries no pointer, so it is
// identical on every target -- which matters more here than anywhere else: a
// caller uses this struct to find out what the library supports, so it is the
// one shape that has to be right before any other can be checked.
const _: () = assert!(size_of::<MigoCapabilities>() == 24);
const _: () = assert!(offset_of!(MigoCapabilities, abi_version_min) == 8);
const _: () = assert!(offset_of!(MigoCapabilities, abi_version_max) == 12);
const _: () = assert!(offset_of!(MigoCapabilities, platform_kinds) == 16);

// Pointer-free structs: identical on ILP32 and LP64, so they are pinned for
// every target rather than only the ones that ship today.
const _: () = assert!(size_of::<MigoSessionConfig>() == 32);
const _: () = assert!(offset_of!(MigoSessionConfig, flags) == 8);
// Appended after v1. It is at the end because that is what makes the growth
// backward compatible: a caller passing the 16-byte record is zero-extended and
// gets no identity, which is the right answer for a host with no external
// producer to authenticate.
const _: () = assert!(offset_of!(MigoSessionConfig, launch_nonce) == 16);

const _: () = assert!(size_of::<MigoSurfaceMetrics>() == 48);
const _: () = assert!(offset_of!(MigoSurfaceMetrics, generation) == 8);
const _: () = assert!(offset_of!(MigoSurfaceMetrics, width_pixels) == 16);
const _: () = assert!(offset_of!(MigoSurfaceMetrics, height_pixels) == 20);
const _: () = assert!(offset_of!(MigoSurfaceMetrics, scale_factor) == 24);
const _: () = assert!(offset_of!(MigoSurfaceMetrics, color_space) == 28);
const _: () = assert!(offset_of!(MigoSurfaceMetrics, alpha_mode) == 32);
const _: () = assert!(offset_of!(MigoSurfaceMetrics, preferred_presentation_mode) == 36);
const _: () = assert!(offset_of!(MigoSurfaceMetrics, flags) == 40);

const _: () = assert!(size_of::<MigoSurfaceReleaseStatus>() == 24);
const _: () = assert!(offset_of!(MigoSurfaceReleaseStatus, generation) == 8);
const _: () = assert!(offset_of!(MigoSurfaceReleaseStatus, state) == 16);
const _: () = assert!(offset_of!(MigoSurfaceReleaseStatus, reserved0) == 20);

// Two u32s then two f32s then an f64: the f64 forces 8-alignment, so the four
// 4-byte fields fill the first two slots exactly and there is no tail padding
// to absorb a reordering. Swapping `x` and `button` -- a u32 and an f32 that
// both sit in the second slot -- would leave the size at 32 and deliver a
// coordinate as a button ordinal.
const _: () = assert!(size_of::<MigoPointerEvent>() == 32);
const _: () = assert!(offset_of!(MigoPointerEvent, event_type) == 8);
const _: () = assert!(offset_of!(MigoPointerEvent, button) == 12);
const _: () = assert!(offset_of!(MigoPointerEvent, x) == 16);
const _: () = assert!(offset_of!(MigoPointerEvent, y) == 20);
const _: () = assert!(offset_of!(MigoPointerEvent, timestamp_ms) == 24);

// `reserved0` exists to pad `delta_mode` out to the f64 alignment rather than
// leaving an unnamed hole: a caller must write it as zero, and validation
// rejects it otherwise, so the bytes stay available for a later meaning.
const _: () = assert!(size_of::<MigoWheelEvent>() == 48);
const _: () = assert!(offset_of!(MigoWheelEvent, delta_mode) == 8);
const _: () = assert!(offset_of!(MigoWheelEvent, reserved0) == 12);
const _: () = assert!(offset_of!(MigoWheelEvent, delta_x) == 16);
const _: () = assert!(offset_of!(MigoWheelEvent, delta_y) == 24);
const _: () = assert!(offset_of!(MigoWheelEvent, delta_z) == 32);
const _: () = assert!(offset_of!(MigoWheelEvent, timestamp_ms) == 40);

/// LP64 sizes for the structs that contain a pointer.
///
/// Split out so the pointer-free assertions above still run on a 32-bit target
/// and only the numbers that genuinely depend on pointer width are skipped.
#[cfg(target_pointer_width = "64")]
mod lp64 {
    use super::*;

    const _: () = assert!(size_of::<MigoEngineConfig>() == 80);
    const _: () = assert!(offset_of!(MigoEngineConfig, code_signing_public_key) == 48);
    const _: () = assert!(offset_of!(MigoEngineConfig, flags) == 8);
    const _: () = assert!(offset_of!(MigoEngineConfig, reserved0) == 16);
    // A four-byte hole follows `reserved0`; the first pointer is aligned to 24.
    // The header must agree, which is why the hole is asserted rather than left
    // to be rediscovered by whoever adds a field into it.
    const _: () = assert!(offset_of!(MigoEngineConfig, files_dir_utf8) == 24);
    const _: () = assert!(offset_of!(MigoEngineConfig, cache_dir_utf8) == 32);
    const _: () = assert!(offset_of!(MigoEngineConfig, code_cache_dir_utf8) == 40);

    const _: () = assert!(size_of::<MigoContentDescriptor>() == 32);
    const _: () = assert!(offset_of!(MigoContentDescriptor, flags) == 8);
    const _: () = assert!(offset_of!(MigoContentDescriptor, reserved0) == 12);
    const _: () = assert!(offset_of!(MigoContentDescriptor, content_id_utf8) == 16);
    const _: () = assert!(offset_of!(MigoContentDescriptor, entry_utf8) == 24);

    const _: () = assert!(size_of::<MigoError>() == 32);
    const _: () = assert!(offset_of!(MigoError, code) == 8);
    const _: () = assert!(offset_of!(MigoError, flags) == 12);
    const _: () = assert!(offset_of!(MigoError, message_utf8) == 16);
    const _: () = assert!(offset_of!(MigoError, message_length) == 24);
    const _: () = assert!(offset_of!(MigoError, reserved0) == 28);

    // Function pointers are `Option<fn>` on the Rust side, which is a plain
    // nullable pointer with no discriminant. Pinning the offsets is what keeps
    // that niche optimisation from being an assumption.
    const _: () = assert!(size_of::<MigoHostCallbacks>() == 128);
    const _: () = assert!(offset_of!(MigoHostCallbacks, user_data) == 8);
    const _: () = assert!(offset_of!(MigoHostCallbacks, dispatcher_data) == 16);
    const _: () = assert!(offset_of!(MigoHostCallbacks, dispatch) == 24);
    const _: () = assert!(offset_of!(MigoHostCallbacks, on_ready) == 32);
    const _: () = assert!(offset_of!(MigoHostCallbacks, on_error) == 40);
    const _: () = assert!(offset_of!(MigoHostCallbacks, on_exit_requested) == 48);
    const _: () = assert!(offset_of!(MigoHostCallbacks, on_surface_lost) == 56);
    const _: () = assert!(offset_of!(MigoHostCallbacks, on_request_frame) == 64);
    const _: () = assert!(offset_of!(MigoHostCallbacks, on_show_keyboard) == 72);
    const _: () = assert!(offset_of!(MigoHostCallbacks, on_hide_keyboard) == 80);
    const _: () = assert!(offset_of!(MigoHostCallbacks, on_update_keyboard) == 88);
    const _: () = assert!(offset_of!(MigoHostCallbacks, on_surface_released) == 96);
    const _: () = assert!(offset_of!(MigoHostCallbacks, on_vibrate) == 104);
    const _: () = assert!(offset_of!(MigoHostCallbacks, on_keep_screen_on) == 112);
    const _: () = assert!(offset_of!(MigoHostCallbacks, on_game_log) == 120);

    const _: () = assert!(size_of::<MigoKeyboardShowOptions>() == 40);
    const _: () = assert!(offset_of!(MigoKeyboardShowOptions, flags) == 8);
    const _: () = assert!(offset_of!(MigoKeyboardShowOptions, max_length) == 12);
    const _: () = assert!(offset_of!(MigoKeyboardShowOptions, confirm_type) == 16);
    const _: () = assert!(offset_of!(MigoKeyboardShowOptions, keyboard_type) == 20);
    const _: () = assert!(offset_of!(MigoKeyboardShowOptions, default_value_utf8) == 24);
    const _: () = assert!(offset_of!(MigoKeyboardShowOptions, default_value_length) == 32);
    const _: () = assert!(offset_of!(MigoKeyboardShowOptions, reserved0) == 36);

    const _: () = assert!(size_of::<MigoSurfaceDescriptor>() == 72);
    const _: () = assert!(offset_of!(MigoSurfaceDescriptor, generation) == 8);
    const _: () = assert!(offset_of!(MigoSurfaceDescriptor, platform_kind) == 16);
    const _: () = assert!(offset_of!(MigoSurfaceDescriptor, width_pixels) == 24);
    const _: () = assert!(offset_of!(MigoSurfaceDescriptor, scale_factor) == 32);
    // `capability_flags` is the first u64 after a run of u32s, so it pulls the
    // rest of the struct to 8-byte alignment; the two u32s that follow share a
    // slot and the pointer lands at 64.
    const _: () = assert!(offset_of!(MigoSurfaceDescriptor, capability_flags) == 48);
    const _: () = assert!(offset_of!(MigoSurfaceDescriptor, platform_descriptor_size) == 56);
    const _: () = assert!(offset_of!(MigoSurfaceDescriptor, platform_descriptor) == 64);

    const _: () = assert!(size_of::<MigoAndroidNativeWindowDescriptor>() == 24);
    const _: () = assert!(offset_of!(MigoAndroidNativeWindowDescriptor, native_window) == 16);

    const _: () = assert!(size_of::<MigoWin32HwndDescriptor>() == 24);
    const _: () = assert!(offset_of!(MigoWin32HwndDescriptor, platform_kind) == 8);
    const _: () = assert!(offset_of!(MigoWin32HwndDescriptor, flags) == 12);
    const _: () = assert!(offset_of!(MigoWin32HwndDescriptor, hwnd) == 16);

    const _: () = assert!(size_of::<MigoWaylandSurfaceDescriptor>() == 32);
    const _: () = assert!(offset_of!(MigoWaylandSurfaceDescriptor, platform_kind) == 8);
    const _: () = assert!(offset_of!(MigoWaylandSurfaceDescriptor, flags) == 12);
    const _: () = assert!(offset_of!(MigoWaylandSurfaceDescriptor, display) == 16);
    const _: () = assert!(offset_of!(MigoWaylandSurfaceDescriptor, surface) == 24);

    const _: () = assert!(size_of::<MigoX11WindowDescriptor>() == 40);
    const _: () = assert!(offset_of!(MigoX11WindowDescriptor, display) == 16);
    const _: () = assert!(offset_of!(MigoX11WindowDescriptor, window) == 24);
    const _: () = assert!(offset_of!(MigoX11WindowDescriptor, screen) == 32);

    const _: () = assert!(size_of::<MigoTouchEvent>() == 32);
    const _: () = assert!(offset_of!(MigoTouchEvent, touch_type) == 8);
    const _: () = assert!(offset_of!(MigoTouchEvent, point_count) == 12);
    const _: () = assert!(offset_of!(MigoTouchEvent, timestamp_ms) == 16);
    const _: () = assert!(offset_of!(MigoTouchEvent, points) == 24);

    // `height_css_px` is an f64 after a pointer, so nothing here shares a slot;
    // a reordering that put the two u32s after the pointer would still size to
    // 32 and silently read the value pointer's high half as a length.
    const _: () = assert!(size_of::<MigoKeyboardEvent>() == 32);
    const _: () = assert!(offset_of!(MigoKeyboardEvent, event_type) == 8);
    const _: () = assert!(offset_of!(MigoKeyboardEvent, value_length) == 12);
    const _: () = assert!(offset_of!(MigoKeyboardEvent, value_utf8) == 16);
    const _: () = assert!(offset_of!(MigoKeyboardEvent, height_css_px) == 24);

    // Two pointers with a u32 on either side: `code_length` and `reserved0`
    // share the slot after the second pointer, and the f64 lands last. A
    // reordering that put both lengths together would still size to 48 while
    // moving every field the copy depends on.
    const _: () = assert!(size_of::<MigoKeyEvent>() == 56);
    const _: () = assert!(offset_of!(MigoKeyEvent, event_type) == 8);
    const _: () = assert!(offset_of!(MigoKeyEvent, key_length) == 12);
    const _: () = assert!(offset_of!(MigoKeyEvent, key_utf8) == 16);
    const _: () = assert!(offset_of!(MigoKeyEvent, code_utf8) == 24);
    const _: () = assert!(offset_of!(MigoKeyEvent, code_length) == 32);
    const _: () = assert!(offset_of!(MigoKeyEvent, reserved0) == 36);
    const _: () = assert!(offset_of!(MigoKeyEvent, timestamp_ms) == 40);
    // Appended after the record shipped; `reserved0` above stays reserved.
    const _: () = assert!(offset_of!(MigoKeyEvent, modifiers) == 48);
    const _: () = assert!(offset_of!(MigoKeyEvent, flags) == 52);

    const _: () = assert!(size_of::<MigoCompositionEvent>() == 24);
    const _: () = assert!(offset_of!(MigoCompositionEvent, event_type) == 8);
    const _: () = assert!(offset_of!(MigoCompositionEvent, data_length) == 12);
    const _: () = assert!(offset_of!(MigoCompositionEvent, data_utf8) == 16);

    const _: () = assert!(size_of::<MigoGamepadInfo>() == 40);
    const _: () = assert!(offset_of!(MigoGamepadInfo, index) == 8);
    const _: () = assert!(offset_of!(MigoGamepadInfo, axis_count) == 12);
    const _: () = assert!(offset_of!(MigoGamepadInfo, button_count) == 16);
    const _: () = assert!(offset_of!(MigoGamepadInfo, id_utf8) == 24);
    const _: () = assert!(offset_of!(MigoGamepadInfo, mapping_utf8) == 32);

    const _: () = assert!(size_of::<MigoGamepadStateEvent>() == 48);
    const _: () = assert!(offset_of!(MigoGamepadStateEvent, index) == 8);
    const _: () = assert!(offset_of!(MigoGamepadStateEvent, axes) == 24);
    const _: () = assert!(offset_of!(MigoGamepadStateEvent, buttons) == 32);
    const _: () = assert!(offset_of!(MigoGamepadStateEvent, timestamp_ms) == 40);
}
