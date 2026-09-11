//! Deterministic GL boundary for readback state tests. No native driver writes.
//!
//! Also home to the crate's single native-EGL display lock, because it is the
//! one test-support module every GL fixture can already reach.

use std::{cell::RefCell, ffi::c_void};

// Mesa's surfaceless platform hands the *same* EGLDisplay to every fixture in
// this crate, and each fixture's scope guard calls `eglTerminate` on drop.
// Two modules used to keep a private mutex each -- `readback_native_test.rs`
// and `canvas/manager/drawing_buffer.rs` -- which serialised each module
// against itself and neither against the other, so a `drawing_buffer` test
// could terminate the display out from under an in-flight readback test. That
// presented as an intermittent `BadDisplay`/`BadContext` in whichever native
// test happened to lose the race, only under parallel `--ignored native` runs.
// One display means one lock: every native EGL fixture takes this one.
static EGL_TEST_DISPLAY: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Hold the surfaceless display for one whole test, poisoning tolerated: a
/// panicking test still has to hand the display to the next one.
pub(crate) fn lock_egl_display() -> std::sync::MutexGuard<'static, ()> {
    EGL_TEST_DISPLAY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Bindings {
    // alignment, row length, skipped rows, skipped pixels
    pub pack: [i32; 4],
    pub unpack: [i32; 4],
    pub pack_buffer: u32,
    pub pack_buffer_size: i32,
    pub unpack_buffer: u32,
    pub read_framebuffer: u32,
    pub draw_framebuffer: u32,
    pub active_texture: u32,
}

impl Default for Bindings {
    fn default() -> Self {
        Self {
            pack: [4, 0, 0, 0],
            unpack: [4, 0, 0, 0],
            pack_buffer: 0,
            pack_buffer_size: 0,
            unpack_buffer: 0,
            read_framebuffer: 0,
            draw_framebuffer: 0,
            active_texture: glow::TEXTURE0,
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
    errors: std::collections::VecDeque<u32>,
    /// Raised *by* the next read, so it lands after the pre-read drain the way
    /// a real driver rejection does.
    read_error: u32,
    /// `0` means "never set"; the accessor answers COMPLETE in that case, so a
    /// fixture that does not care about framebuffers behaves like a good one.
    framebuffer_status: u32,
    bindings: Bindings,
    reads: Vec<Read>,
    mutations: usize,
    gles2: bool,
    pack_subimage: bool,
    unpack_subimage: bool,
    pixel_buffer: bool,
    framebuffer_blit: bool,
    extensions: std::ffi::CString,
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

/// Extension-gated GLES2 capabilities. A driver can expose the pack half of
/// `pixel_store` without the unpack half, so they are independent knobs.
#[derive(Clone, Copy, Default)]
pub(crate) struct Gles2Caps {
    pub pack_subimage: bool,
    pub unpack_subimage: bool,
    pub pixel_buffer: bool,
    pub framebuffer_blit: bool,
}

pub(crate) fn context_gles2(caps: Gles2Caps) -> glow::Context {
    let mut extensions = Vec::new();
    if caps.pack_subimage {
        extensions.push("GL_NV_pack_subimage");
    }
    if caps.unpack_subimage {
        extensions.push("GL_EXT_unpack_subimage");
    }
    if caps.pixel_buffer {
        extensions.push("GL_NV_pixel_buffer_object");
    }
    if caps.framebuffer_blit {
        extensions.push("GL_ANGLE_framebuffer_blit");
    }
    STATE.with(|s| {
        *s.borrow_mut() = State {
            gles2: true,
            pack_subimage: caps.pack_subimage,
            unpack_subimage: caps.unpack_subimage,
            pixel_buffer: caps.pixel_buffer,
            framebuffer_blit: caps.framebuffer_blit,
            extensions: std::ffi::CString::new(extensions.join(" ")).expect("no interior nul"),
            ..Default::default()
        }
    });
    load_context()
}

pub(crate) fn unsupported_calls() -> usize {
    STATE.with(|s| s.borrow().unsupported_calls)
}

pub(crate) fn context_gles2_framebuffer_blit() -> glow::Context {
    context_gles2(Gles2Caps {
        framebuffer_blit: true,
        ..Default::default()
    })
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
            "glGetBufferParameteriv" => get_buffer_parameter as *const c_void,
            "glGetError" => get_error as *const c_void,
            "glCheckFramebufferStatus" => check_framebuffer_status as *const c_void,
            _ => std::ptr::null(),
        })
    }
}

