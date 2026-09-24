//! Fixtures shared by the crate's test modules.
//!
//! They live here rather than in one module's `mod tests` so the surface tests
//! and the lifecycle tests build their engines and sessions the same way. Two
//! copies of a fixture drift, and a drifted fixture makes two tests that look
//! identical prove different things.

use std::{
    ffi::CString,
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize},
    },
};

use crate::{
    EngineInner, MigoEngine, MigoEngineConfig, MigoSession, MigoSessionConfig, SessionState,
    migo_engine_create, migo_engine_destroy, migo_session_create, migo_session_destroy,
};
use migo_capi_abi::{MIGO_ABI_VERSION_CURRENT, MIGO_OK, VersionedHeader};

/// A Session allocation for callback-routing unit tests that do not need a
/// public raw owner or a running Host.
pub(crate) fn callback_session_pin() -> Arc<MigoSession> {
    Arc::new(MigoSession {
        // No external producer in these fixtures.
        launch_nonce: None,
        engine: Arc::new(EngineInner {
            files_dir: PathBuf::new(),
            cache_dir: PathBuf::new(),
            code_cache_dir: PathBuf::new(),
            allow_unsigned_content: false,
            code_signing_public_key: None,
            live_sessions: Mutex::new(0),
            retired_hosts: crate::retirement::RetirementSet::new(),
        }),
        state: Mutex::new(SessionState::default()),
        callback_gate: crate::callback_gate::CallbackGate::new(),
        pending_surface_releases: Arc::new(AtomicUsize::new(0)),
        ingress: OnceLock::new(),
        active_surface_generation: AtomicU64::new(0),
        gamepad_topology: crate::gamepad::GamepadTopology::new(),
        input_saturation_reported: AtomicBool::new(false),
    })
}

pub(crate) fn engine_config(
    dirs: &(CString, CString, CString),
    struct_size: u32,
    abi_version: u32,
) -> MigoEngineConfig {
    MigoEngineConfig {
        header: VersionedHeader {
            struct_size,
            abi_version,
        },
        flags: 0,
        reserved0: 0,
        files_dir_utf8: dirs.0.as_ptr(),
        cache_dir_utf8: dirs.1.as_ptr(),
        code_cache_dir_utf8: dirs.2.as_ptr(),
        code_signing_public_key: [0; 32],
    }
}

/// Storage roots under the process's temp dir, keyed by `tag` so concurrently
/// running tests do not share state.
pub(crate) fn scratch_dirs(tag: &str) -> (CString, CString, CString) {
    let root = std::env::temp_dir().join(format!("migo-capi-test-{tag}-{}", std::process::id()));
    let make =
        |name: &str| CString::new(root.join(name).to_str().expect("utf-8 path")).expect("cstring");
    (make("files"), make("cache"), make("code-cache"))
}

pub(crate) fn session_config() -> MigoSessionConfig {
    MigoSessionConfig {
        header: VersionedHeader {
            struct_size: size_of::<MigoSessionConfig>() as u32,
            abi_version: MIGO_ABI_VERSION_CURRENT,
        },
        flags: 0,
        // No external producer in these tests, so no identity to pair with one.
        launch_nonce: [0u8; 16],
    }
}

/// Create an engine, run `body`, then destroy it.
pub(crate) fn with_engine(tag: &str, body: impl FnOnce(*mut MigoEngine)) {
    let dirs = scratch_dirs(tag);
    let config = engine_config(
        &dirs,
        size_of::<MigoEngineConfig>() as u32,
        MIGO_ABI_VERSION_CURRENT,
    );
    let mut engine: *mut MigoEngine = std::ptr::null_mut();
    assert_eq!(unsafe { migo_engine_create(&config, &mut engine) }, MIGO_OK);
    assert!(!engine.is_null());
    body(engine);
    assert_eq!(unsafe { migo_engine_destroy(engine) }, MIGO_OK);
}

/// Create an engine and one session, run `body`, then tear both down in order.
///
/// The session is real rather than a stand-in: the surface entry points read
/// engine configuration through it, so a hand-built session would not exercise
/// the same field accesses.
pub(crate) fn with_session(tag: &str, body: impl FnOnce(*mut MigoSession)) {
    with_engine(tag, |engine| {
        let config = session_config();
        let mut session: *mut MigoSession = std::ptr::null_mut();
        assert_eq!(
            unsafe { migo_session_create(engine, &config, &mut session) },
            MIGO_OK
        );
        assert!(!session.is_null());
        body(session);
        assert_eq!(unsafe { migo_session_destroy(session) }, MIGO_OK);
    });
}
