use std::{cell::RefCell, rc::Rc};

use deno_core::{OpState, extension, op2};
use deno_error::JsErrorBox;
use tracing::{info, warn};

use shared::{
    error::{EngineError, EngineResult, ErrorCode},
    op_state::{CanvasOpState, HostOpState},
    protocol::{
        render_cmd::{CanvasCmd, RenderCommand},
        send_render_with_resp_async,
    },
    vfs::FileOp,
};

use crate::io_state::IoSchedulerState;

pub(crate) mod cache;
mod inline_src;

const OP_LOAD_IMAGE: &str = "canvas load image";

/// This isolate's handle on its Session's image alias table.
///
/// Resolved **once**, by the extension state initializer below, and never again:
/// finding it means reading a registry shared with every other Session, so doing
/// that per op would put a cross-session lock on the `texImage2D` frame path —
/// the trap Section 7.3 names, and the reason the text texture cache is wired
/// the same way.  WebGL's upload ops read this handle too, which is why it lives
/// in op state rather than being a private detail of the ops below.
///
/// Carries the Session id beside the handle so the decoded-bytes cache below can
/// be told who is asking without a second op-state lookup on the frame path.
pub(crate) struct ImageCacheState {
    pub(crate) aliases: cache::SharedImageCache,
    pub(crate) session: i32,
}

/// A pin held on the decoded-bytes cache for a load that has not settled yet.
///
/// Releases on drop, and that is the whole point rather than a convenience. Two
/// `.await` points sit between taking this pin and the load settling — the
/// decode and the GPU upload — and a Session torn down or restarted while one is
/// in flight has its op future *dropped*, not resumed. Unpinning on each
/// explicit exit path cannot cover that, and the pin left behind is permanent:
/// it makes the entry immune to eviction, to trim and to `clear`, and the
/// decoded-bytes cache is deliberately not cleared on teardown, so nothing later
/// reclaims it.
struct PrePin(migo_io::image_cache::ImageCacheKey);

impl PrePin {
    /// Pin `key` before the decode inserts bytes under it.
    ///
    /// Without this a cold WebGL image can be rejected by the W-TinyLFU
    /// admission filter before the load has a chance to pin it as a live alias;
    /// `texImage2D(image)` then misses one frame later even though the `Image`
    /// object is alive. Taken unconditionally, because the WebGL flag is
    /// monotonic and sampled again by the decode worker, so a request queued
    /// before WebGL creation but started afterwards must not have its
    /// newly-required RGBA backing rejected.
    fn take(key: migo_io::image_cache::ImageCacheKey) -> Self {
        migo_io::global_cache().pin(&key);
        Self(key)
    }

    #[inline]
    fn key(&self) -> &migo_io::image_cache::ImageCacheKey {
        &self.0
    }
}

impl Drop for PrePin {
    fn drop(&mut self) {
        migo_io::global_cache().unpin(&self.0);
    }
}

#[inline]
fn js_err_from_engine(e: EngineError) -> JsErrorBox {
    match &e.detail {
        Some(d) => JsErrorBox::generic(format!("[{:?}] {} ({})", e.code, e.msg, d)),
        None => JsErrorBox::generic(format!("[{:?}] {}", e.code, e.msg)),
    }
}

#[inline]
fn engine_err_to_text(e: &EngineError) -> String {
    match &e.detail {
        Some(d) => format!("[{:?}] {} ({})", e.code, e.msg, d),
        None => format!("[{:?}] {}", e.code, e.msg),
    }
}

#[inline]
fn data_url_cache_identity(src: &str) -> String {
    format!(
        "data:sha256:{}",
        shared::vfs::integrity::sha256_bytes(src.as_bytes())
    )
}

#[inline]
fn ahb_image_decode_allowed(
    gpu_caps: &shared::device::gpu_caps::GpuCaps,
    cpu_backing_required: &std::sync::atomic::AtomicBool,
) -> bool {
    !cpu_backing_required.load(std::sync::atomic::Ordering::Acquire) && gpu_caps.snapshot().ahb
}

/// Resolved image source: real path + version identity (for cache keying).
struct ResolvedSrc {
    path: String,
    /// Source version for cache invalidation.
    /// For mount-backed: mount source_mounted_at.
    /// For mutable filesystem paths: derived from file mtime+size.
    source_version: u64,
    source: migo_io::image_ops::ImageSource,
}

use shared::protocol::io_cmd::{VARIANT_EXTENSIONS, path_stem};

/// Compute a cache/version token for a filesystem-backed image source and all
/// of its known sibling variants.  Uses metadata only (mtime + size) — never
/// reads file content — so it is safe to call on the event loop thread.
fn variant_source_version_token(
    path: &str,
    virtual_src: Option<&str>,
    mount_table: Option<&shared::vfs::MountTable>,
) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut h = DefaultHasher::new();
    let stem = path_stem(path);
    let virtual_stem = virtual_src.map(path_stem);

    let mut candidates: Vec<(String, Option<String>)> =
        Vec::with_capacity(VARIANT_EXTENSIONS.len() + 1);
    candidates.push((path.to_string(), virtual_src.map(|s| s.to_string())));
    for ext in VARIANT_EXTENSIONS {
        let candidate = format!("{}.{}", stem, ext);
        if candidate != path {
            let virtual_candidate = virtual_stem
                .as_ref()
                .map(|vstem| format!("{}.{}", vstem, ext));
            candidates.push((candidate, virtual_candidate));
        }
    }

    for (candidate, virtual_candidate) in candidates {
        candidate.hash(&mut h);
        // Metadata-only versioning: (exists, size, mtime).
        // Never reads file content — keeps this function cheap enough for
        // the event loop thread.
        match std::fs::metadata(&candidate) {
            Ok(meta) => {
                1u8.hash(&mut h);
                meta.len().hash(&mut h);
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                mtime.hash(&mut h);
            }
            Err(_) => {
                0u8.hash(&mut h);
            }
        }
        if let (Some(mt), Some(virtual_candidate)) = (mount_table, virtual_candidate) {
            virtual_candidate.hash(&mut h);
            if let Some(resolved) = mt.resolve_code_path(&virtual_candidate) {
                1u8.hash(&mut h);
                resolved.source_mounted_at.hash(&mut h);
            } else {
                0u8.hash(&mut h);
            }
        }
    }
    h.finish()
}

fn resized_rgba_io_cache_key(
    src: &str,
    target_width: Option<u32>,
    target_height: Option<u32>,
    source_generation: u64,
) -> cache::ImageCacheKey {
    cache::make_cache_key(src, target_width, target_height, source_generation)
}

