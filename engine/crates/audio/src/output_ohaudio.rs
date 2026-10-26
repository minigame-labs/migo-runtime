//! The OHAudio device backend, for OpenHarmony.
//!
//! OpenHarmony's userspace is musl and has no ALSA, and cpal has no OHAudio support, so
//! this is the one platform that reaches a device without cpal. Only the device half
//! lives here: the ring buffer, the watermarks and the real-time render logic are in the
//! parent module, shared with the cpal backend, so the allocation gates that cover the
//! callback cover this one too.
//!
//! Declared by hand against `<ohaudio/native_audiostreambuilder.h>` and
//! `<ohaudio/native_audiorenderer.h>` rather than generated: it is fourteen functions and
//! six enum constants, and a bindgen step would put a clang and a header search path between
//! this crate and a build that already has to pin both for V8.

use std::ffi::{c_int, c_void};
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ringbuf::traits::{Observer, Producer, Split};
use ringbuf::{HeapProd, HeapRb};
use shared::error::{EngineError, EngineResult, ErrorCode};
use tracing::{error, info};

use super::{AudioSync, OutputCallback, RING_BUFFER_FRAMES};

/// What this stream asks the device for.
///
/// OHAudio has no "default output configuration" to query the way cpal does — a renderer
/// is built from values the caller states — so these are chosen rather than discovered.
/// 48 kHz stereo is what the mixer already runs at, so choosing anything else would add a
/// resample on every frame for no benefit.
const OHOS_SAMPLE_RATE: u32 = 48_000;
const OHOS_CHANNELS: u32 = 2;

// OH_AudioStream_Result
const AUDIOSTREAM_SUCCESS: c_int = 0;
// OH_AudioStream_Type
const AUDIOSTREAM_TYPE_RENDERER: c_int = 1;
// OH_AudioStream_SampleFormat: the ring's own format, so the callback never converts.
const AUDIOSTREAM_SAMPLE_F32LE: c_int = 4;
// OH_AudioStream_EncodingType
const AUDIOSTREAM_ENCODING_TYPE_RAW: c_int = 0;
// OH_AudioStream_Usage: games get the low-latency mixer path and the right ducking policy.
const AUDIOSTREAM_USAGE_GAME: c_int = 11;
// OH_AudioStream_LatencyMode
const AUDIOSTREAM_LATENCY_MODE_FAST: c_int = 1;

#[repr(C)]
struct OhAudioStreamBuilder {
    _opaque: [u8; 0],
}

#[repr(C)]
struct OhAudioRenderer {
    _opaque: [u8; 0],
}

/// `OH_AudioRenderer_Callbacks`, field for field and in order.
///
/// The struct is passed **by value** to `OH_AudioStreamBuilder_SetRendererCallback`, so a
/// missing or reordered member is not a link error — it is a call through the wrong
/// pointer at runtime. Every member is declared even though only two are used, because a
/// short struct passed by value reads the caller's stack as if it were the rest.
#[repr(C)]
struct OhAudioRendererCallbacks {
    on_write_data: Option<
        unsafe extern "C" fn(
            renderer: *mut OhAudioRenderer,
            user_data: *mut c_void,
            buffer: *mut c_void,
            length: i32,
        ) -> i32,
    >,
    on_stream_event: Option<
        unsafe extern "C" fn(
            renderer: *mut OhAudioRenderer,
            user_data: *mut c_void,
            event: c_int,
        ) -> i32,
    >,
    on_interrupt_event: Option<
        unsafe extern "C" fn(
            renderer: *mut OhAudioRenderer,
            user_data: *mut c_void,
            force_type: c_int,
            hint: c_int,
        ) -> i32,
    >,
    on_error: Option<
        unsafe extern "C" fn(
            renderer: *mut OhAudioRenderer,
            user_data: *mut c_void,
            error: c_int,
        ) -> i32,
    >,
}

