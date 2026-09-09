//! Code integrity verification: manifest signature + file hash checks.
//!
//! Provides Ed25519 signature verification of a `manifest.json` file
//! and SHA256 hash verification of individual source files listed in the manifest.
//!
//! # Manifest Format
//!
//! ```json
//! {
//!   "version": 1,
//!   "entry": "game.js",
//!   "timestamp": 1709078400,
//!   "files": {
//!     "game.js": "a1b2c3d4e5f6...",
//!     "lib/utils.js": "f7e8d9c0b1a2..."
//!   }
//! }
//! ```
//!
//! # Signature
//!
//! The `manifest.sig` file contains the raw Ed25519 signature (64 bytes)
//! of the exact bytes of `manifest.json`.
//!
//! # Hash Cache
//!
//! File hashes are cached by (path, mtime, size, file identity) to avoid
//! redundant re-hashing on repeated calls (e.g., game restart without changes).

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{EngineError, EngineResult, ErrorCode};

pub const MAX_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_RECEIPT_BYTES: u64 = 16 * 1024;
pub const MAX_MANIFEST_FILES: usize = 65_536;
const PROMOTION_HASH_BUFFER_BYTES: usize = 64 * 1024;
const MAX_CODE_TREE_ENTRIES: usize = MAX_MANIFEST_FILES * 2 + 2;

