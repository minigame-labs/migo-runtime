use std::{borrow::Cow, cell::RefCell, fs, future::Future, pin::Pin, rc::Rc, sync::Arc};

use deno_core::{
    FsModuleLoader, ModuleLoadOptions, ModuleLoadReferrer, ModuleLoadResponse, ModuleLoader,
    ModuleSource, ModuleSourceCode, ModuleSpecifier, ModuleType, RequestedModuleType,
    ResolutionKind, SourceCodeCacheInfo, error::ModuleLoaderError,
};

use migo_io::{
    scheduler::IoScheduler,
    task::{BackendKind, IoRequest, RequestKind},
};
use shared::protocol::io_cmd::MAX_READ_LENGTH;

use shared::vfs::MountTable;

use crate::code_cache::SharedCodeCache;

/// Shared reference to the mount table, updated after `evaluate_module`
/// creates it. The module loader holds a clone and checks it on every
/// resolve/load for sandbox enforcement.
pub(crate) type SharedMountTableRef = Rc<RefCell<Option<Arc<MountTable>>>>;

pub(crate) fn validate_content_module_url(
    url: &ModuleSpecifier,
    mount_table: Option<&MountTable>,
    loader_name: &str,
) -> Result<(), ModuleLoaderError> {
    let Some(mount_table) = mount_table else {
        return Err(ModuleLoaderError::generic(format!(
            "{loader_name} module load blocked: no /code mount table (sandbox unavailable)"
        )));
    };
    if url.scheme() != "file" {
        return Err(ModuleLoaderError::generic(format!(
            "{loader_name} module import blocked: game content may load only file URLs"
        )));
    }
    let path = url.to_file_path().map_err(|_| {
        ModuleLoaderError::generic(format!(
            "{loader_name} module import blocked: invalid file URL"
        ))
    })?;
    if let Some((relative, generation)) = synthetic_pack_path(&path, mount_table)? {
        mount_table
            .pin_pack_read(&relative, generation)
            .map_err(|error| {
                ModuleLoaderError::generic(format!("{loader_name} module import blocked: {error}"))
            })?;
        return Ok(());
    }
    if mount_table.is_allowed_path(&path) {
        return Ok(());
    }

    Err(ModuleLoaderError::generic(format!(
        "{loader_name} module import blocked: path escapes /code sandbox: {}",
        path.display()
    )))
}

/// Decode only the engine's reserved synthetic package path. The generation
/// is an identity check, not permission to bypass the mount's own path rules.
fn synthetic_pack_path(
    path: &std::path::Path,
    mounts: &MountTable,
) -> Result<Option<(String, u64)>, ModuleLoaderError> {
    let Ok(relative) = path.strip_prefix(mounts.code_dir().join(".pack_gen")) else {
        return Ok(None);
    };
    let mut components = relative.components();
    let invalid = || ModuleLoaderError::generic("Invalid synthetic package module path");
    let generation = components
        .next()
        .and_then(|value| value.as_os_str().to_str())
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value != 0)
        .ok_or_else(invalid)?;
    let mut logical = std::path::PathBuf::new();
    for component in components {
        if !matches!(component, std::path::Component::Normal(_)) {
            return Err(invalid());
        }
        logical.push(component);
    }
    let logical = logical
        .to_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(invalid)?
        .to_owned();
    Ok(Some((logical, generation)))
}

/// Module type and source construction stay separate from name resolution.
/// Core owns JSON parsing, Wasm compilation and import-attribute matching.
fn content_module_type(
    url: &ModuleSpecifier,
    requested: &RequestedModuleType,
) -> Result<ModuleType, ModuleLoaderError> {
    let path = url
        .to_file_path()
        .map_err(|_| ModuleLoaderError::type_error("Content modules require a file URL"))?;
    let extension = path.extension().and_then(|ext| ext.to_str()).unwrap_or("");
    let kind = if extension.eq_ignore_ascii_case("json") {
        ModuleType::Json
    } else if extension.eq_ignore_ascii_case("wasm") {
        ModuleType::Wasm
    } else {
        ModuleType::JavaScript
    };
    match requested {
        RequestedModuleType::Json if kind == ModuleType::Json => {}
        RequestedModuleType::Json => {
            return Err(ModuleLoaderError::type_error("Expected a JSON module"));
        }
        RequestedModuleType::None if kind != ModuleType::Json => {}
        RequestedModuleType::None => {
            return Err(ModuleLoaderError::type_error(
                "JSON modules require the type: json import attribute",
            ));
        }
        _ => {
            return Err(ModuleLoaderError::type_error(
                "Unsupported content module import attribute",
            ));
        }
    }
    Ok(kind)
}