fn resolve_local_src(
    vfs: Option<&shared::vfs::VirtualFS>,
    mount_table: Option<&shared::vfs::MountTable>,
    src: &str,
) -> EngineResult<ResolvedSrc> {
    // Note: `http(s)://` and `data:` URLs are NOT rejected here any
    // more.  They're handled earlier in `op_load_image_inner` through
    // the dedicated inline-source path; this helper only resolves
    // local/VFS paths (the `/code`, `/user`, `/cache`, `/tmp` roots
    // listed below).

    // Relative path → normalize to /code/{src} and fall into the /code branch.
    // This ensures "a.png" and "/code/a.png" always take the same path.
    let owned_vpath;
    let effective_src = if !src.starts_with('/') {
        owned_vpath = format!("/code/{}", src);
        owned_vpath.as_str()
    } else {
        src
    };

    // /code paths → resolve through mount table (preferred) or VFS fallback.
    if effective_src == "/code" || effective_src.starts_with("/code/") {
        if let Some(mt) = mount_table {
            let resolved = mt.resolve_code_path(effective_src).ok_or_else(|| {
                EngineError::new(ErrorCode::PermissionDenied)
                    .with_msg("image path resolve failed")
                    .with_detail(format!(
                        "src={}, mount resolve_code_path returned None",
                        src
                    ))
            })?;
            match resolved.real_path {
                Some(real) => {
                    let relative = effective_src.strip_prefix("/code/").unwrap_or("");
                    return Ok(ResolvedSrc {
                        source_version: variant_source_version_token(
                            &real.to_string_lossy(),
                            Some(effective_src),
                            mount_table,
                        ),
                        path: effective_src.to_string(),
                        source: migo_io::image_ops::ImageSource::MountCode {
                            virtual_path: effective_src.to_string(),
                            relative_path: relative.to_string(),
                        },
                    });
                }
                None => {
                    // Pack-backed: carry the relative path so the image worker
                    // pool performs the package read instead of the host thread.
                    let relative = effective_src.strip_prefix("/code/").unwrap_or("");
                    let max_len = shared::protocol::io_cmd::MAX_READ_LENGTH;
                    if let Some(size) = mt.entry_size(relative) {
                        if size > max_len {
                            return Err(EngineError::new(ErrorCode::IoError)
                                .with_msg("pack image too large")
                                .with_detail(format!(
                                    "src={}, size={}, limit={}",
                                    src, size, max_len
                                )));
                        }
                    }
                    return Ok(ResolvedSrc {
                        path: effective_src.to_string(),
                        // The same field the io cache keys this path on. It used to
                        // be `source_mounted_at`, which matched only because the io
                        // side used it too -- and both collided across Sessions.
                        // They have to move together: the pre-pin taken here has to
                        // land on the key the decode inserts under, or the admission
                        // filter can reject the real entry and `texImage2D(image)`
                        // reads no bytes.
                        source_version: resolved.source_identity,
                        source: migo_io::image_ops::ImageSource::MountCode {
                            virtual_path: effective_src.to_string(),
                            relative_path: relative.to_string(),
                        },
                    });
                }
            }
        }
        // Fallback: no mount table, use VFS + file-version token.
        if let Some(vfs) = vfs {
            return vfs
                .resolve(effective_src, FileOp::Read)
                .map(|p| {
                    let path_str = p.to_string_lossy().into_owned();
                    let ver =
                        variant_source_version_token(&path_str, Some(effective_src), mount_table);
                    ResolvedSrc {
                        path: path_str,
                        source_version: ver,
                        source: migo_io::image_ops::ImageSource::Filesystem,
                    }
                })
                .map_err(|e| {
                    EngineError::new(ErrorCode::PermissionDenied)
                        .with_msg("image path resolve failed")
                        .with_detail(format!("src={}, err={}", src, e))
                });
        }
    }

    // /user, /cache, /tmp: mutable paths, use file-version token.
    let is_other_virtual = effective_src == "/user"
        || effective_src.starts_with("/user/")
        || effective_src == "/cache"
        || effective_src.starts_with("/cache/")
        || effective_src == "/tmp"
        || effective_src.starts_with("/tmp/");

    if is_other_virtual {
        if let Some(vfs) = vfs {
            return vfs
                .resolve(effective_src, FileOp::Read)
                .map(|p| {
                    let path_str = p.to_string_lossy().into_owned();
                    let ver = variant_source_version_token(&path_str, None, mount_table);
                    ResolvedSrc {
                        path: path_str,
                        source_version: ver,
                        source: migo_io::image_ops::ImageSource::Filesystem,
                    }
                })
                .map_err(|e| {
                    EngineError::new(ErrorCode::PermissionDenied)
                        .with_msg("image path resolve failed")
                        .with_detail(format!("src={}, err={}", src, e))
                });
        }
    }

    // Non-virtual absolute path: BLOCKED.
    Err(EngineError::new(ErrorCode::PermissionDenied)
        .with_msg("image path not allowed")
        .with_detail(format!(
            "src={}: absolute host paths are not permitted; use /code, /user, /cache, or /tmp",
            src
        )))
}

#[op2(fast)]
pub fn op_create_image(_state: &mut OpState) -> u32 {
    // Allocate the id directly from the process-global counter in
    // `shared::image_id`.  Historically this op did a sync round-trip
    // to the render thread to call `cm.generate_img_id()`, which was a
    // pointless serialisation: the operation is a pure counter bump,
    // but any busy render thread (e.g. mid-FramePacket) blocked
    // `new Image()` in JS for the duration.  On the cocos shop scene
    // first frame that was ~700 ms of head-of-line stall.  The render
    // thread's `ImageStore::generate_id` reads from the same counter,
    // so cross-thread allocation stays unique without coordination.
    shared::image_id::next_image_id()
}

