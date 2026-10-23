//! The encoder thread: draws each frame the render thread copied into the
//! encoder's input, on a context in the render thread's share group, so an
//! encoder that falls behind blocks this thread and never the game's.

use std::{path::PathBuf, sync::Arc};

use crossbeam_channel::{Receiver, Sender};
use glow::HasContext;
use shared::{
    error::{EngineError, EngineResult, ErrorCode},
    game_recorder::RecorderEvents,
    protocol::render_cmd::RenderCmdResp,
};
use tracing::{info, warn};

use super::{EncoderConfig, FromEncoder, SyncHandle, ToEncoder, VideoEncoder, VideoEncoderFactory};
use crate::egl_platform::{EglInstance, EglProvider, GraphicsBackendId};

/// `EGL_RECORDABLE_ANDROID`: a config whose window surfaces an encoder's input
/// takes. Asked for first; a driver without one is given the render config.
const EGL_RECORDABLE_ANDROID: khronos_egl::Int = 0x3142;

/// `eglPresentationTimeANDROID`: the time a frame drawn into an encoder's
/// input is shown at. Where the EGL has none, the encoder stamps arrival.
type PresentationTime = unsafe extern "system" fn(
    khronos_egl::EGLDisplay,
    khronos_egl::EGLSurface,
    i64,
) -> khronos_egl::Boolean;

pub(super) struct Args {
    pub egl_provider: Arc<dyn EglProvider>,
    pub display: khronos_egl::Display,
    pub config: khronos_egl::Config,
    pub share: khronos_egl::Context,
    pub context_attribs: Vec<khronos_egl::Int>,
    pub factory: Arc<dyn VideoEncoderFactory>,
    pub encoder: EncoderConfig,
    pub path: PathBuf,
    pub textures: Vec<glow::NativeTexture>,
    pub events: Arc<RecorderEvents>,
    pub start: RenderCmdResp<()>,
    pub rx: Receiver<ToEncoder>,
    pub tx: Sender<FromEncoder>,
}

/// What crosses to the thread: EGL handles are not `Send`, and cross as the
/// pointers they are, for a thread that loads the same EGL.
struct Carried {
    egl_provider: Arc<dyn EglProvider>,
    backend: GraphicsBackendId,
    display: usize,
    config: usize,
    share: usize,
    context_attribs: Vec<khronos_egl::Int>,
    factory: Arc<dyn VideoEncoderFactory>,
    encoder: EncoderConfig,
    path: PathBuf,
    textures: Vec<glow::NativeTexture>,
    events: Arc<RecorderEvents>,
    rx: Receiver<ToEncoder>,
    tx: Sender<FromEncoder>,
}

// SAFETY: the EGL handles are pointers into the process's one EGL display,
// used on the new thread only; the textures are share-group names.
unsafe impl Send for Carried {}

/// Start the encoder thread. It answers `args.start` itself: once it encodes,
/// or with why it cannot. Whoever holds the handle joins it before the EGL
/// display it uses is terminated.
pub(super) fn spawn(args: Args) -> EngineResult<std::thread::JoinHandle<()>> {
    let carried = Carried {
        backend: args.egl_provider.backend_id(),
        egl_provider: args.egl_provider,
        display: args.display.as_ptr() as usize,
        config: args.config.as_ptr() as usize,
        share: args.share.as_ptr() as usize,
        context_attribs: args.context_attribs,
        factory: args.factory,
        encoder: args.encoder,
        path: args.path,
        textures: args.textures,
        events: args.events,
        rx: args.rx,
        tx: args.tx,
    };
    let start = args.start;
    std::thread::Builder::new()
        .name("Migo-GameRecorder".into())
        // A thread that could not be started drops the reply, which answers
        // content's call as cancelled.
        .spawn(move || run(carried, start))
        .map_err(|e| failure("the recording thread could not start", e))
}

fn failure(what: &'static str, detail: impl std::fmt::Display) -> EngineError {
    EngineError::new(ErrorCode::RenderBackendError)
        .with_msg(what)
        .with_detail(detail.to_string())
}

/// The thread's EGL and GL, and the encoder it feeds.
struct Session {
    egl: EglInstance,
    display: khronos_egl::Display,
    context: khronos_egl::Context,
    surface: khronos_egl::Surface,
    gl: glow::Context,
    presentation_time: Option<PresentationTime>,
    framebuffers: Vec<glow::NativeFramebuffer>,
    size: (i32, i32),
}