fn module_source(
    url: &ModuleSpecifier,
    bytes: Vec<u8>,
    kind: ModuleType,
) -> Result<ModuleSource, ModuleLoaderError> {
    let code = if kind == ModuleType::Wasm {
        ModuleSourceCode::Bytes(bytes.into_boxed_slice().into())
    } else {
        ModuleSourceCode::String(
            String::from_utf8(bytes)
                .map_err(|error| {
                    ModuleLoaderError::type_error(format!("Module source is not UTF-8: {error}"))
                })?
                .into(),
        )
    };
    Ok(ModuleSource::new(kind, code, url, None))
}

pub(crate) struct MyModuleLoader {
    inner: FsModuleLoader,
    code_cache: Option<SharedCodeCache>,
    /// Set after evaluate_module creates the mount table.
    mount_table: SharedMountTableRef,
    io_scheduler: Arc<IoScheduler>,
}

impl MyModuleLoader {
    pub fn new(code_cache: Option<SharedCodeCache>, mount_table: SharedMountTableRef) -> Self {
        Self {
            inner: FsModuleLoader,
            code_cache,
            mount_table,
            io_scheduler: Arc::new(IoScheduler::new(0)),
        }
    }
}

impl MyModuleLoader {
    /// Attach code cache info to a loaded module source.
    fn attach_code_cache(cache: &SharedCodeCache, mut source: ModuleSource) -> ModuleSource {
        if source.module_type != ModuleType::JavaScript {
            return source;
        }
        let hash = cache.compute_hash(source.code.as_bytes());
        let data = cache.get(hash).map(Cow::Owned);
        source.code_cache = Some(SourceCodeCacheInfo { hash, data });
        source
    }

    #[inline]
    fn normalize_specifier<'a>(&self, specifier: &'a str, kind: &ResolutionKind) -> Cow<'a, str> {
        if *kind != ResolutionKind::MainModule
            && !specifier.starts_with("./")
            && !specifier.starts_with("../")
            && !specifier.starts_with(['/', '?', '#'])
            && !specifier.contains(':')
        {
            Cow::Owned(format!("./{specifier}"))
        } else {
            Cow::Borrowed(specifier)
        }
    }

    #[inline]
    fn patch_amd(mut source: ModuleSource) -> Result<ModuleSource, ModuleLoaderError> {
        if source.module_type != ModuleType::JavaScript {
            return Ok(source);
        }
        let code = String::from_utf8_lossy(source.code.as_bytes());
        // The loader's one rewrite, shared with the external session's content
        // origin so both evaluate the same module text.
        if let Some(patched) = shared::cjs_compat::module_source(&code) {
            source.code = ModuleSourceCode::String(patched.into());
        }
        Ok(source)
    }

    /// Validate that a resolved module URL is within the /code sandbox.
    ///
    /// Returns `Ok(())` if the path is allowed, or an error if it escapes
    /// the mount boundaries.
    fn validate_sandbox(&self, url: &ModuleSpecifier) -> Result<(), ModuleLoaderError> {
        let mt_ref = self.mount_table.borrow();
        validate_content_module_url(url, mt_ref.as_deref(), "Main-thread content")
    }
}

