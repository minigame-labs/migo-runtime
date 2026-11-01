//! `getGameRecorder` on the render thread: the frames content drew, taken at
//! the recording's rate and handed to an encoder thread.
//!
//! A capture is a GPU copy made at present time, after content's frame is
//! complete and before the engine's overlay goes over it -- so the native
//! buttons are never recorded -- from the onscreen canvas's drawing buffer into
//! one of a few textures the encoder thread shares. A fence says when the copy
//! is done; the encoder thread waits on it on the GPU, draws the texture into
//! the encoder's input, and fences the texture back. The render thread never
//! waits on the encoder: with no texture free, a frame is not taken.
//!
//! A recording's clock is the time it has been taking frames: it stops while
//! the recording is paused, while the app is in the background, and at the
//! recording's duration, as the reference has it -- "到达指定时长后不会再录入".

mod encoder;

use std::{
    collections::VecDeque,
    ffi::c_void,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use crossbeam_channel::{Receiver, Sender, TryRecvError, TrySendError};
use glow::HasContext;
use shared::{
    error::{EngineError, EngineResult, ErrorCode},
    game_recorder::{RecorderEvents, RecordingParams},
    protocol::render_cmd::RenderCmdResp,
};
use tracing::warn;

use crate::egl_platform::EglProvider;

/// An encoder a platform provides: what the render thread needs to know before
/// it can offer recording at all.
pub trait VideoEncoderFactory: Send + Sync + std::fmt::Debug {
    /// Whether this device has an encoder to record with.
    fn available(&self) -> bool;

    /// How its encoders take their frames.
    fn input(&self) -> FrameInput;

    /// An encoder of `config`, writing a complete MP4 to `path` when finished.
    fn create(&self, config: &EncoderConfig, path: &Path) -> EngineResult<Box<dyn VideoEncoder>>;
}

/// What an encoder encodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_bps: u32,
    /// Frames from one key frame to the next.
    pub key_frame_interval: u32,
}

/// How an encoder takes the frames drawn for it -- the same for every encoder a
/// factory makes, and known before one is made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameInput {
    /// A window ([`VideoEncoder::window`]): each frame is drawn into an EGL
    /// window surface on it and swapped, stamped with
    /// `EGL_ANDROID_presentation_time` where the EGL has it. A codec's input
    /// surface. Drawn on the encoder thread, in the render context's share
    /// group.
    Window,
    /// Buffers it lends one frame at a time ([`VideoEncoder::lend`]): each is
    /// drawn into as an EGL client buffer bound to a texture, and handed back
    /// ([`VideoEncoder::submit`]) once the GPU has finished with it. Drawn on
    /// the encoder thread, in the render context's share group.
    ClientBuffers,
    /// Frames in memory ([`VideoEncoder::submit_pixels`]): the render thread
    /// reads each one back without waiting for it, and the encoder thread
    /// makes no context -- so it records where a second context in the share
    /// group may not be made.
    Memory,
}

/// A buffer an encoder lends for one frame, and how EGL takes it.
#[derive(Debug)]
pub struct ClientBuffer {
    /// What `eglCreatePbufferFromClientBuffer` is given.
    pub buffer_type: khronos_egl::Enum,
    pub handle: *mut c_void,
    /// The same each time one buffer of the encoder's is lent: what EGL and GL
    /// made for it are kept for the next time.
    pub key: u64,
    /// The pbuffer's attributes, without the closing `EGL_NONE`.
    pub attribs: Vec<khronos_egl::Int>,
    /// The attributes of an EGL config that takes it, without `EGL_NONE`.
    pub config_attribs: Vec<khronos_egl::Int>,
    /// The GL texture target it is bound to.
    pub texture_target: u32,
    /// Whether its first row in memory is the picture's top: GL's is the
    /// bottom, so such a buffer is drawn into upside down.
    pub top_row_first: bool,
}

/// One encoder, driven from the encoder thread alone. What it is asked
/// depends on its factory's [`FrameInput`].
pub trait VideoEncoder: Send {
    /// The window its frames are drawn into: an `EGLNativeWindowType`. Only
    /// asked of an encoder whose input is [`FrameInput::Window`].
    fn window(&self) -> *mut c_void {
        std::ptr::null_mut()
    }

    /// A buffer to draw the next frame into, or none when the encoder is not
    /// ready for another frame now: that frame is not recorded. Only asked of
    /// an encoder whose input is [`FrameInput::ClientBuffers`].
    fn lend(&mut self) -> EngineResult<Option<ClientBuffer>> {
        Err(not_lending())
    }

