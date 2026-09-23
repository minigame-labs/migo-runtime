//! The image ops: adapters over `migo_services::image`, which holds the work
//! -- path resolution, decoding, the alias table, uploads -- so the external
//! session's service dispatcher loads images by the same rules.

use std::{cell::RefCell, rc::Rc};

use deno_core::{OpState, extension, op2};
use deno_error::JsErrorBox;

use migo_services::image::{self as images, ImageEnv};
use shared::{
    error::EngineError,
    op_state::{CanvasOpState, HostOpState},
};

use crate::io_state::IoSchedulerState;

/// The alias table and its registry live in `migo_services`; re-exported under
/// the path the rest of this crate names them by.
pub(crate) use migo_services::image::cache;

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

#[inline]
fn js_err_from_engine(e: EngineError) -> JsErrorBox {
    match &e.detail {
        Some(d) => JsErrorBox::generic(format!("[{:?}] {} ({})", e.code, e.msg, d)),
        None => JsErrorBox::generic(format!("[{:?}] {}", e.code, e.msg)),
    }
}

/// The handles a load reads, taken out of op state in one borrow.
fn image_env(state: &OpState) -> ImageEnv {
    let host = state.borrow::<HostOpState>();
    let images = state.borrow::<ImageCacheState>();
    ImageEnv {
        scheduler: state.borrow::<IoSchedulerState>().0.clone(),
        vfs: host.vfs.clone(),
        mount_table: host.mount_table.clone(),
        game_cache_dir: host
            .game_paths
            .as_ref()
            .map(|gp| gp.cache_dir().to_string_lossy().into_owned()),
        gpu_caps: host.gpu_caps.clone(),
        cpu_backing_required: host.webgl_context_created.clone(),
        canvas: state.borrow::<CanvasOpState>().clone(),
        aliases: images.aliases.clone(),
        session: images.session,
    }
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

#[op2(async(lazy), fast)]
#[serde]
pub async fn op_load_image(
    state: Rc<RefCell<OpState>>,
    #[smi] image_id: u32,
    #[string] src: String,
    #[smi] target_width: u32,
    #[smi] target_height: u32,
) -> Result<(u32, (usize, usize)), JsErrorBox> {
    let tw = (target_width > 0).then_some(target_width);
    let th = (target_height > 0).then_some(target_height);
    let started_at = std::time::Instant::now();
    let env = image_env(&state.borrow());
    // The fetch is the service's, under this session's policy and this
    // runtime's shared client: `Image.src = "https://..."` is held to exactly
    // what `fetch()` is.
    let (policy, client) = {
        let mut st = state.borrow_mut();
        let policy = st.borrow::<shared::op_state::HostOpState>().network_policy.clone();
        let client = crate::network::fetch::get_or_create_client_from_state(&mut st, false);
        (policy, client)
    };
    let result = images::load_image(&env, image_id, src.clone(), tw, th, move |url| async move {
        let client = client.map_err(|error| {
            shared::error::EngineError::new(shared::error::ErrorCode::IoError)
                .with_msg("http client not available")
                .with_detail(error.to_string())
        })?;
        migo_services::network::image_source::fetch_http_image(&policy, &client, &url).await
    })
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
/// backend; see `migo_services::image::load_image_subrect`.
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
    let env = image_env(&state.borrow());
    images::load_image_subrect(&env, image_id, src, sx, sy, sw, sh, resize_w, resize_h)
        .await
        .map_err(js_err_from_engine)
}

#[op2(fast)]
pub fn op_destroy_image(state: &mut OpState, #[smi] image_id: u32) -> bool {
    let aliases = state.borrow::<ImageCacheState>().aliases.clone();
    let ctx = state.borrow::<CanvasOpState>();
    images::destroy_image(&aliases, &ctx.tx, image_id);
    true
}

/// Preload multiple images in parallel
/// Returns array of [path, success, width, height, error_msg]
#[op2(async(lazy))]
#[serde]
pub async fn op_preload_images(
    state: Rc<RefCell<OpState>>,
    #[serde] paths: Vec<String>,
) -> Result<Vec<images::PreloadEntry>, JsErrorBox> {
    let env = image_env(&state.borrow());
    Ok(images::preload_images(&env, paths).await)
}

/// Clear all image caches: JS shared cache + IO in-memory cache + derived disk cache.
#[op2(fast)]
pub fn op_clear_image_cache(state: &mut OpState) -> Result<(), JsErrorBox> {
    let gcd = {
        let host = state.borrow::<HostOpState>();
        host.game_paths
            .as_ref()
            .map(|gp| gp.cache_dir().to_string_lossy().into_owned())
    };
    let render_tx = state.borrow::<HostOpState>().render_tx.clone();
    let images = state.borrow::<ImageCacheState>();
    images::clear_image_cache(&images.aliases, &render_tx, gcd.as_deref(), images.session);
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
