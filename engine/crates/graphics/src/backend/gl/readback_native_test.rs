//! Opt-in pixel validation on Mesa's surfaceless EGL platform.

use super::*;
use khronos_egl as egl;

// Mesa returns the same surfaceless EGLDisplay to both fixtures. Its lifetime
// must span one complete test; one test's eglTerminate cannot end the other's
// display while that test is still creating a context or surface.
static EGL_TEST_DISPLAY: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
    let display_lifetime = EGL_TEST_DISPLAY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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
    use crate::canvas::{apply_bypass_rebind, drawing_buffer};
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
        let mut applied = true;
        apply_bypass_rebind(&gl, false, &mut applied, false, Some(db.fbo));
        assert!(
            applied,
            "a mode change cannot be applied to a noncurrent context"
        );
        scope
            .api
            .make_current(scope.display, scope.surface, scope.surface, scope.context)
            .unwrap();
        apply_bypass_rebind(&gl, true, &mut applied, false, Some(db.fbo));
        assert!(!applied);
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
        apply_bypass_rebind(&gl, true, &mut applied, true, Some(db.fbo));
        assert!(applied);
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
        for (name, value) in PACK_NAMES.into_iter().zip([8, 9, 2, 3]) {
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

unsafe fn read_bound_pbo(gl: &glow::Context) -> [u8; 64] {
    // This fixture creates exactly 64 bytes. GLES3 exposes CPU access through
    // MapBufferRange; GetBufferSubData is a desktop GL API.
    unsafe {
        let mapped = gl.map_buffer_range(glow::PIXEL_PACK_BUFFER, 0, 64, glow::MAP_READ_BIT);
        assert_eq!(gl.get_error(), glow::NO_ERROR);
        assert!(!mapped.is_null());
        let mut bytes = [0; 64];
        bytes.copy_from_slice(std::slice::from_raw_parts(mapped, 64));
        gl.unmap_buffer(glow::PIXEL_PACK_BUFFER);
        assert_eq!(gl.get_error(), glow::NO_ERROR);
        bytes
    }
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