async fn op_load_image_inner(
    state: Rc<RefCell<OpState>>,
    image_id: u32,
    src: String,
    target_width: Option<u32>,
    target_height: Option<u32>,
) -> EngineResult<(u32, (usize, usize))> {
    let (scheduler, vfs, mount_table, game_cache_dir, gpu_caps, cpu_backing_required) = {
        let op = state.borrow();
        let host = op.borrow::<HostOpState>();
        let gcd = host
            .game_paths
            .as_ref()
            .map(|gp| gp.cache_dir().to_string_lossy().into_owned());
        (
            op.borrow::<IoSchedulerState>().0.clone(),
            host.vfs.clone(),
            host.mount_table.clone(),
            gcd,
            host.gpu_caps.clone(),
            host.webgl_context_created.clone(),
        )
    };

    let (canvas_ctx, image_cache, session) = {
        let op = state.borrow();
        let images = op.borrow::<ImageCacheState>();
        (
            op.borrow::<CanvasOpState>().clone(),
            images.aliases.clone(),
            images.session,
        )
    };

    // `data:` and `http(s)://` scheme short-circuits.  Both feed raw
    // bytes into the same GPU upload path as local files; they just
    // skip the VFS / mount-table resolution below and use scheme
    // prefixes as the cache key so repeated `img.src = "data:..."`
    // assignments re-use the already-uploaded shared texture.
    if src.starts_with("data:") {
        return load_image_from_inline_bytes(
            scheduler,
            gpu_caps,
            cpu_backing_required,
            canvas_ctx,
            image_cache,
            session,
            image_id,
            src,
            target_width,
            target_height,
        )
        .await;
    }
    if src.starts_with("http://") || src.starts_with("https://") {
        return load_image_from_http(
            state.clone(),
            scheduler,
            gpu_caps,
            cpu_backing_required,
            canvas_ctx,
            image_cache,
            session,
            image_id,
            src,
            target_width,
            target_height,
        )
        .await;
    }

    // Resolve path + generation atomically from a single mount table read.
    let resolved = resolve_local_src(vfs.as_deref(), mount_table.as_deref(), &src)?;
    let src = resolved.path;
    let mount_generation = resolved.source_version;
    let image_source = resolved.source;
    info!("op_load_image begin: image_id={}, src={}", image_id, src);

    // remove previous alias and possibly destroy old shared
    if let Some(to_destroy) = {
        let mut c = image_cache.lock();
        c.remove_previous_alias(image_id)
    } {
        dispatch_destroy_image(&canvas_ctx.tx, to_destroy, "image cache eviction");
    }

    // Structured cache key: (path\0WxH, generation) — no delimiter collision.
    let cache_key = cache::make_cache_key(&src, target_width, target_height, mount_generation);

    match {
        let mut c = image_cache.lock();
        c.begin_load(image_id, &cache_key)
    } {
        cache::BeginLoadResult::AlreadyLoaded((shared_id, dims)) => {
            info!(
                "op_load_image cache hit: image_id={}, shared_id={}, src={}, dims={}x{}",
                image_id, shared_id, src, dims.0, dims.1
            );
            Ok((shared_id, dims))
        }

        cache::BeginLoadResult::Join(rx) => {
            info!(
                "op_load_image join pending load: image_id={}, src={}",
                image_id, src
            );
            match rx.await {
                Ok(Ok((actual_cache_key, shared_id, dims))) => {
                    // IMPORTANT: bind alias for this caller image_id so destroy works even if JS does not replace IDs
                    {
                        let mut c = image_cache.lock();
                        c.bind_alias_existing(image_id, &cache_key, &actual_cache_key, shared_id);
                    }
                    info!(
                        "op_load_image join resolved: image_id={}, shared_id={}, src={}, dims={}x{}",
                        image_id, shared_id, src, dims.0, dims.1
                    );
                    Ok((shared_id, dims))
                }
                Ok(Err(msg)) => {
                    warn!(
                        "op_load_image join failed: image_id={}, src={}, err={}",
                        image_id, src, msg
                    );
                    shared::bail!(ErrorCode::ImageReadError, "cache join failed", msg)
                }
                Err(_) => {
                    warn!(
                        "op_load_image join canceled: image_id={}, src={}",
                        image_id, src
                    );
                    shared::bail!(ErrorCode::Cancelled, "wait canceled")
                }
            }
        }

        cache::BeginLoadResult::StartLoading => {
            // Allocate an independent shared ID for the GPU texture.
            // This must NOT be the caller's image_id — see cache.rs docs.
            let shared_id = cache::alloc_shared_id();
            // Register the alias now so that a concurrent op_destroy_image
            // resolves to the correct shared_id during the upload window.
            {
                let mut c = image_cache.lock();
                c.register_inflight_alias(image_id, shared_id);
            }
            // Held until this arm returns, however it returns: see `PrePin`.
            let pre_pin = PrePin::take(cache_key.clone());
            info!(
                "op_load_image start loader: image_id={}, shared_id={}, src={}, cpu_backing_required={}",
                image_id,
                shared_id,
                src,
                cpu_backing_required.load(std::sync::atomic::Ordering::Acquire)
            );

            let decoded = match migo_io::image_ops::read_image_rgba8(
                scheduler,
                src.clone(),
                target_width,
                target_height,
                mount_generation,
                image_source.clone(),
                game_cache_dir.clone(),
                gpu_caps,
                mount_table.clone(),
                migo_io::image_ops::ImageDecodePolicy::PreferGpuNative {
                    cpu_backing_required,
                },
            )
            .await
            {
                Ok(decoded) => decoded,
                Err(e) => {
                    let msg = engine_err_to_text(&e);
                    warn!(
                        "op_load_image io decode failed: image_id={}, src={}, err={}",
                        image_id, src, msg
                    );
                    let mut c = image_cache.lock();
                    let _ = c.finish_load(image_id, shared_id, &cache_key, &cache_key, Err(msg));
                    // The pre-pin must not survive decode failure — there is no
                    // live alias and no upload to release it — and it does not:
                    // returning drops it.
                    return Err(e);
                }
            };

            let actual_cache_key = resized_rgba_io_cache_key(
                &src,
                target_width,
                target_height,
                decoded.source_generation,
            );
            let img = decoded.image;

            // Store full-resolution + resized RGBA decodes in the
            // io LRU.  `read_image_rgba8` already inserted the
            // freshly decoded bytes; this second insert is a no-op
            // for the common full-resolution case and only matters
            // when callers pass explicit `target_width/height` that
            // `read_image_rgba8` keyed differently from
            // `actual_cache_key`.  Pin bookkeeping for both paths
            // happens in `finish_load` below.
            if target_width.is_some() && target_height.is_some() {
                if let shared::protocol::io_cmd::DecodedImage::Rgba(ref rgba) = img {
                    migo_io::global_cache().insert(actual_cache_key.clone(), rgba.clone(), session);
                }
            }

            // H-5: for *inline* RGBA decodes served by the local
            // file path we also drop bytes into the io LRU so
            // `op_tex_image_2d_from_image` can hit a single source
            // of truth.  The full-resolution branch above only
            // covers resized variants; without this additional
            // insert, non-resized loads would never populate the
            // LRU slot keyed on the full-res key.
            if target_width.is_none() && target_height.is_none() {
                if let shared::protocol::io_cmd::DecodedImage::Rgba(ref rgba) = img {
                    migo_io::global_cache().insert(actual_cache_key.clone(), rgba.clone(), session);
                }
            }

            // Upload texture under shared_id (not caller image_id).
            let res = send_render_with_resp_async(&canvas_ctx, OP_LOAD_IMAGE, |resp| {
                RenderCommand::Canvas(CanvasCmd::LoadImage {
                    image_id: shared_id,
                    image: img,
                    priority: shared::protocol::io_cmd::ImagePriority::Normal,
                    resp,
                })
            })
            .await;

            let maybe_destroy = {
                let mut c = image_cache.lock();
                let destroy = match &res {
                    Ok((w, h)) => c.finish_load(
                        image_id,
                        shared_id,
                        &cache_key,
                        &actual_cache_key,
                        Ok((*w as usize, *h as usize)),
                    ),
                    Err(e) => c.finish_load(
                        image_id,
                        shared_id,
                        &cache_key,
                        &actual_cache_key,
                        Err(engine_err_to_text(e)),
                    ),
                };
                destroy
            };
            // The pre-pin goes back once this arm is done with it. `finish_load`
            // has by then taken the real alias pin, on `actual_cache_key` if the
            // mounted source remapped it, so the bytes never sit unpinned in
            // between.
            drop(pre_pin);

            if let Some(to_destroy) = maybe_destroy {
                dispatch_destroy_image(&canvas_ctx.tx, to_destroy, "image cache eviction");
            }

            match res {
                Ok((w, h)) => {
                    info!(
                        "op_load_image loader resolved: image_id={}, shared_id={}, src={}, dims={}x{}",
                        image_id, shared_id, src, w, h
                    );
                    Ok((shared_id, (w as usize, h as usize)))
                }
                Err(e) => {
                    warn!(
                        "op_load_image gpu upload failed: image_id={}, src={}, err={}",
                        image_id,
                        src,
                        engine_err_to_text(&e)
                    );
                    Err(e)
                }
            }
        }
    }
}