    /// The frame drawn into `buffer`, complete on the GPU, shown at `pts_ns`.
    fn submit(&mut self, buffer: ClientBuffer, pts_ns: i64) -> EngineResult<()> {
        let _ = (buffer, pts_ns);
        Err(not_lending())
    }

    /// A frame in memory, shown at `pts_ns`: RGBA, `width * height * 4` bytes,
    /// rows top first. Only asked of an encoder whose input is
    /// [`FrameInput::Memory`]; it may block while the encoder is behind.
    fn submit_pixels(&mut self, rgba: &[u8], pts_ns: i64) -> EngineResult<()> {
        let _ = (rgba, pts_ns);
        Err(EngineError::new(ErrorCode::RenderBackendError)
            .with_msg("this encoder does not take frames from memory"))
    }

    /// Write what has been encoded so far to the file, waiting for nothing.
    fn drain(&mut self) -> EngineResult<()>;

    /// No more frames: write the rest and complete the file. The window is no
    /// longer drawn into when this is called.
    fn finish(self: Box<Self>) -> EngineResult<()>;

    /// Give it up: nothing more is written and the file is not complete.
    fn discard(self: Box<Self>);
}

fn not_lending() -> EngineError {
    EngineError::new(ErrorCode::RenderBackendError)
        .with_msg("this encoder takes its frames through a window, not lent buffers")
}

/// The longest side a recording is encoded at, and the shorter one's limit:
/// what every H.264 encoder a phone has takes, at the reference's bitrates.
const MAX_LONG_SIDE: u32 = 1280;
const MAX_SHORT_SIDE: u32 = 720;
/// Encoders take sizes in whole macroblocks.
const ALIGN: u32 = 16;
/// Textures between the render thread and the encoder thread: one being
/// copied into, one being encoded, one spare for the encoder running late.
const SLOTS: usize = 3;
/// How often a recording says how far it has got, in recorded milliseconds.
const TIME_REPORT_MS: f64 = 250.0;
/// How long a frame's read back may take once a recording is stopped and its
/// last frames are waited for: far past any frame's, so only a lost GPU
/// reaches it.
const READ_BACK_WITHIN: Duration = Duration::from_secs(2);

/// The size a recording of content `width` x `height` is encoded at: content's
/// shape within the encoder's limits, in whole macroblocks.
pub(crate) fn encoded_size(width: u32, height: u32) -> (u32, u32) {
    let (width, height) = (width.max(1) as f64, height.max(1) as f64);
    let (long, short) = if width >= height {
        (width, height)
    } else {
        (height, width)
    };
    let scale = (MAX_LONG_SIDE as f64 / long)
        .min(MAX_SHORT_SIDE as f64 / short)
        .min(1.0);
    // Down, so a recording is never larger than content.
    let align = |side: f64| ((side * scale / ALIGN as f64).floor() as u32).max(1) * ALIGN;
    (align(width), align(height))
}

/// Where content's `width` x `height` frame goes in a texture of `target`:
/// as large as fits, its shape kept, centred -- what is left is black.
pub(crate) fn fitted(width: u32, height: u32, target: (u32, u32)) -> [i32; 4] {
    let (tw, th) = (target.0 as f64, target.1 as f64);
    let scale = (tw / width.max(1) as f64).min(th / height.max(1) as f64);
    let (w, h) = (
        (width as f64 * scale).round(),
        (height as f64 * scale).round(),
    );
    let (x, y) = (((tw - w) / 2.0).round(), ((th - h) / 2.0).round());
    [x as i32, y as i32, (x + w) as i32, (y + h) as i32]
}

/// A recording's clock: how long it has been taking frames.
#[derive(Debug)]
pub(crate) struct RecordingClock {
    recorded_ms: f64,
    running_since: Option<Instant>,
    started: bool,
    paused: bool,
    suspended: bool,
    limit_ms: f64,
}

impl RecordingClock {
    fn new(limit_ms: f64) -> Self {
        Self {
            recorded_ms: 0.0,
            running_since: None,
            started: false,
            paused: false,
            suspended: false,
            limit_ms,
        }
    }

    pub(crate) fn now(&self, at: Instant) -> f64 {
        let running = self.running_since.map_or(0.0, |since| {
            at.saturating_duration_since(since).as_secs_f64() * 1000.0
        });
        (self.recorded_ms + running).min(self.limit_ms)
    }

    fn settle(&mut self, at: Instant) {
        self.recorded_ms = self.now(at);
        self.running_since = None;
    }

