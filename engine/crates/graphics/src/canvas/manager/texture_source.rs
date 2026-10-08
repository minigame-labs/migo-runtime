//! WebGL uploads whose pixels are a TexImageSource -- a decoded image, a 2D canvas, a snapshot of one, a cached text
//! texture, `ImageData` -- into the texture a WebGL context has bound ([`GLCmd::TexImageSource`]).
//!
//! One path for every source and every call, so each is converted the same way: the pixels the call selects
//! (WebGL 2's UNPACK_SKIP_PIXELS / UNPACK_SKIP_ROWS / UNPACK_IMAGE_HEIGHT / UNPACK_SKIP_IMAGES), in the row order
//! UNPACK_FLIP_Y_WEBGL asks for, with the alpha UNPACK_PREMULTIPLY_ALPHA_WEBGL asks for, packed as the call's format
//! and type ([`pack_source`]). Two ways there:
//!
//! - **A GPU copy**, from a source the renderer holds as a texture, when a copy reproduces the conversion exactly:
//!   nothing to flip, no alpha to change, and a destination of the source's own 8-bit components
//!   ([`copy_reproduces`]). No pixel leaves the GPU -- what keeps a canvas drawn and uploaded every frame cheap.
//! - **The converted bytes** otherwise: a source's own bytes when it has them, the texture read back when it has not
//!   (only the rows the call selects), packed and uploaded with a tight pixel-store state.
//!
//! [`GLCmd::TexImageSource`]: shared::protocol::render_cmd::GLCmd::TexImageSource

use glow::HasContext;
use shared::protocol::render_cmd::CanvasId;
use shared::protocol::render_cmd::{SourceUploadCall, TextureSource};

use super::{
    CanvasManager, NativeTextureFromRawShim, PIXEL_STORE_FLIP_Y, PIXEL_STORE_PREMULTIPLY_ALPHA,
};
use crate::backend::gl::readback::CompactPixelUnpackGuard;
use crate::backend::gl::texture_copy::{TextureCopy, copy_texture, read_texture_rgba8};
use crate::backend::gl::unpack_convert::{SourceAlpha, SourceConversion, SourceRect, pack_source};
use crate::backend::gl::webgl1_formats;
use shared::error::EngineResult;

/// A `tex*Image*` call whose pixels are a TexImageSource, as the command carries it: everything but the source.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SourceUpload {
    pub target: u32,
    pub level: i32,
    pub call: SourceUploadCall,
    pub width: i32,
    pub height: i32,
    pub format: u32,
    pub type_: u32,
    /// A sub call's image's effective internal format, as the facade recorded it; 0 for a full call.
    pub destination_format: u32,
}

impl SourceUpload {
    /// The internal format the pixels end in: a full call's, or the image's a sub call fills.
    fn destination(&self) -> u32 {
        match self.call {
            SourceUploadCall::Image2D { internalformat }
            | SourceUploadCall::Image3D { internalformat, .. } => internalformat as u32,
            SourceUploadCall::SubImage2D { .. } | SourceUploadCall::SubImage3D { .. } => {
                self.destination_format
            }
        }
    }

    fn is_3d(&self) -> bool {
        matches!(
            self.call,
            SourceUploadCall::Image3D { .. } | SourceUploadCall::SubImage3D { .. }
        )
    }
}

/// A source the renderer holds as a texture of the uploading context: the region of `texture` from (`x`, `y`), the
/// source's top row at `y`, `width` x `height`.
#[derive(Clone, Copy, Debug)]
struct TextureRegion {
    texture: glow::NativeTexture,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    alpha: SourceAlpha,
    /// Every pixel opaque, so straight and premultiplied are the same pixels.
    opaque: bool,
}

/// Whether a GPU copy of 8-bit RGBA pixels into an image of `destination` -- an internal format, sized or unsized --
/// produces exactly what packing them as (`format`, `type_`) and uploading them would: the destination takes the
/// source's own 8-bit components, linear. A packed or float type, an sRGB, integer or 16-bit destination is not a
/// copy (ES 3.0 table 3.15 refuses most of those, and the others round differently).
fn copy_reproduces(format: u32, type_: u32, destination: u32) -> bool {
    const R8: u32 = 0x8229;
    const RG8: u32 = 0x822B;
    const RED: u32 = 0x1903;
    const RG: u32 = 0x8227;
    type_ == glow::UNSIGNED_BYTE
        && matches!(
            (format, destination),
            (glow::RGBA, glow::RGBA | glow::RGBA8)
                | (glow::RGB, glow::RGB | glow::RGB8)
                | (RG, RG8)
                | (RED, R8)
                | (glow::LUMINANCE, glow::LUMINANCE)
                | (glow::ALPHA, glow::ALPHA)
                | (glow::LUMINANCE_ALPHA, glow::LUMINANCE_ALPHA)
        )
}

