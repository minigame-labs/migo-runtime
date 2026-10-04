//! WebGL uploads whose source is a loaded image: `texImage2D(…, image)`,
//! `texSubImage2D(…, image)` and their 3D forms.
//!
//! The embedded runtime's op and the external session's frame decoder both
//! resolve the image here, so an image uploads the same way whichever execution
//! recorded the call: from the image's own texture or its decoded bytes,
//! whichever the renderer finds exact and cheapest.

use std::sync::Arc;

use shared::protocol::render_cmd::TextureSource;
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

/// The source a `tex*Image*` call naming `image_id` uploads from: the image store's texture of it when the id names a
/// live alias, its decoded bytes when the cache still holds them, and both when both are there
/// ([`TextureSource::Image`]). `None`, logged, for an id that names neither.
pub fn texture_source(
    aliases: &SharedImageCache,
    session: i32,
    image_id: u32,
) -> Option<TextureSource> {
    let shared = aliases.lock().shared_for_image_id(image_id);
    let lookup = resolve_cached_image_rgba(aliases, session, image_id);
    match (shared, lookup) {
        (
            shared,
            RgbaLookup::Found {
                width,
                height,
                data,
            },
        ) => Some(TextureSource::Image {
            shared_id: shared.map(|(id, _)| id),
            pixels: Some(data),
            width: width as u32,
            height: height as u32,
        }),
        (Some((shared_id, (width, height))), _) => Some(TextureSource::Image {
            shared_id: Some(shared_id),
            pixels: None,
            width: u32::try_from(width).ok()?,
            height: u32::try_from(height).ok()?,
        }),
        (None, miss) => {
            log_miss("texture_source", image_id, &miss);
            None
        }
    }
}
