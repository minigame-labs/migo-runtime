//! # JavaScript Runtime Module
//!
//! This crate provides the JavaScript execution environment for the Migo engine,
//! implementing Mini Program and standard Web APIs on top of Deno Core (V8).
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────┐
//! │                     JavaScript Runtime (V8)                          │
//! │                                                                      │
//! │  ┌────────────────────────────────────────────────────────────────┐ │
//! │  │                      Global Scope                              │ │
//! │  │  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐            │ │
//! │  │  │    migo     │  │   console   │  │   canvas    │    ...     │ │
//! │  │  │  (Mini App) │  │   (logging) │  │  (2D/WebGL) │            │ │
//! │  │  └──────┬──────┘  └──────┬──────┘  └──────┬──────┘            │ │
//! │  └─────────│────────────────│────────────────│────────────────────┘ │
//! │            │                │                │                       │
//! │  ┌─────────▼────────────────▼────────────────▼────────────────────┐ │
//! │  │                   Native Ops (Rust)                            │ │
//! │  │  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐            │ │
//! │  │  │    file     │  │   network   │  │    audio    │    ...     │ │
//! │  │  │   (fs ops)  │  │ (fetch/ws)  │  │  (WebAudio) │            │ │
//! │  │  └─────────────┘  └─────────────┘  └─────────────┘            │ │
//! │  └────────────────────────────────────────────────────────────────┘ │
//! │                                                                      │
//! │  ┌────────────────────────────────────────────────────────────────┐ │
//! │  │                       Deno Core                                │ │
//! │  │  - Module loading (ESM)                                        │ │
//! │  │  - Op dispatch (sync/async)                                    │ │
//! │  │  - Event loop integration                                      │ │
//! │  └────────────────────────────────────────────────────────────────┘ │
//! └─────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Implemented APIs
//!
//! - **File System**: `migo.getFileSystemManager()` - read, write, stat, mkdir, etc.
//! - **Storage**: `migo.setStorageSync()`, `migo.getStorageSync()`
//! - **Audio**: `migo.createInnerAudioContext()` - streaming audio playback
//! - **Canvas**: `migo.createCanvas()`, `migo.createOffscreenCanvas()`
//! - **Image**: `migo.createImage()` - async image loading
//! - **System**: `migo.getSystemInfoSync()`, `migo.getWindowInfo()`
//! - **Lifecycle**: `migo.onShow()`, `migo.onHide()`
//!
//! ### Web Standard APIs
//!
//! - **Console**: `console.log()`, `console.warn()`, `console.error()`, etc.
//! - **Timers**: `setTimeout()`, `setInterval()`, `requestAnimationFrame()`
//! - **Network**: `fetch()` with streaming support
//! - **URL**: `URL`, `URLSearchParams`
//! - **Encoding**: `TextEncoder`, `TextDecoder`
//! - **Canvas 2D**: Full CanvasRenderingContext2D API
//! - **WebGL**: WebGLRenderingContext (WebGL 1.0)
//! - **WebAudio**: AudioContext, AudioBufferSourceNode, GainNode
//!
//! ## Extension System
//!
//! The runtime is built from modular extensions:
//!
//! | Extension | Purpose |
//! |-----------|---------|
//! | `host_v8_base` | Core ops, timers, system info |
//! | `host_v8_console` | Console logging |
//! | `host_v8_file` | File system operations |
//! | `host_v8_network` | HTTP fetch, WebSocket |
//! | `host_v8_rendering` | Canvas, WebGL, Image |
//! | `host_v8_audio` | WebAudio, InnerAudioContext |
//! | `host_v8_input` | Touch event dispatch |
//!
//! ## Usage
//!
//! The V8 runtime is created by the core module and not directly instantiated:
//!
//! ```rust,ignore
//! use runtime_v8::HostJsRuntime;
//!
//! let mut runtime = HostJsRuntime::new(
//!     host_id,
//!     host_state,
//!     cache_dir,
//!     v8_limits,
//!     code_signing_enabled,
//!     code_signing_pubkey,
//! );
//!
//! // Load and run a game
//! runtime.evaluate_module("/game".into(), "main.js".into()).await?;
//!
//! // Run the event loop
//! runtime.run_event_loop(PollEventLoopOptions::default()).await?;
//! ```