    fn update(&mut self, at: Instant) {
        let runs = self.started && !self.paused && !self.suspended;
        match (runs, self.running_since.is_some()) {
            (true, false) => self.running_since = Some(at),
            (false, true) => self.settle(at),
            _ => {}
        }
    }

    /// The encoder is taking frames: the clock may run from `at`.
    pub(crate) fn start(&mut self, at: Instant) {
        self.started = true;
        self.update(at);
    }

    pub(crate) fn pause(&mut self, paused: bool, at: Instant) {
        self.paused = paused;
        self.update(at);
    }

    pub(crate) fn suspend(&mut self, suspended: bool, at: Instant) {
        self.suspended = suspended;
        self.update(at);
    }

    pub(crate) fn taking_frames(&self, at: Instant) -> bool {
        self.running_since.is_some() && self.now(at) < self.limit_ms
    }
}

/// What the render thread tells the encoder thread.
pub(super) enum ToEncoder {
    /// Slot `slot` holds a frame once `ready` signals; it is shown at `pts_ns`.
    Frame {
        slot: usize,
        ready: SyncHandle,
        pts_ns: i64,
    },
    /// A frame read back into memory, shown at `pts_ns`: RGBA, rows top first.
    Pixels { rgba: Vec<u8>, pts_ns: i64 },
    /// No more frames: complete the file and answer with `duration_ms`.
    Finish {
        duration_ms: u64,
        resp: RenderCmdResp<u64>,
    },
    /// No more frames: remove the file, and answer if anyone asked.
    Discard { resp: Option<RenderCmdResp<()>> },
}

/// What the encoder thread tells the render thread.
pub(super) enum FromEncoder {
    /// Encoding: frames may be taken.
    Ready,
    /// Slot `slot` may be copied into once `released` signals.
    Released { slot: usize, released: SyncHandle },
    /// The memory a frame was read back into, for the next one.
    Spent(Vec<u8>),
    /// The recording failed; content has been told.
    Failed,
}

/// A GL fence on its way between the two threads. Fences belong to the share
/// group, not a context, so either thread may wait on and delete one.
pub(super) struct SyncHandle(glow::NativeFence);

// SAFETY: a GLsync is a share-group object named by an opaque pointer; it is
// only ever waited on and deleted, each exactly once, by whichever thread holds
// the handle -- the channel moves it, nothing aliases it.
unsafe impl Send for SyncHandle {}

/// One texture the frames pass through.
struct Slot {
    texture: glow::NativeTexture,
    /// The render thread's framebuffer over it (framebuffers are per context).
    framebuffer: glow::NativeFramebuffer,
    /// For an encoder that takes frames from memory: the buffer the texture is
    /// read back into, without waiting.
    pack: Option<glow::NativeBuffer>,
    /// Signals when the encoder thread is done reading it.
    released: Option<SyncHandle>,
    /// Where content's frame went in it last time: what is outside is black,
    /// and stays black while the frame keeps its size.
    fitted: Option<[i32; 4]>,
}

/// A recording, as the render thread holds it.
pub(crate) struct Recording {
    thread: std::thread::JoinHandle<()>,
    events: Arc<RecorderEvents>,
    size: (u32, u32),
    interval_ms: f64,
    slots: Vec<Slot>,
    free: Vec<usize>,
    /// Frames being read back into their slots' buffers, in the order taken:
    /// the slot, the fence after its read, the frame's time.
    reading: VecDeque<(usize, SyncHandle, i64)>,
    /// Memory a frame was read back into, back from the encoder thread.
    spare: Vec<Vec<u8>>,
    to_encoder: Sender<ToEncoder>,
    from_encoder: Receiver<FromEncoder>,
    ready: bool,
    clock: RecordingClock,
    next_frame_ms: f64,
    last_frame_ms: Option<f64>,
    next_report_ms: f64,
}

/// Why a recording is over on the render thread's side.
pub(crate) enum Ended {
    /// The encoder failed; content has been told.
    Failed,
}

/// What starting a recording takes from the render thread's context.
pub(crate) struct RecordingContext<'a> {
    pub gl: &'a glow::Context,
    pub egl_provider: &'a Arc<dyn EglProvider>,
    pub display: khronos_egl::Display,
    pub config: khronos_egl::Config,
    pub share: khronos_egl::Context,
    pub gles_major: u32,
    pub features: crate::context_features::ContextFeatures,
    pub factory: &'a Arc<dyn VideoEncoderFactory>,
}

