//! The encoder thread: draws each frame the render thread copied into the
//! encoder's input, on a context in the render thread's share group, so an
//! encoder that falls behind blocks this thread and never the game's.

use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};

use crossbeam_channel::{Receiver, Sender};
use glow::HasContext;
use shared::{
    error::{EngineError, EngineResult, ErrorCode},
    game_recorder::RecorderEvents,
    protocol::render_cmd::RenderCmdResp,
};
use tracing::{info, warn};

use super::{
    ClientBuffer, EncoderConfig, FrameInput, FromEncoder, SyncHandle, ToEncoder, VideoEncoder,
    VideoEncoderFactory,
};
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

/// How long a frame drawn into a lent buffer may take on the GPU before the
/// encoder is given it: far past any frame's time, so only a lost GPU reaches it.
const BUFFER_DRAWN_WITHIN: Duration = Duration::from_secs(2);

/// The thread's EGL and GL, and where it draws the frames.
struct Session {
    egl: EglInstance,
    display: khronos_egl::Display,
    context: khronos_egl::Context,
    /// Current while the context draws into no window of the encoder's: a 1x1
    /// pbuffer, or none where the EGL makes a context current without one.
    placeholder: Option<khronos_egl::Surface>,
    target: Target,
    gl: glow::Context,
    /// One per shared texture, to read it.
    framebuffers: Vec<glow::NativeFramebuffer>,
    size: (i32, i32),
}

/// Where a frame is drawn, as the encoder takes its input.
enum Target {
    /// The encoder's window: drawn into and swapped.
    Window {
        surface: khronos_egl::Surface,
        presentation_time: Option<PresentationTime>,
    },
    /// The buffers the encoder lends, and what was made for each, by its key.
    Buffers { bound: HashMap<u64, Bound> },
}

/// One of the encoder's buffers, as this context draws into it.
struct Bound {
    pbuffer: khronos_egl::Surface,
    texture: glow::NativeTexture,
    target: u32,
    framebuffer: glow::NativeFramebuffer,
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

