//! Pixel-store ownership for engine-owned transfers. User `PACK_*`/`UNPACK_*`
//! state belongs to content; a compact temporary must neither inherit it nor
//! leave it changed. Both directions have the same four parameters, the same
//! capability gating and the same restore obligation, so they share one
//! implementation and differ only in which enums name them.

use glow::HasContext;
use shared::{
    error::{EngineError, EngineResult, ErrorCode},
    protocol::{
        pixel_pack::{PixelPackError, PixelPackLayout},
        render_cmd::{
            ReadPixelsData, checked_canvas_rgba_byte_len, webgl_readback_bytes_per_pixel,
            webgl_readback_type_bytes,
        },
    },
};

/// Alignment, row length, skipped rows, skipped pixels — in that order, which
/// is also the order [`PixelStoreState::transition_tight`] pairs against its
/// tight values, so the alignment slot must stay first.
pub(crate) trait PixelStoreDomain {
    const NAMES: [u32; 4];
    const BUFFER: u32;
    const BUFFER_BINDING: u32;
    /// GLES2 exposes only alignment; this extension adds the other three.
    const SUBIMAGE_EXTENSION: &'static str;
}

pub(crate) struct Pack;

impl PixelStoreDomain for Pack {
    const NAMES: [u32; 4] = [
        glow::PACK_ALIGNMENT,
        glow::PACK_ROW_LENGTH,
        glow::PACK_SKIP_ROWS,
        glow::PACK_SKIP_PIXELS,
    ];
    const BUFFER: u32 = glow::PIXEL_PACK_BUFFER;
    const BUFFER_BINDING: u32 = glow::PIXEL_PACK_BUFFER_BINDING;
    const SUBIMAGE_EXTENSION: &'static str = "GL_NV_pack_subimage";
}

pub(crate) struct Unpack;

impl PixelStoreDomain for Unpack {
    const NAMES: [u32; 4] = [
        glow::UNPACK_ALIGNMENT,
        glow::UNPACK_ROW_LENGTH,
        glow::UNPACK_SKIP_ROWS,
        glow::UNPACK_SKIP_PIXELS,
    ];
    const BUFFER: u32 = glow::PIXEL_UNPACK_BUFFER;
    const BUFFER_BINDING: u32 = glow::PIXEL_UNPACK_BUFFER_BINDING;
    const SUBIMAGE_EXTENSION: &'static str = "GL_EXT_unpack_subimage";
}

pub(crate) type PixelPackState = PixelStoreState<Pack>;
pub(crate) type PixelUnpackState = PixelStoreState<Unpack>;

pub(crate) struct PixelStoreState<D: PixelStoreDomain> {
    values: [i32; 4],
    buffer: Option<glow::NativeBuffer>,
    has_subimage: bool,
    has_buffer: bool,
    domain: std::marker::PhantomData<D>,
}

impl<D: PixelStoreDomain> PixelStoreState<D> {
    /// The calling thread must keep this context current through restore.
    pub(crate) fn capture(gl: &glow::Context) -> Self {
        // glow caches version/extensions when the context is created. These
        // capability checks neither query the driver nor allocate on each read.
        let version = gl.version();
        let extensions = gl.supported_extensions();
        let has_subimage = !version.is_embedded
            || version.major >= 3
            || extensions.contains(D::SUBIMAGE_EXTENSION);
        let has_buffer = version.major >= 3
            || (!version.is_embedded && (version.major, version.minor) >= (2, 1))
            || extensions.contains("GL_NV_pixel_buffer_object")
            || extensions.contains("GL_ARB_pixel_buffer_object")
            || extensions.contains("GL_EXT_pixel_buffer_object");
        unsafe {
            let mut values = [0; 4];
            for (value, name) in
                values
                    .iter_mut()
                    .zip(D::NAMES)
                    .take(if has_subimage { 4 } else { 1 })
            {
                *value = gl.get_parameter_i32(name);
            }
            Self {
                values,
                buffer: has_buffer
                    .then(|| gl.get_parameter_buffer(D::BUFFER_BINDING))
                    .flatten(),
                has_subimage,
                has_buffer,
                domain: std::marker::PhantomData,
            }
        }
    }

    /// Establish tightly packed rows at the caller's known alignment (1 or 4).
    pub(crate) fn set_tight(&self, gl: &glow::Context, alignment: i32) {
        self.transition_tight(gl, alignment, false);
    }