impl Recording {
    /// Start recording content's `width` x `height` frames into `path`. The
    /// encoder thread answers `resp` once it is encoding, and from then frames
    /// are taken; `None` when it could not be started, and `resp` is answered
    /// with why.
    pub(crate) fn start(
        cx: RecordingContext<'_>,
        content: (u32, u32),
        params: RecordingParams,
        path: PathBuf,
        events: Arc<RecorderEvents>,
        resp: RenderCmdResp<()>,
    ) -> Option<Self> {
        let size = encoded_size(content.0, content.1);
        let config = EncoderConfig {
            width: size.0,
            height: size.1,
            fps: params.fps,
            bitrate_bps: params.bitrate_kbps.saturating_mul(1000),
            key_frame_interval: params.gop,
        };
        let memory = cx.factory.input() == FrameInput::Memory;
        let mut slots = Vec::with_capacity(SLOTS);
        for _ in 0..SLOTS {
            match unsafe { Slot::new(cx.gl, size, memory) } {
                Ok(slot) => slots.push(slot),
                Err(e) => {
                    for slot in slots {
                        unsafe { slot.delete_with_texture(cx.gl) };
                    }
                    resp.err(e);
                    return None;
                }
            }
        }
        let (to_encoder, encoder_rx) = crossbeam_channel::bounded(SLOTS + 2);
        let (encoder_tx, from_encoder) = crossbeam_channel::unbounded();
        let spawned = encoder::spawn(encoder::Args {
            egl_provider: Arc::clone(cx.egl_provider),
            display: cx.display,
            config: cx.config,
            share: cx.share,
            // It asks for what the render context asked for: the share group
            // requires the two to agree.
            context_attribs: cx.features.context_attribs(cx.gles_major),
            factory: Arc::clone(cx.factory),
            encoder: config,
            path,
            textures: slots.iter().map(|slot| slot.texture).collect(),
            events: Arc::clone(&events),
            start: resp,
            rx: encoder_rx,
            tx: encoder_tx,
        });
        let thread = match spawned {
            Ok(thread) => thread,
            Err(e) => {
                // The reply went with the thread that never ran: content's call
                // is answered as cancelled.
                warn!("game recorder: {e}");
                for slot in slots {
                    unsafe { slot.delete_with_texture(cx.gl) };
                }
                return None;
            }
        };
        Some(Self {
            thread,
            events,
            size,
            interval_ms: 1000.0 / params.fps as f64,
            free: (0..SLOTS).collect(),
            slots,
            reading: VecDeque::with_capacity(SLOTS),
            spare: Vec::with_capacity(SLOTS),
            to_encoder,
            from_encoder,
            ready: false,
            clock: RecordingClock::new(params.duration_ms as f64),
            next_frame_ms: 0.0,
            last_frame_ms: None,
            next_report_ms: TIME_REPORT_MS,
        })
    }

    /// The recording failed on the encoder's side: the render thread's objects
    /// go, and the thread is left to finish.
    pub(crate) fn ended(self, gl: &glow::Context) -> Finishing {
        self.release(gl)
    }

    /// Take what the encoder thread said. `Err` when the recording failed.
    fn hear(&mut self, at: Instant) -> Result<(), Ended> {
        loop {
            match self.from_encoder.try_recv() {
                Ok(FromEncoder::Ready) => {
                    self.ready = true;
                    self.clock.start(at);
                }
                Ok(FromEncoder::Released { slot, released }) => {
                    self.slots[slot].released = Some(released);
                    self.free.push(slot);
                }
                Ok(FromEncoder::Spent(rgba)) => {
                    if self.spare.len() < SLOTS {
                        self.spare.push(rgba);
                    }
                }
                Ok(FromEncoder::Failed) | Err(TryRecvError::Disconnected) => {
                    return Err(Ended::Failed);
                }
                Err(TryRecvError::Empty) => return Ok(()),
            }
        }
    }

    /// A frame is being presented: content's pixels are complete in
    /// `framebuffer` (`width` x `height`), the window's context is current, and
    /// the overlay is not yet drawn. Takes the frame if one is due.
    pub(crate) fn on_present(
        &mut self,
        gl: &glow::Context,
        framebuffer: glow::NativeFramebuffer,
        reads_none: bool,
        width: u32,
        height: u32,
        at: Instant,
    ) -> Result<(), Ended> {
        self.hear(at)?;
        if !self.ready {
            return Ok(());
        }
        self.collect(gl, false)?;
        let now = self.clock.now(at);
        if now >= self.next_report_ms {
            self.events.time(now as u64);
            self.next_report_ms = now + TIME_REPORT_MS;
        }
        if !self.clock.taking_frames(at) || now < self.next_frame_ms {
            return Ok(());
        }
        self.next_frame_ms = (self.next_frame_ms + self.interval_ms).max(now);
        self.take(gl, framebuffer, reads_none, width, height, now)
    }