    // Written beside the video it will become, and moved over it only once
    // complete: the last finished recording stays whole while the next is made.
    let part = in_progress(&path);
    let mut encoder = match factory.create(&config_e, &part) {
        Ok(encoder) => encoder,
        Err(e) => {
            start.err(e);
            let _ = tx.send(FromEncoder::Failed);
            return;
        }
    };
    let input = factory.input();
    if input == FrameInput::Memory {
        // The render thread reads the frames back: no context here.
        info!(
            width = config_e.width,
            height = config_e.height,
            fps = config_e.fps,
            "game recorder: encoding frames from memory"
        );
        let _ = tx.send(FromEncoder::Ready);
        start.ok(());
        let outcome = encode_memory(encoder.as_mut(), &rx, &tx);
        conclude(outcome, encoder, &part, &path, &events, &rx);
        return;
    }
    let session = match open(
        &*egl_provider,
        backend,
        display,
        config,
        share,
        &context_attribs,
        input,
        &*encoder,
        &textures,
        config_e,
    ) {
        Ok(session) => session,
        Err(e) => {
            encoder.discard();
            let _ = std::fs::remove_file(&part);
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
    encode(session, encoder, textures, &part, &path, &events, &rx, &tx);
}

/// Where a recording is written until it is complete: beside the video it
/// becomes, which keeps the last finished one until then.
fn in_progress(path: &std::path::Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_default();
    name.push(".part");
    path.with_file_name(name)
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
    input: FrameInput,
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
    let size = (encoding.width as i32, encoding.height as i32);
    match input {
        FrameInput::Window => {}
        FrameInput::ClientBuffers => {
            return open_offscreen(
                egl,
                display,
                render_config,
                share,
                context_attribs,
                textures,
                size,
                Target::Buffers {
                    bound: HashMap::new(),
                },
            );
        }
        FrameInput::Memory => {
            return Err(failure(
                "an encoder of frames from memory has no context",
                "",
            ));
        }
    }
    let window = encoder.window();

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
        let gl = load_gl(&egl);
        let presentation_time = egl
            .get_proc_address("eglPresentationTimeANDROID")
            .map(|f| unsafe { std::mem::transmute::<extern "system" fn(), PresentationTime>(f) });
        let framebuffers = read_framebuffers(&gl, textures)?;
        return Ok(Session {
            egl,
            display,
            context,
            placeholder: None,
            target: Target::Window {
                surface,
                presentation_time,
            },
            gl,
            framebuffers,
            size,
        });
    }
    Err(failure(
        "the encoder's input could not be drawn into",
        format!("{last:?}"),
    ))
}

/// A context drawing into no window of the encoder's: current on a 1x1
/// pbuffer, or on no surface where the render config has no pbuffers, as the
/// upload thread's is. Every frame goes to `target`, a buffer the encoder
/// lends.
#[allow(clippy::too_many_arguments)]
fn open_offscreen(
    egl: EglInstance,
    display: khronos_egl::Display,
    config: khronos_egl::Config,
    share: khronos_egl::Context,
    context_attribs: &[khronos_egl::Int],
    textures: &[glow::NativeTexture],
    size: (i32, i32),
    target: Target,
) -> EngineResult<Session> {
    let context = egl
        .create_context(display, config, Some(share), context_attribs)
        .map_err(|e| failure("the recording's context", e))?;
    let placeholder = egl
        .create_pbuffer_surface(
            display,
            config,
            &[
                khronos_egl::WIDTH,
                1,
                khronos_egl::HEIGHT,
                1,
                khronos_egl::NONE,
            ],
        )
        .ok();
    if let Err(e) = egl.make_current(display, placeholder, placeholder, Some(context)) {
        if let Some(placeholder) = placeholder {
            let _ = egl.destroy_surface(display, placeholder);
        }
        let _ = egl.destroy_context(display, context);
        return Err(failure(
            "the recording's context could not be made current",
            e,
        ));
    }
    let gl = load_gl(&egl);
    let framebuffers = read_framebuffers(&gl, textures)?;
    Ok(Session {
        egl,
        display,
        context,
        placeholder,
        target,
        gl,
        framebuffers,
        size,
    })
}

fn load_gl(egl: &EglInstance) -> glow::Context {
    unsafe {
        glow::Context::from_loader_function(|name| {
            egl.get_proc_address(name)
                .map_or(std::ptr::null(), |f| f as *const std::ffi::c_void)
        })
    }
}

/// A framebuffer of this context reading each shared texture.
fn read_framebuffers(
    gl: &glow::Context,
    textures: &[glow::NativeTexture],
) -> EngineResult<Vec<glow::NativeFramebuffer>> {
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
    Ok(framebuffers)
}

/// Draw every frame the render thread sends into the encoder's input until it
/// says to finish or to discard, or the encoder fails.
#[allow(clippy::too_many_arguments)]
fn encode(
    session: Session,
    mut encoder: Box<dyn VideoEncoder>,
    textures: Vec<glow::NativeTexture>,
    part: &std::path::Path,
    path: &std::path::Path,
    events: &RecorderEvents,
    rx: &Receiver<ToEncoder>,
    tx: &Sender<FromEncoder>,
) {
    let mut session = session;
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
                unsafe {
                    session.gl.wait_sync(ready, 0, glow::TIMEOUT_IGNORED);
                    session.gl.delete_sync(ready);
                }
                match draw(&mut session, &mut *encoder, slot, pts_ns) {
                    Ok(released) => {
                        let _ = tx.send(FromEncoder::Released {
                            slot,
                            released: SyncHandle(released),
                        });
                    }
                    Err(reason) => break End::Failed(reason),
                }
                if let Err(e) = encoder.drain() {
                    break End::Failed(e.to_string());
                }
            }
            // Read-back frames are only sent to an encoder of frames from
            // memory, which has no context.
            ToEncoder::Pixels { .. } => {}
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
            unsafe { session.gl.delete_sync(ready) };
        }
    }
    close(session, textures);
    conclude(outcome, encoder, part, path, events, rx);
}