/// The rows of a `source_height`-tall source `rect` reads, as the source holds them: the first, counted from the top,
/// and how many; and `rect` as it reads those rows as a source of their own. Only those rows are read back.
fn source_window(rect: &SourceRect, source_height: u32, flip_y: bool) -> (u32, u32, SourceRect) {
    let stride = if rect.image_height == 0 {
        rect.height
    } else {
        rect.image_height
    };
    let depth = rect.depth.max(1);
    let first = rect.y + rect.skip_images * stride;
    let last = rect.y + (rect.skip_images + depth - 1) * stride + rect.height;
    let rows = last - first;
    let top = if flip_y { source_height - last } else { first };
    let window = SourceRect {
        x: 0,
        y: 0,
        width: rect.width,
        height: rect.height,
        depth,
        image_height: stride,
        skip_images: 0,
    };
    (top, rows, window)
}

impl CanvasManager {
    /// [`GLCmd::TexImageSource`] on `canvas_id`: the pixels of `source` the call selects, converted as it asks, into
    /// the texture bound to its target. `false` when there was nothing to upload -- a source that is gone (an image
    /// evicted, a snapshot drained, a text-cache entry evicted despite its pin), a selection the source does not hold
    /// (the facade refuses those), a pair no TexImageSource is packed as -- which is logged and leaves the texture as
    /// it was.
    ///
    /// [`GLCmd::TexImageSource`]: shared::protocol::render_cmd::GLCmd::TexImageSource
    pub(crate) fn upload_texture_source(
        &mut self,
        canvas_id: CanvasId,
        upload: &SourceUpload,
        source: &TextureSource,
    ) -> EngineResult<bool> {
        self.make_current_needed(canvas_id)?;
        let (rect, conversion) = self.source_selection(canvas_id, upload);
        let uploaded = match source {
            TextureSource::Pixels {
                bytes,
                width,
                height,
            } => self.upload_packed(upload, bytes, *width, *height, &rect, &conversion),
            TextureSource::Image {
                shared_id,
                pixels,
                width,
                height,
            } => {
                let region = shared_id.and_then(|id| self.shared_image_region(id, *width, *height));
                match (region, pixels) {
                    (Some(region), _)
                        if self.copy_from_texture(
                            canvas_id,
                            upload,
                            &region,
                            &rect,
                            &conversion,
                        )? =>
                    {
                        true
                    }
                    (_, Some(bytes)) => {
                        self.upload_packed(upload, bytes, *width, *height, &rect, &conversion)
                    }
                    (Some(region), None) => {
                        self.upload_read_back(canvas_id, upload, &region, &rect, &conversion)?
                    }
                    (None, None) => false,
                }
            }
            TextureSource::Snapshot { snapshot_id } => {
                let Some(entry) = self
                    .canvas2d_snapshots
                    .get(snapshot_id)
                    .map(|e| (e.tex, e.width, e.height))
                else {
                    tracing::warn!(
                        "TexImageSource: snapshot {snapshot_id} is not in the pool (drained?)"
                    );
                    return Ok(false);
                };
                let region = premultiplied_region(entry.0, entry.1, entry.2);
                self.upload_from_texture(canvas_id, upload, &region, &rect, &conversion)?
            }
            TextureSource::TextCache { key } => {
                let entry = {
                    let mut cache = self.text_cache.lock();
                    let lookup = cache
                        .get(key)
                        .map(|entry| (entry.texture_id, entry.width, entry.height));
                    // The pin the facade took when it chose this source is given back once, hit or miss.
                    cache.unpin(key);
                    lookup
                };
                let Some((raw, width, height)) = entry else {
                    crate::render_diagnostics::miss_text_cache();
                    tracing::warn!(
                        "TexImageSource: text-cache entry missing at execution (pin or eviction race?)"
                    );
                    return Ok(false);
                };
                let Some(texture) =
                    <glow::NativeTexture as NativeTextureFromRawShim>::try_from_raw(raw)
                else {
                    return Ok(false);
                };
                crate::render_diagnostics::hit_text_cache();
                let region = premultiplied_region(texture, width, height);
                self.upload_from_texture(canvas_id, upload, &region, &rect, &conversion)?
            }
            TextureSource::Canvas { canvas_2d_id } => {
                self.upload_from_canvas(canvas_id, *canvas_2d_id, upload, &rect, &conversion)?
            }
        };
        if uploaded {
            if let SourceUploadCall::Image2D { internalformat } = upload.call {
                if upload.level == 0 {
                    let swizzle =
                        webgl1_formats::driver_format(internalformat, upload.format, upload.type_)
                            .and_then(|driver| driver.swizzle);
                    self.set_webgl1_swizzle(canvas_id, upload.target, swizzle);
                }
            }
            self.mark_all_2d_contexts_stale_bits(
                crate::backend::gl::surface::gr_state_bits::TEXTURE_BINDING,
            );
        }
        Ok(uploaded)
    }