    /// Copy content's frame into a free slot and hand it to the encoder thread
    /// as the frame at `now`. With no slot free the encoder is behind, and this
    /// frame is not taken.
    fn take(
        &mut self,
        gl: &glow::Context,
        framebuffer: glow::NativeFramebuffer,
        reads_none: bool,
        width: u32,
        height: u32,
        now: f64,
    ) -> Result<(), Ended> {
        let Some(index) = self.free.pop() else {
            return Ok(());
        };
        let size = self.size;
        let slot = &mut self.slots[index];
        let ready = unsafe { slot.copy(gl, framebuffer, reads_none, width, height, size) };
        let Some(ready) = ready else {
            // The copy could not be fenced: the slot is still free.
            self.free.push(index);
            return Ok(());
        };
        let pts_ns = (now * 1_000_000.0) as i64;
        self.last_frame_ms = Some(now);
        if slot.pack.is_some() {
            // Read back on a later present, once the GPU has written it.
            self.reading.push_back((index, ready, pts_ns));
            return Ok(());
        }
        if self
            .to_encoder
            .send(ToEncoder::Frame {
                slot: index,
                ready,
                pts_ns,
            })
            .is_err()
        {
            return Err(Ended::Failed);
        }
        Ok(())
    }

    /// Hand the encoder thread the frames whose read back is done, in the
    /// order taken. Waits for none unless `last`: a stopped recording's final
    /// frames are waited for and handed over whatever the encoder's pace.
    /// While the encoder thread is behind, a frame is not handed over and its
    /// memory is kept for the next.
    fn collect(&mut self, gl: &glow::Context, last: bool) -> Result<(), Ended> {
        let bytes = self.size.0 as usize * self.size.1 as usize * 4;
        let row = self.size.0 as usize * 4;
        while let Some(&(index, SyncHandle(read), pts_ns)) = self.reading.front() {
            let within = if last {
                READ_BACK_WITHIN.as_nanos() as i32
            } else {
                0
            };
            let waited = unsafe { gl.client_wait_sync(read, 0, within) };
            if waited == glow::TIMEOUT_EXPIRED && !last {
                return Ok(());
            }
            self.reading.pop_front();
            unsafe { gl.delete_sync(read) };
            if waited == glow::TIMEOUT_EXPIRED || waited == glow::WAIT_FAILED {
                warn!("game recorder: a frame was not read back");
                return Err(Ended::Failed);
            }
            let mut rgba = self.spare.pop().unwrap_or_default();
            rgba.resize(bytes, 0);
            let copied = unsafe { self.slots[index].read_back(gl, &mut rgba, row) };
            self.free.push(index);
            if !copied {
                warn!("game recorder: a frame's read back could not be mapped");
                return Err(Ended::Failed);
            }
            let frame = ToEncoder::Pixels { rgba, pts_ns };
            let sent = if last {
                self.to_encoder
                    .send(frame)
                    .map_err(|e| TrySendError::Disconnected(e.0))
            } else {
                self.to_encoder.try_send(frame)
            };
            match sent {
                Ok(()) => {}
                // Behind: this frame is not recorded.
                Err(TrySendError::Full(ToEncoder::Pixels { rgba, .. })) => self.spare.push(rgba),
                Err(_) => return Err(Ended::Failed),
            }
        }
        Ok(())
    }

    pub(crate) fn pause(&mut self, paused: bool, at: Instant) {
        self.clock.pause(paused, at);
    }

    /// The app went to the background, or came back: the recording's clock
    /// does not run while nothing is presented.
    pub(crate) fn suspend(&mut self, suspended: bool, at: Instant) {
        self.clock.suspend(suspended, at);
    }

    /// End the recording and complete its file; `resp` is answered with its
    /// duration once the file is complete. Content's last frame is taken at the
    /// end of the recording if the one before is older than a frame, so the
    /// video lasts as long as the recording did.
    pub(crate) fn stop(
        mut self,
        gl: &glow::Context,
        content: Option<(glow::NativeFramebuffer, bool, u32, u32)>,
        at: Instant,
        resp: RenderCmdResp<u64>,
    ) -> Finishing {
        if self.hear(at).is_err() {
            resp.err(recording_failed());
            return self.release(gl);
        }
        let duration = self.clock.now(at);
        if let Some((framebuffer, reads_none, width, height)) = content {
            let behind = self
                .last_frame_ms
                .is_none_or(|last| duration - last >= self.interval_ms);
            if self.ready && behind {
                let _ = self.take(gl, framebuffer, reads_none, width, height, duration);
            }
        }
        if self.collect(gl, true).is_err() {
            resp.err(recording_failed());
            return self.release(gl);
        }
        let duration_ms = duration as u64;
        if let Err(crossbeam_channel::SendError(ToEncoder::Finish { resp, .. })) = self
            .to_encoder
            .send(ToEncoder::Finish { duration_ms, resp })
        {
            resp.err(recording_failed());
        }
        self.release(gl)
    }