const INSTALL_RECEIPT_SCHEMA: u32 = 1;
const SEAL_POLICY_VERSION: u32 = 1;
const GENERATION_ANCHOR_MAGIC: [u8; 8] = *b"MIGOR8G1";
const GENERATION_ANCHOR_RECORD_BYTES: usize = 24;
const GENERATION_ANCHOR_SLOTS: usize = 2;
static RECEIPT_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);
#[cfg(test)]
std::thread_local! {
    static MANIFEST_PARSE_COUNT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

// ---------------------------------------------------------------------------
// Manifest schema
// ---------------------------------------------------------------------------

/// Manifest schema for code signing verification.
///
/// Describes the game package contents, entry point, and per-file SHA256 hashes.
/// The manifest itself is signed with Ed25519 to prevent tampering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// Schema version. Currently must be 1.
    pub version: u32,
    /// Entry point file path (relative to code_dir), e.g. `"game.js"`.
    pub entry: String,
    /// Build timestamp (Unix seconds UTC). Used for staleness detection.
    pub timestamp: u64,
    /// Map of relative file paths to their lowercase SHA256 hex digests.
    pub files: HashMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationMode {
    Receipt,
    Full,
}

#[derive(Debug, Clone)]
pub struct VerifiedPackage {
    pub generation: u64,
    pub mode: VerificationMode,
    pub files_hashed: usize,
}

struct SignedManifest {
    bytes: Vec<u8>,
    sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallReceipt {
    schema: u32,
    seal_policy: u32,
    generation: u64,
    manifest_sha256: String,
    pubkey_sha256: String,
    entry: String,
    root: CodeRootIdentity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeRootIdentity {
    dev: u64,
    ino: u64,
    ctime_secs: i64,
    ctime_nanos: i64,
    mode: u32,
}

// ---------------------------------------------------------------------------
// Hash cache entry
// ---------------------------------------------------------------------------

/// Cached hash for a single file, keyed by (mtime, size).
#[derive(Debug, Clone)]
struct CachedHash {
    mtime: SystemTime,
    size: u64,
    file_id: FileIdentity,
    hash_hex: String,
}

/// Best-effort stable file identity used to make cache hits safer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
}

impl FileIdentity {
    fn from_metadata(meta: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            return Self {
                dev: meta.dev(),
                ino: meta.ino(),
            };
        }
        #[cfg(not(unix))]
        {
            let _ = meta;
            Self {}
        }
    }
}

// ---------------------------------------------------------------------------
// IntegrityVerifier
// ---------------------------------------------------------------------------

/// Code integrity verifier with Ed25519 signature check and file hash cache.
///
/// # Lifecycle
///
/// 1. Construct once per host with [`IntegrityVerifier::from_hex_pubkey`].
/// 2. Call [`IntegrityVerifier::verify_launch_receipt`] before loading game JS.
/// 3. On a miss, delegate [`IntegrityVerifier::verify_and_promote_for_launch`]
///    to the bounded filesystem executor before loading game JS.
/// 4. [`IntegrityVerifier::verify_entry`] and
///    [`IntegrityVerifier::verify_all_files`] remain legacy/manual APIs.
#[derive(Debug, Clone)]
pub struct IntegrityVerifier {
    pubkey: VerifyingKey,
    pubkey_sha256: String,
    cache: HashMap<PathBuf, CachedHash>,
}

impl IntegrityVerifier {
    /// Create a verifier from raw 32-byte Ed25519 public key bytes.
    pub fn from_pubkey_bytes(bytes: &[u8; 32]) -> EngineResult<Self> {
        let pubkey = VerifyingKey::from_bytes(bytes).map_err(|e| {
            EngineError::new(ErrorCode::CodeSignatureInvalid)
                .with_msg("invalid Ed25519 public key")
                .with_detail(e.to_string())
        })?;
        let pubkey_sha256 = sha256_bytes(&pubkey.to_bytes());
        Ok(Self {
            pubkey,
            pubkey_sha256,
            cache: HashMap::new(),
        })
    }

    /// Create a verifier from a hex-encoded public key string (64 hex chars = 32 bytes).
    pub fn from_hex_pubkey(hex_str: &str) -> EngineResult<Self> {
        let bytes = hex::decode(hex_str).map_err(|e| {
            EngineError::new(ErrorCode::CodeSignatureInvalid)
                .with_msg("invalid hex public key")
                .with_detail(e.to_string())
        })?;
        if bytes.len() != 32 {
            return Err(EngineError::new(ErrorCode::CodeSignatureInvalid)
                .with_msg("public key must be 32 bytes")
                .with_detail(format!("got {} bytes", bytes.len())));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        Self::from_pubkey_bytes(&arr)
    }

    // -----------------------------------------------------------------------
    // Public verification API
    // -----------------------------------------------------------------------

    /// Verify manifest signature and entry file integrity (MVP).
    ///
    /// Steps:
    /// 1. Read `manifest.json` from `code_dir`.
    /// 2. Read `manifest.sig` and verify the Ed25519 signature.
    /// 3. Parse the manifest and validate schema version.
    /// 4. Ensure the manifest's `entry` matches the requested `entry`.
    /// 5. Verify the entry file's SHA256 hash against the manifest.
    ///
    /// # Returns
    ///
    /// The parsed [`Manifest`] on success, allowing the caller to optionally
    /// run [`verify_all_files`] for full-package verification.
    pub fn verify_entry(&mut self, code_dir: &Path, entry: &str) -> EngineResult<Manifest> {
        // 1. Read manifest.json
        let manifest_path = code_dir.join("manifest.json");
        let manifest_bytes = std::fs::read(&manifest_path).map_err(|e| {
            EngineError::new(ErrorCode::CodeSignatureInvalid)
                .with_msg("read manifest.json")
                .with_detail(format!("{}: {}", manifest_path.display(), e))
        })?;

        // 2. Read manifest.sig
        let sig_path = code_dir.join("manifest.sig");
        let sig_bytes = std::fs::read(&sig_path).map_err(|e| {
            EngineError::new(ErrorCode::CodeSignatureInvalid)
                .with_msg("read manifest.sig")
                .with_detail(format!("{}: {}", sig_path.display(), e))
        })?;

        // 3. Verify Ed25519 signature
        self.verify_signature(&manifest_bytes, &sig_bytes)?;

        // 4. Parse manifest
        let manifest: Manifest = serde_json::from_slice(&manifest_bytes).map_err(|e| {
            EngineError::new(ErrorCode::CodeSignatureInvalid)
                .with_msg("parse manifest.json")
                .with_detail(e.to_string())
        })?;

        // 5. Validate schema version
        if manifest.version != 1 {
            return Err(EngineError::new(ErrorCode::CodeSignatureInvalid)
                .with_msg("unsupported manifest version")
                .with_detail(format!("expected 1, got {}", manifest.version)));
        }

        // 6. Verify manifest entry matches requested entry
        if manifest.entry != entry {
            return Err(EngineError::new(ErrorCode::CodeIntegrityFailed)
                .with_msg("entry mismatch")
                .with_detail(format!(
                    "manifest entry='{}', requested='{}'",
                    manifest.entry, entry
                )));
        }

        // 7. Verify entry file hash
        self.verify_file_hash(code_dir, entry, &manifest.files)?;

        Ok(manifest)
    }

    /// Verify all files listed in the manifest (P1 full-package verification).
    ///
    /// Call this after [`verify_entry`] to ensure every file in the manifest
    /// matches its declared SHA256 hash.
    pub fn verify_all_files(&mut self, code_dir: &Path, manifest: &Manifest) -> EngineResult<()> {
        for (path, _) in &manifest.files {
            self.verify_file_hash(code_dir, path, &manifest.files)?;
        }
        Ok(())
    }

    /// Clear the file hash cache.
    ///
    /// Call on game restart or after file updates to force re-verification.
    pub fn clear_cache(&mut self) {
        self.cache.clear();
    }

    /// Check whether the persistent install receipt proves this sealed code
    /// root was fully verified for the current signed manifest.
    pub fn verify_launch_receipt(
        &self,
        code_dir: &Path,
        receipt_path: &Path,
        entry: &str,
    ) -> EngineResult<Option<VerifiedPackage>> {
        #[cfg(not(unix))]
        {
            let _ = (code_dir, receipt_path, entry);
            return Ok(None);
        }

        #[cfg(unix)]
        {
            // Reject an obvious receipt/root miss before reading and verifying
            // the manifest. The FS worker will read the signed envelope once
            // after taking the promotion lock.
            let Some(receipt) =
                self.read_matching_receipt_candidate(code_dir, receipt_path, entry)?
            else {
                return Ok(None);
            };
            let signed_manifest = self.read_and_verify_signed_manifest(code_dir)?;
            Ok(Self::verified_receipt_for_manifest(
                receipt,
                &signed_manifest.sha256,
            ))
        }
    }

    fn match_launch_receipt(
        &self,
        code_dir: &Path,
        receipt_path: &Path,
        entry: &str,
        manifest_sha256: &str,
    ) -> EngineResult<Option<VerifiedPackage>> {
        #[cfg(not(unix))]
        {
            let _ = (receipt_path, manifest_sha256);
            return Ok(None);
        }

        #[cfg(unix)]
        {
            let Some(receipt) =
                self.read_matching_receipt_candidate(code_dir, receipt_path, entry)?
            else {
                return Ok(None);
            };
            Ok(Self::verified_receipt_for_manifest(
                receipt,
                manifest_sha256,
            ))
        }
    }

    #[cfg(unix)]
    fn read_matching_receipt_candidate(
        &self,
        code_dir: &Path,
        receipt_path: &Path,
        entry: &str,
    ) -> EngineResult<Option<InstallReceipt>> {
        let receipt_bytes = match read_bounded_regular_file(
            receipt_path,
            MAX_RECEIPT_BYTES,
            "install receipt",
            ErrorCode::CodeIntegrityFailed,
        ) {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::warn!(
                    "cannot read install receipt '{}': {}; re-verifying package",
                    receipt_path.display(),
                    error
                );
                return Ok(None);
            }
        };
        let receipt: InstallReceipt = match serde_json::from_slice(&receipt_bytes) {
            Ok(receipt) => receipt,
            Err(error) => {
                tracing::warn!(
                    "invalid install receipt '{}': {}; re-verifying package",
                    receipt_path.display(),
                    error
                );
                return Ok(None);
            }
        };
        if receipt.schema != INSTALL_RECEIPT_SCHEMA
            || receipt.seal_policy != SEAL_POLICY_VERSION
            || receipt.generation == 0
            || receipt.pubkey_sha256 != self.pubkey_sha256
            || receipt.entry != entry
        {
            return Ok(None);
        }
        let Some(root) = code_root_identity(code_dir, true)? else {
            return Ok(None);
        };
        if receipt.root != root {
            return Ok(None);
        }
        Ok(Some(receipt))
    }

    #[cfg(unix)]
    fn verified_receipt_for_manifest(
        receipt: InstallReceipt,
        manifest_sha256: &str,
    ) -> Option<VerifiedPackage> {
        if receipt.manifest_sha256 != manifest_sha256 {
            return None;
        }
        Some(VerifiedPackage {
            generation: receipt.generation,
            mode: VerificationMode::Receipt,
            files_hashed: 0,
        })
    }

    /// Fully verify and seal a receipt miss before untrusted JavaScript runs.
    pub fn verify_and_promote_for_launch(
        &self,
        code_dir: &Path,
        receipt_path: &Path,
        entry: &str,
    ) -> EngineResult<VerifiedPackage> {
        #[cfg(unix)]
        let mut promotion_lock = PromotionLock::acquire(&receipt_path.with_extension("lock"))?;

        // Read after acquiring the per-game lock. Another host/process may
        // have completed promotion while this caller waited, in which case
        // this receipt recheck avoids repeated full-package hashing.
        let signed_manifest = self.read_and_verify_signed_manifest(code_dir)?;
        if let Some(verified) =
            self.match_launch_receipt(code_dir, receipt_path, entry, &signed_manifest.sha256)?
        {
            return Ok(verified);
        }

        let manifest = self.parse_and_validate_manifest(&signed_manifest.bytes, entry)?;

        #[cfg(unix)]
        {
            let initial_root = code_root_identity(code_dir, false)?.ok_or_else(|| {
                EngineError::new(ErrorCode::CodeIntegrityFailed)
                    .with_msg("code root identity is unavailable before sealing")
            })?;
            validate_exact_code_tree(code_dir, &manifest)?;
            seal_tree_read_only(code_dir)?;
            let sealed_root = code_root_identity(code_dir, true)?.ok_or_else(|| {
                EngineError::new(ErrorCode::CodeIntegrityFailed)
                    .with_msg("sealed code root identity is unavailable")
            })?;
            if initial_root.dev != sealed_root.dev || initial_root.ino != sealed_root.ino {
                return Err(EngineError::new(ErrorCode::CodeIntegrityFailed)
                    .with_msg("code root changed while sealing"));
            }
            let files_hashed = self.verify_all_files_exact(code_dir, &manifest)?;
            let root = code_root_identity(code_dir, true)?.ok_or_else(|| {
                EngineError::new(ErrorCode::CodeIntegrityFailed)
                    .with_msg("sealed code root identity is unavailable after hashing")
            })?;
            if root != sealed_root {
                return Err(EngineError::new(ErrorCode::CodeIntegrityFailed)
                    .with_msg("code root changed during exact verification"));
            }
            let generation = promotion_lock.next_generation(receipt_path)?;
            let receipt = InstallReceipt {
                schema: INSTALL_RECEIPT_SCHEMA,
                seal_policy: SEAL_POLICY_VERSION,
                generation,
                manifest_sha256: signed_manifest.sha256,
                pubkey_sha256: self.pubkey_sha256.clone(),
                entry: entry.to_string(),
                root,
            };
            promotion_lock.persist_generation(generation)?;
            write_receipt_atomic(receipt_path, &receipt)?;
            let confirmed = self
                .verify_launch_receipt(code_dir, receipt_path, entry)?
                .ok_or_else(|| {
                    EngineError::new(ErrorCode::CodeIntegrityFailed)
                        .with_msg("committed install receipt did not verify")
                })?;
            if confirmed.generation != generation {
                return Err(EngineError::new(ErrorCode::CodeIntegrityFailed)
                    .with_msg("committed install receipt generation mismatch"));
            }
            return Ok(VerifiedPackage {
                generation,
                mode: VerificationMode::Full,
                files_hashed,
            });
        }

        #[cfg(not(unix))]
        {
            let files_hashed = self.verify_all_files_exact(code_dir, &manifest)?;
            Ok(VerifiedPackage {
                generation: 0,
                mode: VerificationMode::Full,
                files_hashed,
            })
        }
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Verify the Ed25519 signature of raw manifest bytes.
    fn verify_signature(&self, manifest_bytes: &[u8], sig_bytes: &[u8]) -> EngineResult<()> {
        if sig_bytes.len() != 64 {
            return Err(EngineError::new(ErrorCode::CodeSignatureInvalid)
                .with_msg("invalid signature length")
                .with_detail(format!("expected 64 bytes, got {}", sig_bytes.len())));
        }

        let sig = Signature::from_slice(sig_bytes).map_err(|e| {
            EngineError::new(ErrorCode::CodeSignatureInvalid)
                .with_msg("parse Ed25519 signature")
                .with_detail(e.to_string())
        })?;

        self.pubkey.verify(manifest_bytes, &sig).map_err(|e| {
            EngineError::new(ErrorCode::CodeSignatureInvalid)
                .with_msg("manifest signature verification failed")
                .with_detail(e.to_string())
        })?;

        Ok(())
    }

    fn read_and_verify_signed_manifest(&self, code_dir: &Path) -> EngineResult<SignedManifest> {
        let manifest_path = code_dir.join("manifest.json");
        let manifest_bytes = read_bounded_regular_file(
            &manifest_path,
            MAX_MANIFEST_BYTES,
            "manifest.json",
            ErrorCode::CodeSignatureInvalid,
        )?;
        let signature_path = code_dir.join("manifest.sig");
        let signature_bytes = read_bounded_regular_file(
            &signature_path,
            64,
            "manifest.sig",
            ErrorCode::CodeSignatureInvalid,
        )?;
        self.verify_signature(&manifest_bytes, &signature_bytes)?;
        let sha256 = sha256_bytes(&manifest_bytes);
        Ok(SignedManifest {
            bytes: manifest_bytes,
            sha256,
        })
    }

    fn parse_and_validate_manifest(
        &self,
        manifest_bytes: &[u8],
        entry: &str,
    ) -> EngineResult<Manifest> {
        #[cfg(test)]
        MANIFEST_PARSE_COUNT.with(|count| count.set(count.get() + 1));
        let manifest: Manifest = serde_json::from_slice(&manifest_bytes).map_err(|e| {
            EngineError::new(ErrorCode::CodeSignatureInvalid)
                .with_msg("parse manifest.json")
                .with_detail(e.to_string())
        })?;
        if manifest.version != 1 {
            return Err(EngineError::new(ErrorCode::CodeSignatureInvalid)
                .with_msg("unsupported manifest version")
                .with_detail(format!("expected 1, got {}", manifest.version)));
        }
        if manifest.entry != entry {
            return Err(EngineError::new(ErrorCode::CodeIntegrityFailed)
                .with_msg("entry mismatch")
                .with_detail(format!(
                    "manifest entry='{}', requested='{}'",
                    manifest.entry, entry
                )));
        }
        validate_manifest_contract(&manifest)?;
        Ok(manifest)
    }

    fn verify_all_files_exact(&self, code_dir: &Path, manifest: &Manifest) -> EngineResult<usize> {
        validate_exact_code_tree(code_dir, manifest)?;
        let mut paths = manifest.files.keys().collect::<Vec<_>>();
        paths.sort_unstable();
        let mut buffer = vec![0u8; PROMOTION_HASH_BUFFER_BYTES];
        for relative_path in &paths {
            validate_manifest_relative_path(relative_path)?;
            let expected_hash = &manifest.files[*relative_path];
            // The exact walk just proved every component is physical and the
            // Unix promotion path seals all directories before hashing. Avoid
            // two canonicalize syscalls per file; the final open still uses
            // O_NOFOLLOW and validates the opened object is regular.
            #[cfg(unix)]
            let full_path = code_dir.join(relative_path);
            #[cfg(not(unix))]
            let full_path = secure_join_under_code_dir(code_dir, relative_path)?;
            let actual_hash = sha256_file_with_buffer(&full_path, &mut buffer)?;
            if actual_hash != *expected_hash {
                return Err(EngineError::new(ErrorCode::CodeIntegrityFailed)
                    .with_msg("file hash mismatch")
                    .with_detail(format!(
                        "file='{relative_path}', expected='{expected_hash}', actual='{actual_hash}'"
                    )));
            }
        }
        Ok(paths.len())
    }

    /// Verify a single file's SHA256 hash against the manifest.
    fn verify_file_hash(
        &mut self,
        code_dir: &Path,
        relative_path: &str,
        files: &HashMap<String, String>,
    ) -> EngineResult<()> {
        validate_manifest_relative_path(relative_path)?;
        let expected_hash = files.get(relative_path).ok_or_else(|| {
            EngineError::new(ErrorCode::CodeIntegrityFailed)
                .with_msg("file not listed in manifest")
                .with_detail(format!("'{}'", relative_path))
        })?;

        let full_path = secure_join_under_code_dir(code_dir, relative_path)?;
        let actual_hash = self.hash_file_cached(&full_path)?;

        if actual_hash != *expected_hash {
            return Err(EngineError::new(ErrorCode::CodeIntegrityFailed)
                .with_msg("file hash mismatch")
                .with_detail(format!(
                    "file='{}', expected='{}', actual='{}'",
                    relative_path, expected_hash, actual_hash
                )));
        }

        Ok(())
    }

    /// Compute SHA256 of a file with metadata/file-id cache validation.
    ///
    /// If the file's mtime and size match a cached entry, the cached hash
    /// is returned without re-reading the file.
    fn hash_file_cached(&mut self, path: &Path) -> EngineResult<String> {
        let meta = std::fs::metadata(path).map_err(|e| {
            EngineError::new(ErrorCode::CodeIntegrityFailed)
                .with_msg("read file metadata")
                .with_detail(format!("{}: {}", path.display(), e))
        })?;

        let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let size = meta.len();
        let file_id = FileIdentity::from_metadata(&meta);

        // Check cache hit
        if let Some(cached) = self.cache.get(path) {
            if cached.mtime == mtime && cached.size == size && cached.file_id == file_id {
                return Ok(cached.hash_hex.clone());
            }
        }

        // Cache miss: compute hash
        let hash_hex = sha256_file(path)?;

        // Store in cache
        self.cache.insert(
            path.to_path_buf(),
            CachedHash {
                mtime,
                size,
                file_id,
                hash_hex: hash_hex.clone(),
            },
        );

        Ok(hash_hex)
    }
}

fn read_bounded_regular_file(
    path: &Path,
    max_bytes: u64,
    label: &'static str,
    code: ErrorCode,
) -> EngineResult<Vec<u8>> {
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt;

        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
    };
    #[cfg(not(unix))]
    let file = {
        let metadata = fs::symlink_metadata(path).map_err(|error| {
            EngineError::new(code)
                .with_msg(format!("read {label}"))
                .with_detail(format!("{}: {error}", path.display()))
        })?;
        if metadata.file_type().is_symlink() {
            return Err(EngineError::new(code)
                .with_msg(format!("{label} must not be a symlink"))
                .with_detail(path.display().to_string()));
        }
        OpenOptions::new().read(true).open(path)
    };
    let mut file = file.map_err(|error| {
        EngineError::new(code)
            .with_msg(format!("read {label}"))
            .with_detail(format!("{}: {error}", path.display()))
    })?;
    let metadata = file.metadata().map_err(|error| {
        EngineError::new(code)
            .with_msg(format!("read {label} metadata"))
            .with_detail(format!("{}: {error}", path.display()))
    })?;
    if !metadata.is_file() {
        return Err(EngineError::new(code)
            .with_msg(format!("{label} must be a regular file"))
            .with_detail(path.display().to_string()));
    }
    if metadata.len() > max_bytes {
        return Err(EngineError::new(code)
            .with_msg(format!("{label} exceeds {max_bytes} bytes"))
            .with_detail(format!("actual={}", metadata.len())));
    }
    let read_limit = max_bytes.saturating_add(1);
    let mut bytes = Vec::with_capacity(metadata.len().min(max_bytes) as usize);
    Read::by_ref(&mut file)
        .take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            EngineError::new(code)
                .with_msg(format!("read {label}"))
                .with_detail(format!("{}: {error}", path.display()))
        })?;
    if bytes.len() as u64 > max_bytes {
        return Err(EngineError::new(code)
            .with_msg(format!("{label} exceeds {max_bytes} bytes"))
            .with_detail(format!("actual>{max_bytes}")));
    }
    Ok(bytes)
}