// Section 7.3's steady-state allocation gate reads this. `#[cfg(test)]` scopes it
// to this crate's own test binary: a `#[global_allocator]` is unique per binary, so
// one declared unconditionally here would follow the library into every shipped
// cdylib. Deleting it does not make the gates pass silently -- each burst proves the
// allocator is installed before it trusts a zero count.
#[cfg(test)]
#[global_allocator]
static COUNTING_ALLOCATOR: migo_alloc_probe::CountingAllocator =
    migo_alloc_probe::CountingAllocator::system();

use shared::op_state::HostOpState;

// CORE modules (always compiled)
mod base;
mod console;
mod env;
mod event;
mod file;
mod input;
mod io_state;
mod lifecycle;
pub(crate) mod network;
mod permission;
#[cfg(feature = "api-media")]
pub use network::fetch::create_audio_http_client;
mod rendering;
mod storage;
mod url;
mod utility;
mod web;

// OPTIONAL modules (feature-gated)
//
// Each `api-*` feature controls whether the corresponding extension module
// is compiled and included in the extension chain.  When a feature is
// disabled the module (Rust ops + JS ESM files + global scope registration)
// is excluded entirely, reducing binary size.
//
// Mapping:
//   api-sensors      -> device   (sensors, battery, clipboard, vibration, screen, network, location, scan)
//   api-media        -> media    (camera, image_api, video) + audio (WebAudio, InnerAudio, Recorder)
//   api-connectivity -> system   (bluetooth, auth, system info, login, settings, navigate, game_log, etc.)
//   api-commerce     -> share    (share menu) + payment (Midas payment)
//   api-system       -> ui       (Toast/Modal/Loading) + update + ad + worker
#[cfg(feature = "api-system")]
mod ad;
#[cfg(feature = "api-media")]
mod audio;
#[cfg(feature = "api-sensors")]
mod device;
#[cfg(feature = "api-media")]
mod media;
#[cfg(feature = "api-commerce")]
mod payment;
#[cfg(feature = "api-commerce")]
mod share;
#[cfg(feature = "api-connectivity")]
mod system;
#[cfg(feature = "api-system")]
mod ui;
#[cfg(feature = "api-system")]
mod update;
#[cfg(feature = "api-system")]
pub(crate) mod worker;

mod code_cache;
mod host_runtime;
mod icu;
#[allow(dead_code)]
mod isolate_pool;
mod js_bindings;
mod loader;
pub mod snapshot;
pub mod watchdog;

pub use host_runtime::HostJsRuntime;
pub use host_runtime::SharedMountTableRef;
pub use host_runtime::V8LimitsConfig;
pub use icu::install_icu_data;

#[cfg(test)]
mod tests;
pub use rendering::image::cache::{
    drain_shared_image_cache, image_cache_registered, unregister_image_cache,
};

deno_core::extension!(
    runtime,
    esm_entry_point = "ext:runtime/99_main.js",
    esm = [
        dir "src",
        "98_global_scope_shared.js",
        "98_global_scope_window.js",
        "97_migo_namespace.js",
        "99_main.js",
    ],
);

deno_core::extension!(
    worker_runtime,
    esm_entry_point = "ext:worker_runtime/99_worker_main.js",
    esm = [
        dir "src",
        "98_global_scope_shared.js",
        "98_global_scope_worker.js",
        "99_worker_main.js",
    ],
);

