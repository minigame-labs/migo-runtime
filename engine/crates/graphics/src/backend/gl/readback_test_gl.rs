//! Deterministic GL boundary for readback state tests. No native driver writes.

use std::{cell::RefCell, ffi::c_void};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Bindings {
    // alignment, row length, skipped rows, skipped pixels
    pub pack: [i32; 4],
    pub pack_buffer: u32,
    pub read_framebuffer: u32,
    pub draw_framebuffer: u32,
    pub active_texture: u32,
    pub unpack_buffer: u32,
    pub unpack_alignment: i32,
}

impl Default for Bindings {
    fn default() -> Self {
        Self {
            pack: [4, 0, 0, 0],
            pack_buffer: 0,
            read_framebuffer: 0,
            draw_framebuffer: 0,
            active_texture: glow::TEXTURE0,
            unpack_buffer: 0,
            unpack_alignment: 4,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Read {
    pub bindings: Bindings,
    pub width: i32,
    pub height: i32,
}

#[derive(Default)]
struct State {
    bindings: Bindings,
    reads: Vec<Read>,
    mutations: usize,
    gles2: bool,
    pack_subimage: bool,
    pixel_buffer: bool,
    framebuffer_blit: bool,
    unsupported_calls: usize,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

pub(crate) fn bindings() -> Bindings {
    STATE.with(|s| s.borrow().bindings)
}

pub(crate) fn set_bindings(bindings: Bindings) {
    STATE.with(|s| s.borrow_mut().bindings = bindings);
}

pub(crate) fn reads() -> Vec<Read> {
    STATE.with(|s| s.borrow().reads.clone())
}

pub(crate) fn mutations() -> usize {
    STATE.with(|s| s.borrow().mutations)
}

pub(crate) fn context() -> glow::Context {
    STATE.with(|s| *s.borrow_mut() = State::default());
    load_context()
}

pub(crate) fn context_gles2(pack_subimage: bool, pixel_buffer: bool) -> glow::Context {
    STATE.with(|s| {
        *s.borrow_mut() = State {
            gles2: true,
            pack_subimage,
            pixel_buffer,
            ..Default::default()
        }
    });
    load_context()
}

pub(crate) fn unsupported_calls() -> usize {
    STATE.with(|s| s.borrow().unsupported_calls)
}

pub(crate) fn context_gles2_framebuffer_blit() -> glow::Context {
    STATE.with(|s| {
        *s.borrow_mut() = State {
            gles2: true,
            framebuffer_blit: true,
            ..Default::default()
        }
    });
    load_context()
}

fn load_context() -> glow::Context {
    // Every implemented entry point follows GL's ABI. read_pixels intentionally
    // never dereferences the output pointer, so the regression cannot write past
    // a buffer even before production packing is fixed.
    unsafe {
        glow::Context::from_loader_function(|name| match name {
            "glGetString" => get_string as *const c_void,
            "glGetIntegerv" => get_integer as *const c_void,
            "glPixelStorei" => pixel_store as *const c_void,
            "glBindBuffer" => bind_buffer as *const c_void,
            "glBindFramebuffer" => bind_framebuffer as *const c_void,
            "glActiveTexture" => active_texture as *const c_void,
            "glReadPixels" => read_pixels as *const c_void,
            _ => std::ptr::null(),
        })
    }
}

unsafe extern "system" fn get_string(name: u32) -> *const u8 {
    STATE.with(|s| {
        let s = s.borrow();
        let value = if name == glow::EXTENSIONS && s.framebuffer_blit {
            c"GL_ANGLE_framebuffer_blit"
        } else if name == glow::EXTENSIONS {
            match (s.pack_subimage, s.pixel_buffer) {
                (true, true) => c"GL_NV_pack_subimage GL_NV_pixel_buffer_object",
                (true, false) => c"GL_NV_pack_subimage",
                (false, true) => c"GL_NV_pixel_buffer_object",
                (false, false) => c"",
            }
        } else if s.gles2 {
            c"OpenGL ES 2.0 readback-test"
        } else {
            c"OpenGL ES 3.0 readback-test"
        };
        value.as_ptr().cast()
    })
}

const PACK_NAMES: [u32; 4] = [
    glow::PACK_ALIGNMENT,
    glow::PACK_ROW_LENGTH,
    glow::PACK_SKIP_ROWS,
    glow::PACK_SKIP_PIXELS,
];

unsafe extern "system" fn get_integer(name: u32, value: *mut i32) {
    if record_unsupported(name) {
        unsafe { value.write(0) };
        return;
    }
    let b = bindings();
    let result = if let Some(index) = PACK_NAMES.iter().position(|&p| p == name) {
        b.pack[index]
    } else {
        match name {
            glow::PIXEL_PACK_BUFFER_BINDING => b.pack_buffer as i32,
            glow::READ_FRAMEBUFFER_BINDING => b.read_framebuffer as i32,
            glow::DRAW_FRAMEBUFFER_BINDING => b.draw_framebuffer as i32,
            glow::ACTIVE_TEXTURE => b.active_texture as i32,
            glow::PIXEL_UNPACK_BUFFER_BINDING => b.unpack_buffer as i32,
            glow::UNPACK_ALIGNMENT => b.unpack_alignment,
            _ => 0,
        }
    };
    unsafe { value.write(result) };
}

unsafe extern "system" fn pixel_store(name: u32, value: i32) {
    if record_unsupported(name) {
        return;
    }
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.mutations += 1;
        if let Some(index) = PACK_NAMES.iter().position(|&p| p == name) {
            s.bindings.pack[index] = value;
        } else if name == glow::UNPACK_ALIGNMENT {
            s.bindings.unpack_alignment = value;
        }
    });
}

unsafe extern "system" fn bind_buffer(target: u32, value: u32) {
    if record_unsupported(target) {
        return;
    }
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.mutations += 1;
        match target {
            glow::PIXEL_PACK_BUFFER => s.bindings.pack_buffer = value,
            glow::PIXEL_UNPACK_BUFFER => s.bindings.unpack_buffer = value,
            _ => {}
        }
    });
}

fn record_unsupported(name: u32) -> bool {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let unsupported = s.gles2
            && ((!s.pack_subimage && PACK_NAMES[1..].contains(&name))
                || (!s.framebuffer_blit
                    && matches!(
                        name,
                        glow::READ_FRAMEBUFFER_BINDING
                            | glow::READ_FRAMEBUFFER
                            | glow::DRAW_FRAMEBUFFER
                    ))
                || (!s.pixel_buffer
                    && matches!(
                        name,
                        glow::PIXEL_PACK_BUFFER | glow::PIXEL_PACK_BUFFER_BINDING
                    )));
        s.unsupported_calls += usize::from(unsupported);
        unsupported
    })
}

unsafe extern "system" fn bind_framebuffer(target: u32, value: u32) {
    if record_unsupported(target) {
        return;
    }
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.mutations += 1;
        if target == glow::FRAMEBUFFER || target == glow::READ_FRAMEBUFFER {
            s.bindings.read_framebuffer = value;
        }
        if target == glow::FRAMEBUFFER || target == glow::DRAW_FRAMEBUFFER {
            s.bindings.draw_framebuffer = value;
        }
    });
}

unsafe extern "system" fn active_texture(value: u32) {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.mutations += 1;
        s.bindings.active_texture = value;
    });
}

unsafe extern "system" fn read_pixels(
    _x: i32,
    _y: i32,
    width: i32,
    height: i32,
    _format: u32,
    _type: u32,
    _pixels: *mut c_void,
) {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let bindings = s.bindings;
        s.reads.push(Read {
            bindings,
            width,
            height,
        });
    });
}