/// How a module is read from a real file: what the read is charged, and the most it may produce.
///
/// Charged at `MAX_READ_LENGTH` (100 MiB) whatever the module, two reads spent most of the
/// scheduler's pending-byte budget, and deno_core reads a module's static imports concurrently:
/// an entry importing four modules failed to load with `ByteLimitExceeded` whenever a third read
/// overlapped the first two -- one launch in three on the Linux player. So a module is charged its
/// size, and its read produces no more than it was charged: a module that grew after it was sized
/// is refused rather than read past its charge. One that cannot be sized keeps the cautious
/// estimate, as any read nobody can bound does, and its read says why it failed.
///
/// Only a regular file is a module. A FIFO named like one sizes at zero, which the scheduler's
/// small-read rule runs inline, and its read would block the host thread with nothing to end it.
struct ModuleRead {
    request: IoRequest,
    limit: u64,
}

fn module_read(path: &std::path::Path) -> Result<ModuleRead, String> {
    let limit = match fs::metadata(path) {
        Ok(meta) if !meta.is_file() => {
            return Err(format!("{} is not a regular file", path.display()));
        }
        Ok(meta) if meta.len() > MAX_READ_LENGTH => {
            return Err(format!(
                "{} exceeds module read limit {MAX_READ_LENGTH}",
                path.display()
            ));
        }
        Ok(meta) => meta.len(),
        Err(_) => MAX_READ_LENGTH,
    };
    Ok(ModuleRead {
        request: migo_services::fs::read_request(
            BackendKind::Filesystem,
            RequestKind::Async,
            None,
            Some(limit),
        ),
        limit,
    })
}

fn read_module(path: &std::path::Path, limit: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read;

    let file = fs::File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", path.display()),
        ));
    }
    if metadata.len() > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "module exceeds its admitted read charge",
        ));
    }
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "{} grew past the {limit} bytes it was sized at",
                path.display()
            ),
        ));
    }
    Ok(bytes)
}

impl MyModuleLoader {
    fn synthetic_pack_path(
        code_dir: &std::path::Path,
        generation: u64,
        relative: &std::path::Path,
    ) -> std::path::PathBuf {
        code_dir
            .join(".pack_gen")
            .join(generation.to_string())
            .join(relative)
    }