    // Only the values changed by set_tight need restoring when the intervening
    // operation cannot mutate pixel-store state itself, as readPixels and
    // NULL-data reallocation cannot.
    fn transition_tight(&self, gl: &glow::Context, alignment: i32, restore: bool) {
        for ((name, saved), tight) in D::NAMES
            .into_iter()
            .zip(self.values)
            .zip([alignment, 0, 0, 0])
            .take(if self.has_subimage { 4 } else { 1 })
        {
            if saved != tight {
                unsafe { gl.pixel_store_i32(name, if restore { saved } else { tight }) };
            }
        }
        if self.buffer.is_some() {
            unsafe {
                gl.bind_buffer(D::BUFFER, if restore { self.buffer } else { None });
            }
        }
    }

    /// Restore after external GL use, which may have changed every slot.
    pub(crate) fn restore(&self, gl: &glow::Context) {
        unsafe {
            for (name, value) in D::NAMES
                .into_iter()
                .zip(self.values)
                .take(if self.has_subimage { 4 } else { 1 })
            {
                gl.pixel_store_i32(name, value);
            }
            if self.has_buffer {
                gl.bind_buffer(D::BUFFER, self.buffer);
            }
        }
    }
}

/// Compact pixel-store scope. Restores exactly what it changed, so a nested
/// scope over an unchanged state issues no driver call at all.
pub(crate) struct CompactPixelStoreGuard<'a, D: PixelStoreDomain> {
    gl: &'a glow::Context,
    saved: PixelStoreState<D>,
    alignment: i32,
}

pub(crate) type CompactPixelUnpackGuard<'a> = CompactPixelStoreGuard<'a, Unpack>;
type CompactPixelPackGuard<'a> = CompactPixelStoreGuard<'a, Pack>;

#[cfg(all(test, target_os = "linux"))]
#[path = "readback_native_test.rs"]
mod native_tests;

impl<'a, D: PixelStoreDomain> CompactPixelStoreGuard<'a, D> {
    pub(crate) fn new(gl: &'a glow::Context, alignment: i32) -> Self {
        let saved = PixelStoreState::capture(gl);
        Self::from_saved(gl, saved, alignment)
    }

    fn from_saved(gl: &'a glow::Context, saved: PixelStoreState<D>, alignment: i32) -> Self {
        saved.set_tight(gl, alignment);
        Self {
            gl,
            saved,
            alignment,
        }
    }
}

impl<D: PixelStoreDomain> Drop for CompactPixelStoreGuard<'_, D> {
    fn drop(&mut self) {
        self.saved.transition_tight(self.gl, self.alignment, true);
    }
}

/// The target `readPixels` actually reads through. GLES2 has one binding point,
/// so asking it about `READ_FRAMEBUFFER` is an `INVALID_ENUM` rather than an
/// answer. Capability comes from the same probe the pixel-store state uses.
fn read_framebuffer_target(gl: &glow::Context) -> u32 {
    let version = gl.version();
    if version.major >= 3
        || !version.is_embedded
        || gl
            .supported_extensions()
            .contains("GL_ANGLE_framebuffer_blit")
    {
        glow::READ_FRAMEBUFFER
    } else {
        glow::FRAMEBUFFER
    }
}

/// Discard errors that belong to whatever ran before this operation.
///
/// # Safety
/// The caller's context must be current on this thread.
unsafe fn drain_gl_errors(gl: &glow::Context) {
    // Bounded: a driver that never returns NO_ERROR would otherwise spin here.
    for _ in 0..16 {
        if unsafe { gl.get_error() } == glow::NO_ERROR {
            return;
        }
    }
}

/// The first error the operation raised, with the rest of the queue drained so
/// it cannot be attributed to the next one.
///
/// # Safety
/// The caller's context must be current on this thread.
unsafe fn first_gl_error(gl: &glow::Context) -> Option<u32> {
    let first = unsafe { gl.get_error() };
    if first == glow::NO_ERROR {
        return None;
    }
    unsafe { drain_gl_errors(gl) };
    Some(first)
}