/// Common upload path for inline-bytes loaders (`data:` / `http(s)://`).
///
/// Identical in shape to the local-file flow (begin_load → shared_id →
/// LoadImage → finish_load). Parsing and decode have already run through the
/// shared bounded image worker path before this upload stage.
async fn upload_inline_image(
    canvas_ctx: CanvasOpState,
    image_cache: cache::SharedImageCache,
    session: i32,
    image_id: u32,
    shared_id: u32,
    cache_key: cache::ImageCacheKey,
    decoded: shared::protocol::io_cmd::DecodedImage,
    src_label: &str,
) -> EngineResult<(u32, (usize, usize))> {
    use shared::protocol::io_cmd::DecodedImage;
    // Extract dimensions for the error-path logging below; both
    // variants can report their own width/height without needing
    // a CPU-side copy.
    let (w, h) = match &decoded {
        DecodedImage::Rgba(img) => (img.width as i32, img.height as i32),
        DecodedImage::HardwareBuffer(ahb) => (ahb.width as i32, ahb.height as i32),
        DecodedImage::Compressed(c) => (c.width as i32, c.height as i32),
    };

    // H-5: populate migo_io::global_cache BEFORE moving `decoded`
    // into the render command.  Data-URL and http(s):// paths
    // previously skipped the LRU entirely, which made every later
    // `texImage2D(image)` on those images a guaranteed cache miss
    // → black texture.  We now insert exactly like local-file
    // loads so the pin bookkeeping below covers all three load
    // paths uniformly.
    let pre_pin = if matches!(decoded, DecodedImage::Rgba(_)) {
        // Same live-resource invariant as the local-file path: pin before
        // inserting so the admission filter cannot reject bytes for an Image
        // that is already being loaded for WebGL use.
        let pre_pin = PrePin::take(cache_key.clone());
        if let DecodedImage::Rgba(ref rgba) = decoded {
            migo_io::global_cache().insert(pre_pin.key().clone(), rgba.clone(), session);
        }
        Some(pre_pin)
    } else {
        None
    };

    let res = send_render_with_resp_async(&canvas_ctx, OP_LOAD_IMAGE, |resp| {
        RenderCommand::Canvas(CanvasCmd::LoadImage {
            image_id: shared_id,
            image: decoded,
            priority: shared::protocol::io_cmd::ImagePriority::Normal,
            resp,
        })
    })
    .await;

    let maybe_destroy = {
        let mut c = image_cache.lock();
        let destroy = match &res {
            Ok((actual_w, actual_h)) => c.finish_load(
                image_id,
                shared_id,
                &cache_key,
                &cache_key,
                Ok((*actual_w as usize, *actual_h as usize)),
            ),
            Err(e) => c.finish_load(
                image_id,
                shared_id,
                &cache_key,
                &cache_key,
                Err(engine_err_to_text(e)),
            ),
        };
        destroy
    };
    drop(pre_pin);

    if let Some(to_destroy) = maybe_destroy {
        dispatch_destroy_image(&canvas_ctx.tx, to_destroy, "image cache eviction");
    }

    match res {
        Ok((rw, rh)) => {
            info!(
                "op_load_image inline uploaded: image_id={}, shared_id={}, src={}, dims={}x{}",
                image_id, shared_id, src_label, rw, rh
            );
            Ok((shared_id, (rw as usize, rh as usize)))
        }
        Err(e) => {
            warn!(
                "op_load_image inline upload failed: image_id={}, src={}, err={}",
                image_id,
                src_label,
                engine_err_to_text(&e)
            );
            // Surface width/height for debugging even on failure.
            let _ = (w, h);
            Err(e)
        }
    }
}

async fn load_image_from_inline_bytes(
    scheduler: std::sync::Arc<migo_io::scheduler::IoScheduler>,
    gpu_caps: std::sync::Arc<shared::device::gpu_caps::GpuCaps>,
    cpu_backing_required: std::sync::Arc<std::sync::atomic::AtomicBool>,
    canvas_ctx: CanvasOpState,
    image_cache: cache::SharedImageCache,
    session: i32,
    image_id: u32,
    src: String,
    target_width: Option<u32>,
    target_height: Option<u32>,
) -> EngineResult<(u32, (usize, usize))> {
    info!(
        "op_load_image data-url: image_id={}, len={}",
        image_id,
        src.len()
    );
    // Reject pathological metadata/payload lengths before hashing or cloning
    // the source into cache state. The fixed-size SHA-256 identity prevents an
    // otherwise-valid multi-megabyte data URL from being duplicated across
    // the cache's source/alias/loading maps. Data URLs are immutable, so the
    // generation remains zero.
    inline_src::validate_data_url_cache_input(&src)?;
    let cache_identity = data_url_cache_identity(&src);
    let cache_key = cache::make_cache_key(&cache_identity, target_width, target_height, 0);

    // Replace any prior alias (mirrors the local-file flow).
    if let Some(to_destroy) = {
        let mut c = image_cache.lock();
        c.remove_previous_alias(image_id)
    } {
        dispatch_destroy_image(&canvas_ctx.tx, to_destroy, "image cache eviction");
    }

    match {
        let mut c = image_cache.lock();
        c.begin_load(image_id, &cache_key)
    } {
        cache::BeginLoadResult::AlreadyLoaded((shared_id, dims)) => Ok((shared_id, dims)),
        cache::BeginLoadResult::Join(rx) => match rx.await {
            Ok(Ok((actual_key, shared_id, dims))) => {
                let mut c = image_cache.lock();
                c.bind_alias_existing(image_id, &cache_key, &actual_key, shared_id);
                Ok((shared_id, dims))
            }
            Ok(Err(msg)) => {
                shared::bail!(ErrorCode::ImageReadError, "data url join failed", msg)
            }
            Err(_) => shared::bail!(ErrorCode::Cancelled, "data url wait canceled"),
        },
        cache::BeginLoadResult::StartLoading => {
            let shared_id = cache::alloc_shared_id();
            {
                let mut c = image_cache.lock();
                c.register_inflight_alias(image_id, shared_id);
            }

            let encoded_len = src.len();
            let result = migo_io::image_ops::run_bounded_inline_image_job(
                scheduler,
                encoded_len,
                move || {
                    let payload = inline_src::parse_data_url(&src)?;
                    let hint = if payload.mime.is_empty() {
                        None
                    } else {
                        Some(payload.mime.as_str())
                    };
                    // Prefer the API-26 AHB path only if the live worker-time
                    // capability and WebGL policy still permit GPU-only pixels.
                    let allow_ahb = ahb_image_decode_allowed(&gpu_caps, &cpu_backing_required);
                    let decoded = match (target_width, target_height) {
                        (Some(tw), Some(th)) if tw > 0 && th > 0 => {
                            shared::protocol::io_cmd::DecodedImage::Rgba(
                                inline_src::decode_inline_bytes(
                                    &payload.bytes,
                                    hint,
                                    Some(tw),
                                    Some(th),
                                )?,
                            )
                        }
                        _ => inline_src::decode_inline_bytes_any(&payload.bytes, hint, allow_ahb)?,
                    };
                    Ok((decoded, payload.bytes.len()))
                },
            )
            .await;

            match result {
                Ok((decoded, payload_len)) => {
                    let label = format!("data:[{}b]", payload_len);
                    upload_inline_image(
                        canvas_ctx,
                        image_cache,
                        session,
                        image_id,
                        shared_id,
                        cache_key,
                        decoded,
                        &label,
                    )
                    .await
                }
                Err(e) => {
                    let mut c = image_cache.lock();
                    let _ = c.finish_load(
                        image_id,
                        shared_id,
                        &cache_key,
                        &cache_key,
                        Err(engine_err_to_text(&e)),
                    );
                    Err(e)
                }
            }
        }
    }
}

