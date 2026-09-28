// Shared V8-snapshot input fingerprint helpers.
//
// `include!`d verbatim by both `build.rs` (the compile-time embed decision) and
// the `src/tests/snapshot_fingerprint.rs` test module, so the two stay
// byte-identical. (`include!` is why this uses `//` rather than `//!` — inner
// doc comments are rejected when pasted inside a `mod` block.) Kept
// dependency-light (only `sha2` + `std`): `build.rs` pulls `sha2` in as a
// build-dependency and the test pulls it in as a dev-dependency.
//
// The result MUST equal the shell pipeline in
// `scripts/lib/snapshot-fingerprint.sh`:
//
//   find engine/crates/runtime-v8 -type f -name '*.js' \
//     | LC_ALL=C sort | xargs -r -d '\n' sha256sum | sha256sum
//
// Both sides therefore agree on:
//   * the file set — every `*.js` on disk under `engine/crates/runtime-v8`
//     (a filesystem walk, NOT a git query) so the no-`.git` workspace works and
//     untracked/generated embedded JS also invalidates a snapshot; and
//   * the order — the raw bytes of the repo-root-relative, forward-slash path
//     (`LC_ALL=C`), which is NOT the same as `Path`'s component-wise `Ord`
//     (e.g. `worker.js` sorts before `worker/x.js` byte-wise because `.` (0x2e)
//     < `/` (0x2f), but after it component-wise).
//
// Each hashed line is `"<sha256hex>  <relpath>\n"` (two spaces, matching the
// `sha256sum` filename column), fed into an outer SHA-256.

use std::{
    io::{BufReader, Read},
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};

pub const SNAPSHOT_SCHEMA_VERSION: u64 = 3;

const FULL_FEATURES: &[&str] = &[
    "api-commerce",
    "api-connectivity",
    "api-media",
    "api-sensors",
    "api-system",
    "code-signing",
    "profile-full",
    "v8-limits",
];
const SLIM_FEATURES: &[&str] = &["code-signing", "profile-slim", "v8-limits"];

#[derive(Clone, Copy, Debug, Default)]
pub struct ManifestIdentity<'a> {
    pub schema_version: Option<u64>,
    pub snapshot_kind: Option<&'a str>,
    pub profile: Option<&'a str>,
    pub arch: Option<&'a str>,
    pub features_sha256: Option<&'a str>,
    pub rust_sources_sha256: Option<&'a str>,
    pub v8_archive_sha256: Option<&'a str>,
    pub js_sources_sha256: Option<&'a str>,
    pub deno_core_version: Option<&'a str>,
    pub snapshot_size: Option<u64>,
}

#[derive(Clone, Copy, Debug)]
pub struct SnapshotIdentity<'a> {
    pub schema_version: u64,
    pub snapshot_kind: &'a str,
    pub profile: &'a str,
    pub arch: &'a str,
    pub features_sha256: &'a str,
    pub rust_sources_sha256: &'a str,
    pub v8_archive_sha256: &'a str,
    pub js_sources_sha256: &'a str,
    pub deno_core_version: &'a str,
    pub snapshot_size: u64,
}

pub fn validate_snapshot_identity(
    manifest: ManifestIdentity<'_>,
    expected: SnapshotIdentity<'_>,
) -> Result<(), String> {
    require_identity_field(
        "schema_version",
        manifest.schema_version,
        expected.schema_version,
    )?;
    require_identity_field(
        "snapshot_kind",
        manifest.snapshot_kind,
        expected.snapshot_kind,
    )?;
    require_identity_field("profile", manifest.profile, expected.profile)?;
    require_identity_field("arch", manifest.arch, expected.arch)?;
    require_identity_field(
        "features_sha256",
        manifest.features_sha256,
        expected.features_sha256,
    )?;
    require_identity_field(
        "rust_sources_sha256",
        manifest.rust_sources_sha256,
        expected.rust_sources_sha256,
    )?;
    require_identity_field(
        "v8_archive_sha256",
        manifest.v8_archive_sha256,
        expected.v8_archive_sha256,
    )?;
    require_identity_field(
        "js_sources_sha256",
        manifest.js_sources_sha256,
        expected.js_sources_sha256,
    )?;
    require_identity_field(
        "deno_core_version",
        manifest.deno_core_version,
        expected.deno_core_version,
    )?;
    require_identity_field(
        "snapshot_size",
        manifest.snapshot_size,
        expected.snapshot_size,
    )
}

fn require_identity_field<T>(field: &str, actual: Option<T>, expected: T) -> Result<(), String>
where
    T: Copy + PartialEq + std::fmt::Display,
{
    match actual {
        Some(actual) if actual == expected => Ok(()),
        Some(actual) => Err(format!(
            "manifest {field} mismatch (manifest={actual}, target={expected})"
        )),
        None => Err(format!("manifest field {field:?} is missing or invalid")),
    }
}

pub fn snapshot_profile_features(profile: &str) -> Result<&'static [&'static str], String> {
    match profile {
        "full" => Ok(FULL_FEATURES),
        "slim" => Ok(SLIM_FEATURES),
        _ => Err(format!("unsupported snapshot product profile: {profile}")),
    }
}

