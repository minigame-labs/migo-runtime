//! The first behavioural tests that run two Sessions at once.
//!
//! Every concurrent-session property was previously true by reading: no test
//! anywhere created two Sessions, so nothing would have failed if a shared
//! process-global had crept back in. Multi-game support is a product guarantee
//! and is gated rather than assumed, which is what this module starts.
//!
//! What these can and cannot reach: `migo_session_create` produces a Session
//! without an attached surface, so no Host and therefore no V8 isolate exists yet.
//! Isolate separation and per-game storage rooting are therefore *not* covered
//! here -- claiming otherwise would be the same inspection-as-test mistake a
//! documented coverage boundary warns about. What is reachable is the Session lifecycle itself: coexistence,
//! the Engine's refusal to be destroyed under live Sessions, independent teardown,
//! and driving two Sessions from two host threads.

use std::{
    ptr,
    sync::{Arc, Barrier},
};

use migo_capi_abi::{MIGO_ERROR_INVALID_STATE, MIGO_OK};

use crate::{
    MigoEngine, MigoSession, migo_engine_create, migo_engine_destroy, migo_session_create,
    migo_session_destroy,
    test_support::{engine_config, scratch_dirs, session_config, with_engine},
};

/// A raw engine pointer that is safe to move into another thread.
///
/// The C API is explicitly callable from several host threads, and that is the
/// property under test, so the wrapper exists to let the test express it rather
/// than to paper over anything.
#[derive(Clone, Copy)]
struct EnginePtr(*mut MigoEngine);
unsafe impl Send for EnginePtr {}

#[derive(Clone, Copy)]
struct SessionPtr(*mut MigoSession);
unsafe impl Send for SessionPtr {}

/// Accessors rather than direct field reads inside the thread closures. Closure
/// capture is per-field since edition 2021, so writing `engine.0` in the closure
/// captures the raw pointer and the `Send` wrapper never applies; a method call
/// captures the wrapper.
impl EnginePtr {
    fn get(self) -> *mut MigoEngine {
        self.0
    }
}

impl SessionPtr {
    fn get(self) -> *mut MigoSession {
        self.0
    }
}

fn create_engine(tag: &str) -> *mut MigoEngine {
    let dirs = scratch_dirs(tag);
    let config = engine_config(
        &dirs,
        size_of::<crate::MigoEngineConfig>() as u32,
        migo_capi_abi::MIGO_ABI_VERSION_CURRENT,
    );
    let mut engine: *mut MigoEngine = ptr::null_mut();
    assert_eq!(unsafe { migo_engine_create(&config, &mut engine) }, MIGO_OK);
    assert!(!engine.is_null());
    engine
}

fn create_session(engine: *mut MigoEngine) -> *mut MigoSession {
    let config = session_config();
    let mut session: *mut MigoSession = ptr::null_mut();
    assert_eq!(
        unsafe { migo_session_create(engine, &config, &mut session) },
        MIGO_OK
    );
    assert!(!session.is_null());
    session
}

#[test]
fn two_sessions_coexist_on_one_engine() {
    let engine = create_engine("two-sessions-coexist");
    let first = create_session(engine);
    let second = create_session(engine);
    assert_ne!(
        first, second,
        "each session must be its own allocation, not a shared handle"
    );
    assert_eq!(unsafe { migo_session_destroy(first) }, MIGO_OK);
    assert_eq!(unsafe { migo_session_destroy(second) }, MIGO_OK);
    assert_eq!(unsafe { migo_engine_destroy(engine) }, MIGO_OK);
}

/// This is one of the product's gated concurrency requirements.
#[test]
fn engine_destruction_is_refused_while_either_session_is_live() {
    let engine = create_engine("engine-destroy-refused");
    let first = create_session(engine);
    let second = create_session(engine);

    assert_eq!(
        unsafe { migo_engine_destroy(engine) },
        MIGO_ERROR_INVALID_STATE,
        "two live sessions must block destruction"
    );
    assert_eq!(unsafe { migo_session_destroy(first) }, MIGO_OK);
    assert_eq!(
        unsafe { migo_engine_destroy(engine) },
        MIGO_ERROR_INVALID_STATE,
        "one remaining live session must still block destruction"
    );
    assert_eq!(unsafe { migo_session_destroy(second) }, MIGO_OK);
    assert_eq!(
        unsafe { migo_engine_destroy(engine) },
        MIGO_OK,
        "destruction must succeed once the last session is gone"
    );
}