/// CPU-view readPixels path. Validate the actual destination footprint before
/// allocation, then transfer only compact pixel rows across the render channel.
/// PBO destinations require a distinct buffer-offset command and are refused.
/// Source selection runs once after storage is ready, only for nonempty reads.
/// It may change framebuffer bindings, but must leave PACK state untouched.
///
/// The driver gets the last word on whether a format/type pair is legal for the
/// framebuffer it is reading, and it is allowed to reject the call outright.
/// Until this checked it, a rejected read still returned a fully zeroed buffer
/// with no error, so content could not tell "the framebuffer is black" from
/// "the driver refused" -- and the zeros were written into its view either way.
/// An incomplete framebuffer is checked before the call because GLES3 makes it
/// `INVALID_FRAMEBUFFER_OPERATION` rather than something the layout can predict.
pub(crate) fn read_webgl_pixels(
    gl: &glow::Context,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    format: u32,
    type_: u32,
    destination_byte_length: usize,
    select_source: impl FnOnce() -> EngineResult<()>,
) -> EngineResult<ReadPixelsData> {
    let bpp = webgl_readback_bytes_per_pixel(format, type_)
        .ok_or_else(|| EngineError::new(ErrorCode::InvalidArgument))?;
    let pack = PixelPackState::capture(gl);
    if pack.buffer.is_some() {
        return Err(EngineError::new(ErrorCode::InvalidOperation));
    }
    let [alignment, row_length, skip_rows, skip_pixels] = pack.values;
    let layout = PixelPackLayout::new(
        width,
        height,
        bpp,
        alignment,
        row_length,
        skip_rows,
        skip_pixels,
    )
    .map_err(|error| {
        EngineError::new(match error {
            PixelPackError::InvalidValue => ErrorCode::InvalidArgument,
            PixelPackError::InvalidOperation => ErrorCode::InvalidOperation,
            PixelPackError::OutOfMemory => ErrorCode::OutOfMemory,
        })
    })?;
    if layout.required_bytes > destination_byte_length {
        return Err(EngineError::new(ErrorCode::InvalidOperation));
    }
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(layout.compact_bytes)
        .map_err(|_| EngineError::new(ErrorCode::OutOfMemory))?;
    pixels.resize(layout.compact_bytes, 0);
    if !pixels.is_empty() {
        select_source()?;
        let _pack = CompactPixelPackGuard::from_saved(gl, pack, 1);
        unsafe {
            let status = gl.check_framebuffer_status(read_framebuffer_target(gl));
            if status != glow::FRAMEBUFFER_COMPLETE {
                return Err(EngineError::new(ErrorCode::RenderFramebufferIncomplete));
            }
            // Someone else's pending error must not become this read's verdict.
            drain_gl_errors(gl);
            gl.read_pixels(
                x,
                y,
                width,
                height,
                format,
                type_,
                glow::PixelPackData::Slice(Some(&mut pixels)),
            );
            if let Some(error) = first_gl_error(gl) {
                return Err(EngineError::new(match error {
                    glow::INVALID_ENUM => ErrorCode::InvalidArgument,
                    glow::OUT_OF_MEMORY => ErrorCode::OutOfMemory,
                    _ => ErrorCode::InvalidOperation,
                }));
            }
        }
    }
    Ok(ReadPixelsData { pixels, layout })
}