/// Hand the encoder every frame the render thread read back, until it says to
/// finish or to discard, or the encoder fails. Each frame's memory goes back
/// for the next.
fn encode_memory(
    encoder: &mut dyn VideoEncoder,
    rx: &Receiver<ToEncoder>,
    tx: &Sender<FromEncoder>,
) -> End {
    let outcome = loop {
        let Ok(message) = rx.recv() else {
            break End::Discard(None);
        };
        match message {
            ToEncoder::Pixels { rgba, pts_ns } => {
                if let Err(e) = encoder
                    .submit_pixels(&rgba, pts_ns)
                    .and_then(|()| encoder.drain())
                {
                    break End::Failed(e.to_string());
                }
                let _ = tx.send(FromEncoder::Spent(rgba));
            }
            // Frames in textures are only sent to an encoder drawn into.
            ToEncoder::Frame { .. } => {}
            ToEncoder::Finish { duration_ms, resp } => break End::Finish(duration_ms, resp),
            ToEncoder::Discard { resp } => break End::Discard(resp),
        }
    };
    if let End::Failed(_) = outcome {
        let _ = tx.send(FromEncoder::Failed);
    }
    outcome
}

/// The file completed or removed, content told, and every caller answered.
fn conclude(
    outcome: End,
    encoder: Box<dyn VideoEncoder>,
    part: &std::path::Path,
    path: &std::path::Path,
    events: &RecorderEvents,
    rx: &Receiver<ToEncoder>,
) {
    match outcome {
        End::Finish(duration_ms, resp) => {
            let finished = encoder.finish().and_then(|()| {
                std::fs::rename(part, path)
                    .map_err(|e| failure("completing the recording's file", e))
            });
            match finished {
                Ok(()) => {
                    events.close();
                    resp.ok(duration_ms);
                }
                Err(e) => {
                    let _ = std::fs::remove_file(part);
                    events.fail(e.to_string());
                    resp.err(e);
                }
            }
        }
        End::Discard(resp) => {
            encoder.discard();
            let _ = std::fs::remove_file(part);
            events.close();
            if let Some(resp) = resp {
                resp.ok(());
            }
        }
        End::Failed(reason) => {
            warn!("game recorder: {reason}");
            encoder.discard();
            let _ = std::fs::remove_file(part);
            events.fail(reason);
            // What the render thread asks before it hears is answered here:
            // a stop as failed, an abort as done.
            for message in rx.iter() {
                match message {
                    ToEncoder::Finish { resp, .. } => resp.err(failure("the recording failed", "")),
                    ToEncoder::Discard { resp: Some(resp) } => resp.ok(()),
                    ToEncoder::Frame { .. }
                    | ToEncoder::Pixels { .. }
                    | ToEncoder::Discard { resp: None } => {}
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

/// Draw the frame in `slot` into the encoder's input, as of `pts_ns`: the
/// fence after which the shared texture may be written again.
fn draw(
    session: &mut Session,
    encoder: &mut dyn VideoEncoder,
    slot: usize,
    pts_ns: i64,
) -> Result<glow::NativeFence, String> {
    let Session {
        egl,
        display,
        target,
        gl,
        framebuffers,
        size: (w, h),
        ..
    } = session;
    let (w, h) = (*w, *h);
    let fenced = |gl: &glow::Context| unsafe {
        gl.fence_sync(glow::SYNC_GPU_COMMANDS_COMPLETE, 0)
            .map_err(|e| format!("a frame could not be fenced: {e}"))
    };
    match target {
        Target::Window {
            surface,
            presentation_time,
        } => {
            let released = unsafe {
                gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(framebuffers[slot]));
                gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, None);
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
                fenced(gl)?
            };
            if let Some(presentation_time) = presentation_time {
                unsafe { presentation_time(display.as_ptr(), surface.as_ptr(), pts_ns) };
            }
            egl.swap_buffers(*display, *surface)
                .map_err(|e| format!("the encoder's input refused a frame: {e:?}"))?;
            Ok(released)
        }
        Target::Buffers { bound } => {
            let Some(buffer) = encoder.lend().map_err(|e| e.to_string())? else {
                // The encoder is not ready for another frame: this one is not
                // recorded, and its texture is free at once.
                return fenced(gl);
            };
            let into = match bound.entry(buffer.key) {
                std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(bind(egl, *display, gl, &buffer)?)
                }
            };
            unsafe {
                gl.bind_texture(into.target, Some(into.texture));
                egl.bind_tex_image(*display, into.pbuffer, khronos_egl::BACK_BUFFER)
                    .map_err(|e| format!("the encoder's buffer could not be bound: {e:?}"))?;
                gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(into.framebuffer));
                gl.framebuffer_texture_2d(
                    glow::DRAW_FRAMEBUFFER,
                    glow::COLOR_ATTACHMENT0,
                    into.target,
                    Some(into.texture),
                    0,
                );
                gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(framebuffers[slot]));
                let (y0, y1) = if buffer.top_row_first { (h, 0) } else { (0, h) };
                gl.blit_framebuffer(
                    0,
                    0,
                    w,
                    h,
                    0,
                    y0,
                    w,
                    y1,
                    glow::COLOR_BUFFER_BIT,
                    glow::NEAREST,
                );
            }
            let released = fenced(gl)?;
            // The encoder reads the buffer outside GL: it is handed over only
            // once the GPU has written it.
            let drawn = fenced(gl)?;
            let waited = unsafe {
                gl.client_wait_sync(
                    drawn,
                    glow::SYNC_FLUSH_COMMANDS_BIT,
                    BUFFER_DRAWN_WITHIN.as_nanos() as i32,
                )
            };
            unsafe { gl.delete_sync(drawn) };
            let _ = egl.release_tex_image(*display, into.pbuffer, khronos_egl::BACK_BUFFER);
            if waited == glow::TIMEOUT_EXPIRED || waited == glow::WAIT_FAILED {
                unsafe { gl.delete_sync(released) };
                return Err("a frame was not drawn into the encoder's buffer in time".into());
            }
            if let Err(e) = encoder.submit(buffer, pts_ns) {
                unsafe { gl.delete_sync(released) };
                return Err(e.to_string());
            }
            Ok(released)
        }
    }
}