async fn load_image_from_http(
    state: Rc<RefCell<OpState>>,
    scheduler: std::sync::Arc<migo_io::scheduler::IoScheduler>,
    gpu_caps: std::sync::Arc<shared::device::gpu_caps::GpuCaps>,
    cpu_backing_required: std::sync::Arc<std::sync::atomic::AtomicBool>,
    canvas_ctx: CanvasOpState,
    image_cache: cache::SharedImageCache,
    session: i32,
    image_id: u32,
    src: String,
    target_width: Option<u32>,
    target_height: Option<u32>,
) -> EngineResult<(u32, (usize, usize))> {
    info!("op_load_image http: image_id={}, url={}", image_id, src);

    // Reuse an in-flight fetch for the same URL+size if another Image
    // already kicked one off.  Generation is 0 because we assume the
    // URL itself encodes any cache-busting query string the caller
    // needs (matching browser `Image.src` semantics).
    let cache_key = cache::make_cache_key(&src, target_width, target_height, 0);

    if let Some(to_destroy) = {
        let mut c = image_cache.lock();
        c.remove_previous_alias(image_id)
    } {
        dispatch_destroy_image(&canvas_ctx.tx, to_destroy, "image cache eviction");
    }

    match {
        let mut c = image_cache.lock();
        c.begin_load(image_id, &cache_key)
    } {
        cache::BeginLoadResult::AlreadyLoaded((shared_id, dims)) => return Ok((shared_id, dims)),
        cache::BeginLoadResult::Join(rx) => {
            return match rx.await {
                Ok(Ok((actual_key, shared_id, dims))) => {
                    let mut c = image_cache.lock();
                    c.bind_alias_existing(image_id, &cache_key, &actual_key, shared_id);
                    Ok((shared_id, dims))
                }
                Ok(Err(msg)) => shared::bail!(ErrorCode::ImageReadError, "http join failed", msg),
                Err(_) => shared::bail!(ErrorCode::Cancelled, "http wait canceled"),
            };
        }
        cache::BeginLoadResult::StartLoading => {}
    }

    let shared_id = cache::alloc_shared_id();
    {
        let mut c = image_cache.lock();
        c.register_inflight_alias(image_id, shared_id);
    }

    // Active fetch + decode.  Any failure must still call finish_load
    // so joiners unblock (mirrors the local-file error path).  AHB
    // fast path activates when no resize is requested; resize forces
    // the RGBA route because Hardware Buffers are opaque.
    let result: EngineResult<shared::protocol::io_cmd::DecodedImage> = async {
        let bytes = inline_src::fetch_http_image(state, &src).await?;
        let encoded_len = bytes.len();
        migo_io::image_ops::run_bounded_inline_image_job(scheduler, encoded_len, move || {
            let allow_ahb = ahb_image_decode_allowed(&gpu_caps, &cpu_backing_required);
            match (target_width, target_height) {
                (Some(tw), Some(th)) if tw > 0 && th > 0 => {
                    Ok(shared::protocol::io_cmd::DecodedImage::Rgba(
                        inline_src::decode_inline_bytes(&bytes, None, Some(tw), Some(th))?,
                    ))
                }
                _ => inline_src::decode_inline_bytes_any(&bytes, None, allow_ahb),
            }
        })
        .await
    }
    .await;

    match result {
        Ok(decoded) => {
            upload_inline_image(
                canvas_ctx,
                image_cache,
                session,
                image_id,
                shared_id,
                cache_key,
                decoded,
                &src,
            )
            .await
        }
        Err(e) => {
            let msg = engine_err_to_text(&e);
            // Finish the load with error so any pending joiners unblock.
            let mut c = image_cache.lock();
            let _ = c.finish_load(image_id, shared_id, &cache_key, &cache_key, Err(msg));
            Err(e)
        }
    }
}

#[op2(async(lazy), fast)]
#[serde]
pub async fn op_load_image(
    state: Rc<RefCell<OpState>>,
    #[smi] image_id: u32,
    #[string] src: String,
    #[smi] target_width: u32,
    #[smi] target_height: u32,
) -> Result<(u32, (usize, usize)), JsErrorBox> {
    let tw = if target_width > 0 {
        Some(target_width)
    } else {
        None
    };
    let th = if target_height > 0 {
        Some(target_height)
    } else {
        None
    };
    let started_at = std::time::Instant::now();
    let result = op_load_image_inner(state, image_id, src.clone(), tw, th)
        .await
        .map_err(js_err_from_engine);
    let elapsed_ms = started_at.elapsed().as_millis() as u64;
    if elapsed_ms >= 50 {
        tracing::warn!(
            "[MigoPerf][LoadImage] op_load_image {elapsed_ms}ms image_id={image_id} src={src}"
        );
    }
    result
}

/// `createImageBitmap(source, sx, sy, sw, sh[, {resizeWidth, resizeHeight}])`
/// backend.  Decodes the source through the normal
/// `read_image_rgba8` pipeline (which hits the LRU when available),
/// crops the RGBA to the requested sub-rect (filling out-of-bounds
/// regions with transparent black per the WHATWG spec), optionally
/// resizes to the caller's `(rw, rh)`, then uploads as a fresh
/// texture registered under a new shared id.
#[op2(async(lazy), fast)]
#[serde]
#[allow(clippy::too_many_arguments)]
pub async fn op_load_image_subrect(
    state: Rc<RefCell<OpState>>,
    #[smi] image_id: u32,
    #[string] src: String,
    sx: i32,
    sy: i32,
    #[smi] sw: u32,
    #[smi] sh: u32,
    #[smi] resize_w: u32,
    #[smi] resize_h: u32,
) -> Result<(u32, (usize, usize)), JsErrorBox> {
    op_load_image_subrect_inner(state, image_id, src, sx, sy, sw, sh, resize_w, resize_h)
        .await
        .map_err(js_err_from_engine)
}