/// GPU-destination readPixels: the pixels are packed into the bound
/// `PIXEL_PACK_BUFFER` at `offset` and stay on the GPU, so nothing is
/// transferred and the content's PACK state is used exactly as it stands --
/// this overload's whole contract is that the driver does the layout.
///
/// Only the errors the content can observe through `getError` are checked
/// here, before the call: no bound buffer, an offset the type cannot address,
/// and a footprint the buffer cannot hold.
pub(crate) fn read_webgl_pixels_to_buffer(
    gl: &glow::Context,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    format: u32,
    type_: u32,
    offset: i64,
) -> EngineResult<()> {
    let bpp = webgl_readback_bytes_per_pixel(format, type_)
        .ok_or_else(|| EngineError::new(ErrorCode::InvalidArgument))?;
    let pack = PixelPackState::capture(gl);
    // A CPU-view read is the other overload; this one requires a destination.
    if pack.buffer.is_none() {
        return Err(EngineError::new(ErrorCode::InvalidOperation));
    }
    let [alignment, row_length, skip_rows, skip_pixels] = pack.values;
    let layout = PixelPackLayout::new(
        width,
        height,
        bpp,
        alignment,
        row_length,
        skip_rows,
        skip_pixels,
    )
    .map_err(|error| {
        EngineError::new(match error {
            PixelPackError::InvalidValue => ErrorCode::InvalidArgument,
            PixelPackError::InvalidOperation => ErrorCode::InvalidOperation,
            PixelPackError::OutOfMemory => ErrorCode::OutOfMemory,
        })
    })?;
    // GLES3 4.3.2: the offset addresses whole pixels of the requested type.
    // `bpp` is that unit for packed types too, which is what the spec means by
    // "the number of basic machine units needed to store the type".
    let element = webgl_readback_type_bytes(type_).unwrap_or(bpp);
    let offset = u64::try_from(offset).map_err(|_| EngineError::new(ErrorCode::InvalidArgument))?;
    if offset % element as u64 != 0 {
        return Err(EngineError::new(ErrorCode::InvalidOperation));
    }
    let end = offset
        .checked_add(layout.required_bytes as u64)
        .ok_or_else(|| EngineError::new(ErrorCode::InvalidOperation))?;
    let size = unsafe { gl.get_buffer_parameter_i32(glow::PIXEL_PACK_BUFFER, glow::BUFFER_SIZE) };
    if size < 0 || end > size as u64 {
        return Err(EngineError::new(ErrorCode::InvalidOperation));
    }
    if layout.compact_bytes != 0 {
        unsafe {
            gl.read_pixels(
                x,
                y,
                width,
                height,
                format,
                type_,
                glow::PixelPackData::BufferOffset(offset as u32),
            );
        }
    }
    Ok(())
}

