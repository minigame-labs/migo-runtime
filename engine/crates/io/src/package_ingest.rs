//! Zip-to-package ingest: converts a zip archive into a `.mpkg` package.
//!
//! This is the "ingest" step: zip is treated as an input/download format
//! and converted to the runtime-native package format for mounted access.

use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use shared::device::gpu_caps::GpuCapsSnapshot;
use shared::protocol::io_cmd::MAX_READ_LENGTH;
use shared::vfs::package::{PackageError, PackageIdentity, PackageWriter};

#[cfg(feature = "rust-image-decode")]
use crate::ingest_transcode::{is_transcodable_image, transcode_image};
use crate::{
    pools::PoolError,
    scheduler::IoScheduler,
    task::{IoRequest, PriorityClass},
    zip_extract::ExtractBudget,
};

// Without the image decoder there is nothing to transcode with, so the ingest
// loop compiles to its original streaming-only form: `is_transcodable_image` is
// always false and the sidecar branch is dead. Keeping the call sites
// unconditional (rather than sprinkling `#[cfg]` through the loop body) keeps
// the two builds structurally identical.
#[cfg(not(feature = "rust-image-decode"))]
fn is_transcodable_image(_name: &str) -> bool {
    false
}
#[cfg(not(feature = "rust-image-decode"))]
struct TranscodedSidecar {
    name: String,
    bytes: Vec<u8>,
}
#[cfg(not(feature = "rust-image-decode"))]
fn transcode_image(
    _name: &str,
    _bytes: &[u8],
    _caps: shared::device::gpu_caps::GpuCapsSnapshot,
) -> Option<TranscodedSidecar> {
    None
}

fn package_ingest_request_for(zip_path: &Path) -> IoRequest {
    let compressed_bytes = std::fs::metadata(zip_path)
        .map(|meta| meta.len() as usize)
        .unwrap_or(0);
    IoRequest::PackageIngest {
        priority: PriorityClass::Background,
        compressed_bytes,
    }
}

impl From<PoolError> for PackageError {
    fn from(err: PoolError) -> Self {
        match err {
            PoolError::Closed => PackageError::Io(std::io::Error::other("IO worker pool closed")),
            PoolError::ByteLimitExceeded {
                requested,
                available,
            } => PackageError::Io(std::io::Error::other(format!(
                "IO pending-byte budget exhausted: requested {requested} bytes, {available} available"
            ))),
        }
    }
}

/// `Read` wrapper that counts bytes so the caller can enforce an
/// archive-wide budget across entries. Streaming entry-level caps are
/// still enforced inside `PackageWriter::add_entry_streaming`.
struct CountingReader<'a, R: Read> {
    inner: &'a mut R,
    counted: &'a mut u64,
}

impl<'a, R: Read> Read for CountingReader<'a, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        *self.counted = self.counted.saturating_add(n as u64);
        Ok(n)
    }
}

/// Convert a zip archive into a `.mpkg` package file (zstd-chunked).
///
/// Each entry is split into 64 KiB chunks, each independently
/// zstd-compressed for random access. Uses [`ExtractBudget::default`]
/// for zip-bomb defense.
/// `gpu_caps` selects the compressed sidecar format, and the parameter is here
/// rather than defaulted because the default is not free: an ingest that does
/// not know what the device decodes writes the format every device decodes, and
/// a caller that has the answer and does not pass it silently gives up the
/// better one. See `ingest_transcode::transcode_image` for the rule.
pub fn ingest_zip_to_package(
    zip_path: &Path,
    pkg_path: &Path,
    package_name: &str,
    package_version: &str,
    gpu_caps: GpuCapsSnapshot,
) -> Result<PackageIdentity, PackageError> {
    ingest_zip_to_package_with_budget(
        zip_path,
        pkg_path,
        package_name,
        package_version,
        ExtractBudget::default(),
        gpu_caps,
    )
}

