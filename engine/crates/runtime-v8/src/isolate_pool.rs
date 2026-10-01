//! V8 Isolate Prewarming Pool.
//!
//! Pre-creates V8 isolate(s) with runtime extensions compiled at app startup,
//! before the user opens a game. When a game starts, a prewarmed runtime is
//! consumed instead of creating a fresh isolate (saves 100-200ms).
//!
//! # Architecture
//!
//! ```text
//! MigoRuntime.init()  ──▶  IsolatePool::prewarm()  ──▶  background thread
//!                                                          │
//!                                                          ├─ Create JsRuntime
//!                                                          ├─ Compile extensions
//!                                                          ├─ Store in pool
//!                                                          ▼
//! GameSession.start() ──▶  IsolatePool::take()  ──▶  prewarmed runtime
//!                          (or fresh if pool empty)
//! ```

use std::sync::Mutex;

use deno_core::{Extension, JsRuntime, RuntimeOptions};

/// A prewarmed JsRuntime ready to be consumed by a game session.
pub(crate) struct PrewarmedRuntime {
    pub rt: JsRuntime,
}

// SAFETY: JsRuntime is !Send by default because V8 isolates are thread-local.
// We only transfer the runtime between threads when no V8 operations are in flight.
// The receiving thread becomes the new owner before any V8 calls.
unsafe impl Send for PrewarmedRuntime {}

struct PoolState {
    runtimes: Vec<PrewarmedRuntime>,
    /// V8 version + engine extension hash when the pool was warmed.
    /// If this changes, all prewarmed runtimes are stale.
    version_key: String,
}

static POOL: Mutex<Option<PoolState>> = Mutex::new(None);

/// Build a version key from V8 version. If extensions change (new JS files,
/// new ops), the engine binary changes too, so V8 version alone is sufficient.
fn current_version_key() -> String {
    deno_core::v8::V8::get_version().to_string()
}

/// Prewarm one isolate in the current thread.
///
/// Call from a background thread at app startup. Creates a JsRuntime with
/// all extensions compiled but no game-specific state.
///
/// # Arguments
/// * `extensions_fn` — closure that creates the extension list (called on the
///   background thread)
/// * `create_params` — optional V8 create params (heap limits etc.)
pub(crate) fn prewarm(
    extensions: Vec<Extension>,
    create_params: Option<deno_core::v8::CreateParams>,
    ext_code_cache: Option<std::rc::Rc<dyn deno_core::ExtCodeCache>>,
) {
    // This can be the first V8 to start in the process, on a background thread.
    crate::icu::install_icu_data();
    let t0 = std::time::Instant::now();
    let version_key = current_version_key();

    let rt = JsRuntime::new(RuntimeOptions {
        extensions,
        create_params,
        extension_code_cache: ext_code_cache,
        // No module loader — game modules loaded after take()
        module_loader: None,
        ..Default::default()
    });

    let elapsed = t0.elapsed();
    tracing::info!(
        "IsolatePool: prewarmed 1 runtime in {:.1}ms (v8={})",
        elapsed.as_secs_f64() * 1000.0,
        version_key,
    );

    if let Ok(mut guard) = POOL.lock() {
        let state = guard.get_or_insert_with(|| PoolState {
            runtimes: Vec::new(),
            version_key: version_key.clone(),
        });
        // Version changed since last prewarm — discard stale runtimes
        if state.version_key != version_key {
            tracing::info!(
                "IsolatePool: V8 version changed ({} -> {}), clearing stale runtimes",
                state.version_key,
                version_key,
            );
            state.runtimes.clear();
            state.version_key = version_key;
        }
        state.runtimes.push(PrewarmedRuntime { rt });
    }
}

/// Take a prewarmed runtime from the pool, if available.
///
/// Returns `None` if the pool is empty or version mismatched
/// (caller should create a fresh runtime).
pub(crate) fn take() -> Option<PrewarmedRuntime> {
    let mut guard = POOL.lock().ok()?;
    let state = guard.as_mut()?;

    // Auto-invalidate on version change
    let current = current_version_key();
    if state.version_key != current {
        tracing::info!(
            "IsolatePool: version mismatch on take ({} vs {}), clearing",
            state.version_key,
            current,
        );
        state.runtimes.clear();
        state.version_key = current;
        return None;
    }
    state.runtimes.pop()
}

/// Number of prewarmed runtimes available.
#[allow(dead_code)]
pub(crate) fn available() -> usize {
    POOL.lock()
        .ok()
        .and_then(|g| g.as_ref().map(|s| s.runtimes.len()))
        .unwrap_or(0)
}

/// Clear all prewarmed runtimes.
#[allow(dead_code)]
pub(crate) fn clear() {
    if let Ok(mut guard) = POOL.lock() {
        if let Some(state) = guard.as_mut() {
            state.runtimes.clear();
        }
    }
}