    /// The pixels `upload` selects and how they are converted, from `canvas_id`'s pixel-store state as the renderer
    /// holds it when the command runs: the skips (WebGL 2's; a WebGL 1 context never sets them) select, a 3D call
    /// slices by UNPACK_IMAGE_HEIGHT from UNPACK_SKIP_IMAGES, and the WebGL flags convert.
    fn source_selection(
        &self,
        canvas_id: CanvasId,
        upload: &SourceUpload,
    ) -> (SourceRect, SourceConversion) {
        let value = |name: u32| {
            self.gl_state
                .get(&canvas_id)
                .map_or(0, |state| state.pixel_store_i32.value(name))
        };
        let count = |name: u32| u32::try_from(value(name)).unwrap_or(0);
        let three_d = upload.is_3d();
        let rect = SourceRect {
            x: count(glow::UNPACK_SKIP_PIXELS),
            y: count(glow::UNPACK_SKIP_ROWS),
            width: u32::try_from(upload.width).unwrap_or(0),
            height: u32::try_from(upload.height).unwrap_or(0),
            depth: u32::try_from(upload.call.depth()).unwrap_or(0),
            image_height: if three_d {
                count(glow::UNPACK_IMAGE_HEIGHT)
            } else {
                0
            },
            skip_images: if three_d {
                count(glow::UNPACK_SKIP_IMAGES)
            } else {
                0
            },
        };
        let conversion = SourceConversion {
            source_alpha: SourceAlpha::Straight,
            premultiply: value(PIXEL_STORE_PREMULTIPLY_ALPHA) != 0,
            flip_y: value(PIXEL_STORE_FLIP_Y) != 0,
            format: upload.format,
            type_: upload.type_,
        };
        (rect, conversion)
    }

    /// The image store's texture of shared image `shared_id`, an atlas page's entry included, as a source region.
    fn shared_image_region(
        &self,
        shared_id: u32,
        width: u32,
        height: u32,
    ) -> Option<TextureRegion> {
        let stored = self.image_registry.get_shared_texture(shared_id)?;
        let texture =
            <glow::NativeTexture as NativeTextureFromRawShim>::try_from_raw(stored.gl_texture)?;
        let (x, y) = stored
            .atlas_origin
            .map_or((0, 0), |(x, y)| (i32::from(x), i32::from(y)));
        Some(TextureRegion {
            texture,
            x,
            y,
            width,
            height,
            alpha: match stored.info.alpha_type {
                skia_safe::AlphaType::Premul => SourceAlpha::Premultiplied,
                _ => SourceAlpha::Straight,
            },
            opaque: stored.info.alpha_type == skia_safe::AlphaType::Opaque,
        })
    }

