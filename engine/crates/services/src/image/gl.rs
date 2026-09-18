//! WebGL uploads whose source is a loaded image: `texImage2D(…, image)` and
//! `texSubImage2D(…, image)`.
//!
//! The embedded runtime's ops and the external session's frame decoder both
//! turn such a call into a `GLCmd` here, so an image uploads the same way
//! whichever execution recorded the call: a GPU-side copy from the image's own
//! texture when the id names a live alias, the decoded bytes otherwise.

use std::sync::Arc;

use shared::protocol::render_cmd::GLCmd;
use tracing::warn;

use super::cache::{ImageCacheKey, SharedImageCache};

/// Result of trying to resolve RGBA bytes for an `image_id`.
///
/// Encodes the distinction between "the caller is referencing an id we've
/// never seen" and "we know the id but the bytes are missing" so the
/// miss-diagnostic log can tell the two failure modes apart.
pub enum RgbaLookup {
    Found {
        width: i32,
        height: i32,
        data: Arc<Vec<u8>>,
    },
    UnknownAlias,
    AliasKnownButEvicted {
        cache_key: ImageCacheKey,
    },
}

/// The decoded bytes behind `image_id`, from the decoded-bytes cache.
///
/// H-5: `migo_io::global_cache` is the single source of truth for decoded RGBA
/// bytes, with `pin()` / `unpin()` keeping actively referenced entries exempt
/// from LRU eviction. The alias table only says whether `image_id` is known at
/// all and which cache key it names; the byte lookup runs against the cache
/// directly. The alias-known-but-evicted branch therefore only fires when
/// something outside the pin path has cleared the LRU, which is worth a warn
/// rather than a silent black texture.
///
/// Both lookups run under this Session's alias lock, so the key is borrowed out
/// of the alias table rather than copied: an owned key here is a `String` clone
/// per call, and `texSubImage2D(image)` reaches this unconditionally.
///
/// **Lock order: this Session's alias table, then the process-wide
/// decoded-bytes cache.** That is the order every `pin`/`unpin` in `ImageCache`
/// already takes, and the reverse cannot be written: `migo-io` cannot reach an
/// alias table at all.
#[inline]
pub fn resolve_cached_image_rgba(
    aliases: &SharedImageCache,
    session: i32,
    image_id: u32,
) -> RgbaLookup {
    let aliases = aliases.lock();
    let Some(key) = aliases.cache_key_for_image_id(image_id) else {
        return RgbaLookup::UnknownAlias;
    };
    match migo_io::global_cache().get(key, session) {
        Some(entry) => {
            tracing::trace!(
                image_id,
                path = key.0.as_str(),
                gen = key.1,
                width = entry.width,
                height = entry.height,
                "resolve_cached_image_rgba hit"
            );
            RgbaLookup::Found {
                width: entry.width as i32,
                height: entry.height as i32,
                data: Arc::clone(&entry.rgba),
            }
        }
        // The miss path owns its key: it is a diagnostic that outlives the guard,
        // and it is not steady state.
        None => RgbaLookup::AliasKnownButEvicted {
            cache_key: key.clone(),
        },
    }
}

fn log_miss(op: &str, image_id: u32, lookup: &RgbaLookup) {
    match lookup {
        RgbaLookup::UnknownAlias => warn!("{op} miss (unknown alias): image_id={image_id}"),
        RgbaLookup::AliasKnownButEvicted { cache_key } => warn!(
            "{op} miss (bytes evicted): image_id={image_id}, src={}, gen={}",
            cache_key.0, cache_key.1
        ),
        RgbaLookup::Found { .. } => {}
    }
}

/// `texImage2D(target, level, internalformat, format, type, image)`.
///
/// A GPU-side copy from the image's texture when `image_id` is a live alias
/// (the texture is already in the render thread's store, so the CPU bytes are
/// never re-read -- what Chrome does for an `HTMLImageElement` it has promoted);
/// otherwise the decoded bytes. `None`, logged, for an id that names nothing.
#[allow(clippy::too_many_arguments)]
pub fn tex_image_2d_from_image(
    aliases: &SharedImageCache,
    session: i32,
    canvas_id: u32,
    target: u32,
    level: i32,
    internalformat: i32,
    format: u32,
    type_: u32,
    image_id: u32,
) -> Option<GLCmd> {
    let shared = aliases.lock().shared_for_image_id(image_id);
    if let Some((source_shared_id, (w, h))) = shared {
        return Some(GLCmd::TexImage2DFromShared {
            canvas_id,
            target,
            level,
            internalformat,
            format,
            type_,
            source_shared_id,
            src_width: w as i32,
            src_height: h as i32,
        });
    }
    match resolve_cached_image_rgba(aliases, session, image_id) {
        RgbaLookup::Found {
            width,
            height,
            data,
        } => Some(GLCmd::TexImage2D {
            canvas_id,
            target,
            level,
            internalformat,
            width,
            height,
            border: 0,
            format,
            type_,
            data: Some(data),
        }),
        miss => {
            log_miss("op_tex_image_2d_from_image", image_id, &miss);
            None
        }
    }
}

/// `texSubImage2D(target, level, x, y, format, type, image)`: always the
/// decoded bytes -- there is no GPU-side sub-copy command.
#[allow(clippy::too_many_arguments)]
pub fn tex_sub_image_2d_from_image(
    aliases: &SharedImageCache,
    session: i32,
    canvas_id: u32,
    target: u32,
    level: i32,
    xoffset: i32,
    yoffset: i32,
    format: u32,
    type_: u32,
    image_id: u32,
) -> Option<GLCmd> {
    match resolve_cached_image_rgba(aliases, session, image_id) {
        RgbaLookup::Found {
            width,
            height,
            data,
        } => Some(GLCmd::TexSubImage2D {
            canvas_id,
            target,
            level,
            xoffset,
            yoffset,
            width,
            height,
            format,
            type_,
            data,
        }),
        miss => {
            log_miss("op_tex_sub_image_2d_from_image", image_id, &miss);
            None
        }
    }
}