/// Errors the fixture hands out, oldest first, exactly like a driver queue.
pub(crate) fn queue_error(error: u32) {
    STATE.with(|s| s.borrow_mut().errors.push_back(error));
}

/// The error the next `glReadPixels` raises; `NO_ERROR` clears it.
pub(crate) fn set_read_error(error: u32) {
    STATE.with(|s| s.borrow_mut().read_error = error);
}

pub(crate) fn pending_errors() -> usize {
    STATE.with(|s| s.borrow().errors.len())
}

/// What `glCheckFramebufferStatus` answers; `FRAMEBUFFER_COMPLETE` by default.
pub(crate) fn set_framebuffer_status(status: u32) {
    STATE.with(|s| s.borrow_mut().framebuffer_status = status);
}

unsafe extern "system" fn get_error() -> u32 {
    STATE.with(|s| s.borrow_mut().errors.pop_front().unwrap_or(glow::NO_ERROR))
}

unsafe extern "system" fn check_framebuffer_status(target: u32) -> u32 {
    if record_unsupported(target) {
        return 0;
    }
    STATE.with(|s| match s.borrow().framebuffer_status {
        0 => glow::FRAMEBUFFER_COMPLETE,
        status => status,
    })
}

unsafe extern "system" fn get_buffer_parameter(target: u32, name: u32, value: *mut i32) {
    if record_unsupported(target) {
        unsafe { value.write(0) };
        return;
    }
    let b = bindings();
    let result = match (target, name) {
        (glow::PIXEL_PACK_BUFFER, glow::BUFFER_SIZE) => b.pack_buffer_size,
        _ => 0,
    };
    unsafe { value.write(result) };
}

unsafe extern "system" fn get_string(name: u32) -> *const u8 {
    STATE.with(|s| {
        let s = s.borrow();
        // glow copies this string while the context is being created, so a
        // pointer into the fixture's own storage outlives every read of it.
        let value = if name == glow::EXTENSIONS {
            s.extensions.as_c_str()
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

const UNPACK_NAMES: [u32; 4] = [
    glow::UNPACK_ALIGNMENT,
    glow::UNPACK_ROW_LENGTH,
    glow::UNPACK_SKIP_ROWS,
    glow::UNPACK_SKIP_PIXELS,
];

unsafe extern "system" fn get_integer(name: u32, value: *mut i32) {
    if record_unsupported(name) {
        unsafe { value.write(0) };
        return;
    }
    let b = bindings();
    let result = if let Some(index) = PACK_NAMES.iter().position(|&p| p == name) {
        b.pack[index]
    } else if let Some(index) = UNPACK_NAMES.iter().position(|&p| p == name) {
        b.unpack[index]
    } else {
        match name {
            glow::PIXEL_PACK_BUFFER_BINDING => b.pack_buffer as i32,
            glow::READ_FRAMEBUFFER_BINDING => b.read_framebuffer as i32,
            glow::DRAW_FRAMEBUFFER_BINDING => b.draw_framebuffer as i32,
            glow::ACTIVE_TEXTURE => b.active_texture as i32,
            glow::PIXEL_UNPACK_BUFFER_BINDING => b.unpack_buffer as i32,
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
        } else if let Some(index) = UNPACK_NAMES.iter().position(|&p| p == name) {
            s.bindings.unpack[index] = value;
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
                || (!s.unpack_subimage && UNPACK_NAMES[1..].contains(&name))
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
        if s.read_error != glow::NO_ERROR {
            let error = s.read_error;
            s.errors.push_back(error);
        }
    });
}