/// Same as [`ingest_zip_to_package`] but with an explicit resource
/// budget. Every entry is validated against the per-entry cap and the
/// running total is validated against the archive-wide cap.
pub fn ingest_zip_to_package_with_budget(
    zip_path: &Path,
    pkg_path: &Path,
    package_name: &str,
    package_version: &str,
    budget: ExtractBudget,
    gpu_caps: GpuCapsSnapshot,
) -> Result<PackageIdentity, PackageError> {
    let zip_file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(BufReader::new(zip_file))
        .map_err(|e| PackageError::BadIndex(format!("invalid zip: {e}")))?;

    let entry_count = archive.len();
    if entry_count > budget.max_entries {
        return Err(PackageError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "zip has {} entries, exceeds budget {}",
                entry_count, budget.max_entries
            ),
        )));
    }

    // Cheap header-time pre-scan: reject advertised totals that
    // already exceed the archive-wide budget before spending any
    // inflate CPU.
    let mut advertised_total: u64 = 0;
    for i in 0..entry_count {
        let entry = archive.by_index_raw(i).map_err(|e| {
            PackageError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                e.to_string(),
            ))
        })?;
        let sz = entry.size();
        if sz > budget.max_entry_uncompressed {
            return Err(PackageError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "zip entry '{}' advertises {} bytes, exceeds per-entry limit {}",
                    entry.name(),
                    sz,
                    budget.max_entry_uncompressed
                ),
            )));
        }
        advertised_total = advertised_total.saturating_add(sz);
        if advertised_total > budget.max_total_uncompressed {
            return Err(PackageError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "zip advertised total {} bytes exceeds budget {}",
                    advertised_total, budget.max_total_uncompressed
                ),
            )));
        }
    }

    let tmp_pkg_path = pkg_path.with_extension("mpkg.tmp");
    let _ = std::fs::remove_file(&tmp_pkg_path);
    let pkg_file = std::fs::File::create(&tmp_pkg_path)?;
    let mut writer = PackageWriter::new(std::io::BufWriter::new(pkg_file))?;

    let ingest_result: Result<PackageIdentity, PackageError> = (|| {
        let mut ingested_total: u64 = 0;
        for i in 0..entry_count {
            let mut entry = archive.by_index(i).map_err(|e| {
                PackageError::Io(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    e.to_string(),
                ))
            })?;

            if entry.is_dir() {
                continue;
            }

            let name = entry.name().to_string();
            if name.starts_with("__MACOSX/") || name.ends_with(".DS_Store") {
                continue;
            }

            let entry_cap = std::cmp::min(budget.max_entry_uncompressed, MAX_READ_LENGTH);
            let remaining_total = budget.max_total_uncompressed.saturating_sub(ingested_total);
            let cap = std::cmp::min(entry_cap, remaining_total);
            if cap == 0 {
                return Err(PackageError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "archive total budget exhausted",
                )));
            }

            // Per-entry ratio check (inflate bombs).
            if budget.max_compression_ratio > 0 {
                let compressed = entry.compressed_size();
                let uncompressed_hdr = entry.size();
                if compressed > 0 {
                    let ratio = uncompressed_hdr / compressed;
                    if ratio > budget.max_compression_ratio {
                        return Err(PackageError::Io(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!(
                                "zip entry '{}' compression ratio {} exceeds budget {}",
                                name, ratio, budget.max_compression_ratio
                            ),
                        )));
                    }
                }
            }

            // Transcodable images are read whole so they can be decoded and
            // ETC2-encoded; everything else streams, so the uncompressed bytes
            // of a large asset are never materialised as one `Vec<u8>`.
            let before = ingested_total;
            let image_bytes: Option<Vec<u8>> = if is_transcodable_image(&name) {
                // Read one byte beyond the remaining budget.  `take(cap)` turns
                // an over-budget ZIP entry into an apparently clean EOF, which
                // used to publish a truncated original image after a sidecar had
                // consumed part of the archive budget.
                let mut buf = Vec::new();
                let mut limited = (&mut entry).take(cap.saturating_add(1));
                let read = limited.read_to_end(&mut buf)? as u64;
                if read > cap {
                    return Err(PackageError::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "image entry '{}' exceeds remaining budget {} bytes",
                            name, cap
                        ),
                    )));
                }
                ingested_total = ingested_total.saturating_add(read);
                writer.add_entry(&name, &buf)?;
                Some(buf)
            } else {
                let mut counting = CountingReader {
                    inner: &mut entry,
                    counted: &mut ingested_total,
                };
                writer.add_entry_streaming(&name, &mut counting, cap)?;
                None
            };
            if ingested_total > budget.max_total_uncompressed {
                return Err(PackageError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "ingested total {} bytes (entry '{}' added {}) exceeds budget {}",
                        ingested_total,
                        name,
                        ingested_total - before,
                        budget.max_total_uncompressed
                    ),
                )));
            }

            // Produce the ETC2/KTX2 sidecar beside the original. A failed
            // decode or a non-4-aligned image yields no sidecar (the original
            // still carries the asset), so this never fails ingest. The sidecar
            // counts against the archive budget like any other entry.
            if let Some(bytes) = image_bytes {
                if let Some(sidecar) = transcode_image(&name, &bytes, gpu_caps) {
                    let sidecar_len = sidecar.bytes.len() as u64;
                    if ingested_total.saturating_add(sidecar_len) > budget.max_total_uncompressed {
                        return Err(PackageError::Io(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!(
                                "transcoded sidecar '{}' ({} bytes) would exceed budget {}",
                                sidecar.name, sidecar_len, budget.max_total_uncompressed
                            ),
                        )));
                    }
                    writer.add_entry(&sidecar.name, &sidecar.bytes)?;
                    ingested_total = ingested_total.saturating_add(sidecar_len);
                }
            }
        }

        writer.finish(package_name, package_version)
    })();

    let identity = match ingest_result {
        Ok(identity) => identity,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp_pkg_path);
            return Err(e);
        }
    };

    if let Err(e) = std::fs::rename(&tmp_pkg_path, pkg_path) {
        let _ = std::fs::remove_file(&tmp_pkg_path);
        return Err(PackageError::Io(e));
    }

    Ok(identity)
}

