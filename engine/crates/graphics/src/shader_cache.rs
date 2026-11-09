//! Best-effort shader binary cache using `glGetProgramBinary` / `glProgramBinary`.
//!
//! Saves compiled GL program binaries to the app cache directory on first link,
//! and loads them on subsequent runs to skip runtime compilation (~50-200 ms saved
//! on cold start).
//!
//! **Best-effort only:** `glProgramBinary` is not portable across drivers or driver
//! versions.  Any load failure silently falls back to runtime compilation + re-cache.

use glow::HasContext;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::time::{Duration, SystemTime};
use tracing::{debug, trace};

/// A normalized snapshot of every input that affects a program link.
///
/// The descriptor is deliberately assembled before `glProgramBinary` is
/// attempted.  A successful binary load only proves that the driver accepted
/// bytes; it does not prove that the bytes came from this link configuration.
pub(crate) struct LinkDescriptor<'a> {
    pub(crate) vertex_src: &'a str,
    pub(crate) fragment_src: &'a str,
    pub(crate) attrib_key: &'a str,
    pub(crate) tf_varyings: &'a [String],
    pub(crate) tf_buffer_mode: Option<u32>,
}

pub(crate) type PersistJob = (PathBuf, Vec<u8>);
pub(crate) const MAX_ENTRIES: usize = 256;
pub(crate) const MAX_CACHE_BYTES: u64 = 64 * 1024 * 1024;
/// The largest binary an entry holds: a sixteenth of the cache. A compiled program is tens to hundreds of kilobytes;
/// a binary past this carries a driver's pipeline cache with it -- ANGLE's Vulkan back end puts the context's whole
/// cache in every program binary, 0.7 MB more for each program the context has linked, up to 60 MB on the Android
/// emulator -- and costs the render thread more to fetch than linking from source does. It is not fetched, and a file
/// past it is not read.
pub(crate) const MAX_ENTRY_BYTES: u64 = MAX_CACHE_BYTES / 16;

/// Whether a binary of `len` bytes is one an entry holds.
fn entry_fits(len: u64) -> bool {
    len > 0 && len <= MAX_ENTRY_BYTES
}

/// Manages a disk-backed shader binary cache.
pub struct ShaderCache {
    /// Base directory for cached binaries (e.g. `<app_cache>/migo_shader_cache/`).
    cache_dir: PathBuf,
    /// Fingerprint of the GL driver and its WebGL validation mode.
    /// It participates in every key as well as the directory marker so a
    /// stale file cannot be accepted after a driver/version or mode change.
    driver_key: String,
    /// `true` if `GL_NUM_PROGRAM_BINARY_FORMATS > 0`.
    supported: bool,
    /// Bounded, non-blocking handoff to the persistence worker.
    persist_tx: Option<SyncSender<PersistJob>>,
}

impl ShaderCache {
    /// Create a new cache.  `cache_root` is the app's cache directory.
    /// Probes GL capabilities; if program binary is not supported, all
    /// operations become no-ops.
    pub fn new(gl: &glow::Context, cache_root: &Path) -> Self {
        let renderer = unsafe { gl.get_parameter_string(glow::RENDERER) };
        let version = unsafe { gl.get_parameter_string(glow::VERSION) };
        let driver_key = driver_key(
            &renderer,
            &version,
            gl.supported_extensions()
                .contains("GL_ANGLE_webgl_compatibility"),
        );
        let num_formats = unsafe { gl.get_parameter_i32(glow::NUM_PROGRAM_BINARY_FORMATS) };
        let supported = num_formats > 0;
        let cache_dir = cache_root.join("migo_shader_cache");

        if supported {
            std::fs::create_dir_all(&cache_dir).ok();
            let key_path = cache_dir.join(".driver_key");
            let stale = std::fs::read_to_string(&key_path)
                .map(|s| s != driver_key)
                .unwrap_or(true);
            if stale {
                debug!("Shader cache: driver or validation mode changed, clearing cache");
                clear_dir(&cache_dir);
                std::fs::write(&key_path, &driver_key).ok();
            } else {
                remove_abandoned_temp_files(&cache_dir, SystemTime::now());
            }
        }

        debug!("ShaderCache: supported={supported}, driver_key={driver_key:.60}");
        let persist_tx = supported.then(|| spawn_persist_worker(cache_dir.clone()));
        Self {
            cache_dir,
            driver_key,
            supported,
            persist_tx,
        }
    }