    /// A 2D canvas as it is now: the rows the call selects captured into a snapshot texture of their own, uploaded
    /// from, and freed. The capture flushes the canvas's pending drawing first.
    fn upload_from_canvas(
        &mut self,
        canvas_id: CanvasId,
        canvas_2d_id: CanvasId,
        upload: &SourceUpload,
        rect: &SourceRect,
        conversion: &SourceConversion,
    ) -> EngineResult<bool> {
        let Some((width, height)) = self
            .contexts_2d
            .get(&canvas_2d_id)
            .map(|ctx| (ctx.width, ctx.height))
        else {
            tracing::warn!("TexImageSource: canvas {canvas_2d_id} has no 2D surface");
            return Ok(false);
        };
        if !rect.fits(width, height) {
            tracing::warn!(
                "TexImageSource: the selection is outside canvas {canvas_2d_id}'s {width}x{height}"
            );
            return Ok(false);
        }
        let (top, rows, window) = source_window(rect, height, conversion.flip_y);
        let id = Self::DIRECT_CANVAS2D_RESERVED_ID;
        // A capture an earlier upload failed to free.
        if let Some(entry) = self.remove_canvas2d_snapshot(id) {
            unsafe { self.gl.delete_texture(entry.tex) };
        }
        let captured = self.snapshot_canvas2d_region_with_id(
            canvas_2d_id,
            rect.x as i32,
            top as i32,
            rect.width,
            rows,
            id,
        )?;
        if captured == 0 {
            tracing::warn!(
                "TexImageSource: canvas {canvas_2d_id} could not be captured (snapshot pool full?)"
            );
            return Ok(false);
        }
        self.make_current_needed(canvas_id)?;
        let uploaded = match self.canvas2d_snapshots.get(&id).map(|entry| entry.tex) {
            Some(texture) => {
                let region = premultiplied_region(texture, rect.width, rows);
                self.upload_from_texture(canvas_id, upload, &region, &window, conversion)?
            }
            None => false,
        };
        if let Some(entry) = self.remove_canvas2d_snapshot(id) {
            unsafe { self.gl.delete_texture(entry.tex) };
        }
        crate::render_diagnostics::bump_canvas2d_snapshot_upload();
        Ok(uploaded)
    }

    /// A source held only as a texture: copied when a copy is exact, read back and converted otherwise.
    fn upload_from_texture(
        &mut self,
        canvas_id: CanvasId,
        upload: &SourceUpload,
        region: &TextureRegion,
        rect: &SourceRect,
        conversion: &SourceConversion,
    ) -> EngineResult<bool> {
        if self.copy_from_texture(canvas_id, upload, region, rect, conversion)? {
            return Ok(true);
        }
        self.upload_read_back(canvas_id, upload, region, rect, conversion)
    }

    /// The GPU copy, when it reproduces the conversion exactly: a 2D call, nothing to flip, no alpha to change, and a
    /// destination of the source's own 8-bit components ([`copy_reproduces`]). `false`, nothing done, otherwise.
    fn copy_from_texture(
        &mut self,
        canvas_id: CanvasId,
        upload: &SourceUpload,
        region: &TextureRegion,
        rect: &SourceRect,
        conversion: &SourceConversion,
    ) -> EngineResult<bool> {
        let changes_alpha = !region.opaque
            && conversion.premultiply != (region.alpha == SourceAlpha::Premultiplied);
        if upload.is_3d()
            || conversion.flip_y
            || changes_alpha
            || !copy_reproduces(upload.format, upload.type_, upload.destination())
            || !rect.fits(region.width, region.height)
        {
            return Ok(false);
        }
        let framebuffer = self.ensure_image_copy_fbo(canvas_id)?;
        let (source_x, source_y) = (region.x + rect.x as i32, region.y + rect.y as i32);
        let copy = match upload.call {
            SourceUploadCall::Image2D { internalformat } => TextureCopy::Image {
                target: upload.target,
                level: upload.level,
                internal_format: internalformat as u32,
                source_x,
                source_y,
                width: upload.width,
                height: upload.height,
            },
            SourceUploadCall::SubImage2D { xoffset, yoffset } => TextureCopy::SubImage {
                target: upload.target,
                level: upload.level,
                xoffset,
                yoffset,
                source_x,
                source_y,
                width: upload.width,
                height: upload.height,
            },
            SourceUploadCall::Image3D { .. } | SourceUploadCall::SubImage3D { .. } => {
                return Ok(false);
            }
        };
        let status = copy_texture(&self.gl, framebuffer, region.texture, copy);
        if status != glow::FRAMEBUFFER_COMPLETE {
            tracing::warn!(
                "TexImageSource: the source cannot be read as a framebuffer (0x{status:X})"
            );
            return Ok(false);
        }
        Ok(true)
    }