#[allow(clippy::too_many_arguments)]
async fn op_load_image_subrect_inner(
    state: Rc<RefCell<OpState>>,
    image_id: u32,
    src: String,
    sx: i32,
    sy: i32,
    sw: u32,
    sh: u32,
    resize_w: u32,
    resize_h: u32,
) -> EngineResult<(u32, (usize, usize))> {
    use shared::protocol::io_cmd::DecodedImage;

    if sw == 0 || sh == 0 {
        shared::bail!(
            ErrorCode::InvalidOperation,
            "createImageBitmap sub-rect has zero width/height"
        );
    }

    if resize_w > 0 && resize_h > 0 && !migo_io::resize_capable() {
        shared::bail!(
            ErrorCode::Unsupported,
            "createImageBitmap resize requires the rust-image-decode feature"
        );
    }

    // Pull decoder / VFS / mount table handles in one borrow.
    let (scheduler, vfs, mount_table, game_cache_dir, gpu_caps, canvas_ctx, image_cache) = {
        let op = state.borrow();
        let host = op.borrow::<HostOpState>();
        let gcd = host
            .game_paths
            .as_ref()
            .map(|gp| gp.cache_dir().to_string_lossy().into_owned());
        (
            op.borrow::<IoSchedulerState>().0.clone(),
            host.vfs.clone(),
            host.mount_table.clone(),
            gcd,
            host.gpu_caps.clone(),
            op.borrow::<CanvasOpState>().clone(),
            op.borrow::<ImageCacheState>().aliases.clone(),
        )
    };

    // Sub-rect uses only local file / mount-backed sources for now;
    // data: / http(s) handling can be layered on later by mirroring
    // `op_load_image_inner`'s short-circuit branches.
    let resolved = resolve_local_src(vfs.as_deref(), mount_table.as_deref(), &src)?;
    let real_src = resolved.path;
    let mount_generation = resolved.source_version;
    let image_source = resolved.source;

    // Subrect alias key: same `src` with a `\0subrect=...` suffix so
    // repeated `createImageBitmap(img, sx, sy, sw, sh[, opts])` calls
    // with identical arguments share a single GPU texture instead of
    // re-decoding + re-cropping + re-uploading every time.
    //
    // A tile-map that extracts 64 tiles from one atlas (common for
    // platformers) pays the decode + crop + upload cost ONCE across
    // the whole game session instead of once per `createImageBitmap`
    // call.  The alias is refcounted like any other `Image` / bitmap,
    // so a bitmap.close() decrements the texture independently.
    //
    // The crop lives in the path rather than in the key's dimension fields:
    // those name a *resize* of the whole image, and a crop is not one. A NUL
    // delimiter cannot collide, being illegal in filesystem paths.
    let cache_key: cache::ImageCacheKey = migo_io::image_cache::full_res_key(
        format!(
            "{}\0subrect={}x{}+{}+{}@{}x{}",
            real_src, sw, sh, sx, sy, resize_w, resize_h
        ),
        mount_generation,
    );

    // Clear any prior alias this `image_id` held before claiming a
    // new subrect slot — matches the local-file flow's semantic
    // where reassigning `img.src` drops the previous texture.
    if let Some(to_destroy) = {
        let mut c = image_cache.lock();
        c.remove_previous_alias(image_id)
    } {
        dispatch_destroy_image(&canvas_ctx.tx, to_destroy, "image cache eviction");
    }

    // Cache-hit fast path: second+ call with identical args returns
    // the previously uploaded texture's shared id.  In-flight path
    // awaits the first caller's finish_load so N simultaneous
    // identical subrect requests do the decode+upload once.
    match {
        let mut c = image_cache.lock();
        c.begin_load(image_id, &cache_key)
    } {
        cache::BeginLoadResult::AlreadyLoaded((shared_id, dims)) => return Ok((shared_id, dims)),
        cache::BeginLoadResult::Join(rx) => {
            return match rx.await {
                Ok(Ok((actual_key, shared_id, dims))) => {
                    let mut c = image_cache.lock();
                    c.bind_alias_existing(image_id, &cache_key, &actual_key, shared_id);
                    Ok((shared_id, dims))
                }
                Ok(Err(msg)) => {
                    shared::bail!(ErrorCode::ImageReadError, "subrect join failed", msg)
                }
                Err(_) => shared::bail!(ErrorCode::Cancelled, "subrect wait canceled"),
            };
        }
        cache::BeginLoadResult::StartLoading => {}
    }

    // Decode the full-resolution image (LRU-hit when warm).
    let transform_scheduler = std::sync::Arc::clone(&scheduler);

    let decoded = migo_io::image_ops::read_image_rgba8(
        scheduler,
        real_src.clone(),
        None,
        None,
        mount_generation,
        image_source,
        game_cache_dir,
        gpu_caps,
        mount_table,
        migo_io::image_ops::ImageDecodePolicy::RgbaOnly,
    )
    .await?;
    // Convert opaque GPU-native variants to owned RGBA, then submit crop and
    // resize together so the worker keeps the source/intermediate/output
    // reservation until the transform actually ends.
    let rgba = match decoded.image {
        DecodedImage::Rgba(r) => r,
        DecodedImage::HardwareBuffer(ahb) => DecodedImage::HardwareBuffer(ahb).into_rgba()?,
        DecodedImage::Compressed(_) => shared::bail!(
            ErrorCode::InvalidOperation,
            "createImageBitmap sub-rect does not support GPU-compressed sources yet"
        ),
    };
    let final_img = migo_io::image_ops::run_subrect_transform(
        transform_scheduler,
        rgba,
        sx,
        sy,
        sw,
        sh,
        resize_w,
        resize_h,
    )
    .await?;
    let final_w = final_img.width as usize;
    let final_h = final_img.height as usize;

    // Allocate a fresh shared id and upload.  We reuse the standard
    // alias-registration dance so `op_destroy_image(image_id)` on
    // the JS-side bitmap refcount-decrements the texture correctly.
    let shared_id = cache::alloc_shared_id();
    {
        let mut c = image_cache.lock();
        c.register_inflight_alias(image_id, shared_id);
    }

    let send_res = send_render_with_resp_async(&canvas_ctx, OP_LOAD_IMAGE, |resp| {
        RenderCommand::Canvas(CanvasCmd::LoadImage {
            image_id: shared_id,
            image: DecodedImage::Rgba(final_img),
            priority: shared::protocol::io_cmd::ImagePriority::Normal,
            resp,
        })
    })
    .await;

    // `cache_key` was already computed at function entry; `finish_load`
    // uses it to settle any waiters that joined our StartLoading.

    let maybe_destroy = {
        let mut c = image_cache.lock();
        match &send_res {
            Ok((w, h)) => c.finish_load(
                image_id,
                shared_id,
                &cache_key,
                &cache_key,
                Ok((*w as usize, *h as usize)),
            ),
            Err(e) => c.finish_load(
                image_id,
                shared_id,
                &cache_key,
                &cache_key,
                Err(engine_err_to_text(e)),
            ),
        }
    };
    if let Some(to_destroy) = maybe_destroy {
        dispatch_destroy_image(&canvas_ctx.tx, to_destroy, "image cache eviction");
    }

    let (rw, rh) = send_res?;
    let _ = (final_w, final_h); // `rw/rh` from render thread is authoritative
    Ok((shared_id, (rw as usize, rh as usize)))
}

/// Send a single `DestroyImage` as a must-deliver Sync-class command.
///
/// Uses `dispatch()` (bounded-blocking) rather than the legacy non-blocking
/// `send()`, so a full render queue can't silently drop the destroy and leak the
/// GPU texture / AHB. A disconnected render thread (already shut down) returns
/// immediately — the only expected best-effort case. `context` labels the call
/// site in the warn log. All `DestroyImage` producers in this module must route
/// through this helper (or the batch `dispatch_destroy_images`) rather than the
/// bare `send()` that silently drops on backpressure.
fn dispatch_destroy_image(
    tx: &shared::render_command_sender::CommandSender,
    image_id: u32,
    context: &str,
) {
    if let Err(e) = tx.dispatch(RenderCommand::Canvas(CanvasCmd::DestroyImage { image_id })) {
        warn!("{context}: DestroyImage dispatch failed (texture may leak): {e}");
    }
}

/// Batch variant of [`dispatch_destroy_image`]: destroy many shared images in a
/// single must-deliver command so bulk teardown costs one bounded-blocking send
/// instead of N. No-op on an empty list.
fn dispatch_destroy_images(
    tx: &shared::render_command_sender::CommandSender,
    image_ids: Vec<u32>,
    context: &str,
) {
    if image_ids.is_empty() {
        return;
    }
    if let Err(e) = tx.dispatch(RenderCommand::Canvas(CanvasCmd::DestroyImages {
        image_ids,
    })) {
        warn!("{context}: DestroyImages dispatch failed (textures may leak): {e}");
    }
}