/// Destroying one Session must leave the other fully usable, which is the part a
/// shared-state regression would break: a process-global cleared on the first
/// teardown would take the survivor's state with it.
#[test]
fn destroying_one_session_leaves_the_other_usable() {
    let engine = create_engine("one-session-teardown");
    let doomed = create_session(engine);
    let survivor = create_session(engine);

    assert_eq!(unsafe { migo_session_destroy(doomed) }, MIGO_OK);

    // The survivor still answers, and the engine still refuses to go.
    assert_eq!(
        unsafe { migo_engine_destroy(engine) },
        MIGO_ERROR_INVALID_STATE
    );
    let replacement = create_session(engine);
    assert_ne!(replacement, survivor);

    assert_eq!(unsafe { migo_session_destroy(survivor) }, MIGO_OK);
    assert_eq!(unsafe { migo_session_destroy(replacement) }, MIGO_OK);
    assert_eq!(unsafe { migo_engine_destroy(engine) }, MIGO_OK);
}

/// Two Sessions may be driven concurrently from two host threads. The barrier
/// makes the two creations actually overlap rather than merely happen on
/// different threads, which is what a lock held across session construction
/// would show up as.
#[test]
fn two_sessions_are_created_concurrently_from_two_host_threads() {
    let engine = EnginePtr(create_engine("two-host-threads"));
    let barrier = Arc::new(Barrier::new(2));

    let handles: Vec<_> = (0..2)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                SessionPtr(create_session(engine.get()))
            })
        })
        .collect();

    let sessions: Vec<SessionPtr> = handles
        .into_iter()
        .map(|handle| handle.join().expect("session thread must not panic"))
        .collect();
    assert_ne!(
        sessions[0].0, sessions[1].0,
        "concurrent creation must not hand back one shared session"
    );

    // Teardown also overlaps, each session from its own thread.
    let barrier = Arc::new(Barrier::new(2));
    let teardown: Vec<_> = sessions
        .into_iter()
        .map(|session| {
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                assert_eq!(unsafe { migo_session_destroy(session.get()) }, MIGO_OK);
            })
        })
        .collect();
    for handle in teardown {
        handle.join().expect("teardown thread must not panic");
    }

    assert_eq!(unsafe { migo_engine_destroy(engine.get()) }, MIGO_OK);
}

/// A process may create more than one Engine. Two Engines must not share the
/// live-session accounting that gates destruction.
#[test]
fn two_engines_account_for_their_own_sessions() {
    let first = create_engine("two-engines-first");
    let second = create_engine("two-engines-second");
    let session = create_session(first);

    assert_eq!(
        unsafe { migo_engine_destroy(second) },
        MIGO_OK,
        "a session on one engine must not block another engine's destruction"
    );
    assert_eq!(
        unsafe { migo_engine_destroy(first) },
        MIGO_ERROR_INVALID_STATE
    );
    assert_eq!(unsafe { migo_session_destroy(session) }, MIGO_OK);
    assert_eq!(unsafe { migo_engine_destroy(first) }, MIGO_OK);
}

// -- Storage roots ------------------------------------------------------------
//
// The three roots live on the Engine, so every Session of one Engine starts from
// the same three directories and the split between two games happens *below*
// them, at the game identity. That is a decision rather than an oversight, and
// the public header documents this rule; these tests execute it.
//
// What they reach and what they do not: `migo_session_create` binds no game
// identity, so the identity half of the split is covered where it lives, by
// `runtime-v8`'s storage-isolation tests over `GamePaths`. This covers the half
// above it -- that the roots a Session starts from are the host's own, verbatim.
//
// A second test asserting two Engines keep their own roots was written and then
// removed: every mutation that killed it killed the one below first, because
// "each Engine's Sessions get that Engine's configured roots" already forces two
// differently configured Engines apart. Two guards on one case pin it no better
// than one, and `two_engines_account_for_their_own_sessions` above already
// executes two live Engines.