pub fn snapshot_feature_hash(features: &[&str]) -> String {
    let mut features = features.to_vec();
    features.sort_unstable_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
    let mut hasher = Sha256::new();
    for feature in features {
        hasher.update(feature.as_bytes());
        hasher.update(b"\n");
    }
    hex_digest(hasher.finalize().as_slice())
}

/// Recursively collect every `*.js` file under `dir` via a filesystem walk (not
/// a git query), so untracked/generated embedded JS also invalidates a snapshot
/// and enumeration works without a `.git` directory. Symlinks are skipped
/// (`file_type()` does not follow them), matching `find` without `-L`.
///
/// Enumeration is atomic from the caller's perspective: on ANY read failure
/// (e.g. an unreadable or newly-added directory the traversal cannot open) the
/// whole call returns `Err` and no partial `Vec` is produced. This is a
/// correctness red line — a truncated set could hash-match a stale manifest and
/// wrongly accept an outdated snapshot, so callers must never validate/embed on
/// error and instead fall back to source JS.
pub fn collect_js_files(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    collect_files_with_extension(dir, "js")
}

/// Conservatively fingerprint every Rust source in `runtime-v8` and
/// `snapshot-gen`, both crates' feature manifests, and the workspace lockfile.
/// This covers op names/signatures/order, extension assembly, generator options,
/// dependency resolution, and V8 configuration. A non-snapshot refactor may
/// reject a still-compatible snapshot; accepting an incompatible snapshot is
/// the unsafe failure mode, so false positives are intentional here.
pub fn collect_rust_files(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let crates_dir = dir.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "runtime-v8 directory has no crates parent",
        )
    })?;
    let engine_dir = crates_dir.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "crates directory has no engine parent",
        )
    })?;
    let snapshot_gen = crates_dir.parent().expect("crates dir has a parent").join("tools/snapshot-gen");
    let mut output = collect_files_with_extension(dir, "rs")?;
    collect_files_into(&snapshot_gen, "rs", &mut output)?;
    for path in [
        dir.join("Cargo.toml"),
        snapshot_gen.join("Cargo.toml"),
        engine_dir.join("Cargo.lock"),
    ] {
        if !std::fs::metadata(&path)?.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("snapshot runtime input is not a file: {}", path.display()),
            ));
        }
        output.push(path);
    }
    Ok(output)
}

fn collect_files_with_extension(dir: &Path, extension: &str) -> std::io::Result<Vec<PathBuf>> {
    let mut output = Vec::new();
    collect_files_into(dir, extension, &mut output)?;
    Ok(output)
}

fn collect_files_into(
    dir: &Path,
    extension: &str,
    output: &mut Vec<PathBuf>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_files_into(&path, extension, output)?;
        } else if file_type.is_file() && path.extension().is_some_and(|ext| ext == extension) {
            output.push(path);
        }
    }
    Ok(())
}

/// Repo-root-relative, forward-slash path. Used both for ordering and for the
/// hashed line (matches the `sha256sum` filename column emitted by the shell
/// pipeline). On the Linux build host paths are already `/`-separated; the
/// `\\` → `/` replacement is only a Windows-host safety net.
pub fn relative_slash_path(path: &Path, repo_root: &Path) -> Result<String, String> {
    let relative = path
        .strip_prefix(repo_root)
        .map_err(|error| format!("source path {} is outside repo: {error}", path.display()))?;
    Ok(relative.to_string_lossy().replace('\\', "/"))
}

/// Fingerprint of all extension JS. Order-independent of the `js_files` slice:
/// it sorts internally so callers need not pre-sort.
pub fn snapshot_js_hash(repo_root: &Path, js_files: &[PathBuf]) -> Result<String, String> {
    snapshot_files_hash(repo_root, js_files)
}

pub fn snapshot_runtime_hash(repo_root: &Path, rust_files: &[PathBuf]) -> Result<String, String> {
    snapshot_files_hash(repo_root, rust_files)
}

fn snapshot_files_hash(repo_root: &Path, files: &[PathBuf]) -> Result<String, String> {
    let mut entries: Vec<(String, &Path)> = Vec::with_capacity(files.len());
    for path in files {
        entries.push((relative_slash_path(path, repo_root)?, path.as_path()));
    }
    // Order by the raw bytes of the repo-root-relative path (LC_ALL=C), matching
    // `sort` in scripts/lib/snapshot-fingerprint.sh. This is deliberately NOT
    // `Path`'s component-wise `Ord` (`Vec::<PathBuf>::sort()`): the two disagree
    // whenever a filename byte is below `/` (0x2f) — e.g. `worker.js` vs
    // `worker/x.js` — which would make the two fingerprints diverge.
    entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));

    let mut outer = Sha256::new();
    for (relative, path) in &entries {
        let file_hash = sha256_file(path)
            .map_err(|error| format!("cannot hash source {}: {error}", path.display()))?;
        outer.update(format!("{file_hash}  {relative}\n").as_bytes());
    }
    Ok(hex_digest(outer.finalize().as_slice()))
}

pub fn sha256_file(path: &Path) -> Result<String, std::io::Error> {
    let file = std::fs::File::open(path)?;
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex_digest(hasher.finalize().as_slice()))
}

pub fn require_materialized_size(kind: &str, size: u64) -> Result<(), String> {
    if size < 100_000 {
        Err(format!(
            "{kind} is only {size} bytes (likely a Git LFS pointer)"
        ))
    } else {
        Ok(())
    }
}

pub fn hex_digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}
