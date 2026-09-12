//! Opt-in pixel validation on Mesa's surfaceless EGL platform.

use super::*;
use khronos_egl as egl;

// One shared lock for the one Mesa surfaceless display; see
// `readback_test_gl::lock_egl_display` for what a second private mutex cost.

struct EglScope {
    api: egl::DynamicInstance<egl::EGL1_5>,
    display: egl::Display,
    surface: Option<egl::Surface>,
    context: Option<egl::Context>,
    _display_lifetime: std::sync::MutexGuard<'static, ()>,
}

impl Drop for EglScope {
    fn drop(&mut self) {
        let _ = self.api.make_current(self.display, None, None, None);
        if let Some(surface) = self.surface {
            let _ = self.api.destroy_surface(self.display, surface);
        }
        if let Some(context) = self.context {
            let _ = self.api.destroy_context(self.display, context);
        }
        let _ = self.api.terminate(self.display);
    }
}

fn gles3_context() -> (EglScope, glow::Context) {
    let display_lifetime = crate::backend::gl::readback_test_gl::lock_egl_display();
    let api =
        unsafe { egl::DynamicInstance::<egl::EGL1_5>::load_required_from_filename("libEGL.so.1") }
            .expect("load EGL 1.5");
    // EGL_PLATFORM_SURFACELESS_MESA uses EGL_DEFAULT_DISPLAY (null).
    let display =
        unsafe { api.get_platform_display(0x31DD, std::ptr::null_mut(), &[egl::ATTRIB_NONE]) }
            .expect("Mesa surfaceless display");
    api.initialize(display).expect("initialize EGL");
    let mut scope = EglScope {
        api,
        display,
        surface: None,
        context: None,
        _display_lifetime: display_lifetime,
    };
    scope.api.bind_api(egl::OPENGL_ES_API).unwrap();
    let config = scope
        .api
        .choose_first_config(
            display,
            &[
                egl::SURFACE_TYPE,
                egl::PBUFFER_BIT,
                egl::RENDERABLE_TYPE,
                0x40, // EGL_OPENGL_ES3_BIT
                egl::RED_SIZE,
                8,
                egl::GREEN_SIZE,
                8,
                egl::BLUE_SIZE,
                8,
                egl::ALPHA_SIZE,
                8,
                egl::NONE,
            ],
        )
        .unwrap()
        .expect("RGBA8 GLES3 pbuffer config");
    scope.context = Some(
        scope
            .api
            .create_context(
                display,
                config,
                None,
                &[egl::CONTEXT_CLIENT_VERSION, 3, egl::NONE],
            )
            .unwrap(),
    );
    scope.surface = Some(
        scope
            .api
            .create_pbuffer_surface(display, config, &[egl::WIDTH, 3, egl::HEIGHT, 2, egl::NONE])
            .unwrap(),
    );
    scope
        .api
        .make_current(display, scope.surface, scope.surface, scope.context)
        .unwrap();
    let gl = unsafe {
        glow::Context::from_loader_function(|name| {
            scope
                .api
                .get_proc_address(name)
                .map_or(std::ptr::null(), |f| f as *const std::ffi::c_void)
        })
    };
    (scope, gl)
}