    /// End the recording and remove its file.
    pub(crate) fn abort(self, gl: &glow::Context, resp: Option<RenderCmdResp<()>>) -> Finishing {
        if let Err(crossbeam_channel::SendError(ToEncoder::Discard { resp: Some(resp) })) =
            self.to_encoder.send(ToEncoder::Discard { resp })
        {
            resp.ok(());
        }
        self.release(gl)
    }

    /// The share group is gone, or the session is ending: nothing of it is
    /// touched here, and the recording is given up.
    pub(crate) fn abandon(self, reason: &str) -> Finishing {
        self.events.fail(reason);
        let _ = self.to_encoder.send(ToEncoder::Discard { resp: None });
        Finishing(self.thread)
    }

    /// The render thread's own objects. The textures are the encoder thread's
    /// to delete once it has read them for the last time.
    fn release(self, gl: &glow::Context) -> Finishing {
        for (_, SyncHandle(read), _) in self.reading {
            unsafe { gl.delete_sync(read) };
        }
        for slot in self.slots {
            unsafe { slot.delete(gl) };
        }
        Finishing(self.thread)
    }
}

/// An encoder thread finishing a recording on its own. Joined before the EGL
/// display it uses goes; once finished, it is done with.
pub(crate) struct Finishing(std::thread::JoinHandle<()>);

impl Finishing {
    pub(crate) fn is_finished(&self) -> bool {
        self.0.is_finished()
    }

    pub(crate) fn join(self) {
        if self.0.join().is_err() {
            warn!("game recorder: the encoder thread panicked");
        }
    }
}

fn recording_failed() -> EngineError {
    EngineError::new(ErrorCode::RenderBackendError).with_msg("the recording failed")
}

impl Slot {
    /// A black texture of `size` and the render thread's framebuffer over it,
    /// and for an encoder that takes frames from `memory`, the buffer it is
    /// read back into.
    unsafe fn new(gl: &glow::Context, size: (u32, u32), memory: bool) -> EngineResult<Self> {
        unsafe {
            let texture = gl.create_texture().map_err(gl_error)?;
            let previous = gl.get_parameter_texture(glow::TEXTURE_BINDING_2D);
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            gl.tex_storage_2d(
                glow::TEXTURE_2D,
                1,
                glow::RGBA8,
                size.0 as i32,
                size.1 as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                glow::LINEAR as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                glow::LINEAR as i32,
            );
            gl.bind_texture(glow::TEXTURE_2D, previous);
            let framebuffer = match gl.create_framebuffer() {
                Ok(framebuffer) => framebuffer,
                Err(e) => {
                    gl.delete_texture(texture);
                    return Err(gl_error(e));
                }
            };
            let pack = if memory {
                match gl.create_buffer() {
                    Ok(pack) => {
                        let previous = gl.get_parameter_buffer(glow::PIXEL_PACK_BUFFER_BINDING);
                        gl.bind_buffer(glow::PIXEL_PACK_BUFFER, Some(pack));
                        gl.buffer_data_size(
                            glow::PIXEL_PACK_BUFFER,
                            (size.0 * size.1 * 4) as i32,
                            glow::STREAM_READ,
                        );
                        gl.bind_buffer(glow::PIXEL_PACK_BUFFER, previous);
                        Some(pack)
                    }
                    Err(e) => {
                        gl.delete_framebuffer(framebuffer);
                        gl.delete_texture(texture);
                        return Err(gl_error(e));
                    }
                }
            } else {
                None
            };
            Ok(Self {
                texture,
                framebuffer,
                pack,
                released: None,
                fitted: None,
            })
        }
    }

