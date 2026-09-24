use std::{ffi::CString, ffi::c_void, mem::size_of, os::raw::c_char};

use migo_capi_abi::{
    MIGO_ABI_VERSION_CURRENT, MIGO_ERROR_INVALID_ARGUMENT, MIGO_OK, MigoResult, VersionedHeader,
    callbacks::{MigoError, MigoHostCallbacks, MigoKeyboardShowOptions, MigoTaskFn},
    config::{
        MIGO_ENGINE_FLAG_ALLOW_UNSIGNED_CONTENT, MigoContentDescriptor, MigoEngineConfig,
        MigoSessionConfig,
    },
};

fn header<T>() -> VersionedHeader {
    VersionedHeader {
        struct_size: size_of::<T>() as u32,
        abi_version: MIGO_ABI_VERSION_CURRENT,
    }
}

unsafe extern "C" fn dispatch(
    _dispatcher: *mut c_void,
    _task: MigoTaskFn,
    _context: *mut c_void,
) -> MigoResult {
    MIGO_OK
}

unsafe extern "C" fn on_ready(_user: *mut c_void, _session: *mut c_void) {}
unsafe extern "C" fn on_error(_user: *mut c_void, _session: *mut c_void, _error: *const MigoError) {
}
unsafe extern "C" fn on_exit(_user: *mut c_void, _session: *mut c_void) {}
unsafe extern "C" fn on_surface_lost(
    _user: *mut c_void,
    _session: *mut c_void,
    _generation: u64,
    _reason: u32,
) {
}
unsafe extern "C" fn on_surface_released(
    _user: *mut c_void,
    _session: *mut c_void,
    _generation: u64,
) {
}
unsafe extern "C" fn on_request_frame(_user: *mut c_void, _session: *mut c_void) {}
unsafe extern "C" fn on_show_keyboard(
    _user: *mut c_void,
    _session: *mut c_void,
    _options: *const MigoKeyboardShowOptions,
) {
}
unsafe extern "C" fn on_hide_keyboard(_user: *mut c_void, _session: *mut c_void) {}
unsafe extern "C" fn on_update_keyboard(
    _user: *mut c_void,
    _session: *mut c_void,
    _value: *const c_char,
    _length: u32,
) {
}

#[test]
fn engine_config_accepts_only_known_flags_and_zero_reserved_storage() {
    let files = CString::new("files").unwrap();
    let cache = CString::new("cache").unwrap();
    let code_cache = CString::new("code-cache").unwrap();
    let mut raw = MigoEngineConfig {
        header: header::<MigoEngineConfig>(),
        flags: MIGO_ENGINE_FLAG_ALLOW_UNSIGNED_CONTENT,
        reserved0: 0,
        files_dir_utf8: files.as_ptr(),
        cache_dir_utf8: cache.as_ptr(),
        code_cache_dir_utf8: code_cache.as_ptr(),
        code_signing_public_key: [0; 32],
    };

    let validated = unsafe { raw.validate() }.expect("known engine config");
    assert_eq!(
        validated.code_signing_public_key, None,
        "all zero is no key"
    );
    assert!(validated.allow_unsigned_content);
    assert_eq!(validated.files_dir, "files");
    assert_eq!(validated.cache_dir, "cache");
    assert_eq!(validated.code_cache_dir, "code-cache");

    raw.flags = 1 << 63;
    assert_eq!(
        unsafe { raw.validate() }.unwrap_err(),
        MIGO_ERROR_INVALID_ARGUMENT,
    );
    raw.flags = 0;
    raw.reserved0 = 1;
    assert_eq!(
        unsafe { raw.validate() }.unwrap_err(),
        MIGO_ERROR_INVALID_ARGUMENT,
    );
}