fn run(carried: Carried, start: RenderCmdResp<()>) {
    let _autorelease_thread = shared::objc_autorelease::autorelease_scope();
    let Carried {
        egl_provider,
        backend,
        display,
        config,
        share,
        context_attribs,
        factory,
        encoder: config_e,
        path,
        textures,
        events,
        rx,
        tx,
    } = carried;

    let encoder = match factory.create(&config_e, &path) {
        Ok(encoder) => encoder,
        Err(e) => {
            start.err(e);
            let _ = tx.send(FromEncoder::Failed);
            return;
        }
    };
    let session = match open(
        &*egl_provider,
        backend,
        display,
        config,
        share,
        &context_attribs,
        &*encoder,
        &textures,
        config_e,
    ) {
        Ok(session) => session,
        Err(e) => {
            encoder.discard();
            let _ = std::fs::remove_file(&path);
            start.err(e);
            let _ = tx.send(FromEncoder::Failed);
            return;
        }
    };
    info!(
        width = config_e.width,
        height = config_e.height,
        fps = config_e.fps,
        "game recorder: encoding"
    );
    let _ = tx.send(FromEncoder::Ready);
    start.ok(());
    encode(session, encoder, textures, &path, &events, &rx, &tx);
}

/// Make this thread's context and the encoder's input surface, and wrap each
/// shared texture in a framebuffer of this context.
#[allow(clippy::too_many_arguments)]
fn open(
    provider: &dyn EglProvider,
    backend: GraphicsBackendId,
    display: usize,
    render_config: usize,
    share: usize,
    context_attribs: &[khronos_egl::Int],
    encoder: &dyn VideoEncoder,
    textures: &[glow::NativeTexture],
    encoding: EncoderConfig,
) -> EngineResult<Session> {
    if provider.backend_id() != backend {
        return Err(failure(
            "the recording's EGL provider changed",
            provider.label(),
        ));
    }
    let egl = provider.load()?;
    let display = unsafe { khronos_egl::Display::from_ptr(display as *mut _) };
    let render_config = unsafe { khronos_egl::Config::from_ptr(render_config as *mut _) };
    let share = unsafe { khronos_egl::Context::from_ptr(share as *mut _) };

    // The encoder's config first, the render one if the driver has none.
    let recordable = [
        khronos_egl::RED_SIZE,
        8,
        khronos_egl::GREEN_SIZE,
        8,
        khronos_egl::BLUE_SIZE,
        8,
        khronos_egl::ALPHA_SIZE,
        8,
        khronos_egl::SURFACE_TYPE,
        khronos_egl::WINDOW_BIT,
        khronos_egl::RENDERABLE_TYPE,
        0x0040, // ES3
        EGL_RECORDABLE_ANDROID,
        khronos_egl::TRUE as khronos_egl::Int,
        khronos_egl::NONE,
    ];
    let configs = egl
        .choose_first_config(display, &recordable)
        .ok()
        .flatten()
        .into_iter()
        .chain([render_config]);
    let window = encoder.input_window();
    let mut last = None;
    for config in configs {
        let context = match egl.create_context(display, config, Some(share), context_attribs) {
            Ok(context) => context,
            Err(e) => {
                last = Some(e);
                continue;
            }
        };
        let surface = match unsafe { egl.create_window_surface(display, config, window, None) } {
            Ok(surface) => surface,
            Err(e) => {
                let _ = egl.destroy_context(display, context);
                last = Some(e);
                continue;
            }
        };
        if let Err(e) = egl.make_current(display, Some(surface), Some(surface), Some(context)) {
            let _ = egl.destroy_surface(display, surface);
            let _ = egl.destroy_context(display, context);
            last = Some(e);
            continue;
        }
        let gl = unsafe {
            glow::Context::from_loader_function(|name| {
                egl.get_proc_address(name)
                    .map_or(std::ptr::null(), |f| f as *const std::ffi::c_void)
            })
        };
        let presentation_time = egl
            .get_proc_address("eglPresentationTimeANDROID")
            .map(|f| unsafe { std::mem::transmute::<extern "system" fn(), PresentationTime>(f) });
        let mut framebuffers = Vec::with_capacity(textures.len());
        for &texture in textures {
            let framebuffer = unsafe { gl.create_framebuffer() }
                .map_err(|e| failure("a recording framebuffer", e))?;
            unsafe {
                gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(framebuffer));
                gl.framebuffer_texture_2d(
                    glow::READ_FRAMEBUFFER,
                    glow::COLOR_ATTACHMENT0,
                    glow::TEXTURE_2D,
                    Some(texture),
                    0,
                );
            }
            framebuffers.push(framebuffer);
        }
        return Ok(Session {
            egl,
            display,
            context,
            surface,
            gl,
            presentation_time,
            framebuffers,
            size: (encoding.width as i32, encoding.height as i32),
        });
    }
    Err(failure(
        "the encoder's input could not be drawn into",
        format!("{last:?}"),
    ))
}