#[test]
#[ignore = "requires Mesa surfaceless EGL and GLES3"]
fn drawing_buffer_resize_native_preserves_bindings_and_ignores_unpack_pbo() {
    use crate::canvas::drawing_buffer;
    let (_scope, gl) = gles3_context();
    let mut db = drawing_buffer::create(&gl, 3, 2).unwrap();
    unsafe {
        let custom_read = gl.create_framebuffer().unwrap();
        let custom_draw = gl.create_framebuffer().unwrap();
        let texture = gl.create_texture().unwrap();
        let renderbuffer = gl.create_renderbuffer().unwrap();
        let pbo = gl.create_buffer().unwrap();
        gl.active_texture(glow::TEXTURE3);
        gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        gl.bind_renderbuffer(glow::RENDERBUFFER, Some(renderbuffer));
        gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(custom_read));
        gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(custom_draw));
        gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, Some(pbo));
        gl.buffer_data_u8_slice(glow::PIXEL_UNPACK_BUFFER, &[0xA5; 4], glow::STREAM_DRAW);
        gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, 11);
        gl.pixel_store_i32(glow::UNPACK_SKIP_ROWS, 2);
        gl.pixel_store_i32(glow::UNPACK_SKIP_PIXELS, 3);
        assert_eq!(gl.get_error(), glow::NO_ERROR);
        let names = (db.fbo, db.color_tex, db.depth_stencil_rb);
        drawing_buffer::resize(&gl, &mut db, 5, 4).unwrap();
        assert_eq!(
            gl.get_error(),
            glow::NO_ERROR,
            "allocation must not read the user's short PBO"
        );
        assert_eq!((db.fbo, db.color_tex, db.depth_stencil_rb), names);
        assert_eq!((db.width, db.height), (5, 4));
        assert_eq!(
            gl.get_parameter_framebuffer(glow::READ_FRAMEBUFFER_BINDING),
            Some(custom_read)
        );
        assert_eq!(
            gl.get_parameter_framebuffer(glow::DRAW_FRAMEBUFFER_BINDING),
            Some(custom_draw)
        );
        assert_eq!(
            gl.get_parameter_i32(glow::ACTIVE_TEXTURE),
            glow::TEXTURE3 as i32
        );
        assert_eq!(
            gl.get_parameter_texture(glow::TEXTURE_BINDING_2D),
            Some(texture)
        );
        assert_eq!(
            gl.get_parameter_renderbuffer(glow::RENDERBUFFER_BINDING),
            Some(renderbuffer)
        );
        assert_eq!(
            gl.get_parameter_buffer(glow::PIXEL_UNPACK_BUFFER_BINDING),
            Some(pbo)
        );
        assert_eq!(gl.get_parameter_i32(glow::UNPACK_ROW_LENGTH), 11);
        assert_eq!(gl.get_parameter_i32(glow::UNPACK_SKIP_ROWS), 2);
        assert_eq!(gl.get_parameter_i32(glow::UNPACK_SKIP_PIXELS), 3);
        let data = gl.map_buffer_range(glow::PIXEL_UNPACK_BUFFER, 0, 4, glow::MAP_READ_BIT);
        assert!(!data.is_null());
        assert_eq!(std::slice::from_raw_parts(data, 4), [0xA5; 4]);
        gl.unmap_buffer(glow::PIXEL_UNPACK_BUFFER);

        // Full new extent must be renderable without reattaching the objects.
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(db.fbo));
        gl.clear_color(0.0, 1.0, 0.0, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
        let pixels = Rgba8Readback::new(5, 4).unwrap().read(&gl);
        assert!(pixels.chunks_exact(4).all(|p| p == [0, 255, 0, 255]));
        gl.bind_renderbuffer(glow::RENDERBUFFER, Some(db.depth_stencil_rb));
        assert_eq!(
            gl.get_renderbuffer_parameter_i32(glow::RENDERBUFFER, glow::RENDERBUFFER_WIDTH),
            5
        );
        assert_eq!(
            gl.get_renderbuffer_parameter_i32(glow::RENDERBUFFER, glow::RENDERBUFFER_HEIGHT),
            4
        );
        gl.delete_buffer(pbo);
        gl.delete_texture(texture);
        gl.delete_renderbuffer(renderbuffer);
        gl.delete_framebuffer(custom_read);
        gl.delete_framebuffer(custom_draw);
        drawing_buffer::destroy(&gl, db);
        assert_eq!(gl.get_error(), glow::NO_ERROR);
    }
}

#[test]
#[ignore = "requires Mesa surfaceless EGL and GLES3"]
fn drawing_buffer_resize_native_failure_restores_bindings() {
    use crate::canvas::drawing_buffer;
    let (_scope, gl) = gles3_context();
    let mut db = drawing_buffer::create(&gl, 3, 2).unwrap();
    unsafe {
        let custom = gl.create_framebuffer().unwrap();
        let texture = gl.create_texture().unwrap();
        let renderbuffer = gl.create_renderbuffer().unwrap();
        gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(custom));
        gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, None);
        gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        gl.bind_renderbuffer(glow::RENDERBUFFER, Some(renderbuffer));
        // Zero-width storage makes the destination incomplete without unsafe
        // host writes or inducing a real driver OOM.
        assert!(drawing_buffer::resize(&gl, &mut db, 0, 2).is_err());
        assert_eq!(
            gl.get_parameter_texture(glow::TEXTURE_BINDING_2D),
            Some(texture)
        );
        assert_eq!(
            gl.get_parameter_renderbuffer(glow::RENDERBUFFER_BINDING),
            Some(renderbuffer)
        );
        assert_eq!(
            gl.get_parameter_framebuffer(glow::READ_FRAMEBUFFER_BINDING),
            Some(custom)
        );
        assert_eq!(
            gl.get_parameter_framebuffer(glow::DRAW_FRAMEBUFFER_BINDING),
            None
        );
        gl.delete_framebuffer(custom);
        gl.delete_texture(texture);
        gl.delete_renderbuffer(renderbuffer);
        drawing_buffer::destroy(&gl, db);
        assert_eq!(gl.get_error(), glow::NO_ERROR);
    }
}