fn validate_exact_code_tree(code_dir: &Path, manifest: &Manifest) -> EngineResult<()> {
    validate_manifest_contract(manifest)?;

    let mut expected = HashSet::with_capacity(manifest.files.len());
    for path in manifest.files.keys() {
        expected.insert(path.as_str());
    }

    let root_metadata = fs::symlink_metadata(code_dir).map_err(|error| {
        EngineError::new(ErrorCode::CodeIntegrityFailed)
            .with_msg("inspect code root")
            .with_detail(format!("{}: {error}", code_dir.display()))
    })?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(EngineError::new(ErrorCode::CodeIntegrityFailed)
            .with_msg("code root must be a physical directory")
            .with_detail(code_dir.display().to_string()));
    }

    let mut seen = HashSet::with_capacity(expected.len());
    visit_exact_code_tree(code_dir, code_dir, &expected, &mut seen)?;
    if seen.len() != expected.len() {
        let mut missing = expected.difference(&seen).copied().collect::<Vec<_>>();
        missing.sort_unstable();
        return Err(EngineError::new(ErrorCode::CodeIntegrityFailed)
            .with_msg("manifest file is missing from code tree")
            .with_detail(missing.first().copied().unwrap_or_default().to_string()));
    }
    Ok(())
}

fn validate_manifest_contract(manifest: &Manifest) -> EngineResult<()> {
    if manifest.files.len() > MAX_MANIFEST_FILES {
        return Err(EngineError::new(ErrorCode::CodeIntegrityFailed)
            .with_msg("manifest contains too many files")
            .with_detail(format!(
                "limit={}, actual={}",
                MAX_MANIFEST_FILES,
                manifest.files.len()
            )));
    }
    if !manifest.files.contains_key(&manifest.entry) {
        return Err(EngineError::new(ErrorCode::CodeIntegrityFailed)
            .with_msg("manifest entry is not listed as a file")
            .with_detail(manifest.entry.clone()));
    }

    for (path, hash) in &manifest.files {
        validate_manifest_relative_path(path)?;
        if path == "manifest.json" || path == "manifest.sig" {
            return Err(EngineError::new(ErrorCode::CodeIntegrityFailed)
                .with_msg("signing metadata must not be listed as package content")
                .with_detail(path.clone()));
        }
        if hash.len() != 64
            || !hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(EngineError::new(ErrorCode::CodeIntegrityFailed)
                .with_msg("manifest file hash must be 64 lowercase hex characters")
                .with_detail(path.clone()));
        }
    }
    Ok(())
}

fn visit_exact_code_tree<'a>(
    root: &Path,
    directory: &Path,
    expected: &HashSet<&'a str>,
    seen: &mut HashSet<&'a str>,
) -> EngineResult<()> {
    let mut directories = vec![directory.to_path_buf()];
    let mut physical_entries = 0usize;
    while let Some(directory) = directories.pop() {
        let entries = fs::read_dir(&directory).map_err(|error| {
            EngineError::new(ErrorCode::CodeIntegrityFailed)
                .with_msg("enumerate code tree")
                .with_detail(format!("{}: {error}", directory.display()))
        })?;
        for entry in entries {
            physical_entries = physical_entries.checked_add(1).ok_or_else(|| {
                EngineError::new(ErrorCode::CodeIntegrityFailed)
                    .with_msg("code tree entry count overflow")
            })?;
            if physical_entries > MAX_CODE_TREE_ENTRIES {
                return Err(EngineError::new(ErrorCode::CodeIntegrityFailed)
                    .with_msg("code tree contains too many physical entries")
                    .with_detail(format!(
                        "limit={MAX_CODE_TREE_ENTRIES}, actual>{MAX_CODE_TREE_ENTRIES}"
                    )));
            }
            let entry = entry.map_err(|error| {
                EngineError::new(ErrorCode::CodeIntegrityFailed)
                    .with_msg("read code tree entry")
                    .with_detail(error.to_string())
            })?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).map_err(|error| {
                EngineError::new(ErrorCode::CodeIntegrityFailed)
                    .with_msg("inspect code tree entry")
                    .with_detail(format!("{}: {error}", path.display()))
            })?;
            let relative = path.strip_prefix(root).map_err(|_| {
                EngineError::new(ErrorCode::CodeIntegrityFailed)
                    .with_msg("code tree entry escaped its root")
                    .with_detail(path.display().to_string())
            })?;
            let relative = manifest_path_from_relative(relative)?;

            if metadata.file_type().is_symlink() {
                return Err(EngineError::new(ErrorCode::CodeIntegrityFailed)
                    .with_msg("code tree contains a symlink")
                    .with_detail(relative));
            }
            if metadata.is_dir() {
                directories.push(path);
                continue;
            }
            if !metadata.is_file() {
                return Err(EngineError::new(ErrorCode::CodeIntegrityFailed)
                    .with_msg("code tree contains a non-regular entry")
                    .with_detail(relative));
            }
            if relative == "manifest.json" || relative == "manifest.sig" {
                continue;
            }
            let Some(expected_path) = expected.get(relative.as_str()).copied() else {
                return Err(EngineError::new(ErrorCode::CodeIntegrityFailed)
                    .with_msg("code tree contains an unlisted regular file")
                    .with_detail(relative));
            };
            if !seen.insert(expected_path) {
                return Err(EngineError::new(ErrorCode::CodeIntegrityFailed)
                    .with_msg("code tree contains a duplicate manifest path")
                    .with_detail(relative));
            }
        }
    }
    Ok(())
}

fn manifest_path_from_relative(path: &Path) -> EngineResult<String> {
    let mut result = String::new();
    for component in path.components() {
        let std::path::Component::Normal(component) = component else {
            return Err(EngineError::new(ErrorCode::CodeIntegrityFailed)
                .with_msg("code tree contains a non-normal path")
                .with_detail(path.display().to_string()));
        };
        let component = component.to_str().ok_or_else(|| {
            EngineError::new(ErrorCode::CodeIntegrityFailed)
                .with_msg("code tree contains a non-UTF-8 path")
                .with_detail(path.display().to_string())
        })?;
        if !result.is_empty() {
            result.push('/');
        }
        result.push_str(component);
    }
    Ok(result)
}

#[cfg(unix)]
fn code_root_identity(
    code_dir: &Path,
    require_sealed: bool,
) -> EngineResult<Option<CodeRootIdentity>> {
    use std::os::unix::fs::MetadataExt;

    let metadata = fs::symlink_metadata(code_dir).map_err(|error| {
        EngineError::new(ErrorCode::CodeIntegrityFailed)
            .with_msg("read code root metadata")
            .with_detail(format!("{}: {error}", code_dir.display()))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(EngineError::new(ErrorCode::CodeIntegrityFailed)
            .with_msg("code root must be a physical directory")
            .with_detail(code_dir.display().to_string()));
    }
    if require_sealed && metadata.mode() & 0o222 != 0 {
        return Ok(None);
    }
    Ok(Some(CodeRootIdentity {
        dev: metadata.dev(),
        ino: metadata.ino(),
        ctime_secs: metadata.ctime(),
        ctime_nanos: metadata.ctime_nsec(),
        mode: metadata.mode(),
    }))
}

#[cfg(unix)]
fn seal_tree_read_only(path: &Path) -> EngineResult<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let mut pending = vec![(path.to_path_buf(), false)];
    let mut physical_entries = 0usize;
    while let Some((path, children_visited)) = pending.pop() {
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            EngineError::new(ErrorCode::CodeIntegrityFailed)
                .with_msg("inspect code tree while sealing")
                .with_detail(format!("{}: {error}", path.display()))
        })?;
        if metadata.file_type().is_symlink() {
            return Err(EngineError::new(ErrorCode::CodeIntegrityFailed)
                .with_msg("code tree contains a symlink")
                .with_detail(path.display().to_string()));
        }
        if metadata.is_dir() && !children_visited {
            pending.push((path.clone(), true));
            let entries = fs::read_dir(&path).map_err(|error| {
                EngineError::new(ErrorCode::CodeIntegrityFailed)
                    .with_msg("read code directory while sealing")
                    .with_detail(format!("{}: {error}", path.display()))
            })?;
            for entry in entries {
                physical_entries = physical_entries.checked_add(1).ok_or_else(|| {
                    EngineError::new(ErrorCode::CodeIntegrityFailed)
                        .with_msg("code tree entry count overflow while sealing")
                })?;
                if physical_entries > MAX_CODE_TREE_ENTRIES {
                    return Err(EngineError::new(ErrorCode::CodeIntegrityFailed)
                        .with_msg("code tree contains too many physical entries while sealing"));
                }
                let entry = entry.map_err(|error| {
                    EngineError::new(ErrorCode::CodeIntegrityFailed)
                        .with_msg("read code directory entry while sealing")
                        .with_detail(error.to_string())
                })?;
                pending.push((entry.path(), false));
            }
            continue;
        }
        if !metadata.is_dir() && !metadata.is_file() {
            return Err(EngineError::new(ErrorCode::CodeIntegrityFailed)
                .with_msg("code tree contains a non-regular entry")
                .with_detail(path.display().to_string()));
        }
        let sealed_mode = metadata.mode() & !0o222;
        fs::set_permissions(&path, fs::Permissions::from_mode(sealed_mode)).map_err(|error| {
            EngineError::new(ErrorCode::CodeIntegrityFailed)
                .with_msg("seal verified code tree")
                .with_detail(format!("{}: {error}", path.display()))
        })?;
    }
    Ok(())
}