/// Draw every frame the render thread sends into the encoder's input until it
/// says to finish or to discard, or the encoder fails.
fn encode(
    session: Session,
    mut encoder: Box<dyn VideoEncoder>,
    textures: Vec<glow::NativeTexture>,
    path: &std::path::Path,
    events: &RecorderEvents,
    rx: &Receiver<ToEncoder>,
    tx: &Sender<FromEncoder>,
) {
    let gl = &session.gl;
    let outcome = loop {
        let Ok(message) = rx.recv() else {
            // The render thread is gone without a word: the session ended.
            break End::Discard(None);
        };
        match message {
            ToEncoder::Frame {
                slot,
                ready: SyncHandle(ready),
                pts_ns,
            } => {
                let _autorelease_frame = shared::objc_autorelease::autorelease_scope();
                let released = unsafe {
                    gl.wait_sync(ready, 0, glow::TIMEOUT_IGNORED);
                    gl.delete_sync(ready);
                    gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(session.framebuffers[slot]));
                    gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, None);
                    let (w, h) = session.size;
                    gl.blit_framebuffer(
                        0,
                        0,
                        w,
                        h,
                        0,
                        0,
                        w,
                        h,
                        glow::COLOR_BUFFER_BIT,
                        glow::NEAREST,
                    );
                    gl.fence_sync(glow::SYNC_GPU_COMMANDS_COMPLETE, 0)
                };
                if let Some(presentation_time) = session.presentation_time {
                    unsafe {
                        presentation_time(
                            session.display.as_ptr(),
                            session.surface.as_ptr(),
                            pts_ns,
                        )
                    };
                }
                if let Err(e) = session.egl.swap_buffers(session.display, session.surface) {
                    break End::Failed(format!("the encoder's input refused a frame: {e:?}"));
                }
                match released {
                    Ok(released) => {
                        let _ = tx.send(FromEncoder::Released {
                            slot,
                            released: SyncHandle(released),
                        });
                    }
                    Err(e) => break End::Failed(format!("a frame could not be fenced: {e}")),
                }
                if let Err(e) = encoder.drain() {
                    break End::Failed(e.to_string());
                }
            }
            ToEncoder::Finish { duration_ms, resp } => break End::Finish(duration_ms, resp),
            ToEncoder::Discard { resp } => break End::Discard(resp),
        }
    };
    if let End::Failed(_) = outcome {
        // Frames already sent: their fences are deleted while this context is
        // still current; the render thread sends none once it hears.
        let _ = tx.send(FromEncoder::Failed);
        while let Ok(ToEncoder::Frame {
            ready: SyncHandle(ready),
            ..
        }) = rx.try_recv()
        {
            unsafe { gl.delete_sync(ready) };
        }
    }
    close(session, textures);
    match outcome {
        End::Finish(duration_ms, resp) => match encoder.finish() {
            Ok(()) => {
                events.close();
                resp.ok(duration_ms);
            }
            Err(e) => {
                let _ = std::fs::remove_file(path);
                events.fail(e.to_string());
                resp.err(e);
            }
        },
        End::Discard(resp) => {
            encoder.discard();
            let _ = std::fs::remove_file(path);
            events.close();
            if let Some(resp) = resp {
                resp.ok(());
            }
        }
        End::Failed(reason) => {
            warn!("game recorder: {reason}");
            encoder.discard();
            let _ = std::fs::remove_file(path);
            events.fail(reason);
            // What the render thread asks before it hears is answered here:
            // a stop as failed, an abort as done.
            for message in rx.iter() {
                match message {
                    ToEncoder::Finish { resp, .. } => resp.err(failure("the recording failed", "")),
                    ToEncoder::Discard { resp: Some(resp) } => resp.ok(()),
                    ToEncoder::Frame { .. } | ToEncoder::Discard { resp: None } => {}
                }
            }
        }
    }
}

enum End {
    Finish(u64, RenderCmdResp<u64>),
    Discard(Option<RenderCmdResp<()>>),
    Failed(String),
}

/// This thread's GL objects, the textures it read for the last time, its
/// surface and its context. The encoder's input is no longer drawn into after.
fn close(session: Session, textures: Vec<glow::NativeTexture>) {
    let Session {
        egl,
        display,
        context,
        surface,
        gl,
        framebuffers,
        ..
    } = session;
    unsafe {
        for framebuffer in framebuffers {
            gl.delete_framebuffer(framebuffer);
        }
        for texture in textures {
            gl.delete_texture(texture);
        }
        gl.finish();
    }
    let _ = egl.make_current(display, None, None, None);
    let _ = egl.destroy_surface(display, surface);
    let _ = egl.destroy_context(display, context);
}