#[test]
fn engine_config_carries_a_signing_key_and_refuses_it_beside_the_unsigned_flag() {
    let files = CString::new("files").unwrap();
    let cache = CString::new("cache").unwrap();
    let code_cache = CString::new("code-cache").unwrap();
    let mut key = [0u8; 32];
    key[0] = 0xa5;
    key[31] = 0x5a;
    let mut raw = MigoEngineConfig {
        header: header::<MigoEngineConfig>(),
        flags: 0,
        reserved0: 0,
        files_dir_utf8: files.as_ptr(),
        cache_dir_utf8: cache.as_ptr(),
        code_cache_dir_utf8: code_cache.as_ptr(),
        code_signing_public_key: key,
    };
    let validated = unsafe { raw.validate() }.expect("a key alone");
    assert_eq!(validated.code_signing_public_key, Some(key));
    assert!(!validated.allow_unsigned_content);

    // "Verify with this key" and "do not verify" at once is refused, not resolved.
    raw.flags = MIGO_ENGINE_FLAG_ALLOW_UNSIGNED_CONTENT;
    assert_eq!(
        unsafe { raw.validate() }.unwrap_err(),
        MIGO_ERROR_INVALID_ARGUMENT
    );
}

#[test]
fn a_v1_engine_config_without_the_key_is_zero_extended() {
    let files = CString::new("files").unwrap();
    let cache = CString::new("cache").unwrap();
    let code_cache = CString::new("code-cache").unwrap();
    let v1_size = std::mem::offset_of!(MigoEngineConfig, code_signing_public_key);
    let mut raw = MigoEngineConfig {
        header: header::<MigoEngineConfig>(),
        flags: MIGO_ENGINE_FLAG_ALLOW_UNSIGNED_CONTENT,
        reserved0: 0,
        files_dir_utf8: files.as_ptr(),
        cache_dir_utf8: cache.as_ptr(),
        code_cache_dir_utf8: code_cache.as_ptr(),
        // Garbage past the announced size must never be read.
        code_signing_public_key: [0xff; 32],
    };
    raw.header.struct_size = v1_size as u32;
    let validated = unsafe { MigoEngineConfig::parse(&raw) }.expect("the v1 record");
    assert_eq!(validated.code_signing_public_key, None);
    assert!(validated.allow_unsigned_content);

    raw.header.struct_size = (v1_size - 8) as u32;
    assert_eq!(
        unsafe { MigoEngineConfig::parse(&raw) }.unwrap_err(),
        MIGO_ERROR_INVALID_ARGUMENT
    );
}

#[test]
fn session_config_has_no_v1_flags() {
    let mut raw = MigoSessionConfig {
        header: header::<MigoSessionConfig>(),
        flags: 0,
        // Absent, which is what a session with no external producer supplies.
        launch_nonce: [0u8; 16],
    };
    raw.validate().expect("zero flags");

    raw.flags = 1;
    assert_eq!(raw.validate().unwrap_err(), MIGO_ERROR_INVALID_ARGUMENT);
}

#[test]
fn content_descriptor_owns_strings_and_rejects_flags_or_reserved_bits() {
    let content_id = CString::new("demo").unwrap();
    let entry = CString::new("game.js").unwrap();
    let mut raw = MigoContentDescriptor {
        header: header::<MigoContentDescriptor>(),
        flags: 0,
        reserved0: 0,
        content_id_utf8: content_id.as_ptr(),
        entry_utf8: entry.as_ptr(),
    };

    let validated = unsafe { raw.validate() }.expect("valid content");
    assert_eq!(validated.content_id, "demo");
    assert_eq!(validated.entry, "game.js");

    raw.flags = 1;
    assert_eq!(
        unsafe { raw.validate() }.unwrap_err(),
        MIGO_ERROR_INVALID_ARGUMENT,
    );
    raw.flags = 0;
    raw.reserved0 = 1;
    assert_eq!(
        unsafe { raw.validate() }.unwrap_err(),
        MIGO_ERROR_INVALID_ARGUMENT,
    );
}

#[test]
fn a_completely_empty_callback_record_is_a_valid_configured_none() {
    let callbacks = MigoHostCallbacks::empty();
    assert!(matches!(callbacks.validate(), Ok(None)));
}