    /// Content's frame in `framebuffer`, scaled into this slot, and the fence
    /// that signals when the copy is done -- `None` if the fence could not be
    /// made, in which case nothing waits on the copy.
    unsafe fn copy(
        &mut self,
        gl: &glow::Context,
        framebuffer: glow::NativeFramebuffer,
        reads_none: bool,
        width: u32,
        height: u32,
        size: (u32, u32),
    ) -> Option<SyncHandle> {
        unsafe {
            if let Some(SyncHandle(released)) = self.released.take() {
                // The encoder thread's read of the last frame is ordered first.
                gl.wait_sync(released, 0, glow::TIMEOUT_IGNORED);
                gl.delete_sync(released);
            }
            let saved = SavedCopyState::save(gl);
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(self.framebuffer));
            gl.framebuffer_texture_2d(
                glow::DRAW_FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(self.texture),
                0,
            );
            gl.disable(glow::SCISSOR_TEST);
            let fit = fitted(width, height, size);
            if self.fitted != Some(fit) {
                // Content's shape changed, or this is the first frame: black
                // around it from now on.
                gl.color_mask(true, true, true, true);
                gl.clear_color(0.0, 0.0, 0.0, 1.0);
                gl.clear(glow::COLOR_BUFFER_BIT);
                self.fitted = Some(fit);
            }
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(framebuffer));
            if reads_none {
                gl.read_buffer(glow::COLOR_ATTACHMENT0);
            }
            gl.blit_framebuffer(
                0,
                0,
                width as i32,
                height as i32,
                fit[0],
                fit[1],
                fit[2],
                fit[3],
                glow::COLOR_BUFFER_BIT,
                glow::LINEAR,
            );
            if reads_none {
                gl.read_buffer(glow::NONE);
            }
            if let Some(pack) = self.pack {
                // Started now, read on a later present: nothing waits here.
                let packing = SavedPackState::save(gl);
                gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(self.framebuffer));
                gl.bind_buffer(glow::PIXEL_PACK_BUFFER, Some(pack));
                gl.read_pixels(
                    0,
                    0,
                    size.0 as i32,
                    size.1 as i32,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelPackData::BufferOffset(0),
                );
                packing.restore(gl);
            }
            saved.restore(gl);
            let fence = gl.fence_sync(glow::SYNC_GPU_COMMANDS_COMPLETE, 0).ok()?;
            // A fence another context waits on has to have reached the GPU.
            gl.flush();
            Some(SyncHandle(fence))
        }
    }

    /// The frame read back into this slot's buffer, copied into `rgba` top row
    /// first -- GL's first row is the bottom one. Its read must be complete.
    unsafe fn read_back(&self, gl: &glow::Context, rgba: &mut [u8], row: usize) -> bool {
        let Some(pack) = self.pack else {
            return false;
        };
        unsafe {
            let previous = gl.get_parameter_buffer(glow::PIXEL_PACK_BUFFER_BINDING);
            gl.bind_buffer(glow::PIXEL_PACK_BUFFER, Some(pack));
            let mapped = gl.map_buffer_range(
                glow::PIXEL_PACK_BUFFER,
                0,
                rgba.len() as i32,
                glow::MAP_READ_BIT,
            );
            let copied = !mapped.is_null();
            if copied {
                let read = std::slice::from_raw_parts(mapped, rgba.len());
                for (into, from) in rgba.chunks_exact_mut(row).zip(read.chunks_exact(row).rev()) {
                    into.copy_from_slice(from);
                }
                gl.unmap_buffer(glow::PIXEL_PACK_BUFFER);
            }
            gl.bind_buffer(glow::PIXEL_PACK_BUFFER, previous);
            copied
        }
    }

    unsafe fn delete(self, gl: &glow::Context) {
        unsafe {
            if let Some(SyncHandle(released)) = self.released {
                gl.delete_sync(released);
            }
            if let Some(pack) = self.pack {
                gl.delete_buffer(pack);
            }
            gl.delete_framebuffer(self.framebuffer);
        }
    }

    unsafe fn delete_with_texture(self, gl: &glow::Context) {
        let texture = self.texture;
        unsafe {
            self.delete(gl);
            gl.delete_texture(texture);
        }
    }
}

/// The state a copy changes, put back after it: a present lands between any
/// two of content's GL calls.
struct SavedCopyState {
    read: Option<glow::NativeFramebuffer>,
    draw: Option<glow::NativeFramebuffer>,
    scissor: bool,
    color_mask: [bool; 4],
    clear_color: [f32; 4],
}