    #[cfg(test)]
    fn new_for_test(cache_dir: PathBuf, driver_key: String, supported: bool) -> Self {
        std::fs::create_dir_all(&cache_dir).unwrap();
        let persist_tx = supported.then(|| spawn_persist_worker(cache_dir.clone()));
        Self {
            cache_dir,
            driver_key,
            supported,
            persist_tx,
        }
    }

    pub fn is_supported(&self) -> bool {
        self.supported
    }

    pub fn load(&self, descriptor: &LinkDescriptor<'_>) -> Option<(u32, Vec<u8>)> {
        if !self.supported {
            return None;
        }
        let key = make_cache_key(descriptor, &self.driver_key);
        let path = self.cache_dir.join(&key);
        let len = std::fs::metadata(&path).ok()?.len();
        if !entry_fits(len.saturating_sub(4)) {
            std::fs::remove_file(&path).ok();
            return None;
        }
        let mut data = std::fs::read(&path).ok()?;
        if data.len() < 4 {
            return None;
        }
        let format = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        data.drain(..4);
        trace!("Shader cache hit: {key}");
        Some((format, data))
    }

    /// The driver did not link the binary `load` gave for `descriptor`: the entry is removed, so the bytes are not
    /// read and offered again, and the link from source that follows saves a binary of its own. A driver that never
    /// takes its binaries back -- the Android emulator's refuses nearly all of them -- would otherwise be handed every
    /// one again at every launch.
    pub fn refused(&self, descriptor: &LinkDescriptor<'_>) {
        let key = make_cache_key(descriptor, &self.driver_key);
        debug!("Shader cache: the driver refused {key}; removed");
        std::fs::remove_file(self.cache_dir.join(&key)).ok();
    }

    /// Capture a binary on the render thread, then enqueue only owned bytes.
    /// Disk I/O is performed by the bounded worker and is therefore best effort.
    pub fn save(
        &self,
        gl: &glow::Context,
        program: glow::NativeProgram,
        descriptor: &LinkDescriptor<'_>,
    ) {
        if !self.supported {
            return;
        }
        let (format, binary) = match get_program_binary(gl, program) {
            Some(v) => v,
            None => return,
        };
        let key = make_cache_key(descriptor, &self.driver_key);
        let path = self.cache_dir.join(&key);
        let mut data = Vec::with_capacity(4 + binary.len());
        data.extend_from_slice(&format.to_le_bytes());
        data.extend_from_slice(&binary);

        let Some(tx) = &self.persist_tx else {
            return;
        };
        match tx.try_send((path, data)) {
            Ok(()) => trace!("Shader cache save queued: {key}"),
            Err(TrySendError::Full(_)) => {
                debug!("Shader cache persistence queue full; dropping {key}")
            }
            Err(TrySendError::Disconnected(_)) => {
                debug!("Shader cache persistence worker stopped; dropping {key}")
            }
        }
    }
}

fn driver_key(renderer: &str, version: &str, webgl: bool) -> String {
    format!("{renderer}||{version}||webgl={webgl}")
}

pub(crate) fn make_cache_key(descriptor: &LinkDescriptor<'_>, driver_key: &str) -> String {
    let mut hasher = DefaultHasher::new();
    driver_key.hash(&mut hasher);
    descriptor.vertex_src.hash(&mut hasher);
    descriptor.fragment_src.hash(&mut hasher);
    descriptor.attrib_key.hash(&mut hasher);
    descriptor.tf_varyings.hash(&mut hasher);
    descriptor.tf_buffer_mode.hash(&mut hasher);
    format!("{:016x}.bin", hasher.finish())
}

fn spawn_persist_worker(cache_dir: PathBuf) -> SyncSender<PersistJob> {
    let (tx, rx) = mpsc::sync_channel(16);
    std::thread::spawn(move || persist_loop(rx, cache_dir));
    tx
}

fn persist_loop(rx: Receiver<PersistJob>, cache_dir: PathBuf) {
    while let Ok((path, data)) = rx.recv() {
        if let Err(error) = persist(&path, &data) {
            debug!("Shader cache write failed: {error}");
        } else {
            trace!(
                "Shader cache saved: {} ({} bytes)",
                path.display(),
                data.len()
            );
            enforce_cache_limits(&cache_dir, MAX_ENTRIES, MAX_CACHE_BYTES);
        }
    }
}