#[link(name = "ohaudio")]
unsafe extern "C" {
    fn OH_AudioStreamBuilder_Create(
        builder: *mut *mut OhAudioStreamBuilder,
        stream_type: c_int,
    ) -> c_int;
    fn OH_AudioStreamBuilder_Destroy(builder: *mut OhAudioStreamBuilder) -> c_int;
    fn OH_AudioStreamBuilder_SetSamplingRate(
        builder: *mut OhAudioStreamBuilder,
        rate: i32,
    ) -> c_int;
    fn OH_AudioStreamBuilder_SetChannelCount(
        builder: *mut OhAudioStreamBuilder,
        channel_count: i32,
    ) -> c_int;
    fn OH_AudioStreamBuilder_SetSampleFormat(
        builder: *mut OhAudioStreamBuilder,
        format: c_int,
    ) -> c_int;
    fn OH_AudioStreamBuilder_SetEncodingType(
        builder: *mut OhAudioStreamBuilder,
        encoding_type: c_int,
    ) -> c_int;
    fn OH_AudioStreamBuilder_SetLatencyMode(
        builder: *mut OhAudioStreamBuilder,
        latency_mode: c_int,
    ) -> c_int;
    fn OH_AudioStreamBuilder_SetRendererInfo(
        builder: *mut OhAudioStreamBuilder,
        usage: c_int,
    ) -> c_int;
    fn OH_AudioStreamBuilder_SetRendererCallback(
        builder: *mut OhAudioStreamBuilder,
        callbacks: OhAudioRendererCallbacks,
        user_data: *mut c_void,
    ) -> c_int;
    fn OH_AudioStreamBuilder_GenerateRenderer(
        builder: *mut OhAudioStreamBuilder,
        renderer: *mut *mut OhAudioRenderer,
    ) -> c_int;
    fn OH_AudioRenderer_Start(renderer: *mut OhAudioRenderer) -> c_int;
    fn OH_AudioRenderer_Pause(renderer: *mut OhAudioRenderer) -> c_int;
    fn OH_AudioRenderer_Stop(renderer: *mut OhAudioRenderer) -> c_int;
    fn OH_AudioRenderer_Release(renderer: *mut OhAudioRenderer) -> c_int;
}

/// What the OHAudio callback thread reaches through `user_data`.
///
/// Boxed and kept alive by [`AudioOutput`] for exactly as long as the renderer can call
/// back — which is why the box is released *after* `OH_AudioRenderer_Release`, not before.
struct CallbackState {
    render: OutputCallback,
    stream_error: Arc<AtomicBool>,
}

/// Fills the device buffer from the ring.
///
/// `length` is a byte count, and the stream was built as `F32LE`, so it is four bytes per
/// sample. A device that ever handed a length that is not a whole number of samples would
/// make the tail of its buffer undefined, so the remainder is refused rather than rounded.
///
/// No allocation, no lock and no logging: this runs on a thread the platform schedules as
/// real time, and the parent module's gates measure the same `render_native` this calls.
unsafe extern "C" fn on_write_data(
    _renderer: *mut OhAudioRenderer,
    user_data: *mut c_void,
    buffer: *mut c_void,
    length: i32,
) -> i32 {
    if user_data.is_null() || buffer.is_null() || length <= 0 {
        return 0;
    }
    let bytes = length as usize;
    if !bytes.is_multiple_of(size_of::<f32>()) {
        return 0;
    }
    // SAFETY: `user_data` is the pointer handed to SetRendererCallback, which points at a
    // `CallbackState` box that `AudioOutput` keeps alive until after the renderer is
    // released. The reference is taken to the `render` field alone, not to the whole
    // struct: `on_error` is a *separate* callback that may run concurrently with this one,
    // so a `&mut CallbackState` here and a `&CallbackState` there would be two overlapping
    // references with one mutable -- undefined behaviour even though the error path only
    // touches an atomic. Disjoint fields cannot alias, so each callback projects to the one
    // it needs. OHAudio serialises the data callback against itself, which is what makes
    // this `&mut` the only one.
    let render = unsafe { &mut (*user_data.cast::<CallbackState>()).render };
    // SAFETY: the device owns `bytes` writable bytes at `buffer` for this call, and
    // `F32LE` means they are `f32`-aligned.
    let samples =
        unsafe { std::slice::from_raw_parts_mut(buffer.cast::<f32>(), bytes / size_of::<f32>()) };
    render.render_native(samples);
    0
}

/// Records a device error so `is_alive()` can report it, mirroring cpal's error callback.
unsafe extern "C" fn on_error(
    _renderer: *mut OhAudioRenderer,
    user_data: *mut c_void,
    error: c_int,
) -> i32 {
    if !user_data.is_null() {
        // SAFETY: the same box `on_write_data` reaches, projected to a *different* field.
        // Taking `&CallbackState` here would overlap with that callback's `&mut`, which the
        // device is free to run concurrently -- this callback is not the data callback and
        // is not serialised against it.
        let stream_error = unsafe { &(*user_data.cast::<CallbackState>()).stream_error };
        stream_error.store(true, Ordering::Release);
    }
    error!("OHAudio renderer error: {error}");
    0
}

/// Audio output handle
pub struct AudioOutput {
    renderer: *mut OhAudioRenderer,
    /// Owns the callback state for as long as the renderer can reach it.
    state: *mut CallbackState,
    producer: HeapProd<f32>,
    sample_rate: u32,
    channels: u32,
    sync: AudioSync,
    stream_error: Arc<AtomicBool>,
}

