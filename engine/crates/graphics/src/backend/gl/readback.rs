//! Packing ownership for engine-owned CPU reads. User PACK state belongs to
//! content; a compact temporary must neither inherit it nor leave it changed.

use glow::HasContext;
use shared::{
    error::{EngineError, EngineResult, ErrorCode},
    protocol::{
        pixel_pack::{PixelPackError, PixelPackLayout},
        render_cmd::{
            ReadPixelsData, checked_canvas_rgba_byte_len, webgl_readback_bytes_per_pixel,
        },
    },
};

const PACK_NAMES: [u32; 4] = [
    glow::PACK_ALIGNMENT,
    glow::PACK_ROW_LENGTH,
    glow::PACK_SKIP_ROWS,
    glow::PACK_SKIP_PIXELS,
];

pub(crate) struct PixelPackState {
    values: [i32; 4],
    buffer: Option<glow::NativeBuffer>,
    has_pack_subimage: bool,
    has_pack_buffer: bool,
}

impl PixelPackState {
    /// The calling thread must keep this context current through restore.
    pub(crate) fn capture(gl: &glow::Context) -> Self {
        // glow caches version/extensions when the context is created. These
        // capability checks neither query the driver nor allocate on each read.
        let version = gl.version();
        let extensions = gl.supported_extensions();
        let has_pack_subimage = !version.is_embedded
            || version.major >= 3
            || extensions.contains("GL_NV_pack_subimage");
        let has_pack_buffer = version.major >= 3
            || (!version.is_embedded && (version.major, version.minor) >= (2, 1))
            || extensions.contains("GL_NV_pixel_buffer_object")
            || extensions.contains("GL_ARB_pixel_buffer_object")
            || extensions.contains("GL_EXT_pixel_buffer_object");
        unsafe {
            let mut values = [0; 4];
            for (value, name) in values
                .iter_mut()
                .zip(PACK_NAMES)
                .take(if has_pack_subimage { 4 } else { 1 })
            {
                *value = gl.get_parameter_i32(name);
            }
            Self {
                values,
                buffer: has_pack_buffer
                    .then(|| gl.get_parameter_buffer(glow::PIXEL_PACK_BUFFER_BINDING))
                    .flatten(),
                has_pack_subimage,
                has_pack_buffer,
            }
        }
    }

    /// Establish tightly packed rows at the caller's known alignment (1 or 4).
    pub(crate) fn set_tight(&self, gl: &glow::Context, alignment: i32) {
        self.transition_tight(gl, alignment, false);
    }

    // Only the values changed by set_tight need restoring when the intervening
    // operation is readPixels itself, which does not mutate packing state.
    fn transition_tight(&self, gl: &glow::Context, alignment: i32, restore: bool) {
        for ((name, saved), tight) in PACK_NAMES
            .into_iter()
            .zip(self.values)
            .zip([alignment, 0, 0, 0])
            .take(if self.has_pack_subimage { 4 } else { 1 })
        {
            if saved != tight {
                unsafe { gl.pixel_store_i32(name, if restore { saved } else { tight }) };
            }
        }
        if self.buffer.is_some() {
            unsafe {
                gl.bind_buffer(
                    glow::PIXEL_PACK_BUFFER,
                    if restore { self.buffer } else { None },
                );
            }
        }
    }

    /// Restore after external GL use, which may have changed every pack slot.
    pub(crate) fn restore(&self, gl: &glow::Context) {
        unsafe {
            for (name, value) in PACK_NAMES
                .into_iter()
                .zip(self.values)
                .take(if self.has_pack_subimage { 4 } else { 1 })
            {
                gl.pixel_store_i32(name, value);
            }
            if self.has_pack_buffer {
                gl.bind_buffer(glow::PIXEL_PACK_BUFFER, self.buffer);
            }
        }
    }
}

struct CompactPixelPackGuard<'a> {
    gl: &'a glow::Context,
    saved: PixelPackState,
}

#[cfg(all(test, target_os = "linux"))]
#[path = "readback_native_test.rs"]
mod native_tests;

impl<'a> CompactPixelPackGuard<'a> {
    fn new(gl: &'a glow::Context) -> Self {
        let saved = PixelPackState::capture(gl);
        Self::from_saved(gl, saved)
    }

    fn from_saved(gl: &'a glow::Context, saved: PixelPackState) -> Self {
        saved.set_tight(gl, 1);
        Self { gl, saved }
    }
}

impl Drop for CompactPixelPackGuard<'_> {
    fn drop(&mut self) {
        self.saved.transition_tight(self.gl, 1, true);
    }
}

/// CPU-view readPixels path. Validate the actual destination footprint before
/// allocation, then transfer only compact pixel rows across the render channel.
/// PBO destinations require a distinct buffer-offset command and are refused.
/// Source selection runs once after storage is ready, only for nonempty reads.
/// It may change framebuffer bindings, but must leave PACK state untouched.
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
        let _pack = CompactPixelPackGuard::from_saved(gl, pack);
        unsafe {
            gl.read_pixels(
                x,
                y,
                width,
                height,
                format,
                type_,
                glow::PixelPackData::Slice(Some(&mut pixels)),
            );
        }
    }
    Ok(ReadPixelsData { pixels, layout })
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
        let _pack = CompactPixelPackGuard::new(gl);
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
            let _outer = CompactPixelPackGuard::new(&gl);
            let compact = test_gl::bindings();
            let mutations = test_gl::mutations();
            {
                let _inner = CompactPixelPackGuard::new(&gl);
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
            let gl = test_gl::context_gles2(subimage, pbo);
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
}