/// Publish `data` as `path`, whole or not at all.
///
/// Every engine in a process has its own cache and worker over the same
/// directory, so two of them can save one program at once: each write gets its
/// own temporary file, created exclusively, and only a complete one is renamed
/// into place. The bytes are synced before the rename, so a power loss leaves the
/// whole binary or no entry -- never a truncated one, which the next launch would
/// hand to a driver that may not check it.
fn persist(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = temp_path_for(path);
    let result = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .and_then(|mut file| {
            file.write_all(data)?;
            file.sync_all()
        })
        .and_then(|()| std::fs::rename(&tmp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// A temporary name for one write of `path`, unique to this process and this
/// write. Its extension is `tmp`, so neither `load` nor the entry limits see it.
fn temp_path_for(path: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    path.with_extension(format!("{}.{id}.tmp", std::process::id()))
}

/// How old a temporary file must be before it counts as abandoned: a write and its
/// rename take milliseconds, so this never removes one another engine is writing.
const ABANDONED_TEMP_AGE: Duration = Duration::from_secs(60);

/// Remove the temporary files of writes a crash interrupted. Their names are
/// unique per write, so nothing else would ever replace or remove them.
fn remove_abandoned_temp_files(dir: &Path, now: SystemTime) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("tmp") {
            continue;
        }
        let abandoned = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| {
                now.duration_since(modified)
                    .is_ok_and(|age| age >= ABANDONED_TEMP_AGE)
            });
        if abandoned {
            std::fs::remove_file(path).ok();
        }
    }
}

fn enforce_cache_limits(dir: &Path, max_entries: usize, max_bytes: u64) {
    let mut files = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            (path.extension().and_then(|e| e.to_str()) == Some("bin"))
                .then(|| {
                    let metadata = entry.metadata().ok()?;
                    Some((
                        path,
                        metadata.len(),
                        metadata
                            .modified()
                            .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
                    ))
                })
                .flatten()
        })
        .collect::<Vec<_>>();
    let mut total_bytes = files.iter().map(|(_, len, _)| *len).sum::<u64>();
    files.sort_by_key(|(_, _, modified)| *modified);
    while files.len() > max_entries || total_bytes > max_bytes {
        let Some((path, len, _)) = files.first().cloned() else {
            break;
        };
        files.remove(0);
        if std::fs::remove_file(path).is_ok() {
            total_bytes = total_bytes.saturating_sub(len);
        }
    }
}

/// Get the binary representation of a linked program using glow's
/// `get_program_binary` wrapper; `None` for one no entry holds ([`MAX_ENTRY_BYTES`]),
/// which is not fetched.
fn get_program_binary(gl: &glow::Context, program: glow::NativeProgram) -> Option<(u32, Vec<u8>)> {
    unsafe {
        let length = gl.get_program_parameter_i32(program, glow::PROGRAM_BINARY_LENGTH);
        if !entry_fits(u64::try_from(length).unwrap_or(0)) {
            return None;
        }

        // glow wraps glGetProgramBinary → returns (format, Vec<u8>).
        match gl.get_program_binary(program) {
            Some(binary) => {
                if binary.buffer.is_empty() {
                    return None;
                }
                Some((binary.format, binary.buffer))
            }
            None => None,
        }
    }
}