#[cfg(unix)]
fn active_receipt_generation(receipt_path: &Path) -> u64 {
    match read_bounded_regular_file(
        receipt_path,
        MAX_RECEIPT_BYTES,
        "install receipt",
        ErrorCode::CodeIntegrityFailed,
    ) {
        Ok(bytes) => serde_json::from_slice::<InstallReceipt>(&bytes)
            .ok()
            .filter(|receipt| receipt.schema == INSTALL_RECEIPT_SCHEMA)
            .map(|receipt| receipt.generation)
            .unwrap_or(0),
        Err(_) => 0,
    }
}

#[cfg(unix)]
fn write_receipt_atomic(receipt_path: &Path, receipt: &InstallReceipt) -> EngineResult<()> {
    use std::os::unix::fs::OpenOptionsExt;

    let parent = receipt_path.parent().ok_or_else(|| {
        EngineError::new(ErrorCode::CodeIntegrityFailed)
            .with_msg("install receipt has no parent directory")
    })?;
    fs::create_dir_all(parent).map_err(|error| receipt_io_error("create receipt parent", error))?;
    let name = receipt_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("install-receipt");
    let bytes = serde_json::to_vec(receipt).map_err(|error| {
        EngineError::new(ErrorCode::CodeIntegrityFailed)
            .with_msg("serialize install receipt")
            .with_detail(error.to_string())
    })?;
    if bytes.len() as u64 > MAX_RECEIPT_BYTES {
        return Err(EngineError::new(ErrorCode::CodeIntegrityFailed)
            .with_msg("serialized install receipt exceeds size limit")
            .with_detail(format!("limit={MAX_RECEIPT_BYTES}, actual={}", bytes.len())));
    }
    let mut temp = None;
    for _ in 0..128 {
        let sequence = RECEIPT_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temp_path = parent.join(format!(".{name}.tmp.{}.{}", std::process::id(), sequence));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp_path)
        {
            Ok(file) => {
                temp = Some((file, temp_path));
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(receipt_io_error("create receipt temp", error)),
        }
    }
    let (mut file, temp_path) = temp.ok_or_else(|| {
        EngineError::new(ErrorCode::CodeIntegrityFailed)
            .with_msg("cannot allocate a unique install receipt temp file")
    })?;
    let result = (|| -> EngineResult<()> {
        file.write_all(&bytes)
            .map_err(|error| receipt_io_error("write receipt temp", error))?;
        file.sync_all()
            .map_err(|error| receipt_io_error("sync receipt temp", error))?;
        fs::rename(&temp_path, receipt_path)
            .map_err(|error| receipt_io_error("commit install receipt", error))?;
        fs::File::open(parent)
            .and_then(|dir| dir.sync_all())
            .map_err(|error| receipt_io_error("sync receipt parent", error))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

#[cfg(unix)]
fn receipt_io_error(operation: &'static str, error: std::io::Error) -> EngineError {
    EngineError::new(ErrorCode::CodeIntegrityFailed)
        .with_msg(operation)
        .with_detail(error.to_string())
}

#[cfg(unix)]
struct PromotionLock {
    file: fs::File,
}

#[cfg(unix)]
impl PromotionLock {
    fn acquire(path: &Path) -> EngineResult<Self> {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::OpenOptionsExt;

        let parent = path.parent().ok_or_else(|| {
            EngineError::new(ErrorCode::CodeIntegrityFailed)
                .with_msg("integrity lock has no parent directory")
        })?;
        fs::create_dir_all(parent)
            .map_err(|error| receipt_io_error("create integrity lock parent", error))?;
        if let Ok(metadata) = fs::symlink_metadata(path)
            && metadata.file_type().is_symlink()
        {
            return Err(EngineError::new(ErrorCode::CodeIntegrityFailed)
                .with_msg("integrity lock must not be a symlink")
                .with_detail(path.display().to_string()));
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(|error| receipt_io_error("open integrity lock", error))?;
        let metadata = file
            .metadata()
            .map_err(|error| receipt_io_error("inspect integrity lock", error))?;
        if !metadata.is_file() {
            return Err(EngineError::new(ErrorCode::CodeIntegrityFailed)
                .with_msg("integrity lock must be a regular file")
                .with_detail(path.display().to_string()));
        }
        // SAFETY: `file` owns a live descriptor for the duration of the lock;
        // `flock` neither retains the pointer nor accesses Rust memory.
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if result != 0 {
            return Err(receipt_io_error(
                "lock integrity promotion",
                std::io::Error::last_os_error(),
            ));
        }
        Ok(Self { file })
    }

    fn next_generation(&mut self, receipt_path: &Path) -> EngineResult<u64> {
        self.anchored_generation()?
            .max(active_receipt_generation(receipt_path))
            .checked_add(1)
            .ok_or_else(|| {
                EngineError::new(ErrorCode::CodeIntegrityFailed)
                    .with_msg("install receipt generation exhausted")
            })
    }

    fn anchored_generation(&mut self) -> EngineResult<u64> {
        let mut bytes = [0u8; GENERATION_ANCHOR_RECORD_BYTES * GENERATION_ANCHOR_SLOTS];
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|error| receipt_io_error("seek integrity generation anchor", error))?;
        let mut read = 0;
        while read < bytes.len() {
            match self.file.read(&mut bytes[read..]) {
                Ok(0) => break,
                Ok(count) => read += count,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    return Err(receipt_io_error("read integrity generation anchor", error));
                }
            }
        }

        let mut generation = 0;
        for slot in 0..GENERATION_ANCHOR_SLOTS {
            let offset = slot * GENERATION_ANCHOR_RECORD_BYTES;
            if read < offset + GENERATION_ANCHOR_RECORD_BYTES
                || bytes[offset..offset + 8] != GENERATION_ANCHOR_MAGIC
            {
                continue;
            }
            let value = u64::from_le_bytes(bytes[offset + 8..offset + 16].try_into().unwrap());
            let complement =
                u64::from_le_bytes(bytes[offset + 16..offset + 24].try_into().unwrap());
            if value != 0 && complement == !value {
                generation = generation.max(value);
            }
        }
        Ok(generation)
    }

    fn persist_generation(&mut self, generation: u64) -> EngineResult<()> {
        let slot = (generation & 1) as usize;
        let offset = (slot * GENERATION_ANCHOR_RECORD_BYTES) as u64;
        let mut record = [0u8; GENERATION_ANCHOR_RECORD_BYTES];
        record[..8].copy_from_slice(&GENERATION_ANCHOR_MAGIC);
        record[8..16].copy_from_slice(&generation.to_le_bytes());
        record[16..24].copy_from_slice(&(!generation).to_le_bytes());

        self.file
            .seek(SeekFrom::Start(offset))
            .map_err(|error| receipt_io_error("seek integrity generation anchor", error))?;
        self.file
            .write_all(&record)
            .map_err(|error| receipt_io_error("write integrity generation anchor", error))?;
        self.file
            .sync_all()
            .map_err(|error| receipt_io_error("sync integrity generation anchor", error))
    }
}

#[cfg(unix)]
impl Drop for PromotionLock {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;

        // SAFETY: the descriptor remains owned by `self.file` until after this
        // drop body. Unlock failure cannot be recovered during destruction;
        // closing the file immediately afterwards also releases the lock.
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn validate_manifest_relative_path(relative_path: &str) -> EngineResult<()> {
    if relative_path.is_empty() {
        return Err(
            EngineError::new(ErrorCode::CodeIntegrityFailed).with_msg("manifest path is empty")
        );
    }
    if relative_path.starts_with('/') || relative_path.starts_with('\\') {
        return Err(EngineError::new(ErrorCode::CodeIntegrityFailed)
            .with_msg("manifest path must be relative")
            .with_detail(relative_path.to_string()));
    }

    for b in relative_path.bytes() {
        if b < 0x20 {
            return Err(EngineError::new(ErrorCode::CodeIntegrityFailed)
                .with_msg("manifest path contains control characters")
                .with_detail(relative_path.to_string()));
        }
        if b == b'\\' {
            return Err(EngineError::new(ErrorCode::CodeIntegrityFailed)
                .with_msg("manifest path must use '/' separators")
                .with_detail(relative_path.to_string()));
        }
    }

    for component in Path::new(relative_path).components() {
        match component {
            std::path::Component::Normal(_) => {}
            _ => {
                return Err(EngineError::new(ErrorCode::CodeIntegrityFailed)
                    .with_msg("manifest path escapes code directory")
                    .with_detail(relative_path.to_string()));
            }
        }
    }

    Ok(())
}

fn secure_join_under_code_dir(code_dir: &Path, relative_path: &str) -> EngineResult<PathBuf> {
    let full_path = code_dir.join(relative_path);

    // Phase 1: Textual normalization (fast, no I/O)
    let normalized = normalize_textual_path(&full_path);
    let normalized_base = normalize_textual_path(code_dir);
    if !normalized.starts_with(&normalized_base) {
        return Err(EngineError::new(ErrorCode::CodeIntegrityFailed)
            .with_msg("manifest path resolves outside code directory")
            .with_detail(relative_path.to_string()));
    }

    // Phase 2: Filesystem canonicalization (resolves symlinks).
    // If code_dir itself is a symlink, the textual check above could be
    // bypassed.  Canonicalize both paths and re-verify containment.
    if full_path.exists() {
        let canonical_full = std::fs::canonicalize(&full_path).map_err(|e| {
            EngineError::new(ErrorCode::CodeIntegrityFailed)
                .with_msg("canonicalize file path")
                .with_detail(format!("{}: {}", full_path.display(), e))
        })?;
        let canonical_base = std::fs::canonicalize(code_dir).map_err(|e| {
            EngineError::new(ErrorCode::CodeIntegrityFailed)
                .with_msg("canonicalize code directory")
                .with_detail(format!("{}: {}", code_dir.display(), e))
        })?;
        if !canonical_full.starts_with(&canonical_base) {
            return Err(EngineError::new(ErrorCode::CodeIntegrityFailed)
                .with_msg("file resolves outside code directory (symlink escape)")
                .with_detail(relative_path.to_string()));
        }
    }

    Ok(full_path)
}

fn normalize_textual_path(path: &Path) -> PathBuf {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                components.pop();
            }
            std::path::Component::CurDir => {}
            c => components.push(c),
        }
    }
    components.iter().collect()
}

// ---------------------------------------------------------------------------
// Standalone hash utilities
// ---------------------------------------------------------------------------