#[op2(fast)]
pub fn op_destroy_image(state: &mut OpState, #[smi] image_id: u32) -> bool {
    let to_destroy = {
        let mut c = state.borrow::<ImageCacheState>().aliases.lock();
        c.try_release_and_get_destroy_rid(image_id)
    };

    if let Some(rid) = to_destroy {
        let ctx = state.borrow::<CanvasOpState>();
        dispatch_destroy_image(&ctx.tx, rid, "op_destroy_image");
    }
    true
}

/// Preload multiple images in parallel
/// Returns array of [path, success, width, height, error_msg]
#[op2(async(lazy))]
#[serde]
pub async fn op_preload_images(
    state: Rc<RefCell<OpState>>,
    #[serde] paths: Vec<String>,
) -> Result<Vec<(String, bool, u32, u32, String)>, JsErrorBox> {
    let (scheduler, vfs, mount_table, game_cache_dir, gpu_caps) = {
        let op = state.borrow();
        let host = op.borrow::<HostOpState>();
        let gcd = host
            .game_paths
            .as_ref()
            .map(|gp| gp.cache_dir().to_string_lossy().into_owned());
        (
            op.borrow::<IoSchedulerState>().0.clone(),
            host.vfs.clone(),
            host.mount_table.clone(),
            gcd,
            host.gpu_caps.snapshot(),
        )
    };

    // Resolve all paths atomically (path + generation per resolve call).
    // Rejected paths become error entries; never sent to decode.
    let mut io_entries: Vec<(String, u64, migo_io::image_ops::ImageSource)> =
        Vec::with_capacity(paths.len());
    let mut early_errors: Vec<(usize, String)> = Vec::new();
    for (i, p) in paths.iter().enumerate() {
        match resolve_local_src(vfs.as_deref(), mount_table.as_deref(), p) {
            Ok(resolved) => {
                io_entries.push((resolved.path, resolved.source_version, resolved.source))
            }
            Err(e) => {
                early_errors.push((i, engine_err_to_text(&e)));
                io_entries.push((
                    String::new(),
                    0,
                    migo_io::image_ops::ImageSource::Filesystem,
                )); // placeholder
            }
        }
    }

    // Filter out failed entries, keeping per-path generation.
    let io_entries_filtered: Vec<(String, u64, migo_io::image_ops::ImageSource)> = io_entries
        .iter()
        .enumerate()
        .filter(|(i, _)| !early_errors.iter().any(|(ei, _)| ei == i))
        .map(|(_, entry)| entry.clone())
        .collect();

    // Decode successfully resolved entries via the image scheduler pool.
    let io_results = if !io_entries_filtered.is_empty() {
        migo_io::image_ops::preload_images(
            scheduler,
            io_entries_filtered,
            game_cache_dir.clone(),
            gpu_caps,
            mount_table.clone(),
        )
        .await
    } else {
        Vec::new()
    };

    // Merge results with early errors into the output, preserving
    // original order matching the input `paths`.
    let mut io_iter = io_results.into_iter();
    let mut output: Vec<(String, bool, u32, u32, String)> = Vec::with_capacity(paths.len());
    for (i, original_path) in paths.iter().enumerate() {
        if let Some(err_entry) = early_errors.iter().find(|(ei, _)| *ei == i) {
            output.push((original_path.clone(), false, 0, 0, err_entry.1.clone()));
        } else if let Some((_, result)) = io_iter.next() {
            match result {
                Ok((w, h)) => output.push((original_path.clone(), true, w, h, String::new())),
                Err(msg) => output.push((original_path.clone(), false, 0, 0, msg)),
            }
        }
    }

    Ok(output)
}

/// Clear all image caches: JS shared cache + IO in-memory cache + derived disk cache.
#[op2(fast)]
pub fn op_clear_image_cache(state: &mut OpState) -> Result<(), JsErrorBox> {
    // 1. Destroy live shared GPU textures, then clear JS-side shared cache.
    let gcd = {
        let host = state.borrow::<HostOpState>();
        host.game_paths
            .as_ref()
            .map(|gp| gp.cache_dir().to_string_lossy().into_owned())
    };
    let render_tx = state.borrow::<HostOpState>().render_tx.clone();
    // Content reaches this op through `ImageCache.clear()`, so everything it
    // discards has to be this game's own: clearing every Session's would let one
    // game's script black out another's textures and throw away its decoded bytes.
    let session = state.borrow::<ImageCacheState>().session;
    let dead_shared_ids = state.borrow::<ImageCacheState>().aliases.lock().drain();

    // Batch the whole shared-image set into one must-deliver command so a large
    // cache doesn't block the caller up to the send deadline per image.
    dispatch_destroy_images(&render_tx, dead_shared_ids, "op_clear_image_cache");

    // 2. Clear this Session's decoded bytes and its derived disk cache. The disk
    // half is rooted at this game's own cache dir; the in-memory half drops this
    // Session's claim on each entry and evicts only what no other Session still
    // holds, because those bytes are shared on purpose.
    migo_io::image_ops::clear_image_cache(gcd.as_deref(), session);
    Ok(())
}

/// Get image cache statistics
#[op2]
#[serde]
pub fn op_get_image_cache_stats(state: &mut OpState) -> shared::protocol::io_cmd::ImageCacheStats {
    // This game's own figures. The process-wide ones let it watch another game's
    // asset loading, which is an isolation defect and not a diagnostic.
    migo_io::image_ops::get_image_cache_stats(state.borrow::<ImageCacheState>().session)
}

extension!(host_v8_image,
    deps = [host_v8_console, host_v8_base, host_v8_file, host_v8_io_state],
    ops = [
        op_create_image,
        op_load_image,
        op_load_image_subrect,
        op_destroy_image,
        op_preload_images,
        op_clear_image_cache,
        op_get_image_cache_stats
    ],
    esm = [
        dir "src/rendering/image",
        "01_image.js",
        "01_image_data.js",
    ],
    state = |state| {
        let host_id = state.borrow::<HostOpState>().id;
        state.put(ImageCacheState {
            aliases: cache::image_cache_for_host(host_id),
            session: host_id,
        });
    },
);

pub(super) fn image_extensions() -> Vec<deno_core::Extension> {
    vec![host_v8_image::init()]
}

pub(super) fn image_lazy_extensions() -> Vec<deno_core::Extension> {
    vec![host_v8_image::lazy_init()]
}

#[cfg(test)]
mod tests {
    use super::{
        PrePin, data_url_cache_identity, resized_rgba_io_cache_key, resolve_local_src,
        variant_source_version_token,
    };