// SAFETY: the two raw pointers are owned by this handle and only ever dereferenced by the
// device thread through `user_data`, which OHAudio serialises. Everything else is `Send`.
unsafe impl Send for AudioOutput {}

impl AudioOutput {
    /// Create a new audio output
    pub fn new() -> EngineResult<Self> {
        let ring_size = RING_BUFFER_FRAMES * OHOS_CHANNELS as usize;
        let (producer, consumer) = HeapRb::<f32>::new(ring_size).split();
        let sync = AudioSync::new();
        let stream_error = Arc::new(AtomicBool::new(false));

        let state = Box::into_raw(Box::new(CallbackState {
            render: OutputCallback::new(consumer, sync.clone(), 0),
            stream_error: stream_error.clone(),
        }));

        // From here on every early return has to free `state`, so the fallible part is one
        // function and this one owns the cleanup. A `?` in the middle of the builder
        // sequence is how a leak of the ring's consumer half gets written by accident.
        match unsafe { build_renderer(state) } {
            Ok(renderer) => {
                sync.configure_device(OHOS_CHANNELS as usize, 0);
                Ok(Self {
                    renderer,
                    state,
                    producer,
                    sample_rate: OHOS_SAMPLE_RATE,
                    channels: OHOS_CHANNELS,
                    sync,
                    stream_error,
                })
            }
            Err(error) => {
                // SAFETY: no renderer was produced, so nothing can reach `state`.
                drop(unsafe { Box::from_raw(state) });
                Err(error)
            }
        }
    }

    /// Get the sample rate
    #[inline]
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Get the number of channels
    #[inline]
    pub fn channels(&self) -> u32 {
        self.channels
    }

    /// Write samples to the output buffer
    /// Returns the number of samples written
    #[inline]
    pub fn write(&mut self, samples: &[f32]) -> usize {
        self.producer.push_slice(samples)
    }

    /// Get available space in the buffer (in samples)
    #[inline]
    pub fn available(&self) -> usize {
        self.producer.vacant_len()
    }

    /// Currently buffered sample count, read from the producer side so it is always fresh.
    #[inline]
    pub fn buffered(&self) -> usize {
        self.producer.occupied_len()
    }

    /// Refill target depth in samples: fill up to here, not the whole ring.
    #[inline]
    pub fn high_watermark(&self) -> usize {
        self.sync.device_high_watermark()
    }

    /// Check if buffer needs more data
    #[inline]
    pub fn needs_data(&self) -> bool {
        self.buffered() <= self.sync.low_watermark(0)
    }

    /// Get sync handle for checking callback signals
    #[inline]
    pub fn sync(&self) -> &AudioSync {
        &self.sync
    }

    /// Check if the audio stream is still alive (no errors reported).
    #[inline]
    pub fn is_alive(&self) -> bool {
        !self.stream_error.load(Ordering::Acquire)
    }

    /// Pause the audio stream (stops the hardware callback).
    pub fn pause_stream(&self) -> bool {
        // SAFETY: `renderer` is live for this handle's lifetime.
        let result = unsafe { OH_AudioRenderer_Pause(self.renderer) };
        if result != AUDIOSTREAM_SUCCESS {
            tracing::warn!("Failed to pause OHAudio renderer: {result}");
            return false;
        }
        true
    }

    /// Resume the audio stream (restarts the hardware callback).
    pub fn resume_stream(&self) -> bool {
        // SAFETY: as in `pause_stream`.
        let result = unsafe { OH_AudioRenderer_Start(self.renderer) };
        if result != AUDIOSTREAM_SUCCESS {
            tracing::warn!("Failed to resume OHAudio renderer: {result}");
            self.stream_error.store(true, Ordering::Release);
            return false;
        }
        true
    }
}

impl Drop for AudioOutput {
    fn drop(&mut self) {
        // Stop, then release, then free the callback state -- and the last step only if the
        // release succeeded. The device thread reaches that box through `user_data`, so a
        // renderer that is still live when the box is freed is a use-after-free on a
        // real-time thread. `OH_AudioRenderer_Release` can fail, and freeing anyway on that
        // path is the shape this ordering exists to prevent, so an unrecoverable release
        // leaks the state instead. A leaked ring buffer is a bounded, silent cost; a
        // callback into freed memory is neither.
        // SAFETY: `renderer` and `state` are owned by this handle and dropped once.
        unsafe {
            let stopped = OH_AudioRenderer_Stop(self.renderer);
            if stopped != AUDIOSTREAM_SUCCESS {
                error!("OHAudio renderer stop failed: {stopped}");
            }
            let released = OH_AudioRenderer_Release(self.renderer);
            if released == AUDIOSTREAM_SUCCESS {
                drop(Box::from_raw(self.state));
            } else {
                error!(
                    "OHAudio renderer release failed: {released}; leaking its callback state, \
                     because the renderer may still call back into it"
                );
            }
        }
    }
}