/// Compute the SHA256 hex digest of a file using streaming reads
/// to avoid loading the entire file into memory at once.
pub fn sha256_file(path: &Path) -> EngineResult<String> {
    use std::io::Read;

    let file = std::fs::File::open(path).map_err(|e| {
        EngineError::new(ErrorCode::CodeIntegrityFailed)
            .with_msg("read file for hashing")
            .with_detail(format!("{}: {}", path.display(), e))
    })?;
    let mut reader = std::io::BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = reader.read(&mut buf).map_err(|e| {
            EngineError::new(ErrorCode::CodeIntegrityFailed)
                .with_msg("read file for hashing")
                .with_detail(format!("{}: {}", path.display(), e))
        })?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn sha256_file_with_buffer(path: &Path, buffer: &mut [u8]) -> EngineResult<String> {
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt;

        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
    };
    #[cfg(not(unix))]
    let file = OpenOptions::new().read(true).open(path);
    let mut file = file.map_err(|error| {
        EngineError::new(ErrorCode::CodeIntegrityFailed)
            .with_msg("open file for exact hashing")
            .with_detail(format!("{}: {error}", path.display()))
    })?;
    let metadata = file.metadata().map_err(|error| {
        EngineError::new(ErrorCode::CodeIntegrityFailed)
            .with_msg("read exact-hash file metadata")
            .with_detail(format!("{}: {error}", path.display()))
    })?;
    if !metadata.is_file() {
        return Err(EngineError::new(ErrorCode::CodeIntegrityFailed)
            .with_msg("exact-hash target must be a regular file")
            .with_detail(path.display().to_string()));
    }
    if buffer.is_empty() {
        return Err(EngineError::new(ErrorCode::CodeIntegrityFailed)
            .with_msg("exact-hash buffer must not be empty"));
    }

    let mut hasher = Sha256::new();
    loop {
        let count = file.read(buffer).map_err(|error| {
            EngineError::new(ErrorCode::CodeIntegrityFailed)
                .with_msg("read file for exact hashing")
                .with_detail(format!("{}: {error}", path.display()))
        })?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Compute the SHA256 hex digest of a byte slice.
pub fn sha256_bytes(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_DIR_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    /// Create a unique temp directory for a test.
    fn make_test_dir(name: &str) -> PathBuf {
        let sequence = TEST_DIR_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "migo_integrity_test_{name}_{}_{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Sign manifest bytes with the given signing key.
    fn sign_manifest(signing_key: &SigningKey, manifest_json: &[u8]) -> Vec<u8> {
        let sig = signing_key.sign(manifest_json);
        sig.to_bytes().to_vec()
    }

    /// Set up a complete signed game package and return (signing_key, pubkey_bytes).
    fn setup_signed_package(
        dir: &Path,
        entry: &str,
        entry_content: &str,
    ) -> (SigningKey, [u8; 32]) {
        let signing_key = SigningKey::from_bytes(&[42u8; 32]);
        let verifying_key = signing_key.verifying_key();

        // Write entry file
        fs::write(dir.join(entry), entry_content).unwrap();

        // Compute hash
        let hash = sha256_bytes(entry_content.as_bytes());

        // Create manifest
        let mut files = HashMap::new();
        files.insert(entry.to_string(), hash);

        let manifest = Manifest {
            version: 1,
            entry: entry.to_string(),
            timestamp: 1709078400,
            files,
        };

        let manifest_json = serde_json::to_vec_pretty(&manifest).unwrap();

        // Sign and write
        let sig = sign_manifest(&signing_key, &manifest_json);
        fs::write(dir.join("manifest.json"), &manifest_json).unwrap();
        fs::write(dir.join("manifest.sig"), &sig).unwrap();

        (signing_key, verifying_key.to_bytes())
    }

    #[test]
    fn absent_receipt_skips_manifest_io_on_the_host_fast_path() {
        let dir = make_test_dir("receipt_absent_skips_manifest");
        let receipt = dir.with_extension("integrity-receipt.json");
        let signing_key = SigningKey::from_bytes(&[42u8; 32]);
        let verifier =
            IntegrityVerifier::from_pubkey_bytes(&signing_key.verifying_key().to_bytes()).unwrap();

        let result = verifier.verify_launch_receipt(&dir, &receipt, "game.js");

        assert!(matches!(result, Ok(None)));
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_file(&receipt);
    }

    #[cfg(unix)]
    #[test]
    fn bounded_regular_file_probe_never_waits_for_a_fifo_writer() {
        use std::{
            ffi::CString,
            os::unix::ffi::OsStrExt,
            thread,
            time::{Duration, Instant},
        };

        let dir = make_test_dir("bounded_probe_fifo");
        let fifo = dir.join("manifest.json");
        let fifo_c = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);

        let writer_path = fifo.clone();
        let writer = thread::spawn(move || {
            thread::sleep(Duration::from_millis(250));
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(writer_path)
                .unwrap()
        });
        let started = Instant::now();
        let result = read_bounded_regular_file(
            &fifo,
            MAX_MANIFEST_BYTES,
            "manifest.json",
            ErrorCode::CodeSignatureInvalid,
        );
        let probe_elapsed = started.elapsed();
        writer.join().unwrap();

        assert!(result.is_err());
        assert!(
            probe_elapsed < Duration::from_millis(100),
            "FIFO probe blocked for {probe_elapsed:?} before checking the file type"
        );
        let _ = fs::remove_file(&fifo);
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn promotion_lock_rejects_a_fifo_before_generation_io() {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};

        let dir = make_test_dir("promotion_lock_fifo");
        let lock = dir.join("integrity.lock");
        let lock_c = CString::new(lock.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(lock_c.as_ptr(), 0o600) }, 0);

        let result = PromotionLock::acquire(&lock);

        assert!(
            result.is_err(),
            "a FIFO must never be accepted as a lock file"
        );
        drop(result);
        let _ = fs::remove_file(&lock);
        let _ = fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // Basic success case
    // -----------------------------------------------------------------------

    #[test]
    fn test_valid_signature_and_hash() {
        let dir = make_test_dir("valid");
        let (_sk, pubkey) = setup_signed_package(&dir, "game.js", "console.log('hello');");

        let mut verifier = IntegrityVerifier::from_pubkey_bytes(&pubkey).unwrap();
        let manifest = verifier.verify_entry(&dir, "game.js").unwrap();

        assert_eq!(manifest.version, 1);
        assert_eq!(manifest.entry, "game.js");
        assert_eq!(manifest.timestamp, 1709078400);
        assert!(manifest.files.contains_key("game.js"));

        let _ = fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // Tampered JS file
    // -----------------------------------------------------------------------

    #[test]
    fn test_tampered_js_file_rejected() {
        let dir = make_test_dir("tampered_js");
        let (_sk, pubkey) = setup_signed_package(&dir, "game.js", "console.log('hello');");

        // Tamper with the JS file after signing
        fs::write(dir.join("game.js"), "console.log('HACKED');").unwrap();

        let mut verifier = IntegrityVerifier::from_pubkey_bytes(&pubkey).unwrap();
        let result = verifier.verify_entry(&dir, "game.js");

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, ErrorCode::CodeIntegrityFailed);

        let _ = fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // Tampered manifest (signature mismatch)
    // -----------------------------------------------------------------------

    #[test]
    fn test_tampered_manifest_rejected() {
        let dir = make_test_dir("tampered_manifest");
        let (_sk, pubkey) = setup_signed_package(&dir, "game.js", "console.log('hello');");

        // Tamper with manifest.json (change timestamp) without re-signing
        let manifest_path = dir.join("manifest.json");
        let mut manifest_json: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest_json["timestamp"] = serde_json::json!(9999999);
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest_json).unwrap(),
        )
        .unwrap();

        let mut verifier = IntegrityVerifier::from_pubkey_bytes(&pubkey).unwrap();
        let result = verifier.verify_entry(&dir, "game.js");

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, ErrorCode::CodeSignatureInvalid);

        let _ = fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // Wrong public key
    // -----------------------------------------------------------------------

    #[test]
    fn test_wrong_public_key_rejected() {
        let dir = make_test_dir("wrong_key");
        let (_sk, _pubkey) = setup_signed_package(&dir, "game.js", "console.log('hello');");

        // Derive a different key pair
        let wrong_signing = SigningKey::from_bytes(&[99u8; 32]);
        let wrong_pubkey = wrong_signing.verifying_key().to_bytes();

        let mut verifier = IntegrityVerifier::from_pubkey_bytes(&wrong_pubkey).unwrap();
        let result = verifier.verify_entry(&dir, "game.js");

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, ErrorCode::CodeSignatureInvalid);

        let _ = fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // Missing manifest files
    // -----------------------------------------------------------------------

    #[test]
    fn test_missing_manifest_rejected() {
        let dir = make_test_dir("missing_manifest");
        fs::write(dir.join("game.js"), "console.log('hello');").unwrap();

        let signing_key = SigningKey::from_bytes(&[42u8; 32]);
        let pubkey = signing_key.verifying_key().to_bytes();

        let mut verifier = IntegrityVerifier::from_pubkey_bytes(&pubkey).unwrap();
        let result = verifier.verify_entry(&dir, "game.js");

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, ErrorCode::CodeSignatureInvalid);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_missing_signature_rejected() {
        let dir = make_test_dir("missing_sig");

        // Write manifest without signature
        let mut files = HashMap::new();
        files.insert(
            "game.js".to_string(),
            sha256_bytes(b"console.log('hello');"),
        );
        let manifest = Manifest {
            version: 1,
            entry: "game.js".to_string(),
            timestamp: 1709078400,
            files,
        };
        fs::write(
            dir.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        fs::write(dir.join("game.js"), "console.log('hello');").unwrap();

        let signing_key = SigningKey::from_bytes(&[42u8; 32]);
        let pubkey = signing_key.verifying_key().to_bytes();

        let mut verifier = IntegrityVerifier::from_pubkey_bytes(&pubkey).unwrap();
        let result = verifier.verify_entry(&dir, "game.js");

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, ErrorCode::CodeSignatureInvalid);

        let _ = fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // Entry mismatch
    // -----------------------------------------------------------------------

    #[test]
    fn test_entry_mismatch_rejected() {
        let dir = make_test_dir("entry_mismatch");
        let (_sk, pubkey) = setup_signed_package(&dir, "game.js", "console.log('hello');");

        let mut verifier = IntegrityVerifier::from_pubkey_bytes(&pubkey).unwrap();
        // Request a different entry than what's in the manifest
        let result = verifier.verify_entry(&dir, "other.js");

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, ErrorCode::CodeIntegrityFailed);

        let _ = fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // Hash cache
    // -----------------------------------------------------------------------

    #[test]
    fn test_hash_cache_works() {
        let dir = make_test_dir("hash_cache");
        let (_sk, pubkey) = setup_signed_package(&dir, "game.js", "console.log('hello');");

        let mut verifier = IntegrityVerifier::from_pubkey_bytes(&pubkey).unwrap();

        // First call populates cache
        assert!(verifier.verify_entry(&dir, "game.js").is_ok());
        assert!(!verifier.cache.is_empty());

        // Second call uses cache (no I/O for the JS file hash)
        assert!(verifier.verify_entry(&dir, "game.js").is_ok());

        // Clear and verify cache is empty
        verifier.clear_cache();
        assert!(verifier.cache.is_empty());

        // Still works after clearing
        assert!(verifier.verify_entry(&dir, "game.js").is_ok());

        let _ = fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // verify_all_files (P1)
    // -----------------------------------------------------------------------

    #[test]
    fn test_verify_all_files_pass() {
        let dir = make_test_dir("verify_all_pass");
        let signing_key = SigningKey::from_bytes(&[42u8; 32]);
        let pubkey = signing_key.verifying_key().to_bytes();

        // Write multiple files
        fs::write(dir.join("game.js"), "main code").unwrap();
        fs::create_dir_all(dir.join("lib")).unwrap();
        fs::write(dir.join("lib/utils.js"), "utils code").unwrap();

        // Create manifest
        let mut files = HashMap::new();
        files.insert("game.js".to_string(), sha256_bytes(b"main code"));
        files.insert("lib/utils.js".to_string(), sha256_bytes(b"utils code"));

        let manifest = Manifest {
            version: 1,
            entry: "game.js".to_string(),
            timestamp: 1709078400,
            files,
        };
        let manifest_json = serde_json::to_vec_pretty(&manifest).unwrap();
        let sig = sign_manifest(&signing_key, &manifest_json);
        fs::write(dir.join("manifest.json"), &manifest_json).unwrap();
        fs::write(dir.join("manifest.sig"), &sig).unwrap();

        let mut verifier = IntegrityVerifier::from_pubkey_bytes(&pubkey).unwrap();
        let parsed = verifier.verify_entry(&dir, "game.js").unwrap();
        assert!(verifier.verify_all_files(&dir, &parsed).is_ok());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_verify_all_files_tampered_dependency() {
        let dir = make_test_dir("verify_all_tampered");
        let signing_key = SigningKey::from_bytes(&[42u8; 32]);
        let pubkey = signing_key.verifying_key().to_bytes();

        // Write files
        fs::write(dir.join("game.js"), "main code").unwrap();
        fs::create_dir_all(dir.join("lib")).unwrap();
        fs::write(dir.join("lib/utils.js"), "utils code").unwrap();

        // Create manifest
        let mut files = HashMap::new();
        files.insert("game.js".to_string(), sha256_bytes(b"main code"));
        files.insert("lib/utils.js".to_string(), sha256_bytes(b"utils code"));

        let manifest = Manifest {
            version: 1,
            entry: "game.js".to_string(),
            timestamp: 1709078400,
            files,
        };
        let manifest_json = serde_json::to_vec_pretty(&manifest).unwrap();
        let sig = sign_manifest(&signing_key, &manifest_json);
        fs::write(dir.join("manifest.json"), &manifest_json).unwrap();
        fs::write(dir.join("manifest.sig"), &sig).unwrap();

        // Tamper with a dependency
        fs::write(dir.join("lib/utils.js"), "HACKED").unwrap();

        let mut verifier = IntegrityVerifier::from_pubkey_bytes(&pubkey).unwrap();
        let parsed = verifier.verify_entry(&dir, "game.js").unwrap();

        // Entry passes, but full verification catches the tampered dependency
        let result = verifier.verify_all_files(&dir, &parsed);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, ErrorCode::CodeIntegrityFailed);

        let _ = fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // Utility function tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_sha256_bytes_known_vector() {
        // SHA256("hello") = 2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824
        let hash = sha256_bytes(b"hello");
        assert_eq!(
            hash,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn test_sha256_bytes_empty() {
        // SHA256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        let hash = sha256_bytes(b"");
        assert_eq!(
            hash,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn test_from_hex_pubkey_valid() {
        let signing_key = SigningKey::from_bytes(&[42u8; 32]);
        let pubkey = signing_key.verifying_key();
        let hex_str = hex::encode(pubkey.to_bytes());

        assert!(IntegrityVerifier::from_hex_pubkey(&hex_str).is_ok());
    }

    #[test]
    fn test_from_hex_pubkey_too_short() {
        let result = IntegrityVerifier::from_hex_pubkey("aabbccdd");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, ErrorCode::CodeSignatureInvalid);
    }

    #[test]
    fn test_from_hex_pubkey_invalid_hex() {
        let result = IntegrityVerifier::from_hex_pubkey("zzzzzz");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, ErrorCode::CodeSignatureInvalid);
    }

    #[test]
    fn test_invalid_manifest_version() {
        let dir = make_test_dir("invalid_version");
        let signing_key = SigningKey::from_bytes(&[42u8; 32]);
        let pubkey = signing_key.verifying_key().to_bytes();

        fs::write(dir.join("game.js"), "code").unwrap();

        let mut files = HashMap::new();
        files.insert("game.js".to_string(), sha256_bytes(b"code"));

        // Version 99 is unsupported
        let manifest = Manifest {
            version: 99,
            entry: "game.js".to_string(),
            timestamp: 1709078400,
            files,
        };
        let manifest_json = serde_json::to_vec_pretty(&manifest).unwrap();
        let sig = sign_manifest(&signing_key, &manifest_json);
        fs::write(dir.join("manifest.json"), &manifest_json).unwrap();
        fs::write(dir.join("manifest.sig"), &sig).unwrap();

        let mut verifier = IntegrityVerifier::from_pubkey_bytes(&pubkey).unwrap();
        let result = verifier.verify_entry(&dir, "game.js");

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, ErrorCode::CodeSignatureInvalid);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_invalid_signature_length() {
        let dir = make_test_dir("bad_sig_len");
        let signing_key = SigningKey::from_bytes(&[42u8; 32]);
        let pubkey = signing_key.verifying_key().to_bytes();

        fs::write(dir.join("game.js"), "code").unwrap();

        let mut files = HashMap::new();
        files.insert("game.js".to_string(), sha256_bytes(b"code"));

        let manifest = Manifest {
            version: 1,
            entry: "game.js".to_string(),
            timestamp: 1709078400,
            files,
        };
        let manifest_json = serde_json::to_vec_pretty(&manifest).unwrap();
        fs::write(dir.join("manifest.json"), &manifest_json).unwrap();
        // Write a truncated signature
        fs::write(dir.join("manifest.sig"), &[0u8; 32]).unwrap();

        let mut verifier = IntegrityVerifier::from_pubkey_bytes(&pubkey).unwrap();
        let result = verifier.verify_entry(&dir, "game.js");

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, ErrorCode::CodeSignatureInvalid);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_manifest_rejects_parent_dir_path() {
        let dir = make_test_dir("parent_dir_path");
        let signing_key = SigningKey::from_bytes(&[42u8; 32]);
        let pubkey = signing_key.verifying_key().to_bytes();

        fs::write(dir.join("game.js"), "code").unwrap();
        fs::write(dir.join("secret.js"), "secret").unwrap();

        let mut files = HashMap::new();
        files.insert("../secret.js".to_string(), sha256_bytes(b"secret"));

        let manifest = Manifest {
            version: 1,
            entry: "../secret.js".to_string(),
            timestamp: 1709078400,
            files,
        };
        let manifest_json = serde_json::to_vec_pretty(&manifest).unwrap();
        let sig = sign_manifest(&signing_key, &manifest_json);
        fs::write(dir.join("manifest.json"), &manifest_json).unwrap();
        fs::write(dir.join("manifest.sig"), &sig).unwrap();

        let mut verifier = IntegrityVerifier::from_pubkey_bytes(&pubkey).unwrap();
        let result = verifier.verify_entry(&dir, "../secret.js");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, ErrorCode::CodeIntegrityFailed);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_manifest_rejects_absolute_path() {
        let dir = make_test_dir("absolute_path");
        let signing_key = SigningKey::from_bytes(&[42u8; 32]);
        let pubkey = signing_key.verifying_key().to_bytes();

        let mut files = HashMap::new();
        files.insert("/etc/passwd".to_string(), sha256_bytes(b"x"));

        let manifest = Manifest {
            version: 1,
            entry: "/etc/passwd".to_string(),
            timestamp: 1709078400,
            files,
        };
        let manifest_json = serde_json::to_vec_pretty(&manifest).unwrap();
        let sig = sign_manifest(&signing_key, &manifest_json);
        fs::write(dir.join("manifest.json"), &manifest_json).unwrap();
        fs::write(dir.join("manifest.sig"), &sig).unwrap();

        let mut verifier = IntegrityVerifier::from_pubkey_bytes(&pubkey).unwrap();
        let result = verifier.verify_entry(&dir, "/etc/passwd");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, ErrorCode::CodeIntegrityFailed);

        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn first_launch_promotes_and_second_launch_uses_receipt_without_hashing() {
        let dir = make_test_dir("receipt_lifecycle");
        let receipt = dir.with_extension("integrity-receipt.json");
        let lock = receipt.with_extension("lock");
        let _ = fs::remove_file(&receipt);
        let _ = fs::remove_file(&lock);
        let (_sk, pubkey) = setup_signed_package(&dir, "game.js", "trusted code");
        let verifier = IntegrityVerifier::from_pubkey_bytes(&pubkey).unwrap();

        let first = verifier
            .verify_and_promote_for_launch(&dir, &receipt, "game.js")
            .unwrap();
        assert_eq!(first.mode, VerificationMode::Full);
        assert_eq!(first.generation, 1);
        assert_eq!(first.files_hashed, 1);

        let second = verifier
            .verify_and_promote_for_launch(&dir, &receipt, "game.js")
            .unwrap();
        assert_eq!(second.mode, VerificationMode::Receipt);
        assert_eq!(second.generation, 1);
        assert_eq!(second.files_hashed, 0);

        MANIFEST_PARSE_COUNT.with(|count| count.set(0));
        let direct_hit = verifier
            .verify_launch_receipt(&dir, &receipt, "game.js")
            .unwrap()
            .unwrap();
        assert_eq!(direct_hit.mode, VerificationMode::Receipt);
        MANIFEST_PARSE_COUNT.with(|count| assert_eq!(count.get(), 0));

        make_tree_writable_for_test(&dir);
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_file(&receipt);
        let _ = fs::remove_file(&lock);
    }

    #[cfg(unix)]
    #[test]
    fn promotion_rejects_unlisted_regular_file() {
        let dir = make_test_dir("receipt_extra_file");
        let receipt = dir.with_extension("integrity-receipt.json");
        let _ = fs::remove_file(&receipt);
        let (_sk, pubkey) = setup_signed_package(&dir, "game.js", "trusted code");
        fs::write(dir.join("unlisted.js"), "not signed").unwrap();
        let verifier = IntegrityVerifier::from_pubkey_bytes(&pubkey).unwrap();

        let error = verifier
            .verify_and_promote_for_launch(&dir, &receipt, "game.js")
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::CodeIntegrityFailed);
        assert!(error.to_string().contains("unlisted"));
        assert!(!receipt.exists());

        make_tree_writable_for_test(&dir);
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_file(&receipt);
    }

    #[cfg(unix)]
    #[test]
    fn promotion_rejects_symlink_and_writes_no_receipt() {
        use std::os::unix::fs::symlink;

        let dir = make_test_dir("receipt_package_symlink");
        let outside = dir.with_extension("outside-file");
        let receipt = dir.with_extension("integrity-receipt.json");
        let lock = receipt.with_extension("lock");
        let _ = fs::remove_file(&outside);
        let _ = fs::remove_file(&receipt);
        let _ = fs::remove_file(&lock);
        let (_sk, pubkey) = setup_signed_package(&dir, "game.js", "trusted code");
        fs::remove_file(dir.join("game.js")).unwrap();
        fs::write(&outside, "trusted code").unwrap();
        symlink(&outside, dir.join("game.js")).unwrap();
        let verifier = IntegrityVerifier::from_pubkey_bytes(&pubkey).unwrap();

        let error = verifier
            .verify_and_promote_for_launch(&dir, &receipt, "game.js")
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::CodeIntegrityFailed);
        assert!(error.to_string().contains("symlink"));
        assert!(!receipt.exists());

        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_file(&outside);
        let _ = fs::remove_file(&receipt);
        let _ = fs::remove_file(&lock);
    }

    #[cfg(unix)]
    #[test]
    fn promotion_rejects_missing_manifest_file() {
        let dir = make_test_dir("receipt_missing_file");
        let receipt = dir.with_extension("integrity-receipt.json");
        let lock = receipt.with_extension("lock");
        let _ = fs::remove_file(&receipt);
        let _ = fs::remove_file(&lock);
        let (_sk, pubkey) = setup_signed_package(&dir, "game.js", "trusted code");
        fs::remove_file(dir.join("game.js")).unwrap();
        let verifier = IntegrityVerifier::from_pubkey_bytes(&pubkey).unwrap();

        let error = verifier
            .verify_and_promote_for_launch(&dir, &receipt, "game.js")
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::CodeIntegrityFailed);
        assert!(error.to_string().contains("missing"));
        assert!(!receipt.exists());

        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_file(&receipt);
        let _ = fs::remove_file(&lock);
    }

    #[cfg(unix)]
    #[test]
    fn promotion_rejects_non_regular_file() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let dir = make_test_dir("receipt_fifo");
        let receipt = dir.with_extension("integrity-receipt.json");
        let lock = receipt.with_extension("lock");
        let _ = fs::remove_file(&receipt);
        let _ = fs::remove_file(&lock);
        let (_sk, pubkey) = setup_signed_package(&dir, "game.js", "trusted code");
        let entry = dir.join("game.js");
        fs::remove_file(&entry).unwrap();
        let entry_c = CString::new(entry.as_os_str().as_bytes()).unwrap();
        // SAFETY: `entry_c` is a live NUL-terminated path and mode is valid.
        assert_eq!(unsafe { libc::mkfifo(entry_c.as_ptr(), 0o600) }, 0);
        let verifier = IntegrityVerifier::from_pubkey_bytes(&pubkey).unwrap();

        let error = verifier
            .verify_and_promote_for_launch(&dir, &receipt, "game.js")
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::CodeIntegrityFailed);
        assert!(error.to_string().contains("non-regular"));
        assert!(!receipt.exists());

        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_file(&receipt);
        let _ = fs::remove_file(&lock);
    }

    #[test]
    fn promotion_manifest_read_is_bounded_before_signature_or_json_allocation() {
        let dir = make_test_dir("receipt_manifest_bound");
        let receipt = dir.with_extension("integrity-receipt.json");
        let _ = fs::remove_file(&receipt);
        fs::write(
            dir.join("manifest.json"),
            vec![b' '; MAX_MANIFEST_BYTES as usize + 1],
        )
        .unwrap();
        fs::write(dir.join("manifest.sig"), [0u8; 64]).unwrap();
        let signing_key = SigningKey::from_bytes(&[42u8; 32]);
        let verifier =
            IntegrityVerifier::from_pubkey_bytes(&signing_key.verifying_key().to_bytes()).unwrap();

        let error = verifier
            .verify_and_promote_for_launch(&dir, &receipt, "game.js")
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::CodeSignatureInvalid);
        assert!(
            error
                .to_string()
                .contains("manifest.json exceeds 4194304 bytes")
        );

        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_file(&receipt);
    }

    #[test]
    fn full_promotion_validates_manifest_contract_after_receipt_miss() {
        let dir = make_test_dir("receipt_manifest_contract");
        let receipt = dir.with_extension("integrity-receipt.json");
        let lock = receipt.with_extension("lock");
        let _ = fs::remove_file(&receipt);
        let _ = fs::remove_file(&lock);
        let (signing_key, pubkey) = setup_signed_package(&dir, "game.js", "trusted code");
        let manifest_path = dir.join("manifest.json");
        let mut manifest: Manifest =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest
            .files
            .insert("game.js".to_string(), "ABCDEF".repeat(10) + "ABCD");
        let bytes = serde_json::to_vec_pretty(&manifest).unwrap();
        fs::write(&manifest_path, &bytes).unwrap();
        fs::write(
            dir.join("manifest.sig"),
            sign_manifest(&signing_key, &bytes),
        )
        .unwrap();
        let verifier = IntegrityVerifier::from_pubkey_bytes(&pubkey).unwrap();

        assert!(
            verifier
                .verify_launch_receipt(&dir, &receipt, "game.js")
                .unwrap()
                .is_none()
        );
        let error = verifier
            .verify_and_promote_for_launch(&dir, &receipt, "game.js")
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::CodeIntegrityFailed);
        assert!(error.to_string().contains("lowercase hex"));

        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_file(&receipt);
        let _ = fs::remove_file(&lock);
    }

    #[test]
    fn manifest_file_count_is_bounded() {
        let mut files = HashMap::with_capacity(MAX_MANIFEST_FILES + 1);
        for index in 0..=MAX_MANIFEST_FILES {
            files.insert(format!("file-{index}.js"), "0".repeat(64));
        }
        let manifest = Manifest {
            version: 1,
            entry: "file-0.js".to_string(),
            timestamp: 1,
            files,
        };

        let error = validate_manifest_contract(&manifest).unwrap_err();
        assert_eq!(error.code, ErrorCode::CodeIntegrityFailed);
        assert!(error.to_string().contains("too many files"));
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_promotions_hash_once_and_publish_one_generation() {
        use std::sync::{Arc, Barrier};

        let dir = make_test_dir("receipt_concurrent");
        let receipt = dir.with_extension("integrity-receipt.json");
        let lock = receipt.with_extension("lock");
        let _ = fs::remove_file(&receipt);
        let _ = fs::remove_file(&lock);
        let content = "x".repeat(8 * 1024 * 1024);
        let (_sk, pubkey) = setup_signed_package(&dir, "game.js", &content);
        let verifier = IntegrityVerifier::from_pubkey_bytes(&pubkey).unwrap();
        let barrier = Arc::new(Barrier::new(4));

        let mut threads = Vec::new();
        for _ in 0..4 {
            let barrier = Arc::clone(&barrier);
            let verifier = verifier.clone();
            let dir = dir.clone();
            let receipt = receipt.clone();
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                verifier.verify_and_promote_for_launch(&dir, &receipt, "game.js")
            }));
        }
        let results = threads
            .into_iter()
            .map(|thread| thread.join().unwrap().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(
            results
                .iter()
                .filter(|result| result.mode == VerificationMode::Full)
                .count(),
            1
        );
        assert!(results.iter().all(|result| result.generation == 1));

        make_tree_writable_for_test(&dir);
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_file(&receipt);
        let _ = fs::remove_file(&lock);
    }

    #[cfg(unix)]
    #[test]
    fn missing_or_corrupt_receipt_reverifies_and_advances_generation() {
        let dir = make_test_dir("receipt_generation_anchor");
        let receipt = dir.with_extension("integrity-receipt.json");
        let lock = receipt.with_extension("lock");
        let _ = fs::remove_file(&receipt);
        let _ = fs::remove_file(&lock);
        let (_sk, pubkey) = setup_signed_package(&dir, "game.js", "trusted code");
        let verifier = IntegrityVerifier::from_pubkey_bytes(&pubkey).unwrap();

        let first = verifier
            .verify_and_promote_for_launch(&dir, &receipt, "game.js")
            .unwrap();
        assert_eq!(first.generation, 1);

        fs::remove_file(&receipt).unwrap();
        let after_missing = verifier
            .verify_and_promote_for_launch(&dir, &receipt, "game.js")
            .unwrap();
        assert_eq!(after_missing.mode, VerificationMode::Full);
        assert_eq!(after_missing.generation, 2);

        fs::write(&receipt, b"{truncated").unwrap();
        let after_corrupt = verifier
            .verify_and_promote_for_launch(&dir, &receipt, "game.js")
            .unwrap();
        assert_eq!(after_corrupt.mode, VerificationMode::Full);
        assert_eq!(after_corrupt.generation, 3);

        make_tree_writable_for_test(&dir);
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_file(&receipt);
        let _ = fs::remove_file(&lock);
    }

    #[cfg(unix)]
    #[test]
    fn receipt_symlink_is_never_a_fast_path_hit() {
        use std::os::unix::fs::symlink;

        let dir = make_test_dir("receipt_symlink");
        let receipt = dir.with_extension("integrity-receipt.json");
        let receipt_target = dir.with_extension("integrity-receipt-target.json");
        let lock = receipt.with_extension("lock");
        let _ = fs::remove_file(&receipt);
        let _ = fs::remove_file(&receipt_target);
        let _ = fs::remove_file(&lock);
        let (_sk, pubkey) = setup_signed_package(&dir, "game.js", "trusted code");
        let verifier = IntegrityVerifier::from_pubkey_bytes(&pubkey).unwrap();

        verifier
            .verify_and_promote_for_launch(&dir, &receipt, "game.js")
            .unwrap();
        fs::rename(&receipt, &receipt_target).unwrap();
        symlink(&receipt_target, &receipt).unwrap();

        assert!(
            verifier
                .verify_launch_receipt(&dir, &receipt, "game.js")
                .unwrap()
                .is_none()
        );

        make_tree_writable_for_test(&dir);
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_file(&receipt);
        let _ = fs::remove_file(&receipt_target);
        let _ = fs::remove_file(&lock);
    }

    #[cfg(unix)]
    #[test]
    fn oversized_receipt_is_a_miss_and_reverification_advances_generation() {
        let dir = make_test_dir("receipt_oversized");
        let receipt = dir.with_extension("integrity-receipt.json");
        let lock = receipt.with_extension("lock");
        let _ = fs::remove_file(&receipt);
        let _ = fs::remove_file(&lock);
        let (_sk, pubkey) = setup_signed_package(&dir, "game.js", "trusted code");
        let verifier = IntegrityVerifier::from_pubkey_bytes(&pubkey).unwrap();
        verifier
            .verify_and_promote_for_launch(&dir, &receipt, "game.js")
            .unwrap();

        fs::write(&receipt, vec![b'x'; MAX_RECEIPT_BYTES as usize + 1]).unwrap();
        assert!(
            verifier
                .verify_launch_receipt(&dir, &receipt, "game.js")
                .unwrap()
                .is_none()
        );
        let reverified = verifier
            .verify_and_promote_for_launch(&dir, &receipt, "game.js")
            .unwrap();
        assert_eq!(reverified.mode, VerificationMode::Full);
        assert_eq!(reverified.generation, 2);

        make_tree_writable_for_test(&dir);
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_file(&receipt);
        let _ = fs::remove_file(&lock);
    }

    #[cfg(unix)]
    #[test]
    fn partial_seal_without_receipt_is_idempotently_recovered() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let dir = make_test_dir("receipt_partial_seal");
        let receipt = dir.with_extension("integrity-receipt.json");
        let lock = receipt.with_extension("lock");
        let _ = fs::remove_file(&receipt);
        let _ = fs::remove_file(&lock);
        let (_sk, pubkey) = setup_signed_package(&dir, "game.js", "trusted code");
        let entry = dir.join("game.js");
        let mode = fs::metadata(&entry).unwrap().mode();
        fs::set_permissions(&entry, fs::Permissions::from_mode(mode & !0o222)).unwrap();
        let verifier = IntegrityVerifier::from_pubkey_bytes(&pubkey).unwrap();

        let promoted = verifier
            .verify_and_promote_for_launch(&dir, &receipt, "game.js")
            .unwrap();
        assert_eq!(promoted.mode, VerificationMode::Full);
        assert_eq!(promoted.generation, 1);
        assert!(receipt.is_file());

        make_tree_writable_for_test(&dir);
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_file(&receipt);
        let _ = fs::remove_file(&lock);
    }

    #[cfg(unix)]
    #[test]
    fn receipt_commit_failure_never_exposes_partial_receipt() {
        let parent = make_test_dir("receipt_commit_failure");
        let receipt_path = parent.join("receipt.json");
        fs::create_dir(&receipt_path).unwrap();
        let receipt = InstallReceipt {
            schema: INSTALL_RECEIPT_SCHEMA,
            seal_policy: SEAL_POLICY_VERSION,
            generation: 1,
            manifest_sha256: "a".repeat(64),
            pubkey_sha256: "b".repeat(64),
            entry: "game.js".to_string(),
            root: CodeRootIdentity {
                dev: 1,
                ino: 2,
                ctime_secs: 3,
                ctime_nanos: 4,
                mode: 0o40555,
            },
        };

        assert!(write_receipt_atomic(&receipt_path, &receipt).is_err());
        assert!(receipt_path.is_dir());
        assert!(fs::read_dir(&parent).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".receipt.json.tmp.")
        }));

        let _ = fs::remove_dir_all(&parent);
    }

    #[cfg(unix)]
    #[test]
    fn same_size_same_mtime_root_replacement_invalidates_receipt() {
        let dir = make_test_dir("receipt_root_replace");
        let old_dir = dir.with_extension("old-code");
        let receipt = dir.with_extension("integrity-receipt.json");
        let lock = receipt.with_extension("lock");
        make_tree_writable_for_test(&old_dir);
        let _ = fs::remove_dir_all(&old_dir);
        let _ = fs::remove_file(&receipt);
        let _ = fs::remove_file(&lock);
        let (_sk, pubkey) = setup_signed_package(&dir, "game.js", "build-one");
        let verifier = IntegrityVerifier::from_pubkey_bytes(&pubkey).unwrap();
        let first = verifier
            .verify_and_promote_for_launch(&dir, &receipt, "game.js")
            .unwrap();
        assert_eq!(first.generation, 1);
        let original_times = fs::metadata(dir.join("game.js")).unwrap();

        // Writable before the rename, and this is a portability fix rather than
        // tidiness: sealing strips the write bits from the directory ITSELF
        // (`metadata.mode() & !0o222`), and macOS refuses to rename a directory it
        // cannot write, while Linux only needs write on the parent. These three
        // tests passed everywhere they had ever run until the Apple lane started
        // running this crate's tests, where all three failed with EACCES on the
        // line below.
        make_tree_writable_for_test(&dir);
        fs::rename(&dir, &old_dir).unwrap();
        fs::create_dir(&dir).unwrap();
        setup_signed_package(&dir, "game.js", "build-two");
        let game = OpenOptions::new()
            .write(true)
            .open(dir.join("game.js"))
            .unwrap();
        game.set_times(
            fs::FileTimes::new()
                .set_accessed(original_times.accessed().unwrap())
                .set_modified(original_times.modified().unwrap()),
        )
        .unwrap();

        assert!(
            verifier
                .verify_launch_receipt(&dir, &receipt, "game.js")
                .unwrap()
                .is_none()
        );
        let replacement = verifier
            .verify_and_promote_for_launch(&dir, &receipt, "game.js")
            .unwrap();
        assert_eq!(replacement.mode, VerificationMode::Full);
        assert_eq!(replacement.generation, 2);

        make_tree_writable_for_test(&dir);
        make_tree_writable_for_test(&old_dir);
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&old_dir);
        let _ = fs::remove_file(&receipt);
        let _ = fs::remove_file(&lock);
    }

    #[cfg(unix)]
    #[test]
    fn signed_rollback_reverifies_and_advances_generation() {
        let dir = make_test_dir("receipt_signed_rollback");
        let old_dir = dir.with_extension("newer-code");
        let receipt = dir.with_extension("integrity-receipt.json");
        let lock = receipt.with_extension("lock");
        make_tree_writable_for_test(&old_dir);
        let _ = fs::remove_dir_all(&old_dir);
        let _ = fs::remove_file(&receipt);
        let _ = fs::remove_file(&lock);
        let (_sk, pubkey) = setup_signed_package(&dir, "game.js", "newer");
        let verifier = IntegrityVerifier::from_pubkey_bytes(&pubkey).unwrap();
        verifier
            .verify_and_promote_for_launch(&dir, &receipt, "game.js")
            .unwrap();

        // Writable before the rename, and this is a portability fix rather than
        // tidiness: sealing strips the write bits from the directory ITSELF
        // (`metadata.mode() & !0o222`), and macOS refuses to rename a directory it
        // cannot write, while Linux only needs write on the parent. These three
        // tests passed everywhere they had ever run until the Apple lane started
        // running this crate's tests, where all three failed with EACCES on the
        // line below.
        make_tree_writable_for_test(&dir);
        fs::rename(&dir, &old_dir).unwrap();
        fs::create_dir(&dir).unwrap();
        let (signing_key, _) = setup_signed_package(&dir, "game.js", "older");
        resign_manifest_timestamp(&dir, &signing_key, 1);

        let rollback = verifier
            .verify_and_promote_for_launch(&dir, &receipt, "game.js")
            .unwrap();
        assert_eq!(rollback.mode, VerificationMode::Full);
        assert_eq!(rollback.generation, 2);

        make_tree_writable_for_test(&dir);
        make_tree_writable_for_test(&old_dir);
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&old_dir);
        let _ = fs::remove_file(&receipt);
        let _ = fs::remove_file(&lock);
    }

    #[cfg(unix)]
    #[test]
    fn tampered_unsealed_replacement_fails_before_receipt_update() {
        let dir = make_test_dir("receipt_tampered_replacement");
        let old_dir = dir.with_extension("verified-code");
        let receipt = dir.with_extension("integrity-receipt.json");
        let lock = receipt.with_extension("lock");
        make_tree_writable_for_test(&old_dir);
        let _ = fs::remove_dir_all(&old_dir);
        let _ = fs::remove_file(&receipt);
        let _ = fs::remove_file(&lock);
        let (_sk, pubkey) = setup_signed_package(&dir, "game.js", "trusted-v1");
        let verifier = IntegrityVerifier::from_pubkey_bytes(&pubkey).unwrap();
        verifier
            .verify_and_promote_for_launch(&dir, &receipt, "game.js")
            .unwrap();
        let old_receipt = fs::read(&receipt).unwrap();

        // Writable before the rename, and this is a portability fix rather than
        // tidiness: sealing strips the write bits from the directory ITSELF
        // (`metadata.mode() & !0o222`), and macOS refuses to rename a directory it
        // cannot write, while Linux only needs write on the parent. These three
        // tests passed everywhere they had ever run until the Apple lane started
        // running this crate's tests, where all three failed with EACCES on the
        // line below.
        make_tree_writable_for_test(&dir);
        fs::rename(&dir, &old_dir).unwrap();
        fs::create_dir(&dir).unwrap();
        setup_signed_package(&dir, "game.js", "trusted-v2");
        fs::write(dir.join("game.js"), "tampered!").unwrap();

        let error = verifier
            .verify_and_promote_for_launch(&dir, &receipt, "game.js")
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::CodeIntegrityFailed);
        assert!(error.to_string().contains("hash mismatch"));
        assert_eq!(fs::read(&receipt).unwrap(), old_receipt);
        assert!(
            verifier
                .verify_launch_receipt(&dir, &receipt, "game.js")
                .unwrap()
                .is_none()
        );

        make_tree_writable_for_test(&dir);
        make_tree_writable_for_test(&old_dir);
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&old_dir);
        let _ = fs::remove_file(&receipt);
        let _ = fs::remove_file(&lock);
    }

    #[cfg(unix)]
    #[test]
    fn receipt_public_key_entry_and_root_identity_must_match() {
        let dir = make_test_dir("receipt_field_binding");
        let receipt = dir.with_extension("integrity-receipt.json");
        let lock = receipt.with_extension("lock");
        let _ = fs::remove_file(&receipt);
        let _ = fs::remove_file(&lock);
        let (_sk, pubkey) = setup_signed_package(&dir, "game.js", "trusted code");
        let verifier = IntegrityVerifier::from_pubkey_bytes(&pubkey).unwrap();
        verifier
            .verify_and_promote_for_launch(&dir, &receipt, "game.js")
            .unwrap();
        let original = fs::read(&receipt).unwrap();
        let parsed: InstallReceipt = serde_json::from_slice(&original).unwrap();

        let mut changed = parsed.clone();
        changed.pubkey_sha256 = "0".repeat(64);
        fs::write(&receipt, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(
            verifier
                .verify_launch_receipt(&dir, &receipt, "game.js")
                .unwrap()
                .is_none()
        );

        changed = parsed.clone();
        changed.entry = "other.js".to_string();
        fs::write(&receipt, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(
            verifier
                .verify_launch_receipt(&dir, &receipt, "game.js")
                .unwrap()
                .is_none()
        );

        changed = parsed;
        changed.root.ino = changed.root.ino.wrapping_add(1);
        fs::write(&receipt, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(
            verifier
                .verify_launch_receipt(&dir, &receipt, "game.js")
                .unwrap()
                .is_none()
        );

        make_tree_writable_for_test(&dir);
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_file(&receipt);
        let _ = fs::remove_file(&lock);
    }

    #[cfg(unix)]
    #[test]
    fn sealed_tree_rejects_in_place_write_and_entry_replacement() {
        use std::os::unix::fs::MetadataExt;

        let dir = make_test_dir("receipt_sealed_tree");
        let receipt = dir.with_extension("integrity-receipt.json");
        let lock = receipt.with_extension("lock");
        let _ = fs::remove_file(&receipt);
        let _ = fs::remove_file(&lock);
        let (_sk, pubkey) = setup_signed_package(&dir, "game.js", "trusted code");
        let verifier = IntegrityVerifier::from_pubkey_bytes(&pubkey).unwrap();
        verifier
            .verify_and_promote_for_launch(&dir, &receipt, "game.js")
            .unwrap();

        assert_eq!(fs::metadata(&dir).unwrap().mode() & 0o222, 0);
        assert_eq!(fs::metadata(dir.join("game.js")).unwrap().mode() & 0o222, 0);
        assert!(fs::write(dir.join("game.js"), "tampered").is_err());
        assert!(fs::rename(dir.join("game.js"), dir.join("other.js")).is_err());

        make_tree_writable_for_test(&dir);
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_file(&receipt);
        let _ = fs::remove_file(&lock);
    }

    fn resign_manifest_timestamp(dir: &Path, signing_key: &SigningKey, timestamp: u64) {
        let manifest_path = dir.join("manifest.json");
        let mut manifest: Manifest =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest.timestamp = timestamp;
        let bytes = serde_json::to_vec_pretty(&manifest).unwrap();
        fs::write(&manifest_path, &bytes).unwrap();
        fs::write(dir.join("manifest.sig"), sign_manifest(signing_key, &bytes)).unwrap();
    }

    #[cfg(unix)]
    fn make_tree_writable_for_test(path: &Path) {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let Ok(metadata) = fs::symlink_metadata(path) else {
            return;
        };
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(metadata.mode() | 0o700));
        if metadata.is_dir()
            && let Ok(entries) = fs::read_dir(path)
        {
            for entry in entries.flatten() {
                make_tree_writable_for_test(&entry.path());
            }
        }
    }
}