    /// Try loading a module from the mount table (pack backend support).
    ///
    /// Returns `Some(ModuleSource)` if the file was read from a pack-backed
    /// mount (where `real_path()` returns `None`).  Returns `None` if the
    /// path is filesystem-backed or the mount table isn't available, allowing
    /// fallback to `FsModuleLoader`.
    fn try_load_from_mount(
        &self,
        url: &ModuleSpecifier,
        module_type: ModuleType,
    ) -> Option<ModuleLoadResponse> {
        let mt_ref = self.mount_table.borrow();
        let mt = mt_ref.as_ref()?;
        let file_path = url.to_file_path().ok()?;
        let synthetic = match synthetic_pack_path(&file_path, mt) {
            Ok(value) => value,
            Err(error) => return Some(ModuleLoadResponse::Sync(Err(error))),
        };
        let relative = match &synthetic {
            Some((logical, _)) => std::path::PathBuf::from(logical),
            None => file_path.strip_prefix(mt.code_dir()).ok()?.to_path_buf(),
        };
        let relative_str = relative.to_str()?;

        // Check if this path resolves through a pack backend.
        let resolved = match mt.resolve(relative_str) {
            Some(resolved) => resolved,
            None if mt.has_overlay_for(relative_str) => {
                return Some(ModuleLoadResponse::Sync(Err(ModuleLoaderError::generic(
                    format!(
                        "Module import blocked by mounted overlay shadow: {}",
                        relative_str,
                    ),
                ))));
            }
            None => return None,
        };
        if resolved.real_path.is_some() {
            // Filesystem-backed: let FsModuleLoader handle it.
            return None;
        }

        let generation = synthetic
            .as_ref()
            .map_or(resolved.mount_generation, |(_, generation)| *generation);
        let pinned = match mt.pin_pack_read(relative_str, generation) {
            Ok(pinned) => pinned,
            Err(error) => {
                return Some(ModuleLoadResponse::Sync(Err(ModuleLoaderError::generic(
                    error.to_string(),
                ))));
            }
        };
        let limit = pinned
            .entry_size()
            .unwrap_or(MAX_READ_LENGTH)
            .min(MAX_READ_LENGTH);
        let request = migo_services::fs::read_request(
            BackendKind::Pack,
            RequestKind::Async,
            None,
            Some(limit),
        );
        let scheduler = Arc::clone(&self.io_scheduler);
        let cache = self.code_cache.clone();
        let url = url.clone();
        Some(ModuleLoadResponse::Async(Box::pin(async move {
            let bytes = scheduler
                .run_async(request, move || pinned.read_limited(limit))
                .await
                .map_err(|error| {
                    ModuleLoaderError::generic(format!("package module IO: {error:?}"))
                })?
                .map_err(|error| {
                    ModuleLoaderError::generic(format!("package module read: {error}"))
                })?;
            let source = Self::patch_amd(module_source(&url, bytes, module_type)?)?;
            Ok(match &cache {
                Some(cache) => Self::attach_code_cache(cache, source),
                None => source,
            })
        })))
    }
    fn load_from_filesystem(
        &self,
        module_specifier: &ModuleSpecifier,
        module_type: ModuleType,
    ) -> ModuleLoadResponse {
        let Ok(path) = module_specifier.to_file_path() else {
            return ModuleLoadResponse::Sync(Err(ModuleLoaderError::generic(
                "filesystem module URL is not a file URL",
            )));
        };
        let scheduler = Arc::clone(&self.io_scheduler);
        let url = module_specifier.clone();
        let read = match module_read(&path) {
            Ok(read) => read,
            Err(error) => {
                return ModuleLoadResponse::Sync(Err(ModuleLoaderError::generic(format!(
                    "failed to read module: {error}"
                ))));
            }
        };
        let cache = self.code_cache.clone();
        ModuleLoadResponse::Async(Box::pin(async move {
            let limit = read.limit;
            let bytes = scheduler
                .run_async(read.request, move || read_module(&path, limit))
                .await
                .map_err(|error| {
                    ModuleLoaderError::generic(format!("failed to read module: {error:?}"))
                })?
                .map_err(|error| {
                    ModuleLoaderError::generic(format!("failed to read module: {error}"))
                })?;
            let source = module_source(&url, bytes, module_type)?;
            let source = Self::patch_amd(source)?;
            Ok(match &cache {
                Some(cache) => Self::attach_code_cache(cache, source),
                None => source,
            })
        }))
    }
}

impl ModuleLoader for MyModuleLoader {
    fn resolve(
        &self,
        specifier: &str,
        referrer: &str,
        kind: ResolutionKind,
    ) -> Result<ModuleSpecifier, ModuleLoaderError> {
        let spec = self.normalize_specifier(specifier, &kind);
        let mut url = self.inner.resolve(spec.as_ref(), referrer, kind)?;
        // Resolve before inferring the legacy .js suffix: decoded filenames
        // distinguish an explicit extension (including %2Ejson) from a bare
        // module. Query/fragment never participate in extension detection.
        if url.scheme() == "file" && !url.path().ends_with('/') {
            if let Ok(path) = url.to_file_path() {
                if path
                    .file_name()
                    .is_some_and(|name| !name.to_string_lossy().contains('.'))
                {
                    url.set_path(&format!("{}.js", url.path()));
                }
            }
        }
        let query = url.query().map(str::to_owned);
        let fragment = url.fragment().map(str::to_owned);

        if let Some(mt) = self.mount_table.borrow().as_ref() {
            if let Ok(file_path) = url.to_file_path() {
                let code_dir = mt.code_dir();
                if let Ok(relative) = file_path.strip_prefix(&code_dir) {
                    if let Some(relative_str) = relative.to_str() {
                        match mt.resolve(relative_str) {
                            Some(resolved) => {
                                if let Some(real_path) = resolved.real_path {
                                    url = ModuleSpecifier::from_file_path(real_path).map_err(
                                        |_| {
                                            ModuleLoaderError::generic(
                                                "failed to remap module path",
                                            )
                                        },
                                    )?;
                                } else {
                                    let synthetic = Self::synthetic_pack_path(
                                        &code_dir,
                                        resolved.mount_generation,
                                        relative,
                                    );
                                    url = ModuleSpecifier::from_file_path(synthetic).map_err(
                                        |_| {
                                            ModuleLoaderError::generic(
                                                "failed to synthesize pack module path",
                                            )
                                        },
                                    )?;
                                }
                            }
                            None if mt.has_overlay_for(relative_str) => {
                                return Err(ModuleLoaderError::generic(format!(
                                    "Module import blocked by mounted overlay shadow: {}",
                                    relative_str,
                                )));
                            }
                            None => {}
                        }
                    }
                }
            }
        }

        // Remapping a mounted generation changes only the physical path.
        // Keep module identity distinct for query/fragment variants.
        url.set_query(query.as_deref());
        url.set_fragment(fragment.as_deref());
        // Sandbox enforcement: resolved path must be within mount boundaries.
        self.validate_sandbox(&url)?;

        Ok(url)
    }