#[test]
#[ignore = "requires Mesa surfaceless EGL and GLES3"]
fn default_snapshot_native_preserves_split_bindings_and_scissor() {
    use crate::canvas::drawing_buffer;
    let (_scope, gl) = gles3_context();
    let db = drawing_buffer::create(&gl, 3, 2).unwrap();
    unsafe {
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        gl.clear_color(0.0, 1.0, 0.0, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
        let custom_read = gl.create_framebuffer().unwrap();
        let custom_draw = gl.create_framebuffer().unwrap();
        gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(custom_read));
        gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(custom_draw));
        gl.enable(glow::SCISSOR_TEST);
        gl.scissor(0, 0, 1, 1);
        assert!(drawing_buffer::blit_from_surface(&gl, &db, 3, 2));
        assert_eq!(
            gl.get_parameter_framebuffer(glow::READ_FRAMEBUFFER_BINDING),
            Some(custom_read)
        );
        assert_eq!(
            gl.get_parameter_framebuffer(glow::DRAW_FRAMEBUFFER_BINDING),
            Some(custom_draw)
        );
        assert!(gl.is_enabled(glow::SCISSOR_TEST));
        gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(db.fbo));
        let pixels = Rgba8Readback::new(3, 2).unwrap().read(&gl);
        assert!(pixels.chunks_exact(4).all(|p| p == [0, 255, 0, 255]));
        // Incomplete destination: no copy, but restore the same split bindings
        // and scissor state on the failure path as on the success path.
        gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(db.fbo));
        gl.framebuffer_texture_2d(
            glow::DRAW_FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            None,
            0,
        );
        gl.framebuffer_renderbuffer(
            glow::DRAW_FRAMEBUFFER,
            glow::DEPTH_STENCIL_ATTACHMENT,
            glow::RENDERBUFFER,
            None,
        );
        gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(custom_read));
        gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(custom_draw));
        assert!(!drawing_buffer::blit_from_surface(&gl, &db, 3, 2));
        assert_eq!(
            gl.get_parameter_framebuffer(glow::READ_FRAMEBUFFER_BINDING),
            Some(custom_read)
        );
        assert_eq!(
            gl.get_parameter_framebuffer(glow::DRAW_FRAMEBUFFER_BINDING),
            Some(custom_draw)
        );
        assert!(gl.is_enabled(glow::SCISSOR_TEST));
        gl.delete_framebuffer(custom_read);
        gl.delete_framebuffer(custom_draw);
        drawing_buffer::destroy(&gl, db);
        assert_eq!(gl.get_error(), glow::NO_ERROR);
    }
}

#[test]
#[ignore = "requires Mesa surfaceless EGL and GLES3"]
fn default_framebuffer_native_remaps_after_context_return() {
    use crate::canvas::{apply_default_framebuffer, drawing_buffer};
    let (scope, gl) = gles3_context();
    let db = drawing_buffer::create(&gl, 3, 2).unwrap();
    unsafe {
        let custom = gl.create_framebuffer().unwrap();
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        gl.clear_color(0.0, 1.0, 0.0, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
        gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(custom));
        assert!(drawing_buffer::blit_from_surface(&gl, &db, 3, 2));

        scope
            .api
            .make_current(scope.display, None, None, None)
            .unwrap();
        let mut applied = None;
        apply_default_framebuffer(&gl, false, &mut applied, Some(db.fbo));
        assert_eq!(
            applied, None,
            "a pending mapping cannot be applied to a noncurrent context"
        );
        scope
            .api
            .make_current(scope.display, scope.surface, scope.surface, scope.context)
            .unwrap();
        apply_default_framebuffer(&gl, true, &mut applied, Some(db.fbo));
        assert_eq!(applied, Some(db.fbo));
        assert_eq!(
            gl.get_parameter_framebuffer(glow::READ_FRAMEBUFFER_BINDING),
            Some(db.fbo)
        );
        assert_eq!(
            gl.get_parameter_framebuffer(glow::DRAW_FRAMEBUFFER_BINDING),
            Some(custom)
        );
        let pixels = Rgba8Readback::new(3, 2).unwrap().read(&gl);
        assert!(pixels.chunks_exact(4).all(|p| p == [0, 255, 0, 255]));

        // Inverse split: READ custom must survive when DRAW moves to FBO 0.
        gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(custom));
        gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(db.fbo));
        apply_default_framebuffer(&gl, true, &mut applied, None);
        assert_eq!(applied, None);
        assert_eq!(
            gl.get_parameter_framebuffer(glow::READ_FRAMEBUFFER_BINDING),
            Some(custom)
        );
        assert_eq!(
            gl.get_parameter_framebuffer(glow::DRAW_FRAMEBUFFER_BINDING),
            None
        );
        gl.delete_framebuffer(custom);
        drawing_buffer::destroy(&gl, db);
        assert_eq!(gl.get_error(), glow::NO_ERROR);
        eprintln!(
            "default framebuffer renderer: {}",
            gl.get_parameter_string(glow::RENDERER)
        );
    }
}

