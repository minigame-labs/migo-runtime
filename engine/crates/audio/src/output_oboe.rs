//! Android device output. C++ owns all Oboe objects; Rust owns the shared render
//! callback until Oboe's final shared callback owner releases it.

use std::{ffi::c_void, ptr::NonNull};

use ringbuf::traits::{Observer, Producer, Split};
use ringbuf::{HeapProd, HeapRb};
use shared::error::{EngineError, EngineResult, ErrorCode};

use super::{AudioSync, OutputCallback, RING_BUFFER_FRAMES};

#[repr(C)]
struct Callbacks {
    context: *mut c_void,
    render: unsafe extern "C" fn(*mut c_void, *mut f32, usize),
    release: unsafe extern "C" fn(*mut c_void),
}

#[repr(C)]
#[derive(Default)]
struct Config {
    sample_rate: i32,
    channels: i32,
    frames_per_burst: i32,
    buffer_capacity_frames: i32,
    buffer_size_frames: i32,
    audio_api: i32,
}

unsafe extern "C" {
    fn migo_oboe_open(callbacks: Callbacks, config: *mut Config, output: *mut *mut c_void) -> i32;
    fn migo_oboe_start(stream: *mut c_void) -> i32;
    fn migo_oboe_pause(stream: *mut c_void) -> i32;
    fn migo_oboe_error(stream: *const c_void) -> i32;
    fn migo_oboe_close(stream: *mut c_void);
}

struct Stream(NonNull<c_void>);

// Control methods and Drop belong to the single audio thread. Moving its opaque
// owner is safe; callbacks own a separate context, not a borrow of this handle.
unsafe impl Send for Stream {}

impl Drop for Stream {
    fn drop(&mut self) {
        // SAFETY: unique opaque handle from open, closed exactly once, away from
        // the device callback. Oboe retains its own shared error-thread owners.
        unsafe { migo_oboe_close(self.0.as_ptr()) };
    }
}

unsafe extern "C" fn render(context: *mut c_void, samples: *mut f32, sample_count: usize) {
    // SAFETY: the bridge only starts after verifying float/stereo. Oboe supplies
    // an exclusive initialized output buffer and serializes this stream's data
    // callbacks. No control/error callback accesses OutputCallback.
    let callback = unsafe { &mut *context.cast::<OutputCallback>() };
    let samples = unsafe { std::slice::from_raw_parts_mut(samples, sample_count) };
    callback.render_native(samples);
}

unsafe extern "C" fn release(context: *mut c_void) {
    // SAFETY: open consumes this Box on every outcome. The C++ context guard is
    // move-only; only its final owner destroys this exact pointer.
    drop(unsafe { Box::from_raw(context.cast::<OutputCallback>()) });
}

pub struct AudioOutput {
    stream: Stream,
    producer: HeapProd<f32>,
    sync: AudioSync,
    sample_rate: u32,
    failed: bool,
}

impl AudioOutput {
    pub fn new() -> EngineResult<Self> {
        const CHANNELS: usize = 2;
        let (producer, consumer) = HeapRb::<f32>::new(RING_BUFFER_FRAMES * CHANNELS).split();
        let sync = AudioSync::new();
        let callback = Box::new(OutputCallback::new(consumer, sync.clone(), 0));
        let callbacks = Callbacks {
            context: Box::into_raw(callback).cast(),
            render,
            release,
        };
        let mut config = Config::default();
        let mut pointer = std::ptr::null_mut();
        // SAFETY: valid out pointers and a uniquely transferred callback Box.
        // The bridge consumes it on success, invalid config and exceptions.
        let result = unsafe { migo_oboe_open(callbacks, &mut config, &mut pointer) };
        if result != 0 {
            return Err(device_error("open", result));
        }
        let stream = Stream(NonNull::new(pointer).ok_or_else(|| device_error("open", -1))?);
        sync.configure_device(
            CHANNELS,
            usize::try_from(config.frames_per_burst).unwrap_or(0),
        );
        tracing::info!(
            sample_rate = config.sample_rate,
            channels = config.channels,
            frames_per_burst = config.frames_per_burst,
            buffer_size_frames = config.buffer_size_frames,
            buffer_capacity_frames = config.buffer_capacity_frames,
            api = config.audio_api,
            "Oboe output opened"
        );
        // SAFETY: open validated this stream; the callback owns no reference to
        // the not-yet-returned AudioOutput.
        let result = unsafe { migo_oboe_start(stream.0.as_ptr()) };
        if result != 0 {
            return Err(device_error("start", result));
        }
        Ok(Self {
            stream,
            producer,
            sync,
            sample_rate: config.sample_rate as u32,
            failed: false,
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    pub fn channels(&self) -> u32 {
        2
    }
    pub fn write(&mut self, samples: &[f32]) -> usize {
        self.producer.push_slice(samples)
    }
    pub fn available(&self) -> usize {
        self.producer.vacant_len()
    }
    pub fn buffered(&self) -> usize {
        self.producer.occupied_len()
    }
    pub fn high_watermark(&self) -> usize {
        self.sync.device_high_watermark()
    }
    pub fn needs_data(&self) -> bool {
        self.buffered() <= self.sync.low_watermark(0)
    }
    pub fn sync(&self) -> &AudioSync {
        &self.sync
    }
    pub fn is_alive(&self) -> bool {
        // SAFETY: this only reads the live callback's atomic error value.
        !self.failed && unsafe { migo_oboe_error(self.stream.0.as_ptr()) == 0 }
    }
    pub fn pause_stream(&self) -> bool {
        // SAFETY: the live stream is controlled only by its audio-thread owner.
        let result = unsafe { migo_oboe_pause(self.stream.0.as_ptr()) };
        if result != 0 {
            tracing::warn!(result, "Oboe pause failed");
        }
        result == 0
    }
    pub fn resume_stream(&mut self) -> bool {
        // SAFETY: same single control-thread ownership as pause.
        let result = unsafe { migo_oboe_start(self.stream.0.as_ptr()) };
        if result != 0 {
            tracing::warn!(result, "Oboe resume failed");
            self.failed = true;
        }
        result == 0
    }
}

fn device_error(operation: &str, result: i32) -> EngineError {
    EngineError::from_detail(
        ErrorCode::Internal,
        format!("Oboe {operation} failed ({result})"),
    )
}