    fn load(
        &self,
        module_specifier: &ModuleSpecifier,
        maybe_referrer: Option<&ModuleLoadReferrer>,
        options: ModuleLoadOptions,
    ) -> ModuleLoadResponse {
        // Defense in depth: validate sandbox on load too.
        if let Err(e) = self.validate_sandbox(module_specifier) {
            return ModuleLoadResponse::Sync(Err(e));
        }

        let module_type =
            match content_module_type(module_specifier, &options.requested_module_type) {
                Ok(kind) => kind,
                Err(error) => return ModuleLoadResponse::Sync(Err(error)),
            };

        // Try pack backend first.  If the module is in a package file,
        // read it directly without touching the filesystem.
        if let Some(response) = self.try_load_from_mount(module_specifier, module_type.clone()) {
            return response;
        }

        // Filesystem fallback (directory-backed mounts) is always admitted to
        // the bounded IO executor. In particular, a small module must not
        // silently reintroduce a synchronous read on the isolate path.
        let _ = maybe_referrer;
        self.load_from_filesystem(module_specifier, module_type)
    }

    fn prepare_load(
        &self,
        module_specifier: &ModuleSpecifier,
        maybe_referrer: Option<String>,
        maybe_content: Option<String>,
        options: ModuleLoadOptions,
    ) -> Pin<Box<dyn Future<Output = Result<(), ModuleLoaderError>>>> {
        self.inner
            .prepare_load(module_specifier, maybe_referrer, maybe_content, options)
    }

    fn finish_load(&self) {}

    fn code_cache_ready(
        &self,
        _module_specifier: ModuleSpecifier,
        hash: u64,
        code_cache: &[u8],
    ) -> Pin<Box<dyn Future<Output = ()>>> {
        if let Some(cache) = &self.code_cache {
            cache.set(hash, code_cache);
        }
        Box::pin(std::future::ready(()))
    }

    fn purge_and_prevent_code_cache(&self, _module_specifier: &str) {
        // No per-specifier tracking; stale entries are evicted by hash mismatch
        // or LRU eviction.
    }
}