    /// The rows the call selects read back from the source's texture, then converted as bytes.
    fn upload_read_back(
        &mut self,
        canvas_id: CanvasId,
        upload: &SourceUpload,
        region: &TextureRegion,
        rect: &SourceRect,
        conversion: &SourceConversion,
    ) -> EngineResult<bool> {
        if !rect.fits(region.width, region.height) {
            tracing::warn!(
                "TexImageSource: the selection is outside its {}x{} source",
                region.width,
                region.height
            );
            return Ok(false);
        }
        let (top, rows, window) = source_window(rect, region.height, conversion.flip_y);
        let framebuffer = self.ensure_image_copy_fbo(canvas_id)?;
        let Some(pixels) = read_texture_rgba8(
            &self.gl,
            framebuffer,
            region.texture,
            region.x + rect.x as i32,
            region.y + top as i32,
            rect.width as i32,
            rows as i32,
        ) else {
            tracing::warn!("TexImageSource: the source cannot be read back");
            return Ok(false);
        };
        let conversion = SourceConversion {
            source_alpha: if region.opaque {
                SourceAlpha::Straight
            } else {
                region.alpha
            },
            ..*conversion
        };
        Ok(self.upload_packed(upload, &pixels, rect.width, rows, &window, &conversion))
    }

    /// `rgba` -- straight unless `conversion` says otherwise, `width` x `height`, top row first -- packed as the call
    /// asks ([`pack_source`]) and uploaded with tight rows: UNPACK_ALIGNMENT 1, no row length, skips or image height,
    /// and no PIXEL_UNPACK_BUFFER, whatever the content set, which describe its own bytes and not these.
    fn upload_packed(
        &self,
        upload: &SourceUpload,
        rgba: &[u8],
        width: u32,
        height: u32,
        rect: &SourceRect,
        conversion: &SourceConversion,
    ) -> bool {
        // Packed as WebGL names the format (EXT_sRGB's as the bytes they hold), uploaded as the driver takes it
        // (`webgl1_formats`).
        let packing = SourceConversion {
            format: webgl1_formats::source_pack_format(conversion.format),
            ..*conversion
        };
        let Some(packed) = pack_source(rgba, width, height, rect, &packing) else {
            tracing::warn!(
                "TexImageSource: {}x{} as format 0x{:X} type 0x{:X} from a {width}x{height} source cannot be packed",
                upload.width,
                upload.height,
                upload.format,
                upload.type_
            );
            return false;
        };
        let gl = &self.gl;
        let _unpack = CompactPixelUnpackGuard::new(gl, 1);
        let _volume = upload.is_3d().then(|| TightVolumeUnpack::new(gl));
        let pixels = glow::PixelUnpackData::Slice(Some(&packed));
        let (w, h) = (upload.width, upload.height);
        let (format, type_) = webgl1_formats::driver_sub_format(upload.format, upload.type_);
        let driver_internalformat = |internalformat: i32| {
            webgl1_formats::driver_format(internalformat, upload.format, upload.type_)
                .map_or(internalformat, |driver| driver.internalformat)
        };
        unsafe {
            match upload.call {
                SourceUploadCall::Image2D { internalformat } => gl.tex_image_2d(
                    upload.target,
                    upload.level,
                    driver_internalformat(internalformat),
                    w,
                    h,
                    0,
                    format,
                    type_,
                    pixels,
                ),
                SourceUploadCall::SubImage2D { xoffset, yoffset } => gl.tex_sub_image_2d(
                    upload.target,
                    upload.level,
                    xoffset,
                    yoffset,
                    w,
                    h,
                    format,
                    type_,
                    pixels,
                ),
                SourceUploadCall::Image3D {
                    internalformat,
                    depth,
                } => gl.tex_image_3d(
                    upload.target,
                    upload.level,
                    driver_internalformat(internalformat),
                    w,
                    h,
                    depth,
                    0,
                    format,
                    type_,
                    pixels,
                ),
                SourceUploadCall::SubImage3D {
                    xoffset,
                    yoffset,
                    zoffset,
                    depth,
                } => gl.tex_sub_image_3d(
                    upload.target,
                    upload.level,
                    xoffset,
                    yoffset,
                    zoffset,
                    w,
                    h,
                    depth,
                    format,
                    type_,
                    pixels,
                ),
            }
        }
        true
    }
}