fn clear_dir(dir: &Path) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                std::fs::remove_file(&path).ok();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Cache key completeness ----

    #[test]
    fn different_tf_varyings_produce_different_keys() {
        let vs = "void main() {}";
        let fs = "void main() {}";
        let driver = "Mesa||GLES 3.0";
        let a = vec!["vPosition".to_string()];
        let b = vec!["vColor".to_string()];
        let k1 = make_cache_key(
            &LinkDescriptor {
                vertex_src: vs,
                fragment_src: fs,
                attrib_key: "",
                tf_varyings: &a,
                tf_buffer_mode: Some(0x8C8D),
            },
            driver,
        );
        let k2 = make_cache_key(
            &LinkDescriptor {
                vertex_src: vs,
                fragment_src: fs,
                attrib_key: "",
                tf_varyings: &b,
                tf_buffer_mode: Some(0x8C8D),
            },
            driver,
        );
        assert_ne!(
            k1, k2,
            "distinct TF varyings must produce distinct cache keys"
        );
    }

    #[test]
    fn different_tf_buffer_mode_produces_different_key() {
        let driver = "Mesa||GLES 3.0";
        let varyings = vec!["vPos".to_string()];
        let k1 = make_cache_key(
            &LinkDescriptor {
                vertex_src: "v",
                fragment_src: "f",
                attrib_key: "",
                tf_varyings: &varyings,
                tf_buffer_mode: Some(0x8C8C),
            },
            driver,
        );
        let k2 = make_cache_key(
            &LinkDescriptor {
                vertex_src: "v",
                fragment_src: "f",
                attrib_key: "",
                tf_varyings: &varyings,
                tf_buffer_mode: Some(0x8C8D),
            },
            driver,
        );
        assert_ne!(
            k1, k2,
            "different TF buffer modes must produce distinct cache keys"
        );
    }

    #[test]
    fn different_driver_key_produces_different_key() {
        let empty: Vec<String> = vec![];
        let desc = LinkDescriptor {
            vertex_src: "v",
            fragment_src: "f",
            attrib_key: "",
            tf_varyings: &empty,
            tf_buffer_mode: None,
        };
        let k1 = make_cache_key(&desc, "DriverA||1.0");
        let k2 = make_cache_key(&desc, "DriverB||1.0");
        assert_ne!(
            k1, k2,
            "different driver/version must produce distinct cache keys"
        );
    }

    #[test]
    fn attribute_binding_names_are_not_collapsed() {
        let empty: Vec<String> = vec![];
        let first = LinkDescriptor {
            vertex_src: "v",
            fragment_src: "f",
            attrib_key: "0=position;",
            tf_varyings: &empty,
            tf_buffer_mode: None,
        };
        let second = LinkDescriptor {
            attrib_key: "0=color;",
            ..first
        };
        assert_ne!(
            make_cache_key(&first, "Driver||1"),
            make_cache_key(&second, "Driver||1"),
            "binding different attribute names at one index must not share a key"
        );
    }

    #[test]
    fn multiple_attribute_names_at_one_index_remain_distinct_inputs() {
        let empty: Vec<String> = vec![];
        let history = LinkDescriptor {
            vertex_src: "v",
            fragment_src: "f",
            attrib_key: "color=0;position=0;",
            tf_varyings: &empty,
            tf_buffer_mode: Some(glow::INTERLEAVED_ATTRIBS),
        };
        let final_only = LinkDescriptor {
            attrib_key: "color=0;",
            ..history
        };
        assert_ne!(
            make_cache_key(&history, "Driver||1"),
            make_cache_key(&final_only, "Driver||1"),
            "binding history for distinct names at one index must not collapse"
        );
    }

    #[test]
    fn tf_varying_order_matters_for_key() {
        let driver = "Mesa||GLES 3.0";
        let ab = vec!["vA".to_string(), "vB".to_string()];
        let ba = vec!["vB".to_string(), "vA".to_string()];
        let k1 = make_cache_key(
            &LinkDescriptor {
                vertex_src: "v",
                fragment_src: "f",
                attrib_key: "",
                tf_varyings: &ab,
                tf_buffer_mode: Some(0x8C8D),
            },
            driver,
        );
        let k2 = make_cache_key(
            &LinkDescriptor {
                vertex_src: "v",
                fragment_src: "f",
                attrib_key: "",
                tf_varyings: &ba,
                tf_buffer_mode: Some(0x8C8D),
            },
            driver,
        );
        assert_ne!(k1, k2, "TF varying order must affect the cache key");
    }

    #[test]
    fn no_tf_varyings_differs_from_with_tf_varyings() {
        let driver = "Mesa||GLES 3.0";
        let with_v = vec!["v".to_string()];
        let empty: Vec<String> = vec![];
        let k1 = make_cache_key(
            &LinkDescriptor {
                vertex_src: "s",
                fragment_src: "s",
                attrib_key: "",
                tf_varyings: &empty,
                tf_buffer_mode: None,
            },
            driver,
        );
        let k2 = make_cache_key(
            &LinkDescriptor {
                vertex_src: "s",
                fragment_src: "s",
                attrib_key: "",
                tf_varyings: &with_v,
                tf_buffer_mode: Some(0x8C8C),
            },
            driver,
        );
        assert_ne!(k1, k2);
    }

    // ---- Async save, torn-file safety, limits ----

    fn make_test_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("migo_sc_test_{tag}_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn save_enqueues_without_synchronous_fs_write() {
        const SOURCE: &str = include_str!("shader_cache.rs");
        let start = SOURCE.find("pub fn save(").expect("save method exists");
        let end = SOURCE[start..]
            .find("pub(crate) fn make_cache_key")
            .map(|offset| start + offset)
            .expect("cache-key helper follows save");
        let save = &SOURCE[start..end];
        assert!(save.contains("try_send"), "save must use the bounded queue");
        assert!(
            !save.contains("std::fs::write"),
            "render-thread save must not perform synchronous disk writes"
        );
    }

    #[test]
    fn torn_tmp_file_is_not_loadable() {
        let dir = make_test_dir("torn");
        let driver = "drv||1";
        let empty: Vec<String> = vec![];
        let desc = LinkDescriptor {
            vertex_src: "v",
            fragment_src: "f",
            attrib_key: "",
            tf_varyings: &empty,
            tf_buffer_mode: None,
        };
        let key = make_cache_key(&desc, driver);

        // Simulate a crash-interrupted write: a .tmp file with valid content.
        let tmp = dir.join(format!("{key}.tmp"));
        let mut data = 99u32.to_le_bytes().to_vec();
        data.extend_from_slice(b"binarydata");
        std::fs::write(&tmp, &data).unwrap();

        let cache = ShaderCache::new_for_test(dir.clone(), driver.into(), true);
        assert!(
            cache.load(&desc).is_none(),
            "a torn .tmp file must never be loaded"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_round_trips_binary_after_direct_write() {
        let dir = make_test_dir("roundtrip");
        let driver = "drv||2";
        let varyings = vec!["vPos".to_string()];
        let desc = LinkDescriptor {
            vertex_src: "vs",
            fragment_src: "fs",
            attrib_key: "0=a;",
            tf_varyings: &varyings,
            tf_buffer_mode: Some(0x8C8D),
        };
        let key = make_cache_key(&desc, driver);
        let path = dir.join(&key);

        let format: u32 = 0xDEAD_BEEF;
        let payload = b"thebinary";
        let mut file_data = format.to_le_bytes().to_vec();
        file_data.extend_from_slice(payload);
        std::fs::write(&path, &file_data).unwrap();

        let cache = ShaderCache::new_for_test(dir.clone(), driver.into(), true);
        let result = cache.load(&desc);
        assert!(result.is_some(), "must load a valid cache file");
        let (fmt, bin) = result.unwrap();
        assert_eq!(fmt, format);
        assert_eq!(bin, payload);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_entry_past_the_entry_bound_is_not_read_and_is_removed() {
        let dir = make_test_dir("oversize");
        let driver = "drv||3";
        let empty: Vec<String> = vec![];
        let desc = LinkDescriptor {
            vertex_src: "vs",
            fragment_src: "fs",
            attrib_key: "",
            tf_varyings: &empty,
            tf_buffer_mode: None,
        };
        let path = dir.join(make_cache_key(&desc, driver));
        // The four-byte format, then one byte more binary than an entry holds.
        std::fs::write(&path, vec![0u8; 4 + MAX_ENTRY_BYTES as usize + 1]).unwrap();

        let cache = ShaderCache::new_for_test(dir.clone(), driver.into(), true);
        assert!(cache.load(&desc).is_none());
        assert!(
            !path.exists(),
            "an entry no binary of which is kept must not stay"
        );

        // One at the bound is an entry.
        std::fs::write(&path, vec![0u8; 4 + MAX_ENTRY_BYTES as usize]).unwrap();
        assert_eq!(
            cache.load(&desc).map(|(_, binary)| binary.len() as u64),
            Some(MAX_ENTRY_BYTES)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_binary_the_driver_refused_is_removed() {
        let dir = make_test_dir("refused");
        let driver = "drv||4";
        let empty: Vec<String> = vec![];
        let desc = LinkDescriptor {
            vertex_src: "vs",
            fragment_src: "fs",
            attrib_key: "",
            tf_varyings: &empty,
            tf_buffer_mode: None,
        };
        let other = LinkDescriptor {
            fragment_src: "another fs",
            ..desc
        };
        for d in [&desc, &other] {
            let mut data = 7u32.to_le_bytes().to_vec();
            data.extend_from_slice(b"binary");
            std::fs::write(dir.join(make_cache_key(d, driver)), data).unwrap();
        }

        let cache = ShaderCache::new_for_test(dir.clone(), driver.into(), true);
        assert!(cache.load(&desc).is_some());
        cache.refused(&desc);
        assert!(
            cache.load(&desc).is_none(),
            "a refused binary is not offered again"
        );
        assert!(cache.load(&other).is_some(), "only the refused entry goes");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_binary_no_entry_holds_is_not_fetched() {
        assert!(!entry_fits(0));
        assert!(entry_fits(1));
        assert!(entry_fits(MAX_ENTRY_BYTES));
        assert!(!entry_fits(MAX_ENTRY_BYTES + 1));
        // The length is asked first and the fetch is gated on it.
        const SOURCE: &str = include_str!("shader_cache.rs");
        let start = SOURCE
            .find("fn get_program_binary(")
            .expect("fetch helper exists");
        let body = &SOURCE[start..];
        let gate = body
            .find("entry_fits(")
            .expect("the fetch is gated on the length");
        let fetch = body.find("gl.get_program_binary(").expect("the fetch");
        assert!(
            gate < fetch,
            "the length is checked before the binary is fetched"
        );
    }

    #[test]
    fn cache_entry_limit_is_enforced() {
        let dir = make_test_dir("limit");
        let (tx, rx) = std::sync::mpsc::sync_channel::<PersistJob>(MAX_ENTRIES + 20);
        let dir2 = dir.clone();
        let th = std::thread::spawn(move || persist_loop(rx, dir2));
        for i in 0..=(MAX_ENTRIES + 9) as u64 {
            let path = dir.join(format!("{i:016x}.bin"));
            let data = vec![0u8, 0, 0, 0, b'x'];
            tx.send((path, data)).unwrap();
        }
        drop(tx);
        th.join().unwrap();
        let count = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().map_or(false, |x| x == "bin"))
            .count();
        assert!(
            count <= MAX_ENTRIES,
            "cache eviction must keep count ≤ {MAX_ENTRIES}; found {count}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_byte_limit_is_enforced() {
        let dir = make_test_dir("bytes");
        for i in 0..3 {
            std::fs::write(dir.join(format!("{i}.bin")), vec![i as u8; 8]).unwrap();
        }
        enforce_cache_limits(&dir, 100, 10);
        let total = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter_map(|entry| {
                if entry.path().extension().and_then(|ext| ext.to_str()) == Some("bin") {
                    entry.metadata().ok().map(|metadata| metadata.len())
                } else {
                    None
                }
            })
            .sum::<u64>();
        assert!(total <= 10, "cache byte budget exceeded: {total}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_persisted_entry_holds_the_whole_payload_and_leaves_no_temporary_file() {
        let dir = make_test_dir("persist");
        let path = dir.join("0123456789abcdef.bin");
        persist(&path, b"first").unwrap();
        persist(&path, b"second, longer").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second, longer");
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(
            names,
            ["0123456789abcdef.bin"],
            "a temporary file outlived its write"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_writes_of_one_entry_never_share_a_temporary_file() {
        let path = Path::new("/cache/0123456789abcdef.bin");
        let (a, b) = (temp_path_for(path), temp_path_for(path));
        assert_ne!(
            a, b,
            "concurrent saves of one program would interleave bytes"
        );
        for tmp in [&a, &b] {
            assert_eq!(
                tmp.parent(),
                path.parent(),
                "a rename must not cross directories"
            );
            assert_eq!(tmp.extension().and_then(|e| e.to_str()), Some("tmp"));
        }
    }

    #[test]
    fn persistence_syncs_the_bytes_before_publishing_the_name() {
        // A power loss after an unsynced rename can publish a truncated binary.
        const SOURCE: &str = include_str!("shader_cache.rs");
        let start = SOURCE.find("fn persist(").expect("persistence exists");
        let body = &SOURCE[start..start + SOURCE[start..].find("\n}\n").unwrap()];
        let sync = body.find("sync_all").expect("the payload is synced");
        let rename = body
            .find("std::fs::rename")
            .expect("the payload is renamed");
        assert!(
            sync < rename,
            "the name is published before the bytes are durable"
        );
    }

    #[test]
    fn startup_removes_abandoned_temporary_files_and_spares_ones_being_written() {
        let dir = make_test_dir("abandoned");
        let old = dir.join("aaaaaaaaaaaaaaaa.1.0.tmp");
        let fresh = dir.join("bbbbbbbbbbbbbbbb.1.1.tmp");
        let entry = dir.join("cccccccccccccccc.bin");
        for path in [&old, &fresh, &entry] {
            std::fs::write(path, b"x").unwrap();
        }
        let written = std::fs::metadata(&old).unwrap().modified().unwrap();
        remove_abandoned_temp_files(&dir, written + ABANDONED_TEMP_AGE / 2);
        assert!(
            old.exists() && fresh.exists(),
            "a write in progress was removed"
        );
        std::fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(written - ABANDONED_TEMP_AGE)
            .unwrap();
        remove_abandoned_temp_files(&dir, written + ABANDONED_TEMP_AGE / 2);
        assert!(!old.exists(), "a crash leftover survived startup");
        assert!(fresh.exists() && entry.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ordinary_and_webgl_modes_do_not_share_cached_binaries() {
        let dir = make_test_dir("validation_mode");
        let descriptor = LinkDescriptor {
            vertex_src: "v",
            fragment_src: "f",
            attrib_key: "",
            tf_varyings: &[],
            tf_buffer_mode: None,
        };
        // The renderer/version can be identical in two contexts. The mode is
        // still a compilation input, including when an old worker writes late.
        let ordinary = ShaderCache::new_for_test(
            dir.clone(),
            driver_key("same GPU", "same GLES", false),
            true,
        );
        let webgl =
            ShaderCache::new_for_test(dir.clone(), driver_key("same GPU", "same GLES", true), true);
        let ordinary_path = dir.join(make_cache_key(&descriptor, &ordinary.driver_key));
        let webgl_path = dir.join(make_cache_key(&descriptor, &webgl.driver_key));
        let ordinary_bytes = [7u32.to_le_bytes().as_slice(), b"ordinary binary"].concat();
        let webgl_bytes = [9u32.to_le_bytes().as_slice(), b"WebGL binary"].concat();
        std::fs::write(&ordinary_path, &ordinary_bytes).unwrap();
        assert_eq!(
            ordinary.load(&descriptor),
            Some((7, b"ordinary binary".to_vec()))
        );
        assert!(
            webgl.load(&descriptor).is_none(),
            "ordinary binary must not bypass WebGL validation"
        );
        std::fs::write(&webgl_path, &webgl_bytes).unwrap();
        assert_eq!(webgl.load(&descriptor), Some((9, b"WebGL binary".to_vec())));
        std::fs::write(&ordinary_path, ordinary_bytes).unwrap(); // A late ordinary writer.
        assert_eq!(webgl.load(&descriptor), Some((9, b"WebGL binary".to_vec())));
        assert_eq!(
            ordinary.load(&descriptor),
            Some((7, b"ordinary binary".to_vec()))
        );
        drop((ordinary, webgl));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[ignore = "requires MIGO_TEST_ANGLE_EGL_LIBRARY and ANGLE/SwiftShader Vulkan"]
    fn angle_program_binary_cache_native() {
        fn pixel(gl: &glow::Context, program: glow::NativeProgram) -> [u8; 4] {
            unsafe {
                gl.use_program(Some(program));
                let tint = gl
                    .get_uniform_location(program, "tint")
                    .expect("active tint");
                gl.uniform_4_f32(Some(&tint), 0.0, 0.5, 0.0, 1.0);
                let vao = gl.create_vertex_array().unwrap();
                gl.bind_vertex_array(Some(vao));
                gl.viewport(0, 0, 3, 2);
                gl.clear_color(1.0, 0.0, 1.0, 1.0);
                gl.clear(glow::COLOR_BUFFER_BIT);
                gl.draw_arrays(glow::TRIANGLES, 0, 3);
                let mut rgba = [0u8; 4];
                gl.read_pixels(
                    0,
                    0,
                    1,
                    1,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelPackData::Slice(Some(&mut rgba)),
                );
                assert_eq!(gl.get_error(), glow::NO_ERROR);
                gl.delete_vertex_array(vao);
                rgba
            }
        }
        for webgl in [false, true] {
            let (scope, gl) = crate::backend::gl::readback_test_gl::native_angle_context(3, webgl);
            assert_eq!(
                gl.supported_extensions()
                    .contains("GL_ANGLE_webgl_compatibility"),
                webgl
            );
            let dir = make_test_dir(if webgl {
                "native_webgl"
            } else {
                "native_ordinary"
            });
            let cache = ShaderCache::new(&gl, &dir);
            assert!(cache.is_supported(), "driver provides program binaries");
            let vs = "#version 300 es\nconst vec2 p[3] = vec2[3](vec2(-1.0,-1.0),vec2(3.0,-1.0),vec2(-1.0,3.0)); void main() { gl_Position = vec4(p[gl_VertexID],0.0,1.0); }";
            let fs = "#version 300 es\nprecision highp float; uniform vec4 tint; out vec4 color; void main() { color = tint; }";
            let descriptor = LinkDescriptor {
                vertex_src: vs,
                fragment_src: fs,
                attrib_key: "",
                tf_varyings: &[],
                tf_buffer_mode: None,
            };
            let compile = |kind, source: &str| unsafe {
                let shader = gl.create_shader(kind).unwrap();
                gl.shader_source(shader, source);
                gl.compile_shader(shader);
                assert!(
                    gl.get_shader_compile_status(shader),
                    "{}",
                    gl.get_shader_info_log(shader)
                );
                shader
            };
            let vertex = compile(glow::VERTEX_SHADER, vs);
            let fragment = compile(glow::FRAGMENT_SHADER, fs);
            let source_program = unsafe { gl.create_program().unwrap() };
            unsafe {
                gl.attach_shader(source_program, vertex);
                gl.attach_shader(source_program, fragment);
                type Hint = unsafe extern "system" fn(u32, u32, i32);
                let hint: Hint = std::mem::transmute(
                    scope
                        .api
                        .get_proc_address("glProgramParameteri")
                        .expect("binary hint"),
                );
                hint(source_program.0.get(), 0x8257, 1);
                gl.link_program(source_program);
                assert!(
                    gl.get_program_link_status(source_program),
                    "{}",
                    gl.get_program_info_log(source_program)
                );
            }
            assert_eq!(pixel(&gl, source_program), [0, 128, 0, 255]);
            let (format, binary) =
                get_program_binary(&gl, source_program).expect("retrievable bounded binary");
            let renderer = unsafe { gl.get_parameter_string(glow::RENDERER) };
            let version = unsafe { gl.get_parameter_string(glow::VERSION) };
            let legacy_key = format!("{renderer}||{version}");
            let legacy_path = cache
                .cache_dir
                .join(make_cache_key(&descriptor, &legacy_key));
            let legacy_data = [format.to_le_bytes().as_slice(), binary.as_slice()].concat();
            std::fs::write(&legacy_path, legacy_data).unwrap();
            assert!(
                cache.load(&descriptor).is_none(),
                "unqualified legacy binary must not be restored"
            );
            let opposite_path = cache.cache_dir.join(make_cache_key(
                &descriptor,
                &driver_key(&renderer, &version, !webgl),
            ));
            std::fs::write(
                &opposite_path,
                [format.to_le_bytes().as_slice(), binary.as_slice()].concat(),
            )
            .unwrap();
            assert!(
                cache.load(&descriptor).is_none(),
                "binary for the opposite validation mode must miss"
            );
            // The existing startup marker migrates old unqualified caches too.
            std::fs::write(cache.cache_dir.join(".driver_key"), &legacy_key).unwrap();
            drop(cache);
            let cache = ShaderCache::new(&gl, &dir);
            assert!(!legacy_path.exists(), "legacy marker clears stale entries");

            cache.save(&gl, source_program, &descriptor);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let (loaded_format, loaded_binary) = loop {
                if let Some(entry) = cache.load(&descriptor) {
                    break entry;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "async binary persistence timed out"
                );
                std::thread::sleep(std::time::Duration::from_millis(1));
            };
            assert_eq!(loaded_format, format);
            assert_eq!(loaded_binary, binary);
            let restored = unsafe { gl.create_program().unwrap() };
            unsafe {
                // Production keeps the source shaders attached for binary refusal fallback.
                gl.attach_shader(restored, vertex);
                gl.attach_shader(restored, fragment);
                gl.program_binary(
                    restored,
                    &glow::ProgramBinary {
                        format: loaded_format,
                        buffer: loaded_binary,
                    },
                );
                assert!(
                    gl.get_program_link_status(restored),
                    "{}",
                    gl.get_program_info_log(restored)
                );
                let tint = gl.get_uniform_location(restored, "tint").unwrap();
                let mut value = [1.0; 4];
                gl.get_uniform_f32(restored, &tint, &mut value);
                assert_eq!(value, [0.0; 4], "binary load resets uniforms");
            }
            assert_eq!(pixel(&gl, restored), [0, 128, 0, 255]);
            // A cached blob can be unreadable even under the right identity.
            std::fs::write(
                cache
                    .cache_dir
                    .join(make_cache_key(&descriptor, &cache.driver_key)),
                [format.to_le_bytes().as_slice(), &binary[..8]].concat(),
            )
            .unwrap();
            let (bad_format, bad_binary) =
                cache.load(&descriptor).expect("read truncated cache entry");
            unsafe {
                gl.program_binary(
                    restored,
                    &glow::ProgramBinary {
                        format: bad_format,
                        buffer: bad_binary,
                    },
                );
                assert!(
                    !gl.get_program_link_status(restored),
                    "truncated binary must fail link"
                );
                eprintln!(
                    "webgl={webgl} truncated binary error=0x{:x}",
                    gl.get_error()
                );
            }
            cache.refused(&descriptor);
            assert!(
                cache.load(&descriptor).is_none(),
                "refused binary removed before source fallback"
            );
            unsafe {
                gl.link_program(restored);
                assert!(
                    gl.get_program_link_status(restored),
                    "{}",
                    gl.get_program_info_log(restored)
                );
            }
            assert_eq!(pixel(&gl, restored), [0, 128, 0, 255]);
            eprintln!(
                "PASS webgl={webgl}: source / persisted binary / refusal fallback; {} bytes",
                binary.len()
            );
            unsafe {
                gl.delete_program(source_program);
                gl.delete_program(restored);
                gl.delete_shader(vertex);
                gl.delete_shader(fragment);
            }
            drop(cache);
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}
