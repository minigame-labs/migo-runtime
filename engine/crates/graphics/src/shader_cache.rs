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
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
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

/// Manages a disk-backed shader binary cache.
pub struct ShaderCache {
    /// Base directory for cached binaries (e.g. `<app_cache>/migo_shader_cache/`).
    cache_dir: PathBuf,
    /// Fingerprint of the current GL driver (GL_RENDERER + GL_VERSION).
    /// It participates in every key as well as the directory marker so a
    /// stale file can never be accepted after a driver/version change.
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
        let driver_key = format!("{renderer}||{version}");
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
                debug!("Shader cache: driver changed, clearing cache");
                clear_dir(&cache_dir);
                std::fs::write(&key_path, &driver_key).ok();
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
        let mut data = std::fs::read(&path).ok()?;
        if data.len() < 4 {
            return None;
        }
        let format = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        data.drain(..4);
        trace!("Shader cache hit: {key}");
        Some((format, data))
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
        let tmp = path.with_extension("tmp");
        let result = std::fs::write(&tmp, &data).and_then(|()| std::fs::rename(&tmp, &path));
        if let Err(error) = result {
            debug!("Shader cache write failed: {error}");
            let _ = std::fs::remove_file(&tmp);
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
/// `get_program_binary` wrapper.
fn get_program_binary(gl: &glow::Context, program: glow::NativeProgram) -> Option<(u32, Vec<u8>)> {
    unsafe {
        let length = gl.get_program_parameter_i32(program, glow::PROGRAM_BINARY_LENGTH);
        if length <= 0 {
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

    // ---- R7: cache key completeness ----

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

    // ---- P2-PERF5: async save, torn-file safety, limits ----

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
    fn persistence_uses_atomic_temp_rename() {
        const SOURCE: &str = include_str!("shader_cache.rs");
        let start = SOURCE
            .find("fn persist_loop")
            .expect("persistence worker exists");
        let end = SOURCE[start..]
            .find("fn enforce_cache_limits")
            .map(|offset| start + offset)
            .expect("cache limits follow persistence worker");
        let worker = &SOURCE[start..end];
        assert!(worker.contains("with_extension(\"tmp\")"));
        assert!(worker.contains("std::fs::rename"));
    }
}
