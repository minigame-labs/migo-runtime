//! Regression tests for the V8-snapshot JS fingerprint.
//!
//! The fingerprint is computed independently in two places that MUST agree:
//!   * `build.rs` (compile-time: decides whether to embed a snapshot), through
//!     the shared `build_snapshot.rs` helper `include!`d below, and
//!   * `scripts/lib/snapshot-fingerprint.sh` (snapshot generation + freshness).
//! If they disagree, a freshly generated snapshot is silently rejected at build
//! time (fail-safe, but the cold-start win is lost). These tests pin the Rust
//! helper to filesystem enumeration + `LC_ALL=C` relative-path byte ordering and
//! prove it needs no `.git` directory.

#[allow(dead_code)]
mod build_snapshot {
    include!("../../build_snapshot.rs");
}

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use sha2::{Digest, Sha256};

use build_snapshot::{
    ManifestIdentity, SNAPSHOT_SCHEMA_VERSION, SnapshotIdentity, collect_js_files,
    collect_rust_files, require_materialized_size, sha256_file, snapshot_feature_hash,
    snapshot_js_hash, snapshot_profile_features, snapshot_runtime_hash, validate_snapshot_identity,
};

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn rel(repo_root: &Path, path: &Path) -> String {
    path.strip_prefix(repo_root)
        .expect("path under repo root")
        .to_string_lossy()
        .replace('\\', "/")
}

/// Independent reference for the shell pipeline
/// `find ... | LC_ALL=C sort | xargs sha256sum | sha256sum`: order files by the
/// raw bytes of their repo-root-relative path, emit `"<filehash>  <relpath>\n"`,
/// hash the concatenation.
fn reference_byte_order(repo_root: &Path, files: &[PathBuf]) -> String {
    let mut entries: Vec<(String, PathBuf)> = files
        .iter()
        .map(|p| (rel(repo_root, p), p.clone()))
        .collect();
    entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut outer = Sha256::new();
    for (relpath, path) in &entries {
        let file_hash = hex(&Sha256::digest(std::fs::read(path).unwrap()));
        outer.update(format!("{file_hash}  {relpath}\n").as_bytes());
    }
    hex(&outer.finalize())
}

/// The pre-fix behavior: order by `Path`'s component-wise `Ord` (what
/// `Vec<PathBuf>::sort()` did). Kept only to prove the fixture actually triggers
/// the divergence the fix addresses.
fn reference_component_order(repo_root: &Path, files: &[PathBuf]) -> String {
    let mut entries: Vec<PathBuf> = files.to_vec();
    entries.sort();
    let mut outer = Sha256::new();
    for path in &entries {
        let relpath = rel(repo_root, path);
        let file_hash = hex(&Sha256::digest(std::fs::read(path).unwrap()));
        outer.update(format!("{file_hash}  {relpath}\n").as_bytes());
    }
    hex(&outer.finalize())
}