/// Creates all JavaScript runtime extensions with the given host state.
///
/// This assembles the complete set of extensions needed for the Migo runtime,
/// including both Mini Program APIs and Web standard APIs.
///
/// # Arguments
///
/// * `host` - Shared operational state containing channels to other services
///
/// # Returns
///
/// A vector of Deno extensions, ordered to respect dependencies.
///
/// # Extension Order
///
/// Extensions are chained in dependency order. All are currently loaded
/// eagerly at runtime startup. The CORE / OPTIONAL annotations below
/// indicate which extensions are required for every game vs. which could
/// potentially be deferred or lazy-loaded in a future optimization pass.
///
/// CORE = required for basic game execution (JS env, rendering, input, timers).
/// OPTIONAL = feature-specific; many games never use these APIs.
///
/// ```text
///  #  Extension       Tag       Reason
///  1  base            CORE      ops, async utils, subpackage loader
///  2  console         CORE      console.log / warn / error
///  3  event           CORE      EventTarget / EventEmitter
///  4  utility         CORE      TextEncoder / TextDecoder
///  5  device          OPTIONAL  sensors, battery, clipboard, vibration, screen, network, location, scan
///  6  ui              OPTIONAL  Toast / Modal / Loading / ActionSheet / UserInfoButton
///  7  system          OPTIONAL  bluetooth, auth, window/system/device info, login, settings, navigate
///  8  env             CORE      environment variables
///  9  lifecycle       CORE      onShow / onHide, restart / exit
/// 10  update          OPTIONAL  update manager
/// 11  storage         CORE      setStorage / getStorage (most games use local storage)
/// 12  input           CORE      touch events, keyboard events
/// 13  file            CORE      file system manager
/// 14  rendering       CORE      Canvas / WebGL / Image / RAF / Font
/// 15  web             CORE      setTimeout / setInterval / Performance / Location
/// 16  url             CORE      URL / URLSearchParams
/// 17  network         CORE      fetch / WebSocket / upload / download / TCP / UDP
/// 18  media           OPTIONAL  Camera / ImageAPI
/// 19  audio           OPTIONAL  WebAudio / InnerAudio / MediaAudioPlayer / RecorderManager
/// 20  worker          OPTIONAL  Worker threads
/// 21  share           OPTIONAL  share menu / shareAppMessage
/// 22  payment         OPTIONAL  Midas payment
/// 23  ad              OPTIONAL  BannerAd / RewardedVideoAd / InterstitialAd / etc.
/// 24  runtime         CORE      global scope registration (98_global_scope_*.js + 99_main.js)
/// ```
pub fn main_extensions(host: HostOpState) -> Vec<deno_core::Extension> {
    // Build extension list using a Vec so optional extensions can be
    // conditionally appended based on feature flags.
    let mut exts: Vec<deno_core::Extension> = Vec::new();

    // ---- CORE extensions (always loaded) ----
    exts.extend(base::base_extensions(host)); // ops, async utils, subpackage loader
    exts.extend(io_state::io_state_extensions()); // shared IO scheduler state
    exts.extend(console::console_extensions()); // console.log / warn / error
    exts.extend(event::event_extensions()); // EventTarget / EventEmitter
    exts.extend(utility::utility_extensions()); // TextEncoder / TextDecoder

    // ---- OPTIONAL: api-sensors ----
    #[cfg(feature = "api-sensors")]
    exts.extend(device::device_extensions()); // sensors, battery, clipboard, vibration, screen, network, location, scan

    // ---- OPTIONAL: api-system ----
    #[cfg(feature = "api-system")]
    exts.extend(ui::ui_extensions()); // Toast / Modal / Loading / ActionSheet / UserInfoButton

    // ---- OPTIONAL: api-connectivity ----
    #[cfg(feature = "api-connectivity")]
    exts.extend(system::system_extensions()); // bluetooth, auth, system info, login, settings, navigate

    // ---- CORE (continued) ----
    exts.extend(env::env_extensions()); // environment variables
    exts.extend(lifecycle::lifecycle_extensions()); // onShow / onHide, restart / exit

    // ---- OPTIONAL: api-system (continued) ----
    #[cfg(feature = "api-system")]
    exts.extend(update::update_extensions()); // update manager

    // ---- CORE (continued) ----
    exts.extend(storage::storage_extensions()); // setStorage / getStorage
    exts.extend(input::touch_extensions()); // touch events, keyboard events
    exts.extend(file::file_extensions()); // file system manager
    exts.extend(rendering::rendering_extensions()); // Canvas / WebGL / Image / RAF / Font
    exts.extend(web::web_extensions()); // setTimeout / setInterval / Performance
    exts.extend(url::url_extensions()); // URL / URLSearchParams
    exts.extend(network::network_extensions()); // fetch / WebSocket / upload / download / TCP / UDP

    // ---- OPTIONAL: api-media ----
    #[cfg(feature = "api-media")]
    exts.extend(media::media_extensions()); // Camera / ImageAPI / Video
    #[cfg(feature = "api-media")]
    exts.extend(audio::audio_extensions()); // WebAudio / InnerAudio / Recorder

    // ---- OPTIONAL: api-system (continued) ----
    #[cfg(feature = "api-system")]
    exts.extend(worker::worker_extensions()); // Worker threads

    // ---- OPTIONAL: api-commerce ----
    #[cfg(feature = "api-commerce")]
    exts.extend(share::share_extensions()); // share menu / shareAppMessage
    #[cfg(feature = "api-commerce")]
    exts.extend(payment::payment_extensions()); // Midas payment

    // ---- OPTIONAL: api-system (continued) ----
    #[cfg(feature = "api-system")]
    exts.extend(ad::ad_extensions()); // BannerAd / RewardedVideoAd / InterstitialAd / etc.

    // ---- CORE: runtime (must be last) ----
    exts.push(runtime::init()); // global scope registration (98_global_scope_*.js + 99_main.js)

    exts
}