#[test]
#[ignore = "requires Mesa surfaceless EGL and GLES3"]
fn internal_readback_native_pixels_and_pack_buffer_are_preserved() {
    let (_scope, gl) = gles3_context();
    unsafe {
        gl.clear_color(1.0, 0.0, 0.0, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
        let pbo = gl.create_buffer().unwrap();
        gl.bind_buffer(glow::PIXEL_PACK_BUFFER, Some(pbo));
        gl.buffer_data_u8_slice(glow::PIXEL_PACK_BUFFER, &[0xA5; 64], glow::STREAM_READ);
        assert_eq!(read_bound_pbo(&gl), [0xA5; 64], "initialized PBO control");
        for (name, value) in Pack::NAMES.into_iter().zip([8, 9, 2, 3]) {
            gl.pixel_store_i32(name, value);
        }
        assert_eq!(gl.get_error(), glow::NO_ERROR);
        let pixels = Rgba8Readback::new(3, 2).unwrap().read(&gl);
        assert_eq!(pixels.len(), 24);
        for pixel in pixels.chunks_exact(4) {
            assert_eq!(pixel, [255, 0, 0, 255]);
        }
        assert_eq!(gl.get_error(), glow::NO_ERROR);
        let restored = PixelPackState::capture(&gl);
        assert_eq!(restored.values, [8, 9, 2, 3]);
        assert_eq!(restored.buffer, Some(pbo));
        assert_eq!(read_bound_pbo(&gl), [0xA5; 64]);
        assert_eq!(gl.get_error(), glow::NO_ERROR);
        let rejected = read_webgl_pixels(
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
        assert!(
            rejected.is_err(),
            "a CPU view cannot be used while a PBO is bound"
        );
        assert_eq!(read_bound_pbo(&gl), [0xA5; 64]);
        gl.bind_buffer(glow::PIXEL_PACK_BUFFER, None);
        let public = read_webgl_pixels(
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
        assert_eq!(public.pixels, pixels);
        assert_eq!(public.layout.first_byte, 92);
        assert_eq!(public.layout.row_stride, 40);
        assert_eq!(public.layout.required_bytes, 144);
        let restored = PixelPackState::capture(&gl);
        assert_eq!(restored.values, [8, 9, 2, 3]);
        assert_eq!(restored.buffer, None);
        assert_eq!(gl.get_error(), glow::NO_ERROR);
        // A color-renderable integer source exercises the 16-byte scalar
        // representation using only the checked compact allocation path.
        let texture = gl.create_texture().unwrap();
        gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA32UI as i32,
            3,
            2,
            0,
            glow::RGBA_INTEGER,
            glow::UNSIGNED_INT,
            glow::PixelUnpackData::Slice(None),
        );
        let framebuffer = gl.create_framebuffer().unwrap();
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
        gl.framebuffer_texture_2d(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            Some(texture),
            0,
        );
        assert_eq!(
            gl.check_framebuffer_status(glow::FRAMEBUFFER),
            glow::FRAMEBUFFER_COMPLETE
        );
        gl.clear_buffer_u32_slice(glow::COLOR, 0, &[10, 20, 30, 40]);
        assert_eq!(gl.get_error(), glow::NO_ERROR);
        let integer = read_webgl_pixels(
            &gl,
            0,
            0,
            3,
            2,
            glow::RGBA_INTEGER,
            glow::UNSIGNED_INT,
            528,
            || Ok(()),
        )
        .unwrap();
        assert_eq!(integer.pixels.len(), 96);
        assert_eq!(integer.layout.required_bytes, 528);
        for pixel in integer.pixels.chunks_exact(16) {
            for (component, expected) in pixel.chunks_exact(4).zip([10, 20, 30, 40]) {
                assert_eq!(u32::from_ne_bytes(component.try_into().unwrap()), expected);
            }
        }
        assert_eq!(PixelPackState::capture(&gl).values, [8, 9, 2, 3]);
        assert_eq!(gl.get_error(), glow::NO_ERROR);
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        gl.delete_framebuffer(framebuffer);
        gl.delete_texture(texture);
        eprintln!("renderer: {}", gl.get_parameter_string(glow::RENDERER));
        gl.delete_buffer(pbo);
    }
}

#[test]
#[ignore = "requires Mesa surfaceless EGL and GLES3"]
fn buffer_readback_native_packs_into_the_bound_buffer_at_its_offset() {
    let (_scope, gl) = gles3_context();
    unsafe {
        gl.clear_color(1.0, 0.0, 0.0, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
        let pbo = gl.create_buffer().unwrap();
        gl.bind_buffer(glow::PIXEL_PACK_BUFFER, Some(pbo));
        gl.buffer_data_u8_slice(glow::PIXEL_PACK_BUFFER, &[0xA5; 64], glow::STREAM_READ);
        assert_eq!(gl.get_error(), glow::NO_ERROR);

        // Tight rows, so the destination footprint is 24 bytes from the offset.
        for (name, value) in Pack::NAMES.into_iter().zip([1, 0, 0, 0]) {
            gl.pixel_store_i32(name, value);
        }
        // 64 - 24 = 40 is the last offset that fits; 44 is one pixel too far.
        assert!(
            read_webgl_pixels_to_buffer(&gl, 0, 0, 3, 2, glow::RGBA, glow::UNSIGNED_BYTE, 44)
                .is_err()
        );
        assert_eq!(read_bound_pbo(&gl), [0xA5; 64], "a rejected read wrote");
        read_webgl_pixels_to_buffer(&gl, 0, 0, 3, 2, glow::RGBA, glow::UNSIGNED_BYTE, 40).unwrap();
        assert_eq!(gl.get_error(), glow::NO_ERROR);

        let bytes = read_bound_pbo(&gl);
        assert_eq!(bytes[..40], [0xA5; 40], "wrote outside the offset");
        for pixel in bytes[40..].chunks_exact(4) {
            assert_eq!(pixel, [255, 0, 0, 255]);
        }
        // The content's PACK state is the driver's to use here, not ours to
        // replace: an alignment of 8 pads each row to 16 bytes, so the same
        // three-pixel rows now need 16 + 12 bytes and no longer fit at 40.
        // Refill first -- the read above owns 40..64 and its pixels would
        // otherwise be mistaken for this one writing into the row padding.
        gl.buffer_data_u8_slice(glow::PIXEL_PACK_BUFFER, &[0xA5; 64], glow::STREAM_READ);
        gl.pixel_store_i32(glow::PACK_ALIGNMENT, 8);
        assert!(
            read_webgl_pixels_to_buffer(&gl, 0, 0, 3, 2, glow::RGBA, glow::UNSIGNED_BYTE, 40)
                .is_err()
        );
        read_webgl_pixels_to_buffer(&gl, 0, 0, 3, 2, glow::RGBA, glow::UNSIGNED_BYTE, 32).unwrap();
        assert_eq!(gl.get_error(), glow::NO_ERROR);
        let padded = read_bound_pbo(&gl);
        let red: [u8; 12] = [255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255];
        assert_eq!(padded[..32], [0xA5; 32], "wrote before the offset");
        assert_eq!(padded[32..44], red, "first row");
        // The 8-aligned stride leaves the four bytes after the first row alone.
        assert_eq!(padded[44..48], [0xA5; 4], "row padding was written");
        assert_eq!(padded[48..60], red, "second row");
        assert_eq!(padded[60..], [0xA5; 4], "wrote past the footprint");
        assert_eq!(PixelPackState::capture(&gl).values, [8, 0, 0, 0]);

        gl.bind_buffer(glow::PIXEL_PACK_BUFFER, None);
        assert!(
            read_webgl_pixels_to_buffer(&gl, 0, 0, 3, 2, glow::RGBA, glow::UNSIGNED_BYTE, 0)
                .is_err(),
            "a GPU destination requires a bound buffer"
        );
        assert_eq!(gl.get_error(), glow::NO_ERROR);
        gl.delete_buffer(pbo);
        eprintln!("renderer: {}", gl.get_parameter_string(glow::RENDERER));
    }
}

/// A rect that hangs off the framebuffer must leave the outside bytes at zero.
///
/// WebGL requires the values outside the framebuffer to be defined rather than
/// whatever the staging allocation happened to contain, and the compact staging
/// buffer is zero-filled precisely so a driver that skips those pixels cannot
/// publish uninitialized memory. This pins that against a real driver: the
/// in-range window has to land at its own offsets inside the destination, not
/// packed to the front.
#[test]
#[ignore = "requires Mesa surfaceless EGL and GLES3"]
fn webgl_readback_native_clips_to_the_framebuffer_and_zeroes_the_rest() {
    let (_scope, gl) = gles3_context();
    unsafe {
        // The fixture surface is 3x2.
        gl.clear_color(1.0, 0.0, 0.0, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
        for (name, value) in Pack::NAMES.into_iter().zip([1, 0, 0, 0]) {
            gl.pixel_store_i32(name, value);
        }
        assert_eq!(gl.get_error(), glow::NO_ERROR);

        // 5x4 starting one pixel outside the origin: rows 1..3 and columns 1..4
        // of the destination are the framebuffer, everything else is outside.
        let read = read_webgl_pixels(
            &gl,
            -1,
            -1,
            5,
            4,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            5 * 4 * 4,
            || Ok(()),
        )
        .unwrap();
        assert_eq!(
            gl.get_error(),
            glow::NO_ERROR,
            "a clipped read is not an error"
        );
        assert_eq!(read.pixels.len(), 80);
        for row in 0..4 {
            for column in 0..5 {
                let inside = (1..3).contains(&row) && (1..4).contains(&column);
                let at = (row * 5 + column) * 4;
                let pixel = &read.pixels[at..at + 4];
                let expected: [u8; 4] = if inside {
                    [255, 0, 0, 255]
                } else {
                    [0, 0, 0, 0]
                };
                assert_eq!(pixel, expected, "row {row} column {column}");
            }
        }
        eprintln!("renderer: {}", gl.get_parameter_string(glow::RENDERER));
    }
}

/// Every requested rectangle is checked against the 3x2 framebuffer while the
/// PBO starts with a non-zero sentinel. This distinguishes an untouched
/// out-of-framebuffer byte from a driver-written zero.
#[test]
#[ignore = "requires Mesa surfaceless EGL and GLES3"]
fn webgl_readback_native_clipping_matrix_preserves_outside_bytes() {
    let (_scope, gl) = gles3_context();
    unsafe {
        gl.clear_color(1.0, 0.0, 0.0, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
        for (name, value) in Pack::NAMES.into_iter().zip([1, 0, 0, 0]) {
            gl.pixel_store_i32(name, value);
        }
        let pbo = gl.create_buffer().unwrap();
        gl.bind_buffer(glow::PIXEL_PACK_BUFFER, Some(pbo));
        assert_eq!(gl.get_error(), glow::NO_ERROR);

        let cases = [
            // One partial overlap for each edge.
            (-1, 0, 4, 2),
            (2, 0, 4, 2),
            (0, -1, 3, 3),
            (0, 1, 3, 3),
            // All four corners.
            (-1, -1, 4, 3),
            (2, -1, 4, 3),
            (-1, 1, 4, 3),
            (2, 1, 4, 3),
            // Fully disjoint, followed by both zero-area forms.
            (-4, 0, 2, 2),
            (0, 0, 0, 3),
            (0, 0, 3, 0),
        ];
        for (x, y, width, height) in cases {
            let byte_len = (width * height * 4) as usize;
            gl.buffer_data_u8_slice(
                glow::PIXEL_PACK_BUFFER,
                &vec![0xA5; byte_len],
                glow::STREAM_READ,
            );
            assert_eq!(
                gl.get_error(),
                glow::NO_ERROR,
                "case {x},{y} {width}x{height}"
            );
            read_webgl_pixels_to_buffer(
                &gl,
                x,
                y,
                width,
                height,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                0,
            )
            .unwrap();
            let bytes = read_bound_pbo_bytes(&gl, byte_len);
            for row in 0..height {
                for column in 0..width {
                    let inside = (0..3).contains(&(x + column)) && (0..2).contains(&(y + row));
                    let at = ((row * width + column) * 4) as usize;
                    let expected = if inside { [255, 0, 0, 255] } else { [0xA5; 4] };
                    assert_eq!(
                        bytes[at..at + 4],
                        expected,
                        "case {x},{y} {width}x{height}, row {row}, column {column}"
                    );
                }
            }
            assert_eq!(gl.get_error(), glow::NO_ERROR);
        }
        gl.delete_buffer(pbo);
    }
}

unsafe fn read_bound_pbo_bytes(gl: &glow::Context, len: usize) -> Vec<u8> {
    if len == 0 {
        return Vec::new();
    }
    // GLES3 exposes CPU access through MapBufferRange; GetBufferSubData is a
    // desktop GL API.
    unsafe {
        let mapped =
            gl.map_buffer_range(glow::PIXEL_PACK_BUFFER, 0, len as i32, glow::MAP_READ_BIT);
        assert_eq!(gl.get_error(), glow::NO_ERROR);
        assert!(!mapped.is_null());
        let bytes = std::slice::from_raw_parts(mapped, len).to_vec();
        gl.unmap_buffer(glow::PIXEL_PACK_BUFFER);
        assert_eq!(gl.get_error(), glow::NO_ERROR);
        bytes
    }
}

unsafe fn read_bound_pbo(gl: &glow::Context) -> [u8; 64] {
    unsafe { read_bound_pbo_bytes(gl, 64) }
        .try_into()
        .expect("fixed-size PBO fixture")
}

#[test]
#[ignore = "requires Mesa surfaceless EGL and GLES3"]
fn framebuffer_copy_native_preserves_source_and_destination_bindings() {
    use crate::backend::gl::{
        state_tracker,
        texture_copy::{TextureCopy, copy_texture},
    };
    let (_scope, gl) = gles3_context();
    unsafe {
        let read_fbo = gl.create_framebuffer().unwrap();
        let draw_fbo = gl.create_framebuffer().unwrap();
        let copy_fbo = gl.create_framebuffer().unwrap();
        let read_texture = rgba_texture(&gl, 1, 1, &[255, 0, 0, 255]);
        let draw_texture = rgba_texture(&gl, 1, 1, &[0, 0, 255, 255]);
        for (fbo, texture) in [(read_fbo, read_texture), (draw_fbo, draw_texture)] {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(texture),
                0,
            );
            assert_eq!(
                gl.check_framebuffer_status(glow::FRAMEBUFFER),
                glow::FRAMEBUFFER_COMPLETE
            );
        }
        let mut shadow = crate::canvas::CanvasGLState::default();
        // Client IDs intentionally differ from native GL names. Exercise the
        // production dedup against actual driver state and pixel values.
        for (target, client, native) in [
            (glow::FRAMEBUFFER, 9001, read_fbo),
            (glow::READ_FRAMEBUFFER, 9002, draw_fbo),
            (glow::FRAMEBUFFER, 9001, read_fbo),
            (glow::DRAW_FRAMEBUFFER, 9002, draw_fbo),
        ] {
            assert_ne!(client, native.0.get());
            if state_tracker::update_bind_framebuffer(&mut shadow, target, Some(client)) {
                gl.bind_framebuffer(target, Some(native));
            }
        }
        assert_eq!(
            Rgba8Readback::new(1, 1).unwrap().read(&gl),
            [255, 0, 0, 255]
        );

        let source_pixels: [u8; 24] = [
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255, 0, 255, 255, 255,
            255, 0, 255, 255,
        ];
        let source = rgba_texture(&gl, 3, 2, &source_pixels);
        let destination = gl.create_texture().unwrap();
        gl.bind_texture(glow::TEXTURE_2D, Some(destination));
        // Named client FBO, virtual default backed by a native FBO, real FBO 0.
        for (client, native) in [
            (Some(9001), Some(read_fbo)),
            (None, Some(read_fbo)),
            (None, None),
        ] {
            shadow
                .bound_framebuffer
                .update(glow::READ_FRAMEBUFFER, client);
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, native);
            let status = copy_texture(
                &gl,
                copy_fbo,
                source,
                TextureCopy::Image {
                    target: glow::TEXTURE_2D,
                    level: 0,
                    internal_format: glow::RGBA,
                    source_x: 1,
                    source_y: 0,
                    width: 2,
                    height: 2,
                },
            );
            assert_eq!(status, glow::FRAMEBUFFER_COMPLETE);
            assert_eq!(
                gl.get_parameter_framebuffer(glow::READ_FRAMEBUFFER_BINDING),
                native
            );
            assert_eq!(
                gl.get_parameter_framebuffer(glow::DRAW_FRAMEBUFFER_BINDING),
                Some(draw_fbo)
            );
            assert_eq!(
                shadow.bound_framebuffer.get(glow::READ_FRAMEBUFFER),
                Some(client)
            );
            assert_eq!(
                shadow.bound_framebuffer.get(glow::DRAW_FRAMEBUFFER),
                Some(Some(9002))
            );
            assert!(!state_tracker::update_bind_framebuffer(
                &mut shadow,
                glow::READ_FRAMEBUFFER,
                client
            ));
            assert_eq!(
                read_copy_texture(&gl, copy_fbo, destination, 2, 2),
                [
                    0, 255, 0, 255, 0, 0, 255, 255, 0, 255, 255, 255, 255, 0, 255, 255
                ]
            );
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, native);
        }

        // Subimage copy preserves destination pixels outside its rectangle.
        gl.bind_texture(glow::TEXTURE_2D, Some(destination));
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA as i32,
            3,
            2,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(Some(&[165; 24])),
        );
        gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(read_fbo));
        assert_eq!(
            copy_texture(
                &gl,
                copy_fbo,
                source,
                TextureCopy::SubImage {
                    target: glow::TEXTURE_2D,
                    level: 0,
                    xoffset: 1,
                    yoffset: 1,
                    width: 2,
                    height: 1,
                }
            ),
            glow::FRAMEBUFFER_COMPLETE
        );
        assert_eq!(
            gl.get_parameter_framebuffer(glow::READ_FRAMEBUFFER_BINDING),
            Some(read_fbo)
        );
        assert_eq!(
            gl.get_parameter_framebuffer(glow::DRAW_FRAMEBUFFER_BINDING),
            Some(draw_fbo)
        );
        let mut expected = vec![165; 24];
        expected[16..].copy_from_slice(&source_pixels[..8]);
        assert_eq!(
            read_copy_texture(&gl, copy_fbo, destination, 3, 2),
            expected
        );
        gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(read_fbo));

        // An unallocated source is incomplete: no copy, no retained attachment,
        // and both binding points survive the failure.
        let incomplete = gl.create_texture().unwrap();
        gl.bind_texture(glow::TEXTURE_2D, Some(incomplete));
        gl.bind_texture(glow::TEXTURE_2D, Some(destination));
        assert_ne!(
            copy_texture(
                &gl,
                copy_fbo,
                incomplete,
                TextureCopy::SubImage {
                    target: glow::TEXTURE_2D,
                    level: 0,
                    xoffset: 0,
                    yoffset: 0,
                    width: 1,
                    height: 1,
                }
            ),
            glow::FRAMEBUFFER_COMPLETE
        );
        assert_eq!(
            gl.get_parameter_framebuffer(glow::READ_FRAMEBUFFER_BINDING),
            Some(read_fbo)
        );
        assert_eq!(
            gl.get_parameter_framebuffer(glow::DRAW_FRAMEBUFFER_BINDING),
            Some(draw_fbo)
        );
        assert_eq!(
            read_copy_texture(&gl, copy_fbo, destination, 3, 2),
            expected
        );
        assert_eq!(gl.get_error(), glow::NO_ERROR);
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        for fbo in [read_fbo, draw_fbo, copy_fbo] {
            gl.delete_framebuffer(fbo);
        }
        for texture in [source, destination, read_texture, draw_texture, incomplete] {
            gl.delete_texture(texture);
        }
        eprintln!("renderer: {}", gl.get_parameter_string(glow::RENDERER));
    }
}

unsafe fn rgba_texture(
    gl: &glow::Context,
    width: i32,
    height: i32,
    pixels: &[u8],
) -> glow::NativeTexture {
    unsafe {
        let texture = gl.create_texture().unwrap();
        gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA as i32,
            width,
            height,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(Some(pixels)),
        );
        texture
    }
}

unsafe fn read_copy_texture(
    gl: &glow::Context,
    copy_fbo: glow::NativeFramebuffer,
    texture: glow::NativeTexture,
    width: u32,
    height: u32,
) -> Vec<u8> {
    unsafe {
        gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(copy_fbo));
        assert_eq!(
            gl.get_framebuffer_attachment_parameter_i32(
                glow::READ_FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE
            ),
            glow::NONE as i32,
            "copy helper must detach the source even on failure"
        );
        gl.framebuffer_texture_2d(
            glow::READ_FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            Some(texture),
            0,
        );
        assert_eq!(
            gl.check_framebuffer_status(glow::READ_FRAMEBUFFER),
            glow::FRAMEBUFFER_COMPLETE
        );
        let pixels = Rgba8Readback::new(width, height).unwrap().read(gl);
        gl.framebuffer_texture_2d(
            glow::READ_FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            None,
            0,
        );
        pixels
    }
}