#[test]
fn every_callback_field_participates_in_dispatcher_validation() {
    let mut ready = MigoHostCallbacks::empty();
    ready.on_ready = Some(on_ready);
    let mut error = MigoHostCallbacks::empty();
    error.on_error = Some(on_error);
    let mut exit = MigoHostCallbacks::empty();
    exit.on_exit_requested = Some(on_exit);
    let mut surface_lost = MigoHostCallbacks::empty();
    surface_lost.on_surface_lost = Some(on_surface_lost);
    let mut request_frame = MigoHostCallbacks::empty();
    request_frame.on_request_frame = Some(on_request_frame);
    let mut show_keyboard = MigoHostCallbacks::empty();
    show_keyboard.on_show_keyboard = Some(on_show_keyboard);
    let mut hide_keyboard = MigoHostCallbacks::empty();
    hide_keyboard.on_hide_keyboard = Some(on_hide_keyboard);
    let mut update_keyboard = MigoHostCallbacks::empty();
    update_keyboard.on_update_keyboard = Some(on_update_keyboard);
    let mut surface_released = MigoHostCallbacks::empty();
    surface_released.on_surface_released = Some(on_surface_released);

    for callbacks in [
        ready,
        error,
        exit,
        surface_lost,
        request_frame,
        show_keyboard,
        hide_keyboard,
        update_keyboard,
        surface_released,
    ] {
        assert_eq!(
            callbacks.validate().unwrap_err(),
            MIGO_ERROR_INVALID_ARGUMENT,
        );
    }
}

#[test]
fn the_pre_release_callback_prefix_remains_compatible() {
    // A client built before `on_surface_released` was appended declares a prefix
    // that ends where that final field begins: 96 on LP64, 52 on ILP32. Deriving
    // it keeps the copy in bounds on a 32-bit target instead of overrunning both
    // buffers by the width of the missing tail.
    const PRE_RELEASE_PREFIX: usize = std::mem::offset_of!(MigoHostCallbacks, on_surface_released);

    let mut full = MigoHostCallbacks::empty();
    full.header.struct_size = PRE_RELEASE_PREFIX as u32;
    full.dispatch = Some(dispatch);
    full.on_surface_released = Some(on_surface_released);
    let mut bytes = CallbackBytes([0xA5; size_of::<MigoHostCallbacks>()]);
    unsafe {
        std::ptr::copy_nonoverlapping(
            (&full as *const MigoHostCallbacks).cast::<u8>(),
            bytes.0.as_mut_ptr(),
            PRE_RELEASE_PREFIX,
        );
    }

    let validated = unsafe { MigoHostCallbacks::parse(bytes.0.as_ptr().cast()) }
        .expect("pre-release callback prefix")
        .expect("configured callbacks");
    assert!(validated.on_surface_released.is_none());
}

#[test]
fn keyboard_callbacks_install_all_three_or_none() {
    let mut partial = MigoHostCallbacks::empty();
    partial.dispatch = Some(dispatch);
    partial.on_show_keyboard = Some(on_show_keyboard);
    assert_eq!(partial.validate().unwrap_err(), MIGO_ERROR_INVALID_ARGUMENT,);

    let mut complete = partial;
    complete.on_hide_keyboard = Some(on_hide_keyboard);
    complete.on_update_keyboard = Some(on_update_keyboard);
    let validated = complete
        .validate()
        .expect("complete keyboard set")
        .expect("configured callbacks");
    assert!(validated.supplies_keyboard());
}

#[repr(C, align(8))]
struct CallbackBytes([u8; size_of::<MigoHostCallbacks>()]);

#[test]
fn a_short_pre_keyboard_callback_prefix_is_zero_extended() {
    // The prefix an old (pre-keyboard) client declared ends exactly where the
    // first keyboard callback begins. That offset is 72 on LP64 but only 40 on
    // ILP32, where pointers are 4 bytes -- hardcoding 72 would copy past both
    // the source and this destination on a 32-bit target. Derive it instead.
    const PRE_KEYBOARD_PREFIX: usize = std::mem::offset_of!(MigoHostCallbacks, on_show_keyboard);

    let mut full = MigoHostCallbacks::empty();
    full.header.struct_size = PRE_KEYBOARD_PREFIX as u32;
    full.dispatch = Some(dispatch);
    full.on_request_frame = Some(on_request_frame);
    let mut bytes = CallbackBytes([0xA5; size_of::<MigoHostCallbacks>()]);
    unsafe {
        std::ptr::copy_nonoverlapping(
            (&full as *const MigoHostCallbacks).cast::<u8>(),
            bytes.0.as_mut_ptr(),
            PRE_KEYBOARD_PREFIX,
        );
    }

    let validated = unsafe { MigoHostCallbacks::parse(bytes.0.as_ptr().cast()) }
        .expect("old prefix")
        .expect("configured callbacks");
    assert!(validated.drives_frames());
    assert!(!validated.supplies_keyboard());
}