/// The builder sequence, as one fallible unit.
///
/// # Safety
///
/// `state` must point at a live `CallbackState` that outlives the returned renderer.
unsafe fn build_renderer(state: *mut CallbackState) -> EngineResult<*mut OhAudioRenderer> {
    fn failed(what: &str, code: c_int) -> EngineError {
        EngineError::from_detail(
            ErrorCode::Internal,
            format!("OHAudio {what} failed: {code}"),
        )
    }

    let mut builder: *mut OhAudioStreamBuilder = ptr::null_mut();
    let code = unsafe { OH_AudioStreamBuilder_Create(&mut builder, AUDIOSTREAM_TYPE_RENDERER) };
    if code != AUDIOSTREAM_SUCCESS || builder.is_null() {
        return Err(failed("stream builder creation", code));
    }

    // The builder is destroyed on every path out of here, success included: it describes
    // the renderer rather than owning it, and OHAudio's own documentation requires the
    // Destroy call once GenerateRenderer has been made.
    let outcome = unsafe { configure_and_generate(builder, state) };
    let destroy = unsafe { OH_AudioStreamBuilder_Destroy(builder) };
    if destroy != AUDIOSTREAM_SUCCESS {
        // Not fatal on the success path: the renderer is already generated and usable, and
        // reporting a leaked builder as a failed device would silence audio for it.
        error!("OHAudio stream builder destroy failed: {destroy}");
    }
    outcome
}

/// # Safety
///
/// `builder` must be live, and `state` must outlive the renderer it produces.
unsafe fn configure_and_generate(
    builder: *mut OhAudioStreamBuilder,
    state: *mut CallbackState,
) -> EngineResult<*mut OhAudioRenderer> {
    let steps: [(&str, c_int); 6] = unsafe {
        [
            (
                "set sampling rate",
                OH_AudioStreamBuilder_SetSamplingRate(builder, OHOS_SAMPLE_RATE as i32),
            ),
            (
                "set channel count",
                OH_AudioStreamBuilder_SetChannelCount(builder, OHOS_CHANNELS as i32),
            ),
            (
                "set sample format",
                OH_AudioStreamBuilder_SetSampleFormat(builder, AUDIOSTREAM_SAMPLE_F32LE),
            ),
            (
                "set encoding type",
                OH_AudioStreamBuilder_SetEncodingType(builder, AUDIOSTREAM_ENCODING_TYPE_RAW),
            ),
            (
                "set latency mode",
                OH_AudioStreamBuilder_SetLatencyMode(builder, AUDIOSTREAM_LATENCY_MODE_FAST),
            ),
            (
                "set renderer info",
                OH_AudioStreamBuilder_SetRendererInfo(builder, AUDIOSTREAM_USAGE_GAME),
            ),
        ]
    };
    for (what, code) in steps {
        if code != AUDIOSTREAM_SUCCESS {
            return Err(EngineError::from_detail(
                ErrorCode::Internal,
                format!("OHAudio {what} failed: {code}"),
            ));
        }
    }

    let callbacks = OhAudioRendererCallbacks {
        on_write_data: Some(on_write_data),
        on_stream_event: None,
        on_interrupt_event: None,
        on_error: Some(on_error),
    };
    let code =
        unsafe { OH_AudioStreamBuilder_SetRendererCallback(builder, callbacks, state.cast()) };
    if code != AUDIOSTREAM_SUCCESS {
        return Err(EngineError::from_detail(
            ErrorCode::Internal,
            format!("OHAudio set renderer callback failed: {code}"),
        ));
    }

    let mut renderer: *mut OhAudioRenderer = ptr::null_mut();
    let code = unsafe { OH_AudioStreamBuilder_GenerateRenderer(builder, &mut renderer) };
    if code != AUDIOSTREAM_SUCCESS || renderer.is_null() {
        return Err(EngineError::from_detail(
            ErrorCode::Internal,
            format!("OHAudio renderer generation failed: {code}"),
        ));
    }

    // Started here rather than by the caller, so this function's contract is "a renderer
    // that is producing audio" and there is no window where the ring drains unheard.
    let code = unsafe { OH_AudioRenderer_Start(renderer) };
    if code != AUDIOSTREAM_SUCCESS {
        unsafe {
            OH_AudioRenderer_Release(renderer);
        }
        return Err(EngineError::from_detail(
            ErrorCode::Internal,
            format!("OHAudio renderer start failed: {code}"),
        ));
    }

    info!("OHAudio output: sample_rate={OHOS_SAMPLE_RATE}, channels={OHOS_CHANNELS}, format=F32LE");
    Ok(renderer)
}