/// Checked, initialized CPU storage prepared before selecting a GL source.
pub(crate) struct Rgba8Readback {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl Rgba8Readback {
    pub(crate) fn new(width: u32, height: u32) -> EngineResult<Self> {
        let bytes = checked_canvas_rgba_byte_len(width, height).ok_or_else(|| {
            EngineError::new(ErrorCode::OutOfMemory)
                .with_msg("RGBA readback exceeds surface limits")
        })?;
        let mut pixels = Vec::new();
        pixels.try_reserve_exact(bytes).map_err(|_| {
            EngineError::new(ErrorCode::OutOfMemory).with_msg("RGBA readback allocation failed")
        })?;
        pixels.resize(bytes, 0);
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    /// The caller owns selection/restoration of the READ framebuffer and must
    /// supply a complete RGBA8 source with the prepared dimensions.
    pub(crate) fn read(mut self, gl: &glow::Context) -> Vec<u8> {
        if self.pixels.is_empty() {
            return self.pixels;
        }
        let _pack = CompactPixelPackGuard::new(gl, 1);
        unsafe {
            gl.read_pixels(
                0,
                0,
                self.width as i32,
                self.height as i32,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(&mut self.pixels)),
            );
        }
        self.pixels
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::gl::readback_test_gl as test_gl;

    #[test]
    fn source_selection_runs_only_after_valid_nonempty_storage() {
        let gl = test_gl::context();
        for (width, length, pbo) in [(0, 0, 0), (3, 23, 0), (3, 24, 17)] {
            test_gl::set_bindings(test_gl::Bindings {
                pack_buffer: pbo,
                ..Default::default()
            });
            let mut selected = false;
            let result = read_webgl_pixels(
                &gl,
                0,
                0,
                width,
                2,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                length,
                || {
                    selected = true;
                    Ok(())
                },
            );
            assert_eq!(result.is_ok(), width == 0);
            assert!(!selected);
        }
        assert!(test_gl::reads().is_empty());
        assert_eq!(test_gl::mutations(), 0);
    }

    #[test]
    fn source_selection_failure_does_not_read_or_change_pack_state() {
        let gl = test_gl::context();
        let original = test_gl::Bindings {
            pack: [8, 9, 2, 3],
            ..Default::default()
        };
        test_gl::set_bindings(original);
        let error = read_webgl_pixels(
            &gl,
            0,
            0,
            3,
            2,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            144,
            || Err(EngineError::new(ErrorCode::RenderBackendError)),
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::RenderBackendError);
        assert_eq!(test_gl::bindings(), original);
        assert!(test_gl::reads().is_empty());
        assert_eq!(test_gl::mutations(), 0);
    }

    #[test]
    fn source_selection_precedes_the_read_without_changing_destination_layout() {
        let gl = test_gl::context();
        test_gl::set_bindings(test_gl::Bindings {
            pack: [8, 9, 2, 3],
            draw_framebuffer: 19,
            ..Default::default()
        });
        let result = read_webgl_pixels(
            &gl,
            0,
            0,
            3,
            2,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            144,
            || {
                let mut bindings = test_gl::bindings();
                assert_eq!(bindings.pack, [8, 9, 2, 3]);
                bindings.read_framebuffer = 17;
                test_gl::set_bindings(bindings);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(result.pixels.len(), 24);
        assert_eq!(result.layout.required_bytes, 144);
        let reads = test_gl::reads();
        assert_eq!(reads.len(), 1);
        assert_eq!(reads[0].bindings.read_framebuffer, 17);
        assert_eq!(reads[0].bindings.draw_framebuffer, 19);
        assert_eq!(test_gl::bindings().pack, [8, 9, 2, 3]);
    }

    #[test]
    fn internal_readback_uses_exact_compact_storage_and_restores_native_names() {
        let gl = test_gl::context();
        let original = test_gl::Bindings {
            pack: [8, 9, 2, 3],
            pack_buffer: 0x8000_0001,
            read_framebuffer: 0x8000_0002,
            ..Default::default()
        };
        test_gl::set_bindings(original);
        let pixels = Rgba8Readback::new(3, 2).unwrap().read(&gl);
        assert_eq!(pixels, [0; 24]);
        let reads = test_gl::reads();
        assert_eq!(reads.len(), 1);
        assert_eq!((reads[0].width, reads[0].height), (3, 2));
        assert_eq!(reads[0].bindings.pack, [1, 0, 0, 0]);
        assert_eq!(reads[0].bindings.pack_buffer, 0);
        assert_eq!(
            reads[0].bindings.read_framebuffer,
            original.read_framebuffer
        );
        assert_eq!(test_gl::bindings(), original);
    }

    #[test]
    fn internal_readback_empty_images_do_not_touch_gl() {
        let gl = test_gl::context();
        for (width, height) in [(0, 0), (0, 8192), (8192, 0)] {
            assert!(
                Rgba8Readback::new(width, height)
                    .unwrap()
                    .read(&gl)
                    .is_empty()
            );
        }
        assert!(test_gl::reads().is_empty());
        assert_eq!(test_gl::mutations(), 0);
    }

    #[test]
    fn internal_readback_rejects_unbounded_dimensions_before_gl() {
        let _gl = test_gl::context();
        for (width, height) in [(8193, 1), (1, u32::MAX), (4097, 4096)] {
            assert!(Rgba8Readback::new(width, height).is_err());
        }
        assert!(test_gl::reads().is_empty());
        assert_eq!(test_gl::mutations(), 0);
    }

    #[test]
    fn internal_readback_nested_scopes_restore_each_prior_state() {
        let gl = test_gl::context();
        let original = test_gl::Bindings {
            pack: [8, 9, 2, 3],
            pack_buffer: 17,
            ..Default::default()
        };
        test_gl::set_bindings(original);
        {
            let _outer = CompactPixelPackGuard::new(&gl, 1);
            let compact = test_gl::bindings();
            let mutations = test_gl::mutations();
            {
                let _inner = CompactPixelPackGuard::new(&gl, 1);
                assert_eq!(test_gl::bindings(), compact);
            }
            assert_eq!(test_gl::bindings(), compact);
            assert_eq!(test_gl::mutations(), mutations);
        }
        assert_eq!(test_gl::bindings(), original);
    }

    #[test]
    fn internal_readback_already_compact_packing_needs_no_state_writes() {
        let gl = test_gl::context();
        test_gl::set_bindings(test_gl::Bindings {
            pack: [1, 0, 0, 0],
            ..Default::default()
        });
        Rgba8Readback::new(1, 1).unwrap().read(&gl);
        assert_eq!(test_gl::mutations(), 0);
        assert_eq!(test_gl::reads().len(), 1);
    }

    #[test]
    fn webgl_readback_keeps_transfer_compact_and_reports_destination_layout() {
        let gl = test_gl::context();
        let original = test_gl::Bindings {
            pack: [8, 9, 2, 3],
            ..Default::default()
        };
        test_gl::set_bindings(original);
        let result = read_webgl_pixels(
            &gl,
            0,
            0,
            3,
            2,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            144,
            || Ok(()),
        )
        .unwrap();
        assert_eq!(result.pixels.len(), 24);
        assert_eq!(result.layout.first_byte, 92);
        assert_eq!(result.layout.row_stride, 40);
        assert_eq!(result.layout.row_bytes, 12);
        assert_eq!(result.layout.required_bytes, 144);
        assert_eq!(test_gl::reads()[0].bindings.pack, [1, 0, 0, 0]);
        assert_eq!(test_gl::bindings(), original);
    }

    #[test]
    fn webgl_readback_gles2_uses_only_supported_pack_state() {
        for (subimage, pbo) in [(false, false), (true, false), (false, true), (true, true)] {
            let gl = test_gl::context_gles2(test_gl::Gles2Caps {
                pack_subimage: subimage,
                pixel_buffer: pbo,
                ..Default::default()
            });
            let original = test_gl::Bindings {
                pack: if subimage { [8, 9, 2, 3] } else { [8, 0, 0, 0] },
                ..Default::default()
            };
            test_gl::set_bindings(original);
            let result = read_webgl_pixels(
                &gl,
                0,
                0,
                3,
                2,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                144,
                || Ok(()),
            )
            .unwrap();
            assert_eq!(result.pixels.len(), 24);
            assert_eq!(
                result.layout.required_bytes,
                if subimage { 144 } else { 28 }
            );
            assert_eq!(test_gl::bindings(), original);
            let saved = PixelPackState::capture(&gl);
            saved.set_tight(&gl, 1);
            saved.restore(&gl);
            assert_eq!(test_gl::bindings(), original);
            assert_eq!(
                test_gl::unsupported_calls(),
                0,
                "GLES2 extensions: {subimage}, {pbo}"
            );
        }
    }

    fn read_to_buffer(gl: &glow::Context, offset: i64) -> EngineResult<()> {
        read_webgl_pixels_to_buffer(gl, 0, 0, 3, 2, glow::RGBA, glow::UNSIGNED_BYTE, offset)
    }

    /// The driver does the packing for a GPU destination, so this path must
    /// hand it the content's own PACK state rather than a compact temporary.
    #[test]
    fn buffer_readback_uses_the_content_pack_state_unchanged() {
        let gl = test_gl::context();
        let original = test_gl::Bindings {
            pack: [8, 9, 2, 3],
            pack_buffer: 17,
            // first_byte 92 + one stride 40 + 12 row bytes = 144, plus offset 8.
            pack_buffer_size: 152,
            ..Default::default()
        };
        test_gl::set_bindings(original);
        read_to_buffer(&gl, 8).unwrap();
        let reads = test_gl::reads();
        assert_eq!(reads.len(), 1);
        assert_eq!(reads[0].bindings.pack, [8, 9, 2, 3]);
        assert_eq!(reads[0].bindings.pack_buffer, 17);
        assert_eq!(test_gl::bindings(), original);
        assert_eq!(test_gl::mutations(), 0);
    }

    #[test]
    fn buffer_readback_requires_a_bound_buffer() {
        let gl = test_gl::context();
        test_gl::set_bindings(test_gl::Bindings {
            pack_buffer_size: 4096,
            ..Default::default()
        });
        assert_eq!(
            read_to_buffer(&gl, 0).unwrap_err().code,
            ErrorCode::InvalidOperation
        );
        assert!(test_gl::reads().is_empty());
    }

    #[test]
    fn buffer_readback_rejects_offsets_the_type_cannot_address() {
        for (type_, element) in [
            (glow::UNSIGNED_BYTE, 1),
            (glow::UNSIGNED_SHORT_5_6_5, 2),
            (glow::FLOAT, 4),
        ] {
            let gl = test_gl::context();
            test_gl::set_bindings(test_gl::Bindings {
                pack_buffer: 17,
                pack_buffer_size: 1 << 20,
                ..Default::default()
            });
            let format = if type_ == glow::UNSIGNED_SHORT_5_6_5 {
                glow::RGB
            } else {
                glow::RGBA
            };
            for offset in [0, element, element * 3] {
                read_webgl_pixels_to_buffer(&gl, 0, 0, 3, 2, format, type_, offset).unwrap();
            }
            if element > 1 {
                assert_eq!(
                    read_webgl_pixels_to_buffer(&gl, 0, 0, 3, 2, format, type_, element - 1)
                        .unwrap_err()
                        .code,
                    ErrorCode::InvalidOperation
                );
            }
            assert_eq!(test_gl::reads().len(), 3);
        }
    }

    #[test]
    fn buffer_readback_rejects_a_footprint_the_buffer_cannot_hold() {
        let gl = test_gl::context();
        for (size, offset, accepted) in [(24, 0, true), (23, 0, false), (24, 4, false)] {
            test_gl::set_bindings(test_gl::Bindings {
                pack_buffer: 17,
                pack_buffer_size: size,
                ..Default::default()
            });
            assert_eq!(
                read_to_buffer(&gl, offset).is_ok(),
                accepted,
                "{size}/{offset}"
            );
        }
        assert_eq!(test_gl::reads().len(), 1);
    }

    #[test]
    fn buffer_readback_of_an_empty_area_validates_without_reading() {
        let gl = test_gl::context();
        test_gl::set_bindings(test_gl::Bindings {
            pack_buffer: 17,
            pack_buffer_size: 0,
            ..Default::default()
        });
        read_webgl_pixels_to_buffer(&gl, 0, 0, 0, 2, glow::RGBA, glow::UNSIGNED_BYTE, 0).unwrap();
        assert!(test_gl::reads().is_empty());
        // An unknown type has no addressable unit and no footprint to check.
        assert_eq!(
            read_webgl_pixels_to_buffer(&gl, 0, 0, 0, 2, glow::RGBA, 0xFFFF, 0)
                .unwrap_err()
                .code,
            ErrorCode::InvalidArgument
        );
        assert_eq!(test_gl::mutations(), 0);
    }

    #[test]
    fn compact_unpack_scope_isolates_and_restores_every_supported_slot() {
        let gl = test_gl::context();
        let original = test_gl::Bindings {
            unpack: [8, 9, 2, 3],
            unpack_buffer: 17,
            pack: [2, 5, 7, 1],
            pack_buffer: 19,
            ..Default::default()
        };
        test_gl::set_bindings(original);
        {
            let _unpack = CompactPixelUnpackGuard::new(&gl, 4);
            let inside = test_gl::bindings();
            assert_eq!(inside.unpack, [4, 0, 0, 0]);
            assert_eq!(inside.unpack_buffer, 0);
            // The pack half belongs to a different scope and must be untouched.
            assert_eq!(inside.pack, original.pack);
            assert_eq!(inside.pack_buffer, original.pack_buffer);
        }
        assert_eq!(test_gl::bindings(), original);
    }

    #[test]
    fn compact_unpack_scope_on_gles2_uses_only_supported_state() {
        for (subimage, pbo) in [(false, false), (true, false), (false, true), (true, true)] {
            let gl = test_gl::context_gles2(test_gl::Gles2Caps {
                unpack_subimage: subimage,
                pixel_buffer: pbo,
                ..Default::default()
            });
            // Row length and skips exist in the fixture either way; without the
            // extension the scope must not read or write them at all, which is
            // observable as the content's values surviving inside the scope.
            let original = test_gl::Bindings {
                unpack: [8, 9, 2, 3],
                unpack_buffer: if pbo { 17 } else { 0 },
                ..Default::default()
            };
            test_gl::set_bindings(original);
            {
                let _unpack = CompactPixelUnpackGuard::new(&gl, 4);
                let inside = test_gl::bindings();
                assert_eq!(inside.unpack[0], 4);
                assert_eq!(inside.unpack_buffer, 0);
                assert_eq!(
                    inside.unpack[1..],
                    if subimage { [0, 0, 0] } else { [9, 2, 3] }
                );
            }
            assert_eq!(test_gl::bindings(), original);
            assert_eq!(
                test_gl::unsupported_calls(),
                0,
                "GLES2 extensions: {subimage}, {pbo}"
            );
        }
    }

    #[test]
    fn webgl_readback_empty_layouts_validate_without_writing_gl_state() {
        let gl = test_gl::context();
        test_gl::set_bindings(test_gl::Bindings {
            pack: [8, i32::MAX, i32::MAX, 0],
            ..Default::default()
        });
        let result = read_webgl_pixels(&gl, 0, 0, 0, 3, glow::RGBA, glow::UNSIGNED_BYTE, 0, || {
            Ok(())
        })
        .unwrap();
        assert!(result.pixels.is_empty());
        assert_eq!(result.layout.required_bytes, 0);
        test_gl::set_bindings(test_gl::Bindings {
            pack_buffer: 17,
            ..Default::default()
        });
        assert!(
            read_webgl_pixels(&gl, 0, 0, 0, 3, glow::RGBA, glow::UNSIGNED_BYTE, 0, || Ok(
                ()
            ))
            .is_err()
        );
        assert!(test_gl::reads().is_empty());
        assert_eq!(test_gl::mutations(), 0);
    }

    #[test]
    fn webgl_readback_rejects_short_destinations_and_bound_pbos_without_writes() {
        let gl = test_gl::context();
        test_gl::set_bindings(test_gl::Bindings {
            pack: [8, 9, 2, 3],
            ..Default::default()
        });
        let err = read_webgl_pixels(
            &gl,
            0,
            0,
            3,
            2,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            143,
            || Ok(()),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidOperation);
        test_gl::set_bindings(test_gl::Bindings {
            pack_buffer: 17,
            ..Default::default()
        });
        let err = read_webgl_pixels(&gl, 0, 0, 1, 1, glow::RGBA, glow::UNSIGNED_BYTE, 4, || {
            Ok(())
        })
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidOperation);
        assert!(test_gl::reads().is_empty());
        assert_eq!(test_gl::mutations(), 0);
    }

    /// A driver that refuses the read must not look like a black framebuffer.
    ///
    /// Before this, a rejected `glReadPixels` still returned the zeroed staging
    /// buffer with `Ok`, and those zeros were written into the caller's view.
    #[test]
    fn webgl_readback_reports_the_drivers_own_rejection() {
        for (gl_error, expected) in [
            (glow::INVALID_ENUM, ErrorCode::InvalidArgument),
            (glow::INVALID_OPERATION, ErrorCode::InvalidOperation),
            (glow::OUT_OF_MEMORY, ErrorCode::OutOfMemory),
        ] {
            let gl = test_gl::context();
            test_gl::set_read_error(gl_error);
            let error = read_webgl_pixels(
                &gl,
                0,
                0,
                3,
                2,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                144,
                || Ok(()),
            )
            .unwrap_err();
            assert_eq!(error.code, expected);
            assert_eq!(test_gl::pending_errors(), 0, "queue must be drained");
        }
    }

    /// An error left behind by an earlier command is not this read's verdict.
    #[test]
    fn webgl_readback_does_not_inherit_a_previous_operations_error() {
        let gl = test_gl::context();
        // Two stale errors, then nothing for the read itself.
        test_gl::queue_error(glow::INVALID_OPERATION);
        test_gl::queue_error(glow::INVALID_ENUM);
        let result = read_webgl_pixels(
            &gl,
            0,
            0,
            3,
            2,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            144,
            || Ok(()),
        );
        assert!(result.is_ok(), "stale errors were attributed to this read");
        assert_eq!(test_gl::pending_errors(), 0);
    }

    #[test]
    fn webgl_readback_refuses_an_incomplete_framebuffer_before_reading() {
        let gl = test_gl::context();
        test_gl::set_framebuffer_status(glow::FRAMEBUFFER_INCOMPLETE_ATTACHMENT);
        let error = read_webgl_pixels(
            &gl,
            0,
            0,
            3,
            2,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            144,
            || Ok(()),
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::RenderFramebufferIncomplete);
        assert!(
            test_gl::reads().is_empty(),
            "an incomplete framebuffer must be refused before the read"
        );
    }

    /// An empty read has nothing to reject: it never reaches the driver, so it
    /// cannot report a driver error either.
    #[test]
    fn webgl_readback_of_an_empty_area_ignores_driver_state() {
        let gl = test_gl::context();
        test_gl::set_framebuffer_status(glow::FRAMEBUFFER_INCOMPLETE_ATTACHMENT);
        test_gl::queue_error(glow::INVALID_OPERATION);
        let result = read_webgl_pixels(&gl, 0, 0, 0, 2, glow::RGBA, glow::UNSIGNED_BYTE, 0, || {
            Ok(())
        })
        .unwrap();
        assert!(result.pixels.is_empty());
        assert!(test_gl::reads().is_empty());
    }
}