fn unique_tmp_dir(tag: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "migo-snap-fp-{tag}-{}-{nanos}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_file(path: &Path, content: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

#[test]
fn fingerprint_uses_lc_all_c_byte_order_not_path_component_order() {
    let root = unique_tmp_dir("order");
    let jsr = root.join("engine/crates/runtime-v8");

    // `worker.js` (top level) vs `worker/x.js` (nested): byte-wise `.` (0x2e) <
    // `/` (0x2f) puts `worker.js` first, but `Path` component order puts
    // `worker/x.js` first. `app-init.js` vs `app/init.js` is a second `-`
    // (0x2d) < `/` divergence.
    write_file(&jsr.join("worker.js"), b"top-level worker\n");
    write_file(&jsr.join("worker/x.js"), b"nested worker\n");
    write_file(&jsr.join("app-init.js"), b"a\n");
    write_file(&jsr.join("app/init.js"), b"b\n");
    write_file(&jsr.join("web/02_timers.js"), b"timers\n");
    write_file(&jsr.join("notes.txt"), b"ignored\n");

    let files = collect_js_files(&jsr).unwrap();

    let byte_order = reference_byte_order(&root, &files);
    let component_order = reference_component_order(&root, &files);
    assert_ne!(
        byte_order, component_order,
        "the fixture must actually exercise the component-vs-byte divergence"
    );

    let actual = snapshot_js_hash(&root, &files).unwrap();
    assert_eq!(
        actual, byte_order,
        "snapshot_js_hash must order by LC_ALL=C relative-path bytes, matching \
         scripts/lib/snapshot-fingerprint.sh"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn fingerprint_enumerates_every_js_on_disk_and_ignores_non_js() {
    let root = unique_tmp_dir("set");
    let jsr = root.join("engine/crates/runtime-v8");
    write_file(&jsr.join("98_glob.js"), b"g\n");
    write_file(&jsr.join("web/03_canvas.js"), b"c\n");
    write_file(&jsr.join("worker/02_worker_inner.js"), b"w\n");
    write_file(&jsr.join("README.md"), b"doc\n");
    write_file(&jsr.join("web/notes.txt"), b"nope\n");
    write_file(&jsr.join("web/upper.JS"), b"case\n"); // extension match is case-sensitive

    let files = collect_js_files(&jsr).unwrap();
    let mut found: Vec<String> = files.iter().map(|p| rel(&root, p)).collect();
    found.sort();
    assert_eq!(
        found,
        vec![
            "engine/crates/runtime-v8/98_glob.js".to_string(),
            "engine/crates/runtime-v8/web/03_canvas.js".to_string(),
            "engine/crates/runtime-v8/worker/02_worker_inner.js".to_string(),
        ]
    );

    assert_eq!(
        snapshot_js_hash(&root, &files).unwrap(),
        reference_byte_order(&root, &files)
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn fingerprint_works_without_a_git_directory() {
    let root = unique_tmp_dir("nogit");
    let jsr = root.join("engine/crates/runtime-v8");
    write_file(&jsr.join("web/a.js"), b"x\n");
    assert!(!root.join(".git").exists(), "fixture must have no .git");

    let files = collect_js_files(&jsr).unwrap();
    let h = snapshot_js_hash(&root, &files).unwrap();
    assert_eq!(h.len(), 64);
    assert!(h.bytes().all(|b| b.is_ascii_hexdigit()));
    let _ = std::fs::remove_dir_all(&root);
}

/// Cross-language equivalence on the real tree: the shared shell helper (sourced
/// exactly as gen-snapshot / check-snapshot-freshness use it) and the Rust
/// `snapshot_js_hash` must produce the same digest over the live
/// `engine/crates/runtime-v8` extension JS. This is what guarantees a freshly
/// generated snapshot's manifest hash matches `build.rs` at embed time.
/// Linux-only: the shell helper relies on GNU `sha256sum`/`xargs` on the build
/// host; the three tests above cover the pure-Rust behavior everywhere.
#[cfg(target_os = "linux")]
#[test]
fn rust_and_shell_fingerprints_match_on_the_real_tree() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir
        .ancestors()
        .nth(3)
        .expect("runtime-v8 lives under <repo>/engine/crates");

    let files = collect_js_files(manifest_dir).unwrap();
    assert!(!files.is_empty(), "expected extension JS under runtime-v8");
    let rust_hash = snapshot_js_hash(repo_root, &files).unwrap();

    let script = repo_root.join("scripts/lib/snapshot-fingerprint.sh");
    let output = std::process::Command::new("bash")
        .arg("-c")
        .arg(format!(
            "source '{}' && snapshot_js_hash '{}'",
            script.display(),
            repo_root.display()
        ))
        .output()
        .expect("run scripts/lib/snapshot-fingerprint.sh");
    assert!(
        output.status.success(),
        "shell fingerprint failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let shell_hash = String::from_utf8(output.stdout).unwrap().trim().to_string();

    assert_eq!(
        rust_hash, shell_hash,
        "build.rs (Rust) and scripts/lib/snapshot-fingerprint.sh must agree \
         byte-for-byte on the real extension-JS tree"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn rust_and_shell_profile_runtime_fingerprints_match() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir
        .ancestors()
        .nth(3)
        .expect("runtime-v8 lives under <repo>/engine/crates");
    let rust_files = collect_rust_files(manifest_dir).unwrap();
    let rust_runtime_hash = snapshot_runtime_hash(repo_root, &rust_files).unwrap();
    let rust_feature_hash = snapshot_feature_hash(snapshot_profile_features("slim").unwrap());
    let script = repo_root.join("scripts/lib/snapshot-fingerprint.sh");

    let output = std::process::Command::new("bash")
        .arg("-c")
        .arg(format!(
            "source '{}' && snapshot_runtime_hash '{}' && snapshot_feature_hash slim",
            script.display(),
            repo_root.display()
        ))
        .output()
        .expect("run shell snapshot helpers");
    assert!(
        output.status.success(),
        "shell fingerprints failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lines: Vec<_> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(lines, vec![rust_runtime_hash, rust_feature_hash]);
}

#[cfg(target_os = "linux")]
#[test]
fn shell_v8_fingerprint_uses_content_hash_for_archive_and_lfs_pointer() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir
        .ancestors()
        .nth(3)
        .expect("runtime-v8 lives under <repo>/engine/crates");
    let script = repo_root.join("scripts/lib/snapshot-fingerprint.sh");
    let root = unique_tmp_dir("v8-lfs");
    let archive = root.join("librusty_v8.a");
    let pointer = root.join("librusty_v8.pointer");
    let malformed = root.join("malformed-small-file");

    std::fs::write(&archive, vec![0x5a; 100_000]).unwrap();
    let expected = sha256_file(&archive).unwrap();
    std::fs::write(
        &pointer,
        format!(
            "version https://git-lfs.github.com/spec/v1\n\
             oid sha256:{expected}\n\
             size 100000\n"
        ),
    )
    .unwrap();
    std::fs::write(&malformed, b"not a materialized archive or LFS pointer\n").unwrap();

    let output = std::process::Command::new("bash")
        .arg("-c")
        .arg(
            "source \"$1\" && snapshot_v8_archive_hash \"$2\" && \
             snapshot_v8_archive_hash \"$3\" && \
             snapshot_artifact_size \"$2\" && snapshot_artifact_size \"$3\"",
        )
        .arg("migo-v8-hash-test")
        .arg(&script)
        .arg(&archive)
        .arg(&pointer)
        .output()
        .expect("run shell V8 fingerprint helper");
    assert!(
        output.status.success(),
        "archive/LFS identity helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let hashes: Vec<_> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(
        hashes,
        vec![
            expected.clone(),
            expected,
            "100000".to_string(),
            "100000".to_string()
        ]
    );

    let malformed_output = std::process::Command::new("bash")
        .arg("-c")
        .arg("source \"$1\" && snapshot_v8_archive_hash \"$2\"")
        .arg("migo-v8-hash-test")
        .arg(&script)
        .arg(&malformed)
        .output()
        .expect("run malformed V8 fingerprint helper");
    assert!(
        !malformed_output.status.success(),
        "small non-LFS files must not be accepted as a V8 archive identity"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn materialized_snapshot_and_v8_inputs_reject_lfs_pointer_sizes() {
    assert!(require_materialized_size("snapshot", 132).is_err());
    assert!(require_materialized_size("V8 archive", 132).is_err());
    assert!(require_materialized_size("snapshot", 99_999).is_err());
    assert!(require_materialized_size("snapshot", 100_000).is_ok());
}

/// Enumeration is atomic from the caller's perspective: pointing at a
/// non-directory fails outright and returns no `Vec`. The `io::Result<Vec>`
/// signature is what makes it impossible for `build.rs` to observe a partial
/// set and hash it into a false match against a stale manifest.
#[test]
fn enumeration_of_a_non_directory_is_err() {
    let root = unique_tmp_dir("enum-nondir");
    let not_a_dir = root.join("engine/crates/runtime-v8");
    write_file(&not_a_dir, b"i am a file, not a directory\n");

    assert!(
        collect_js_files(&not_a_dir).is_err(),
        "enumerating a non-directory must return Err, not a partial/empty Ok"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// The correctness red line: a subdirectory the walk cannot read must fail the
/// WHOLE enumeration, never silently drop it and return the partial set already
/// collected (which could hash-match a stale manifest and accept an outdated
/// snapshot). Guards against a future "lenient" walk that swallows read errors.
#[cfg(unix)]
#[test]
fn unreadable_subdir_fails_enumeration_without_a_partial_set() {
    use std::os::unix::fs::PermissionsExt;

    let root = unique_tmp_dir("enum-locked");
    let jsr = root.join("engine/crates/runtime-v8");
    // A collectible top-level file plus an unreadable subdir that also contains
    // JS, so a lenient walk would be tempted to return just the top-level set.
    write_file(&jsr.join("keep.js"), b"keep\n");
    let locked = jsr.join("locked");
    write_file(&locked.join("inner.js"), b"inner\n");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

    if std::fs::read_dir(&locked).is_ok() {
        // Privileged runs (e.g. root in CI) bypass the 0o000 bit, so the
        // permission-based failure cannot be simulated here — restore and skip.
        let _ = std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755));
        let _ = std::fs::remove_dir_all(&root);
        return;
    }

    assert!(
        collect_js_files(&jsr).is_err(),
        "an unreadable subdirectory must fail enumeration, not yield a partial set"
    );

    let _ = std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755));
    let _ = std::fs::remove_dir_all(&root);
}

/// The manifest-writing path must not hard-depend on git for locating the repo
/// root (`git rev-parse` exits 128 without an accessible `.git`). Run the real
/// script with a `git` that always fails and assert it still writes a valid
/// manifest (ROOT derived from the script's own location, `git_commit`
/// best-effort `unknown`).
#[cfg(target_os = "linux")]
#[test]
fn write_snapshot_manifest_succeeds_without_git() {
    use std::os::unix::fs::PermissionsExt;

    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir
        .ancestors()
        .nth(3)
        .expect("runtime-v8 lives under <repo>/engine/crates");
    let script = repo_root.join("scripts/write-snapshot-manifest.sh");

    let tmp = unique_tmp_dir("manifest-nogit");
    // A fake `git` on PATH that always fails, so this is a real no-git
    // regression regardless of the host's actual .git accessibility.
    let fake_bin = tmp.join("bin");
    let fake_git = fake_bin.join("git");
    write_file(&fake_git, b"#!/bin/sh\nexit 128\n");
    std::fs::set_permissions(&fake_git, std::fs::Permissions::from_mode(0o755)).unwrap();

    let bin = tmp.join("SNAPSHOT-aarch64.bin");
    std::fs::write(&bin, vec![0x53; 100_000]).unwrap();
    let v8_archive = tmp.join("librusty_v8.a");
    // Snapshot generation must use the materialized V8 archive. A tiny Git LFS
    // pointer is useful to freshness-only CI, but must never create a manifest.
    std::fs::write(&v8_archive, vec![0x41; 100_000]).unwrap();
    let manifest = PathBuf::from(format!("{}.manifest.json", bin.display()));

    let path_env = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = std::process::Command::new("bash")
        .arg(&script)
        .arg("full")
        .arg("android")
        .arg("aarch64")
        .arg(&bin)
        .arg("worker")
        .env("MIGO_V8_ARCHIVE", &v8_archive)
        .env("PATH", path_env)
        .output()
        .expect("run scripts/write-snapshot-manifest.sh");
    assert!(
        output.status.success(),
        "write-snapshot-manifest.sh must succeed without git (ROOT from SCRIPT_DIR, \
         git_commit best-effort); stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let written = std::fs::read_to_string(&manifest).expect("manifest written");
    assert!(
        written.contains("\"arch\": \"aarch64\""),
        "manifest: {written}"
    );
    assert!(
        written.contains("\"target_triple\": \"aarch64-linux-android\""),
        "manifest: {written}"
    );
    assert!(
        written.contains("\"schema_version\": 3"),
        "manifest: {written}"
    );
    assert!(
        written.contains("\"snapshot_kind\": \"worker\""),
        "manifest: {written}"
    );
    assert!(
        written.contains("\"profile\": \"full\""),
        "manifest: {written}"
    );
    assert!(
        written.contains("\"features_sha256\""),
        "manifest: {written}"
    );
    assert!(
        written.contains("\"rust_sources_sha256\""),
        "manifest: {written}"
    );
    assert!(
        written.contains("\"v8_archive_sha256\""),
        "manifest: {written}"
    );
    assert!(
        written.contains("\"js_sources_sha256\""),
        "manifest: {written}"
    );
    assert!(
        written.contains("\"deno_core_version\""),
        "manifest: {written}"
    );
    assert!(
        written.contains("\"git_commit\": \"unknown\""),
        "git_commit must degrade to unknown without git; manifest: {written}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn product_feature_hash_is_canonical_and_profile_specific() {
    let full = snapshot_profile_features("full").expect("full features");
    let slim = snapshot_profile_features("slim").expect("slim features");
    assert_ne!(snapshot_feature_hash(full), snapshot_feature_hash(slim));

    let mut reversed = full.to_vec();
    reversed.reverse();
    assert_eq!(
        snapshot_feature_hash(full),
        snapshot_feature_hash(&reversed),
        "feature hash must sort internally"
    );
    assert!(snapshot_profile_features("custom").is_err());
}

#[test]
fn runtime_fingerprint_changes_when_an_op_source_changes() {
    let root = unique_tmp_dir("runtime-rs");
    let jsr = root.join("engine/crates/runtime-v8");
    let snapshot_gen = root.join("engine/tools/snapshot-gen");
    write_file(&jsr.join("lib.rs"), b"fn stable() {}\n");
    write_file(
        &jsr.join("device/mod.rs"),
        b"#[op2(fast)] fn op_value() -> i32 { 1 }\n",
    );
    write_file(&jsr.join("device/ignored.js"), b"ignored here\n");
    write_file(&jsr.join("Cargo.toml"), b"[features]\nprofile-slim=[]\n");
    write_file(
        &snapshot_gen.join("src/main.rs"),
        b"fn create_snapshot() {}\n",
    );
    write_file(
        &snapshot_gen.join("Cargo.toml"),
        b"[features]\nprofile-slim=[]\n",
    );
    write_file(&root.join("engine/Cargo.lock"), b"version = 4\n");

    let files = collect_rust_files(&jsr).unwrap();
    let relative: Vec<_> = files.iter().map(|path| rel(&root, path)).collect();
    for required in [
        "engine/crates/runtime-v8/Cargo.toml",
        "engine/tools/snapshot-gen/src/main.rs",
        "engine/tools/snapshot-gen/Cargo.toml",
        "engine/Cargo.lock",
    ] {
        assert!(
            relative.iter().any(|path| path == required),
            "snapshot runtime fingerprint omitted {required}: {relative:?}"
        );
    }

    let before = snapshot_runtime_hash(&root, &files).unwrap();
    write_file(
        &jsr.join("device/mod.rs"),
        b"#[op2(fast)] fn op_value() -> i32 { 2 }\n",
    );
    let after = snapshot_runtime_hash(&root, &files).unwrap();
    assert_ne!(before, after);

    write_file(
        &snapshot_gen.join("src/main.rs"),
        b"fn create_snapshot_with_warmup() {}\n",
    );
    let generator_after = snapshot_runtime_hash(&root, &files).unwrap();
    assert_ne!(after, generator_after);

    write_file(
        &snapshot_gen.join("Cargo.toml"),
        b"[features]\nprofile-slim=[\"runtime-v8/profile-slim\"]\n",
    );
    assert_ne!(
        generator_after,
        snapshot_runtime_hash(&root, &files).unwrap()
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn legacy_or_mismatched_snapshot_identity_is_rejected() {
    let expected = SnapshotIdentity {
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        snapshot_kind: "worker",
        profile: "full",
        arch: "aarch64",
        features_sha256: "features",
        rust_sources_sha256: "runtime",
        v8_archive_sha256: "v8",
        js_sources_sha256: "js",
        deno_core_version: "0.385.0",
        snapshot_size: 1_900_000,
    };
    let legacy = ManifestIdentity {
        arch: Some("aarch64"),
        ..ManifestIdentity::default()
    };
    assert!(validate_snapshot_identity(legacy, expected).is_err());

    let valid = ManifestIdentity {
        schema_version: Some(SNAPSHOT_SCHEMA_VERSION),
        snapshot_kind: Some("worker"),
        profile: Some("full"),
        arch: Some("aarch64"),
        features_sha256: Some("features"),
        rust_sources_sha256: Some("runtime"),
        v8_archive_sha256: Some("v8"),
        js_sources_sha256: Some("js"),
        deno_core_version: Some("0.385.0"),
        snapshot_size: Some(1_900_000),
    };
    assert!(validate_snapshot_identity(valid, expected).is_ok());
    assert!(
        validate_snapshot_identity(
            ManifestIdentity {
                snapshot_kind: Some("host"),
                ..valid
            },
            expected
        )
        .is_err(),
        "a host snapshot must never load in a Worker"
    );
    assert!(
        validate_snapshot_identity(
            ManifestIdentity {
                snapshot_kind: None,
                ..valid
            },
            expected
        )
        .is_err(),
        "schema 3 must reject a manifest without snapshot_kind"
    );
    assert!(
        validate_snapshot_identity(
            ManifestIdentity {
                profile: Some("slim"),
                ..valid
            },
            expected
        )
        .is_err(),
        "a slim snapshot must never load in a full Worker"
    );
    assert!(
        validate_snapshot_identity(
            ManifestIdentity {
                js_sources_sha256: Some("other-js"),
                ..valid
            },
            expected
        )
        .is_err(),
        "changed extension JS must invalidate a snapshot"
    );
    assert!(
        validate_snapshot_identity(
            ManifestIdentity {
                deno_core_version: Some("other-deno"),
                ..valid
            },
            expected
        )
        .is_err(),
        "a deno_core version mismatch must invalidate a snapshot"
    );
}