/// A pbuffer on one of the encoder's buffers, and a texture and framebuffer
/// of this context to draw into it.
fn bind(
    egl: &EglInstance,
    display: khronos_egl::Display,
    gl: &glow::Context,
    buffer: &ClientBuffer,
) -> Result<Bound, String> {
    let mut config_attribs = buffer.config_attribs.clone();
    config_attribs.push(khronos_egl::NONE);
    let config = egl
        .choose_first_config(display, &config_attribs)
        .map_err(|e| format!("no EGL config takes the encoder's buffers: {e:?}"))?
        .ok_or_else(|| "no EGL config takes the encoder's buffers".to_string())?;
    let mut attribs = buffer.attribs.clone();
    attribs.push(khronos_egl::NONE);
    let pbuffer = egl
        .create_pbuffer_from_client_buffer(
            display,
            buffer.buffer_type,
            unsafe { khronos_egl::ClientBuffer::from_ptr(buffer.handle) },
            config,
            &attribs,
        )
        .map_err(|e| format!("the encoder's buffer could not be wrapped: {e:?}"))?;
    let made = unsafe {
        gl.create_texture()
            .and_then(|t| gl.create_framebuffer().map(|f| (t, f)))
    };
    match made {
        Ok((texture, framebuffer)) => Ok(Bound {
            pbuffer,
            texture,
            target: buffer.texture_target,
            framebuffer,
        }),
        Err(e) => {
            let _ = egl.destroy_surface(display, pbuffer);
            Err(format!("a texture for the encoder's buffer: {e}"))
        }
    }
}

/// This thread's GL objects, the textures it read for the last time, its
/// surfaces and its context. The encoder's input is no longer drawn into after.
fn close(session: Session, textures: Vec<glow::NativeTexture>) {
    let Session {
        egl,
        display,
        context,
        placeholder,
        target,
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
    }
    let mut surfaces: Vec<_> = placeholder.into_iter().collect();
    match target {
        Target::Window { surface, .. } => surfaces.push(surface),
        Target::Buffers { bound } => {
            for (_, bound) in bound {
                unsafe {
                    gl.delete_framebuffer(bound.framebuffer);
                    gl.delete_texture(bound.texture);
                }
                surfaces.push(bound.pbuffer);
            }
        }
    }
    unsafe { gl.finish() };
    let _ = egl.make_current(display, None, None, None);
    for surface in surfaces {
        let _ = egl.destroy_surface(display, surface);
    }
    let _ = egl.destroy_context(display, context);
}