pub async fn ingest_zip_to_package_with_scheduler(
    scheduler: Arc<IoScheduler>,
    zip_path: PathBuf,
    pkg_path: PathBuf,
    package_name: String,
    package_version: String,
    gpu_caps: GpuCapsSnapshot,
) -> Result<PackageIdentity, PackageError> {
    ingest_zip_to_package_with_scheduler_and_budget(
        scheduler,
        zip_path,
        pkg_path,
        package_name,
        package_version,
        ExtractBudget::default(),
        gpu_caps,
    )
    .await
}

pub async fn ingest_zip_to_package_with_scheduler_and_budget(
    scheduler: Arc<IoScheduler>,
    zip_path: PathBuf,
    pkg_path: PathBuf,
    package_name: String,
    package_version: String,
    budget: ExtractBudget,
    gpu_caps: GpuCapsSnapshot,
) -> Result<PackageIdentity, PackageError> {
    let request = package_ingest_request_for(&zip_path);

    scheduler
        .run_async(request, move || {
            ingest_zip_to_package_with_budget(
                &zip_path,
                &pkg_path,
                &package_name,
                &package_version,
                budget,
                gpu_caps,
            )
        })
        .await
        .map_err(PackageError::from)?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_DIR_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn make_test_dir(name: &str) -> std::path::PathBuf {
        let sequence = TEST_DIR_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "migo_ingest_test_{name}_{}_{sequence}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn create_test_zip(dir: &Path) -> std::path::PathBuf {
        let zip_path = dir.join("input.zip");
        let file = std::fs::File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);

        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);

        zip.start_file("main.js", options).unwrap();
        zip.write_all(b"console.log('hello')").unwrap();

        zip.start_file("lib/utils.js", options).unwrap();
        zip.write_all(b"export function x() {}").unwrap();

        zip.start_file("img/bg.png", options).unwrap();
        zip.write_all(b"\x89PNG_fake_data").unwrap();

        zip.finish().unwrap();
        zip_path
    }

    /// A real, 4-aligned RGBA PNG so the transcode path actually runs, unlike
    /// the fake `\x89PNG_...` bytes in `create_test_zip` (which exercise the
    /// decode-fails-so-no-sidecar branch).
    #[cfg(feature = "rust-image-decode")]
    fn real_png(width: u32, height: u32) -> Vec<u8> {
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                rgba.extend_from_slice(&[(x * 4) as u8, (y * 4) as u8, 0x80, 0xFF]);
            }
        }
        let buffer = image::RgbaImage::from_raw(width, height, rgba).unwrap();
        let mut out = std::io::Cursor::new(Vec::new());
        buffer.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    /// End-to-end: a package built from a zip containing a real aligned PNG
    /// carries BOTH the original and a `.ktx2` ETC2 sidecar, and the sidecar is
    /// exactly what the runtime's KTX2 parser recognises. This is the whole
    /// point of the ingest wiring: the runtime already prefers the `.ktx2`
    /// companion (VARIANT_EXTENSIONS lists it first), so producing it here makes
    /// the zero-decode path live without any runtime change.
    #[cfg(feature = "rust-image-decode")]
    #[test]
    fn an_aligned_png_gains_an_etc2_sidecar_the_runtime_parser_accepts() {
        use shared::vfs::package::PackageReader;

        let dir = make_test_dir("ingest_sidecar");
        let zip_path = dir.join("in.zip");
        {
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zip.start_file("game.js", opts).unwrap();
            zip.write_all(b"//game").unwrap();
            zip.start_file("img/hero.png", opts).unwrap();
            zip.write_all(&real_png(32, 32)).unwrap();
            zip.finish().unwrap();
        }
        let pkg_path = dir.join("out.mpkg");
        ingest_zip_to_package(&zip_path, &pkg_path, "g", "1", GpuCapsSnapshot::default()).unwrap();

        let reader = PackageReader::open(&pkg_path, "g", "1").unwrap();
        let names: Vec<String> = reader.entry_paths().map(|s| s.to_string()).collect();

        // The original survives (ES 2.0 / getImageData fallback).
        assert!(
            names.iter().any(|n| n == "img/hero.png"),
            "names: {names:?}"
        );
        // And the sidecar was produced next to it, on the companion path the
        // runtime probes first.
        assert!(
            names.iter().any(|n| n == "img/hero.ktx2"),
            "names: {names:?}"
        );

        let ktx2 = reader.read_entry("img/hero.ktx2").unwrap();
        let parsed = crate::ktx2::parse_ktx2(&ktx2).expect("runtime parser accepts the sidecar");
        assert_eq!(
            parsed.header.format,
            crate::ktx2::VkFormat::Etc2R8G8B8UnormBlock
        );
        assert_eq!((parsed.header.width, parsed.header.height), (32, 32));
    }

    #[test]
    fn ingest_basic() {
        let dir = make_test_dir("ingest_basic");
        let zip_path = create_test_zip(&dir);
        let pkg_path = dir.join("output.mpkg");

        let identity = ingest_zip_to_package(
            &zip_path,
            &pkg_path,
            "test-game",
            "1.0.0",
            GpuCapsSnapshot::default(),
        )
        .unwrap();

        assert_eq!(identity.name, "test-game");
        assert_eq!(identity.version, "1.0.0");

        // Verify the package is readable.
        let reader =
            shared::vfs::package::PackageReader::open(&pkg_path, "test-game", "1.0.0").unwrap();
        assert_eq!(reader.entry_count(), 3);
        assert_eq!(
            reader.read_entry("main.js").unwrap(),
            b"console.log('hello')"
        );
        assert_eq!(
            reader.read_entry("lib/utils.js").unwrap(),
            b"export function x() {}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ingest_with_compression() {
        let dir = make_test_dir("ingest_compress");
        let zip_path = create_test_zip(&dir);
        let pkg_path = dir.join("output.mpkg");

        let _identity = ingest_zip_to_package(
            &zip_path,
            &pkg_path,
            "test-game",
            "1.0.0",
            GpuCapsSnapshot::default(),
        )
        .unwrap();

        let reader =
            shared::vfs::package::PackageReader::open(&pkg_path, "test-game", "1.0.0").unwrap();
        // JS files should be compressed, PNG should be stored.
        assert_eq!(
            reader.read_entry("main.js").unwrap(),
            b"console.log('hello')"
        );
        // PNG entry should still be readable.
        assert_eq!(
            reader.read_entry("img/bg.png").unwrap(),
            b"\x89PNG_fake_data"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn package_ingest_requests_default_to_background_priority() {
        let request = package_ingest_request_for(Path::new("/tmp/archive.zip"));
        match request {
            IoRequest::PackageIngest { priority, .. } => {
                assert_eq!(priority, PriorityClass::Background);
            }
            other => panic!("unexpected request: {other:?}"),
        }
    }

    #[test]
    fn ingest_full_install_flow() {
        use shared::vfs::mount::{MountTable, StagingArea};

        let dir = make_test_dir("ingest_install");
        let code_dir = dir.join("code");
        std::fs::create_dir_all(&code_dir).unwrap();

        let zip_path = create_test_zip(&dir);

        // 1. Create staging area.
        let staging = StagingArea::create(&dir, "stage1").unwrap();

        // 2. Ingest zip to package in staging.
        let pkg_filename = "stage1.mpkg";
        let staged_pkg = staging.dir().join(pkg_filename);
        ingest_zip_to_package(
            &zip_path,
            &staged_pkg,
            "stage1",
            "1.0",
            GpuCapsSnapshot::default(),
        )
        .unwrap();

        // 3. Mount table with base code dir.
        let mt = MountTable::new(code_dir.clone());

        // 4. Atomic install: validates, renames, mounts.
        let final_pkg = dir.join("packages").join("stage1.mpkg");
        let installed = staging
            .install_package(
                &mt,
                pkg_filename,
                &final_pkg,
                "subpackages/stage1",
                "stage1",
                "1.0",
            )
            .unwrap();

        assert_eq!(installed.identity.name, "stage1");

        // 5. Verify mount works.
        assert!(mt.exists("subpackages/stage1/main.js"));
        let data = mt.read("subpackages/stage1/main.js").unwrap();
        assert_eq!(data, b"console.log('hello')");

        // 6. Base code dir is still accessible.
        std::fs::write(code_dir.join("base.js"), "// base").unwrap();
        assert!(mt.exists("base.js"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_then_replace_subpackage() {
        use shared::vfs::mount::{MountTable, StagingArea};

        let dir = make_test_dir("install_replace");
        let code_dir = dir.join("code");
        std::fs::create_dir_all(&code_dir).unwrap();
        let cache_dir = dir.join("cache");
        let pkgs_dir = cache_dir.join("migo_packages");

        // Create v1 zip.
        let zip_v1 = dir.join("v1.zip");
        {
            let file = std::fs::File::create(&zip_v1).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zip.start_file("game.js", opts).unwrap();
            zip.write_all(b"// version 1").unwrap();
            zip.finish().unwrap();
        }

        // Create v2 zip with different content.
        let zip_v2 = dir.join("v2.zip");
        {
            let file = std::fs::File::create(&zip_v2).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zip.start_file("game.js", opts).unwrap();
            zip.write_all(b"// version 2").unwrap();
            zip.start_file("new_asset.png", opts).unwrap();
            zip.write_all(b"PNG_DATA").unwrap();
            zip.finish().unwrap();
        }

        let mt = MountTable::new(code_dir.clone());
        let pkg_filename = "stage1.mpkg";
        let final_pkg_path = pkgs_dir.join(pkg_filename);

        // --- Install v1 ---
        {
            let staging = StagingArea::create(&cache_dir, "stage1").unwrap();
            let staged_pkg = staging.dir().join(pkg_filename);
            ingest_zip_to_package(
                &zip_v1,
                &staged_pkg,
                "stage1",
                "1.0",
                GpuCapsSnapshot::default(),
            )
            .unwrap();
            staging
                .install_package(
                    &mt,
                    pkg_filename,
                    &final_pkg_path,
                    "sub/stage1",
                    "stage1",
                    "1.0",
                )
                .unwrap();
        }

        // v1 content visible.
        let data = mt.read("sub/stage1/game.js").unwrap();
        assert_eq!(data, b"// version 1");
        assert!(!mt.exists("sub/stage1/new_asset.png"));
        let gen_v1 = mt.generation();

        // --- Replace with v2 ---
        {
            let staging = StagingArea::create(&cache_dir, "stage1").unwrap();
            let staged_pkg = staging.dir().join(pkg_filename);
            ingest_zip_to_package(
                &zip_v2,
                &staged_pkg,
                "stage1",
                "2.0",
                GpuCapsSnapshot::default(),
            )
            .unwrap();
            staging
                .install_package(
                    &mt,
                    pkg_filename,
                    &final_pkg_path,
                    "sub/stage1",
                    "stage1",
                    "2.0",
                )
                .unwrap();
        }

        // v2 content visible, v1 gone.
        let data = mt.read("sub/stage1/game.js").unwrap();
        assert_eq!(data, b"// version 2");
        assert!(mt.exists("sub/stage1/new_asset.png"));
        assert!(
            mt.generation() > gen_v1,
            "generation must increase after replace"
        );

        // Base still accessible.
        std::fs::write(code_dir.join("base.js"), "// base").unwrap();
        assert!(mt.exists("base.js"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_failure_does_not_pollute() {
        use shared::vfs::mount::{MountTable, StagingArea};

        let dir = make_test_dir("install_fail");
        let code_dir = dir.join("code");
        std::fs::create_dir_all(&code_dir).unwrap();
        let cache_dir = dir.join("cache");

        // Install a valid v1 first.
        let zip_v1 = dir.join("v1.zip");
        {
            let file = std::fs::File::create(&zip_v1).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zip.start_file("game.js", opts).unwrap();
            zip.write_all(b"// v1 ok").unwrap();
            zip.finish().unwrap();
        }

        let mt = MountTable::new(code_dir.clone());
        let pkg_filename = "stage1.mpkg";
        let pkgs_dir = cache_dir.join("migo_packages");
        let final_pkg_path = pkgs_dir.join(pkg_filename);

        {
            let staging = StagingArea::create(&cache_dir, "stage1").unwrap();
            let staged_pkg = staging.dir().join(pkg_filename);
            ingest_zip_to_package(
                &zip_v1,
                &staged_pkg,
                "stage1",
                "1.0",
                GpuCapsSnapshot::default(),
            )
            .unwrap();
            staging
                .install_package(
                    &mt,
                    pkg_filename,
                    &final_pkg_path,
                    "sub/stage1",
                    "stage1",
                    "1.0",
                )
                .unwrap();
        }

        // v1 is live.
        assert_eq!(mt.read("sub/stage1/game.js").unwrap(), b"// v1 ok");

        // Try to install a corrupt "zip" (not a valid zip file).
        let bad_zip = dir.join("bad.zip");
        std::fs::write(&bad_zip, b"THIS IS NOT A ZIP").unwrap();

        {
            let staging = StagingArea::create(&cache_dir, "stage1_bad").unwrap();
            let staged_pkg = staging.dir().join(pkg_filename);
            let result = ingest_zip_to_package(
                &bad_zip,
                &staged_pkg,
                "stage1",
                "2.0",
                GpuCapsSnapshot::default(),
            );
            assert!(result.is_err(), "bad zip should fail ingest");
            // Staging is dropped/cleaned up automatically.
        }

        // v1 must still be live and readable.
        assert_eq!(mt.read("sub/stage1/game.js").unwrap(), b"// v1 ok");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // Per-game manifest + restore across sessions
    // -----------------------------------------------------------------------

    #[test]
    fn manifest_restore_across_sessions() {
        use shared::vfs::mount::{
            MountTable, PackageManifest, StagingArea, package_store_dir, restore_installed_packages,
        };

        let dir = make_test_dir("manifest_restore");
        let code_dir = dir.join("code");
        let game_cache_dir = dir.join("cache");
        std::fs::create_dir_all(&code_dir).unwrap();

        let zip_path = create_test_zip(&dir);
        let store = package_store_dir(&game_cache_dir);

        // --- Session 1: install a subpackage ---
        {
            let mt = MountTable::new(code_dir.clone());

            let staging = StagingArea::create(&game_cache_dir, "stage1").unwrap();
            let staged_pkg = staging.dir().join("stage1.mpkg");
            ingest_zip_to_package(
                &zip_path,
                &staged_pkg,
                "stage1",
                "1.0",
                GpuCapsSnapshot::default(),
            )
            .unwrap();
            let installed = staging
                .install_package(
                    &mt,
                    "stage1.mpkg",
                    &store.join("stage1.mpkg"),
                    "subpackages/stage1",
                    "stage1",
                    "1.0",
                )
                .unwrap();

            // Write manifest.
            let mut manifest = PackageManifest::load(&store);
            manifest.record(
                "stage1".into(),
                "subpackages/stage1".into(),
                "1.0".into(),
                &installed.digest,
            );
            manifest.save(&store).unwrap();

            // Verify readable in session 1.
            assert_eq!(
                mt.read("subpackages/stage1/main.js").unwrap(),
                b"console.log('hello')"
            );
        }
        // Session 1 ends — MountTable dropped.

        // --- Session 2: fresh MountTable, restore from manifest ---
        {
            let mt = MountTable::new(code_dir.clone());
            // No overlays yet.
            assert!(mt.read("subpackages/stage1/main.js").is_err());

            // Restore from manifest.
            restore_installed_packages(&mt, &game_cache_dir, false);

            // Now the subpackage is visible again!
            assert_eq!(
                mt.read("subpackages/stage1/main.js").unwrap(),
                b"console.log('hello')"
            );
            assert!(mt.exists("subpackages/stage1/lib/utils.js"));
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn code_tree_subpackage_runs_without_download() {
        use shared::vfs::mount::MountTable;

        let dir = make_test_dir("code_tree_sub");
        let code_dir = dir.join("code");

        // Simulate subpackage already in code tree (pre-extracted).
        let sub_dir = code_dir.join("subpackages").join("stage1");
        std::fs::create_dir_all(&sub_dir).unwrap();
        std::fs::write(sub_dir.join("game.js"), b"// stage1 game").unwrap();

        let mt = MountTable::new(code_dir.clone());

        // No overlay, no package store — but the base DirSource covers it.
        assert!(mt.exists("subpackages/stage1/game.js"));
        assert_eq!(
            mt.read("subpackages/stage1/game.js").unwrap(),
            b"// stage1 game"
        );

        // loadSubpackage's _tryLocalExecute would call amdRequire which
        // calls op_require_resolve_and_read which goes through MountTable.
        // This proves the code tree path works without download.

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn predownload_then_load_no_redownload() {
        use shared::vfs::mount::{MountTable, PackageManifest, StagingArea, package_store_dir};

        let dir = make_test_dir("predownload");
        let code_dir = dir.join("code");
        let game_cache_dir = dir.join("cache");
        std::fs::create_dir_all(&code_dir).unwrap();

        let zip_path = create_test_zip(&dir);
        let store = package_store_dir(&game_cache_dir);

        // preDownloadSubpackage: install but don't execute.
        let mt = MountTable::new(code_dir.clone());
        {
            let staging = StagingArea::create(&game_cache_dir, "stage1").unwrap();
            let staged_pkg = staging.dir().join("stage1.mpkg");
            ingest_zip_to_package(
                &zip_path,
                &staged_pkg,
                "stage1",
                "1.0",
                GpuCapsSnapshot::default(),
            )
            .unwrap();
            let installed = staging
                .install_package(
                    &mt,
                    "stage1.mpkg",
                    &store.join("stage1.mpkg"),
                    "subpackages/stage1",
                    "stage1",
                    "1.0",
                )
                .unwrap();

            let mut manifest = PackageManifest::load(&store);
            manifest.record(
                "stage1".into(),
                "subpackages/stage1".into(),
                "1.0".into(),
                &installed.digest,
            );
            manifest.save(&store).unwrap();
        }

        // loadSubpackage: should find it already mounted, no download needed.
        // (In the real JS flow, _tryLocalExecute would succeed here.)
        assert!(mt.exists("subpackages/stage1/main.js"));
        assert_eq!(
            mt.read("subpackages/stage1/main.js").unwrap(),
            b"console.log('hello')"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// FIO-01: When a sidecar has already consumed part of the archive budget,
    /// the next image entry must not be silently truncated and published.  The
    /// ingest must either include the original entry in full or fail cleanly.
    ///
    /// Three sub-cases from the audit (docs/audits/2026-09-09/full/io-network.md:31):
    ///
    /// * **cap+1 / sidecar boundary**: after img1 + its sidecar are ingested the
    ///   remaining budget is exactly `png_size - 1`; the second image needs one
    ///   more byte than the cap.  `take(cap)` silently truncates today — after fix
    ///   this must return `Err`.
    /// * **exact-cap**: remaining budget equals the second image's size exactly —
    ///   the entry fits, ingest must return `Ok` with an intact original.
    #[cfg(feature = "rust-image-decode")]
    #[test]
    fn sidecar_budget_exhaustion_rejects_truncated_original() {
        use shared::vfs::package::PackageReader;

        let dir = make_test_dir("sidecar_budget");
        let first_png = real_png(8, 8);
        let first_size = first_png.len() as u64;
        // A 7x7 image is still a real PNG, but its dimensions are not
        // 4-aligned, so transcode_image produces no second sidecar.  That
        // isolates the budget boundary to the original entry under test.
        let second_png = real_png(7, 7);
        let second_size = second_png.len() as u64;
        // ── Discovery run: ingest a single image to learn the sidecar size.
        let zip_single = dir.join("single.zip");
        {
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&zip_single).unwrap());
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zip.start_file("img.png", opts).unwrap();
            zip.write_all(&first_png).unwrap();
            zip.finish().unwrap();
        }
        let pkg_single = dir.join("single.mpkg");
        ingest_zip_to_package(
            &zip_single,
            &pkg_single,
            "disc",
            "0",
            GpuCapsSnapshot::default(),
        )
        .unwrap();
        let reader_single = PackageReader::open(&pkg_single, "disc", "0").unwrap();
        let sidecar_size = reader_single.entry_raw_size("img.ktx2").unwrap_or(0);
        if sidecar_size == 0 {
            // No sidecar produced (e.g. ETC2 not built in); vacuously true.
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }

        // ── Case 0 (sidecar itself cannot fit): the original fits exactly,
        // but there is no budget left for its optional sidecar.  The sidecar
        // must be skipped by failing the atomic ingest, never by publishing a
        // partial package or replacing the prior target.
        let pkg_sidecar_failure = dir.join("sidecar_too_large.mpkg");
        let previous_sidecar_target = b"previous sidecar package";
        std::fs::write(&pkg_sidecar_failure, previous_sidecar_target).unwrap();
        let sidecar_failure_budget = ExtractBudget {
            max_total_uncompressed: first_size,
            max_entry_uncompressed: first_size,
            max_compression_ratio: 0,
            max_entries: 1000,
        };
        let sidecar_failure = ingest_zip_to_package_with_budget(
            &zip_single,
            &pkg_sidecar_failure,
            "disc",
            "0",
            sidecar_failure_budget,
            GpuCapsSnapshot::default(),
        );
        assert!(
            sidecar_failure.is_err(),
            "FIO-01 sidecar boundary: an over-budget sidecar must fail ingest"
        );
        assert_eq!(
            std::fs::read(&pkg_sidecar_failure).unwrap(),
            previous_sidecar_target,
            "FIO-01 sidecar boundary: failed ingest must preserve the prior target"
        );
        assert!(
            !pkg_sidecar_failure.with_extension("mpkg.tmp").exists(),
            "FIO-01 sidecar boundary: failed ingest must clean its temporary output"
        );

        // ── Zip with two images: only the first is 4-aligned and gets a sidecar.
        let zip_two = dir.join("two.zip");
        {
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&zip_two).unwrap());
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zip.start_file("img1.png", opts).unwrap();
            zip.write_all(&first_png).unwrap();
            zip.start_file("img2.png", opts).unwrap();
            zip.write_all(&second_png).unwrap();
            zip.finish().unwrap();
        }

        // ── Case A (cap+1 / sidecar boundary):
        // Budget = img1 + sidecar1 + (img2 - 1).  Pre-scan sees img1+img2 ≤ budget.
        // At runtime: ingested_total after img1+sidecar = first_size + sidecar_size.
        // Remaining = budget - (first_size + sidecar_size) = second_size - 1.
        // cap = second_size - 1 < second_size → take(cap) truncates by 1 byte today.
        let tight_budget = ExtractBudget {
            max_total_uncompressed: first_size + second_size + sidecar_size - 1,
            max_entry_uncompressed: first_size.max(second_size) * 4,
            max_compression_ratio: 0,
            max_entries: 1000,
        };
        let pkg_tight = dir.join("tight.mpkg");
        let previous_target = b"previous package";
        std::fs::write(&pkg_tight, previous_target).unwrap();
        let result_tight = ingest_zip_to_package_with_budget(
            &zip_two,
            &pkg_tight,
            "t",
            "1",
            tight_budget,
            GpuCapsSnapshot::default(),
        );
        assert!(
            result_tight.is_err(),
            "FIO-01 cap+1: sidecar-exhausted budget silently published a truncated \
             original — ingest must fail instead"
        );
        assert_eq!(
            std::fs::read(&pkg_tight).unwrap(),
            previous_target,
            "FIO-01 cap+1: failed ingest must preserve the previous target"
        );
        assert!(
            !pkg_tight.with_extension("mpkg.tmp").exists(),
            "FIO-01 cap+1: failed ingest must clean its temporary output"
        );

        // ── Case B (exact-cap):
        // Budget = img1 + sidecar1 + img2 exactly — second image fits in full.
        let exact_budget = ExtractBudget {
            max_total_uncompressed: first_size + second_size + sidecar_size,
            max_entry_uncompressed: first_size.max(second_size) * 4,
            max_compression_ratio: 0,
            max_entries: 1000,
        };
        let pkg_exact = dir.join("exact.mpkg");
        ingest_zip_to_package_with_budget(
            &zip_two,
            &pkg_exact,
            "t",
            "1",
            exact_budget,
            GpuCapsSnapshot::default(),
        )
        .expect("FIO-01 exact-cap: second image that fits exactly must succeed");

        let reader = PackageReader::open(&pkg_exact, "t", "1").unwrap();
        assert_eq!(
            reader.read_entry("img1.png").unwrap().as_slice(),
            first_png.as_slice(),
            "FIO-01 exact-cap: img1 must be bit-exact after a tight-budget ingest"
        );
        assert_eq!(
            reader.read_entry("img2.png").unwrap().as_slice(),
            second_png.as_slice(),
            "FIO-01 exact-cap: img2 must be bit-exact after a tight-budget ingest"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