#[cfg(test)]
#[path = "loader_module_tests.rs"]
mod module_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn loader(mount_table: Option<Arc<MountTable>>) -> MyModuleLoader {
        MyModuleLoader::new(None, Rc::new(RefCell::new(mount_table)))
    }

    #[test]
    fn a_module_read_is_charged_what_the_module_is_and_reads_no_more() {
        let dir = std::env::temp_dir().join(format!("migo-module-read-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let module = dir.join("a.js");
        std::fs::write(&module, vec![b' '; 1234]).unwrap();

        let read = module_read(&module).expect("a regular file is a module");
        let IoRequest::ReadFile {
            estimated_bytes,
            priority,
            ..
        } = read.request
        else {
            panic!("a read request");
        };
        assert_eq!((estimated_bytes, read.limit), (1234, 1234));
        assert_eq!(priority, migo_io::task::PriorityClass::ForegroundAsync);
        assert_eq!(read_module(&module, read.limit).unwrap().len(), 1234);

        // Grown after it was sized: refused, not read past the charge.
        std::fs::write(&module, vec![b' '; 5000]).unwrap();
        let grown = read_module(&module, read.limit).expect_err("a module larger than its charge");
        assert_eq!(grown.kind(), std::io::ErrorKind::InvalidData);

        // Not a regular file: refused before anything is read.
        let directory = dir.join("dir.js");
        std::fs::create_dir_all(&directory).unwrap();
        let refused = module_read(&directory)
            .err()
            .expect("a directory is not a module");
        assert!(refused.contains("not a regular file"), "{refused}");

        // Unsizable: the cautious estimate, and the read says why.
        let missing = module_read(&dir.join("missing.js")).expect("charged the maximum");
        assert_eq!(missing.limit, MAX_READ_LENGTH);
        assert_eq!(
            read_module(&dir.join("missing.js"), missing.limit)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::NotFound
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_named_like_a_module_is_refused_without_reading_it() {
        let dir = std::env::temp_dir().join(format!("migo-module-fifo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fifo = dir.join("dep.js");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("mkfifo");
        assert!(made.success());
        let refused = module_read(&fifo).err().expect("a FIFO is not a module");
        assert!(refused.contains("not a regular file"), "{refused}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn content_loader_rejects_internal_schemes_for_all_import_kinds() {
        let root = std::env::temp_dir().join("migo-main-loader-sandbox");
        let loader = loader(Some(Arc::new(MountTable::new(root))));
        let referrer = "file:///tmp/migo-main-loader-sandbox/main.js";

        for kind in [ResolutionKind::Import, ResolutionKind::DynamicImport] {
            let error = loader
                .resolve("ext:core/mod.js", referrer, kind)
                .expect_err("game content must not resolve runtime extension modules");
            assert!(
                error.to_string().contains("file"),
                "unexpected rejection: {error}"
            );
        }
    }

    #[test]
    fn content_loader_fails_closed_without_a_mount_table() {
        let loader = loader(None);
        let error = loader
            .resolve(
                "./dependency.js",
                "file:///tmp/migo-main-loader-sandbox/main.js",
                ResolutionKind::Import,
            )
            .expect_err("game content must not load before its sandbox exists");
        assert!(
            error.to_string().contains("mount table"),
            "unexpected rejection: {error}"
        );
    }

    // The scheduler runs a foreground read inline when it is small enough that a pool hop costs
    // more than the read, and on its executor otherwise; a module read is held to the same rule by
    // its size. It went to the executor whatever its size only while every module was charged the
    // 100 MiB maximum.
    #[test]
    fn filesystem_module_reads_go_through_the_io_scheduler_by_their_size() {
        let root = std::env::temp_dir().join(format!("migo-main-loader-io-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("temporary module root");
        let mount_table = Arc::new(MountTable::new(root.clone()));
        let loader = loader(Some(mount_table));
        let cheap = loader.io_scheduler.policy().small_read_bytes as usize;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let load = |name: &str, padding: usize| {
            let path = root.join(name);
            std::fs::write(
                &path,
                format!("export const answer = 42;\n//{}", " ".repeat(padding)),
            )
            .expect("temporary module");
            let url = ModuleSpecifier::from_file_path(&path).expect("module URL");
            let ModuleLoadResponse::Async(future) =
                loader.load_from_filesystem(&url, ModuleType::JavaScript)
            else {
                panic!("filesystem module load must not execute inline");
            };
            let source = runtime.block_on(future).expect("module load");
            assert!(source.code.as_bytes().starts_with(b"export const answer"));
            let metrics = loader.io_scheduler.metrics();
            (metrics.inline_runs, metrics.delegated_runs)
        };

        assert_eq!(load("small.js", 0), (1, 0), "a small module is read inline");
        assert_eq!(
            load("large.js", cheap),
            (1, 1),
            "a large one on the bounded IO executor"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