    /// Section 6.5, on the one process-global structure two Sessions both reach
    /// that holds their *content*: the decoded-image cache. What keeps one game's
    /// pixels out of another's is the cache key, and for a directory-mounted
    /// `/code` asset the key's path component is the **virtual** string
    /// `/code/logo.png` — byte-identical for both games. Separation rests entirely
    /// on the source-version token, which hashes the real path behind the mount.
    /// Nothing asserted that until this test; the pack-backed half of the same
    /// branch got its own identity under task 0.28, and this is the other half.
    ///
    /// **The two files are given identical bytes and identical mtimes on purpose.**
    /// Left to the filesystem they would differ, and the test would then pass on
    /// metadata while a token that had stopped hashing the real path walked
    /// straight through it — the keys would still differ, for a reason that has
    /// nothing to do with isolation. Two titles from one publisher shipping the
    /// same logo, unpacked by the same reproducible extraction, is that fixture in
    /// production.
    ///
    /// The second assertion is the control the first one needs: a token that
    /// varied per call would satisfy "the keys differ" while destroying the cache
    /// it exists to key.
    #[test]
    fn two_games_do_not_share_a_cache_entry_for_an_identical_asset() {
        use shared::vfs::MountTable;
        use std::fs::{self, File, FileTimes};
        use std::time::{Duration, SystemTime};

        let root = std::env::temp_dir().join(format!(
            "migo-image-cache-isolation-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);

        // Identical bytes at the identical virtual path, in two games' own code
        // directories, with the same modification time.
        let stamp = SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
        let mut code_dirs = Vec::new();
        for game in ["game-a", "game-b"] {
            let code = root.join(game).join("code");
            fs::create_dir_all(&code).expect("game code dir");
            let asset = code.join("logo.png");
            fs::write(&asset, b"identical bytes").expect("write asset");
            File::options()
                .write(true)
                .open(&asset)
                .expect("reopen asset")
                .set_times(FileTimes::new().set_modified(stamp).set_accessed(stamp))
                .expect("equalise mtime");
            code_dirs.push(code);
        }

        // One mount table per game, rooted at that game's code directory, which is
        // what `evaluate_module` builds.
        let key_for = |code: &std::path::Path| {
            let mount = MountTable::new(code.to_path_buf());
            let resolved = resolve_local_src(None, Some(&mount), "/code/logo.png")
                .expect("a mounted /code asset must resolve");
            resized_rgba_io_cache_key(&resolved.path, None, None, resolved.source_version)
        };

        let a = key_for(&code_dirs[0]);
        let b = key_for(&code_dirs[1]);
        let a_again = key_for(&code_dirs[0]);

        let _ = fs::remove_dir_all(&root);

        assert_ne!(
            a, b,
            "two games' identically-named assets share one process-global cache              entry, so one game is served the other's decoded pixels"
        );
        assert_eq!(
            a, a_again,
            "the same game's asset keyed differently on a second load, so the              cache can never hit and the isolation above holds for the wrong reason"
        );
    }

    /// A Session torn down or restarted with a load in flight has that op's future
    /// dropped rather than resumed, so nothing on the load path runs again. The pin
    /// it took before the decode has to come back anyway: nothing else can return
    /// it, and the decoded-bytes cache is deliberately not cleared on teardown, so a
    /// pin left here makes those bytes immune to eviction, to trim and to `clear`
    /// for the life of the process.
    #[tokio::test(start_paused = true)]
    async fn a_load_abandoned_mid_flight_releases_its_pre_pin() {
        let key: migo_io::image_cache::ImageCacheKey =
            ("/code/abandoned-mid-load.png".into(), 21, 0, 0);

        let abandoned = tokio::time::timeout(std::time::Duration::from_millis(20), async {
            let _pre_pin = PrePin::take(key.clone());
            assert_eq!(
                migo_io::global_cache().pin_count(&key),
                1,
                "the fixture must actually hold the pin it is about to abandon"
            );
            // Stands in for the decode or the GPU upload never coming back.
            std::future::pending::<()>().await;
        })
        .await;

        assert!(
            abandoned.is_err(),
            "the fixture must abandon the load rather than complete it, or it proves \
             nothing about cancellation"
        );
        assert_eq!(
            migo_io::global_cache().pin_count(&key),
            0,
            "an abandoned load left its pin behind; those decoded bytes can now never \
             be evicted, trimmed or cleared"
        );
    }

    #[test]
    fn data_url_cache_identity_is_fixed_size_and_content_addressed() {
        let first = data_url_cache_identity("data:image/png;base64,AAAA");
        let same = data_url_cache_identity("data:image/png;base64,AAAA");
        let different = data_url_cache_identity("data:image/png;base64,AAAB");

        assert_eq!(first, same);
        assert_ne!(first, different);
        assert_eq!(first.len(), "data:sha256:".len() + 64);
    }

    #[test]
    fn companion_appearing_invalidates_variant_token() {
        let dir = std::env::temp_dir().join("migo_image_variant_token");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let png = dir.join("tex.png");
        let ktx2 = dir.join("tex.ktx2");
        std::fs::write(&png, b"png-v1").unwrap();

        // v1: only PNG exists.
        let v1 = variant_source_version_token(png.to_str().unwrap(), None, None);
        // v2: KTX2 companion appears (different file set = different token).
        std::fs::write(&ktx2, b"ktx2-v1").unwrap();
        let v2 = variant_source_version_token(png.to_str().unwrap(), None, None);

        assert_ne!(v1, v2, "adding a companion must change the version token");

        // v3: KTX2 companion removed.
        std::fs::remove_file(&ktx2).unwrap();
        let v3 = variant_source_version_token(png.to_str().unwrap(), None, None);
        assert_eq!(v1, v3, "removing companion should restore original token");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn companion_size_change_invalidates_variant_token() {
        let dir = std::env::temp_dir().join("migo_image_variant_token_size");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let png = dir.join("tex.png");
        let ktx2 = dir.join("tex.ktx2");
        std::fs::write(&png, b"png-v1").unwrap();
        std::fs::write(&ktx2, b"ktx2-v1").unwrap();
        let v1 = variant_source_version_token(png.to_str().unwrap(), None, None);

        // Write different-size content to companion.
        std::fs::write(&ktx2, b"ktx2-v2-much-larger-content").unwrap();
        let v2 = variant_source_version_token(png.to_str().unwrap(), None, None);

        assert_ne!(v1, v2, "companion size change must invalidate token");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extensionless_primary_size_change_invalidates_token() {
        let dir = std::env::temp_dir().join("migo_image_variant_token_extless");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let raw = dir.join("tex");
        std::fs::write(&raw, b"raw-v1-short").unwrap();
        let v1 = variant_source_version_token(raw.to_str().unwrap(), None, None);
        // Write different-size content.
        std::fs::write(&raw, b"raw-v2-much-longer-content-here").unwrap();
        let v2 = variant_source_version_token(raw.to_str().unwrap(), None, None);

        assert_ne!(v1, v2, "primary size change must invalidate token");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resized_rgba_cache_key_uses_decoded_source_generation() {
        let stale = resized_rgba_io_cache_key("/code/tex.png", Some(64), Some(64), 10);
        let refreshed = resized_rgba_io_cache_key("/code/tex.png", Some(64), Some(64), 20);

        assert_ne!(stale, refreshed);
        assert_eq!(refreshed.1, 20);
    }

    #[test]
    fn public_image_data_factory_has_html_dimension_semantics_and_allocation_caps() {
        let source = include_str!("01_image_data.js");
        assert!(source.contains("MAX_IMAGE_DATA_BYTES = 64 * 1024 * 1024"));
        assert!(source.contains("Math.abs(width)"));
        assert!(source.contains("\"IndexSizeError\""));
        assert!(source.contains("sourceLength !== dimensions.bytes"));
        assert!(!source.contains("Array.from(srcData)"));
    }
}