/// Harden the game-visible global scope by removing deno_core's bootstrap
/// internals (`Deno` op surface, `__bootstrap`) so game code can never see them.
///
/// Call this ONCE, at runtime, after a *main* runtime is fully constructed
/// (snapshot-restored or built from source). It is deliberately NOT performed
/// inside the bootstrap module (99_main.js): that module is captured in the V8
/// startup snapshot, and deno_core's snapshot RESTORE re-runs
/// `store_js_callbacks`, reading `Deno.core.{ops,eventLoopTick,buildCustomError,
/// runImmediateCallbacks}` back out of the snapshotted heap. If `Deno` were
/// deleted before the snapshot was taken, restore would panic ("Deno is not
/// defined" / "unable to convert"). Deferring the deletion to here keeps
/// `Deno.core` intact in the snapshot while still hiding it from games — the
/// same approach deno's own runtime uses (hardening runs at startup, not in the
/// snapshot).
///
/// Workers use their own bootstrap helper because they must also start and
/// remove a deferred message-pump hook after runtime-only op state exists.
pub fn harden_global_scope(rt: &mut deno_core::JsRuntime) {
    if let Err(e) = rt.execute_script(
        "ext:runtime/harden_global.js",
        deno_core::FastString::from_static(
            "delete globalThis.Deno; delete globalThis.__bootstrap;",
        ),
    ) {
        tracing::error!("failed to harden global scope (Deno/__bootstrap not removed): {e}");
    }
}
#[cfg(all(feature = "profile-full", feature = "profile-slim"))]
compile_error!("profile-full and profile-slim are mutually exclusive");
#[cfg(all(feature = "worker-snapshot", not(feature = "profile-full")))]
compile_error!("worker-snapshot requires profile-full");
#[cfg(all(
    target_os = "android",
    feature = "worker-snapshot",
    not(migo_has_worker_snapshot)
))]
compile_error!(
    "worker-snapshot was requested, but no exact schema-v3 Worker snapshot was validated"
);
#[cfg(all(
    feature = "profile-slim",
    any(
        feature = "api-sensors",
        feature = "api-media",
        feature = "api-connectivity",
        feature = "api-commerce",
        feature = "api-system"
    )
))]
compile_error!("profile-slim is exact and cannot be combined with optional API groups");
