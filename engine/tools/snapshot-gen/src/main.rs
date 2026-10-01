//! V8 startup snapshot generator for the Migo JS runtime.
//!
//! Generates a serialized V8 heap snapshot containing all pre-parsed and
//! pre-compiled extension JS modules. Loading from this snapshot avoids source
//! parsing/compilation at runtime; actual device latency and size tradeoffs
//! require matched measurement.
//!
//! # Usage
//!
//! Snapshots are platform-bound (OS + CPU arch), so this generator must be
//! cross-compiled to a target Android ABI and run on that ABI's emulator
//! (x86_64) or device (arm64):
//!
//! ```bash
//! # Cross-compile to the target ABI (see crates/runtime-v8 memory notes for
//! # the exact RUSTY_V8_ARCHIVE / cargo-ndk invocation), push to the device,
//! # then run with MIGO_SNAPSHOT_OUT and `adb pull` the result into
//! # crates/runtime-v8/snapshots/SNAPSHOT-<profile>-<arch>.bin.
//! ```
//!
//! When run without `MIGO_SNAPSHOT_OUT`, it writes to
//! `crates/runtime-v8/snapshots/SNAPSHOT-<profile>-<arch>.bin` (arch = the ABI this
//! binary was compiled for). `runtime-v8/build.rs` embeds the matching file
//! for android targets at compile time.
//!
//! # When to regenerate
//!
//! The snapshot is tied to the exact V8 version and extension set.  Regenerate
//! whenever any of the following change:
//!
//! - deno_core / V8 version
//! - Extension JS source files (any modification)
//! - Extension registration order
//! - Op signatures (number of ops, their names)
//!
//! In the build pipeline, always regenerate the snapshot before compiling the
//! final release artifact.

use std::path::PathBuf;

const DEFAULT_SNAPSHOT_DIR_FROM_MANIFEST: &str = "../../crates/runtime-v8/snapshots";

#[cfg(all(feature = "profile-full", feature = "profile-slim"))]
compile_error!("profile-full and profile-slim are mutually exclusive");

#[cfg(feature = "profile-slim")]
const PRODUCT_PROFILE: &str = "slim";
#[cfg(not(feature = "profile-slim"))]
const PRODUCT_PROFILE: &str = "full";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SnapshotKind {
    Host,
    Worker,
}

impl SnapshotKind {
    fn from_env() -> Self {
        match std::env::var("MIGO_SNAPSHOT_KIND") {
            Ok(value) if value == "host" => Self::Host,
            Ok(value) if value == "worker" => Self::Worker,
            Ok(value) => panic!("invalid MIGO_SNAPSHOT_KIND={value:?}; expected host|worker"),
            Err(std::env::VarError::NotPresent) => Self::Host,
            Err(error) => panic!("MIGO_SNAPSHOT_KIND is not valid Unicode: {error}"),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Worker => "worker",
        }
    }
}

fn main() {
    let snapshot_kind = SnapshotKind::from_env();
    #[cfg(feature = "profile-slim")]
    if snapshot_kind == SnapshotKind::Worker {
        panic!("Worker snapshot requires product profile full");
    }

    // Initialize V8 platform (required before any V8 operations).
    deno_core::JsRuntime::init_platform(None);

    // The runtime hands V8 its locale data before the first isolate; a snapshot is made by an isolate too, and one made
    // without the data it will run with is a snapshot of a different V8.
    runtime_v8::install_icu_data();

    let extensions = match snapshot_kind {
        SnapshotKind::Host => runtime_v8::snapshot::lazy_extensions(),
        #[cfg(feature = "profile-full")]
        SnapshotKind::Worker => runtime_v8::snapshot::worker_lazy_extensions(),
        #[cfg(feature = "profile-slim")]
        SnapshotKind::Worker => unreachable!("Worker+slim rejected before V8 initialization"),
    };

    println!(
        "Creating {} V8 snapshot with {} extensions...",
        snapshot_kind.as_str(),
        extensions.len()
    );
    for ext in &extensions {
        println!("  - {}", ext.name);
    }

    let output = deno_core::snapshot::create_snapshot(
        deno_core::snapshot::CreateSnapshotOptions {
            cargo_manifest_dir: env!("CARGO_MANIFEST_DIR"),
            startup_snapshot: None,
            skip_op_registration: false,
            extensions,
            extension_transpiler: None,
            with_runtime_cb: match snapshot_kind {
                SnapshotKind::Host => None,
                SnapshotKind::Worker => Some(Box::new(|rt| {
                    rt.execute_script(
                        "worker-snapshot-generation-check",
                        "if (typeof Deno.core.eventLoopTick !== 'function') throw new Error('Deno.core callbacks missing');\n\
                         if (typeof globalThis.__migoStartWorkerMessagePump !== 'function') throw new Error('deferred Worker pump hook missing');",
                    )
                    .expect("Worker snapshot bootstrap must remain restorable");
                    let mut context =
                        std::task::Context::from_waker(std::task::Waker::noop());
                    assert!(
                        matches!(
                            rt.poll_event_loop(
                                &mut context,
                                deno_core::PollEventLoopOptions::default()
                            ),
                            std::task::Poll::Ready(Ok(()))
                        ),
                        "Worker snapshot must not capture a pending receive op"
                    );
                })),
            },
        },
        None, // no warmup script
    )
    .expect("Failed to create V8 snapshot");

    // Output path. MIGO_SNAPSHOT_OUT overrides the default — required when the
    // generator runs cross-compiled on a device/emulator (V8 startup snapshots
    // are platform-bound, so each ABI's snapshot must be produced by the SAME
    // android V8 the .so links), where the host CARGO_MANIFEST_DIR doesn't
    // exist: write to e.g. /data/local/tmp/SNAPSHOT.bin then `adb pull` it into
    // `crates/runtime-v8/snapshots/SNAPSHOT-<profile>-<arch>.bin`.
    //
    // The default path is per-arch (`std::env::consts::ARCH` is the arch this
    // generator was compiled for), matching what `runtime-v8/build.rs` selects.
    let snapshot_path: PathBuf = match std::env::var_os("MIGO_SNAPSHOT_OUT") {
        Some(path) => PathBuf::from(path),
        None => {
            let file_name = match snapshot_kind {
                SnapshotKind::Host => format!(
                    "SNAPSHOT-{}-{}.bin",
                    PRODUCT_PROFILE,
                    std::env::consts::ARCH
                ),
                SnapshotKind::Worker => format!(
                    "SNAPSHOT-worker-{}-{}.bin",
                    PRODUCT_PROFILE,
                    std::env::consts::ARCH
                ),
            };
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join(DEFAULT_SNAPSHOT_DIR_FROM_MANIFEST)
                .join(file_name)
        }
    };

    if let Some(dir) = snapshot_path.parent() {
        std::fs::create_dir_all(dir).ok();
    }

    let snapshot_data = &*output.output;
    std::fs::write(&snapshot_path, snapshot_data).expect("Failed to write SNAPSHOT.bin");

    println!(
        "V8 snapshot written to {} ({} bytes, {:.1} KB)",
        snapshot_path.display(),
        snapshot_data.len(),
        snapshot_data.len() as f64 / 1024.0,
    );

    // Print files that were loaded during snapshot creation.
    // Useful for CI cache invalidation.
    if !output.files_loaded_during_snapshot.is_empty() {
        println!("\nFiles loaded during snapshot creation:");
        for f in &output.files_loaded_during_snapshot {
            println!("  {}", f.display());
        }
    }
}