/// A texture the renderer rendered with Skia -- a snapshot, a cached text texture, a captured canvas --, whole:
/// premultiplied.
fn premultiplied_region(texture: glow::NativeTexture, width: u32, height: u32) -> TextureRegion {
    TextureRegion {
        texture,
        x: 0,
        y: 0,
        width,
        height,
        alpha: SourceAlpha::Premultiplied,
        opaque: false,
    }
}

/// UNPACK_IMAGE_HEIGHT and UNPACK_SKIP_IMAGES held at 0 for a 3D upload of tight bytes, and given back after:
/// [`CompactPixelUnpackGuard`] holds the four a 2D upload reads.
struct TightVolumeUnpack<'a> {
    gl: &'a glow::Context,
    image_height: i32,
    skip_images: i32,
}

impl<'a> TightVolumeUnpack<'a> {
    fn new(gl: &'a glow::Context) -> Self {
        // SAFETY: the uploading context is current, and a 3D upload means GLES 3.0, which has both.
        let (image_height, skip_images) = unsafe {
            (
                gl.get_parameter_i32(glow::UNPACK_IMAGE_HEIGHT),
                gl.get_parameter_i32(glow::UNPACK_SKIP_IMAGES),
            )
        };
        unsafe {
            if image_height != 0 {
                gl.pixel_store_i32(glow::UNPACK_IMAGE_HEIGHT, 0);
            }
            if skip_images != 0 {
                gl.pixel_store_i32(glow::UNPACK_SKIP_IMAGES, 0);
            }
        }
        Self {
            gl,
            image_height,
            skip_images,
        }
    }
}

impl Drop for TightVolumeUnpack<'_> {
    fn drop(&mut self) {
        unsafe {
            if self.image_height != 0 {
                self.gl
                    .pixel_store_i32(glow::UNPACK_IMAGE_HEIGHT, self.image_height);
            }
            if self.skip_images != 0 {
                self.gl
                    .pixel_store_i32(glow::UNPACK_SKIP_IMAGES, self.skip_images);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{copy_reproduces, source_window};
    use crate::backend::gl::unpack_convert::SourceRect;

    #[test]
    fn only_a_destination_of_the_source_s_own_8_bit_components_is_a_copy() {
        assert!(copy_reproduces(glow::RGBA, glow::UNSIGNED_BYTE, glow::RGBA));
        assert!(copy_reproduces(
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::RGBA8
        ));
        assert!(copy_reproduces(
            glow::LUMINANCE,
            glow::UNSIGNED_BYTE,
            glow::LUMINANCE
        ));
        assert!(copy_reproduces(0x1903, glow::UNSIGNED_BYTE, 0x8229)); // RED into R8
        // Packed, float, sRGB and a format of other components are conversions.
        assert!(!copy_reproduces(glow::RGBA, 0x8033, glow::RGBA));
        assert!(!copy_reproduces(glow::RGBA, glow::HALF_FLOAT, 0x881A));
        assert!(!copy_reproduces(glow::RGBA, glow::UNSIGNED_BYTE, 0x8C43)); // SRGB8_ALPHA8
        assert!(!copy_reproduces(
            glow::RGB,
            glow::UNSIGNED_BYTE,
            glow::RGBA8
        ));
        // A sub call whose image the facade could not name is not copied into.
        assert!(!copy_reproduces(glow::RGBA, glow::UNSIGNED_BYTE, 0));
    }

    #[test]
    fn only_the_rows_a_selection_reads_are_read_back() {
        let rect = SourceRect {
            x: 1,
            y: 2,
            width: 2,
            height: 3,
            depth: 1,
            image_height: 0,
            skip_images: 0,
        };
        // Rows 2..5 of a 10-row source, read as rows 0..3 of their own.
        let (top, rows, window) = source_window(&rect, 10, false);
        assert_eq!((top, rows), (2, 3));
        assert_eq!((window.x, window.y, window.height), (0, 0, 3));
        // Flipped, the selection is of the turned source: its rows 2..5 are the source's 5..8.
        assert_eq!(source_window(&rect, 10, true).0, 5);
        // Slices: two of 1 row, 3 apart, from slice 1 -- rows 3 and 6, so 3..7.
        let volume = SourceRect {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            depth: 2,
            image_height: 3,
            skip_images: 1,
        };
        let (top, rows, window) = source_window(&volume, 10, false);
        assert_eq!(
            (top, rows, window.image_height, window.skip_images),
            (3, 4, 3, 0)
        );
    }
}