impl SavedCopyState {
    unsafe fn save(gl: &glow::Context) -> Self {
        unsafe {
            let mut mask = [0i32; 4];
            gl.get_parameter_i32_slice(glow::COLOR_WRITEMASK, &mut mask);
            let mut clear = [0f32; 4];
            gl.get_parameter_f32_slice(glow::COLOR_CLEAR_VALUE, &mut clear);
            Self {
                read: gl.get_parameter_framebuffer(glow::READ_FRAMEBUFFER_BINDING),
                draw: gl.get_parameter_framebuffer(glow::DRAW_FRAMEBUFFER_BINDING),
                scissor: gl.is_enabled(glow::SCISSOR_TEST),
                color_mask: mask.map(|bit| bit != 0),
                clear_color: clear,
            }
        }
    }

    unsafe fn restore(self, gl: &glow::Context) {
        unsafe {
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, self.read);
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, self.draw);
            if self.scissor {
                gl.enable(glow::SCISSOR_TEST);
            }
            let [r, g, b, a] = self.color_mask;
            gl.color_mask(r, g, b, a);
            let [r, g, b, a] = self.clear_color;
            gl.clear_color(r, g, b, a);
        }
    }
}

/// How content left pixel packing, put back after a frame is read back:
/// the buffer packed into, and how rows are laid out in it.
struct SavedPackState {
    buffer: Option<glow::NativeBuffer>,
    alignment: i32,
    row_length: i32,
    skip_rows: i32,
    skip_pixels: i32,
}

impl SavedPackState {
    /// Content's packing, saved, and the tight packing a frame is read with set.
    unsafe fn save(gl: &glow::Context) -> Self {
        unsafe {
            let saved = Self {
                buffer: gl.get_parameter_buffer(glow::PIXEL_PACK_BUFFER_BINDING),
                alignment: gl.get_parameter_i32(glow::PACK_ALIGNMENT),
                row_length: gl.get_parameter_i32(glow::PACK_ROW_LENGTH),
                skip_rows: gl.get_parameter_i32(glow::PACK_SKIP_ROWS),
                skip_pixels: gl.get_parameter_i32(glow::PACK_SKIP_PIXELS),
            };
            gl.pixel_store_i32(glow::PACK_ALIGNMENT, 4);
            gl.pixel_store_i32(glow::PACK_ROW_LENGTH, 0);
            gl.pixel_store_i32(glow::PACK_SKIP_ROWS, 0);
            gl.pixel_store_i32(glow::PACK_SKIP_PIXELS, 0);
            saved
        }
    }

    unsafe fn restore(self, gl: &glow::Context) {
        unsafe {
            gl.pixel_store_i32(glow::PACK_ALIGNMENT, self.alignment);
            gl.pixel_store_i32(glow::PACK_ROW_LENGTH, self.row_length);
            gl.pixel_store_i32(glow::PACK_SKIP_ROWS, self.skip_rows);
            gl.pixel_store_i32(glow::PACK_SKIP_PIXELS, self.skip_pixels);
            gl.bind_buffer(glow::PIXEL_PACK_BUFFER, self.buffer);
        }
    }
}

fn gl_error(e: String) -> EngineError {
    EngineError::new(ErrorCode::RenderBackendError)
        .with_msg("a recording texture could not be made")
        .with_detail(e)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_recording_keeps_content_s_shape_in_whole_macroblocks() {
        assert_eq!(
            encoded_size(1080, 2340),
            (576, 1280),
            "a tall phone's frame"
        );
        assert_eq!(encoded_size(2340, 1080), (1280, 576));
        assert_eq!(
            encoded_size(640, 360),
            (640, 352),
            "never larger than content"
        );
        assert_eq!(encoded_size(1920, 1080), (1280, 720));
    }

    #[test]
    fn content_s_frame_is_fitted_and_centred() {
        assert_eq!(fitted(1080, 2340, (576, 1280)), [0, 16, 576, 1264]);
        assert_eq!(fitted(100, 100, (200, 100)), [50, 0, 150, 100]);
    }

    #[test]
    fn the_clock_runs_only_while_frames_are_taken() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut clock = RecordingClock::new(5_000.0);
        assert_eq!(clock.now(at(100)), 0.0, "not started");
        clock.start(at(100));
        assert_eq!(clock.now(at(1_100)), 1_000.0);
        clock.pause(true, at(1_100));
        assert_eq!(clock.now(at(9_000)), 1_000.0, "paused");
        clock.pause(false, at(9_000));
        clock.suspend(true, at(10_000));
        assert_eq!(clock.now(at(20_000)), 2_000.0, "in the background");
        clock.suspend(false, at(20_000));
        assert!(clock.taking_frames(at(20_000)));
        assert_eq!(clock.now(at(30_000)), 5_000.0, "at its duration");
        assert!(!clock.taking_frames(at(30_000)));
    }
}