/// Every Session of one Engine starts from the roots the host named, unchanged.
///
/// "Unchanged" is the part worth executing: the header promises Migo "never
/// invents a location of its own", and a derived subdirectory inserted here
/// would put every game's data somewhere the host cannot find or clear.
#[test]
fn every_session_of_one_engine_starts_from_the_roots_the_host_named() {
    let dirs = scratch_dirs("engine-scoped-roots");
    let config = engine_config(
        &dirs,
        size_of::<crate::MigoEngineConfig>() as u32,
        migo_capi_abi::MIGO_ABI_VERSION_CURRENT,
    );
    let mut engine: *mut MigoEngine = ptr::null_mut();
    assert_eq!(unsafe { migo_engine_create(&config, &mut engine) }, MIGO_OK);

    let first = create_session(engine);
    let second = create_session(engine);

    let named = |dir: &std::ffi::CString| {
        std::path::PathBuf::from(dir.to_str().expect("utf-8 path").to_owned())
    };
    for session in [first, second] {
        let options = unsafe { &*session }.engine.session_init_options(1.0_f32);
        assert_eq!(
            options.files_dir(),
            named(&dirs.0),
            "a session's files root is not the one the host gave the engine"
        );
        assert_eq!(options.cache_dir(), named(&dirs.1));
        assert_eq!(options.code_cache_dir(), named(&dirs.2));
    }

    assert_eq!(unsafe { migo_session_destroy(first) }, MIGO_OK);
    assert_eq!(unsafe { migo_session_destroy(second) }, MIGO_OK);
    assert_eq!(unsafe { migo_engine_destroy(engine) }, MIGO_OK);
}

/// The process-level measurement cannot reach this session create/destroy cycle.
///
/// `scripts/measure-steady-state-growth.sh` watches resident memory over a long
/// *render* workload, which never creates or destroys a Session; session
/// create/destroy is a separate cycle with no gate of its own. It is the shape
/// this cycle gate exists for: a Session legitimately allocates for a living, so no burst can be
/// written over it, and anything it retains is retained for the life of the process.
///
/// **The engine, the config and the scratch paths are built once, outside the
/// measurement.** Everything hoisted out of the loop is something whose own lifecycle
/// would otherwise be measured as this cycle's growth — and the engine in particular
/// registers process-wide state on first use.
///
/// **What only this can see.** Two tests already watch the handle's own lifetime —
/// `an_entered_call_pins_the_arc_allocation_across_reentrant_destroy` and its sibling in
/// `callbacks` — and a `mem::forget` of the exported `Arc` fails all three. That makes
/// them the same claim at two levels rather than two guards, so the pin is a leak the
/// strong count cannot see: a Session owns at least one heap block that is *not* the
/// handle's own, and `pending_surface_releases` is it. An extra clone of that inner
/// `Arc` escaping the Session — the realistic shape, since the asynchronous
/// surface-release path is exactly what wants a handle outliving a call — fails this
/// test and **nothing else in the crate's 143**.
///
/// **What it cannot see.** A Session created here has no surface, so no Host and no V8
/// isolate: what is measured is the C-ABI handle, its inner allocations, the engine's
/// live-session accounting and the teardown. The isolate's heap across a create/destroy
/// pair needs a surface and stays uncovered, as does anything a Session would register
/// process-wide once it had one. And a block allocated *before* the window and leaked
/// inside it is invisible to any delta, which is why the warm-up is generous rather than
/// minimal: every bounded structure the first Session touches must have reached its bound
/// before measurement starts, or a cache filling legitimately reads as growth.
#[test]
fn creating_and_destroying_a_session_gives_back_what_it_took() {
    use migo_alloc_probe::{Cycle, assert_no_steady_state_growth};

    with_engine("session-cycle-growth", |engine| {
        let config = session_config();
        assert_no_steady_state_growth(
            Cycle {
                path: "migo_session_create/migo_session_destroy",
                warmup: 16,
                measured: 64,
            },
            |_| {
                let mut session: *mut MigoSession = ptr::null_mut();
                assert_eq!(
                    unsafe { migo_session_create(engine, &config, &mut session) },
                    MIGO_OK,
                    "a Session must be creatable, or the cycle measures nothing"
                );
                assert_eq!(
                    unsafe { migo_session_destroy(session) },
                    MIGO_OK,
                    "a Session must be destroyable, or the cycle measures a leak it caused"
                );
            },
        );
    });
}
