//! The cpal device backend: oboe on Android, ALSA on Linux, CoreAudio on Apple.
//!
//! Only the device half lives here. The ring buffer, the watermarks and the real-time
//! render logic are in the parent module, shared with the OpenHarmony backend.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, SampleFormat, Stream, StreamConfig};
use ringbuf::traits::{Observer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};
use shared::error::{EngineError, EngineResult, ErrorCode};
use tracing::{error, info};

use super::{
    AudioSync, FromNormalizedF32, HIGH_WATERMARK_FRAMES, LOW_WATERMARK_FRAMES, OutputCallback,
    RING_BUFFER_FRAMES,
};

/// Audio output handle
pub struct AudioOutput {
    stream: Stream,
    producer: HeapProd<f32>,
    sample_rate: u32,
    channels: u32,
    sync: AudioSync,
    low_watermark_samples: usize,
    high_watermark_samples: usize,
    stream_error: Arc<AtomicBool>,
}

impl AudioOutput {
    /// Create a new audio output
    pub fn new() -> EngineResult<Self> {
        // iOS plays through the process's audio session, and a unit opened
        // before it has a category and is active is silent or refuses to
        // start. Nothing else on this lane sets it up.
        #[cfg(target_os = "ios")]
        crate::apple_session::ensure_configured();

        let host = cpal::default_host();

        let device = host.default_output_device().ok_or_else(|| {
            EngineError::from_detail(ErrorCode::NotFound, "No audio output device found")
        })?;

        info!("Audio output device: {:?}", device.name());

        let config = device.default_output_config().map_err(|e| {
            EngineError::from_detail(
                ErrorCode::Internal,
                format!("Failed to get default output config: {}", e),
            )
        })?;

        let sample_rate = config.sample_rate().0;
        let channels = config.channels() as u32;

        info!(
            "Audio output config: sample_rate={}, channels={}, format={:?}",
            sample_rate,
            channels,
            config.sample_format()
        );

        let ring_size = RING_BUFFER_FRAMES * channels as usize;
        let ring = HeapRb::<f32>::new(ring_size);
        let (producer, consumer) = ring.split();

        let sync = AudioSync::new();
        let low_watermark_samples = LOW_WATERMARK_FRAMES * channels as usize;
        let high_watermark_samples = HIGH_WATERMARK_FRAMES * channels as usize;
        let stream_error = Arc::new(AtomicBool::new(false));

        let stream = match config.sample_format() {
            SampleFormat::F32 => build_stream_f32(
                &device,
                &config.into(),
                consumer,
                sync.clone(),
                low_watermark_samples,
                stream_error.clone(),
            )?,
            SampleFormat::I16 => build_stream_converted::<i16>(
                &device,
                &config.into(),
                consumer,
                sync.clone(),
                low_watermark_samples,
                stream_error.clone(),
            )?,
            SampleFormat::U16 => build_stream_converted::<u16>(
                &device,
                &config.into(),
                consumer,
                sync.clone(),
                low_watermark_samples,
                stream_error.clone(),
            )?,
            format => {
                return Err(EngineError::from_detail(
                    ErrorCode::Unsupported,
                    format!("Unsupported sample format: {:?}", format),
                ));
            }
        };

        stream.play().map_err(|e| {
            EngineError::from_detail(
                ErrorCode::Internal,
                format!("Failed to start audio stream: {}", e),
            )
        })?;

        Ok(Self {
            stream,
            producer,
            sample_rate,
            channels,
            sync,
            low_watermark_samples,
            high_watermark_samples,
            stream_error,
        })
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

    /// Currently buffered sample count, read from the producer side so it is
    /// always fresh — unlike the callback-updated `buffer_level` hint behind
    /// [`needs_data`](Self::needs_data). Use this to decide how much to refill.
    #[inline]
    pub fn buffered(&self) -> usize {
        self.producer.occupied_len()
    }

    /// Refill target depth in samples: fill up to here, not the whole ring.
    ///
    /// Never below twice the largest observed device callback, so a device that
    /// requests large blocks can always hold a full callback (otherwise every
    /// callback would partially underrun). The fill loop's `available() >=
    /// buffer_size` check still bounds this to the ring capacity.
    #[inline]
    pub fn high_watermark(&self) -> usize {
        self.high_watermark_samples
            .max(self.sync.max_callback().saturating_mul(2))
    }

    /// Check if buffer needs more data
    #[inline]
    pub fn needs_data(&self) -> bool {
        self.sync.buffer_level() < self.low_watermark_samples
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
    /// Use when the app is backgrounded or no audio is playing to save power.
    pub fn pause_stream(&self) -> bool {
        match self.stream.pause() {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!("Failed to pause audio stream: {}", e);
                false
            }
        }
    }

    /// Resume the audio stream (restarts the hardware callback).
    pub fn resume_stream(&self) -> bool {
        match self.stream.play() {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!("Failed to resume audio stream: {}", e);
                self.stream_error.store(true, Ordering::Release);
                false
            }
        }
    }
}

fn build_stream_f32(
    device: &Device,
    config: &StreamConfig,
    consumer: HeapCons<f32>,
    sync: AudioSync,
    low_watermark: usize,
    stream_error: Arc<AtomicBool>,
) -> EngineResult<Stream> {
    let mut callback = OutputCallback::new(consumer, sync, low_watermark);
    build_stream(device, config, stream_error, move |data: &mut [f32]| {
        callback.render_native(data)
    })
}

fn build_stream_converted<T>(
    device: &Device,
    config: &StreamConfig,
    consumer: HeapCons<f32>,
    sync: AudioSync,
    low_watermark: usize,
    stream_error: Arc<AtomicBool>,
) -> EngineResult<Stream>
where
    T: cpal::SizedSample + FromNormalizedF32 + 'static,
{
    let mut callback = OutputCallback::new(consumer, sync, low_watermark);
    build_stream(device, config, stream_error, move |data: &mut [T]| {
        callback.render_converted(data)
    })
}

fn build_stream<T, F>(
    device: &Device,
    config: &StreamConfig,
    stream_error: Arc<AtomicBool>,
    mut render: F,
) -> EngineResult<Stream>
where
    T: cpal::SizedSample + 'static,
    F: FnMut(&mut [T]) + Send + 'static,
{
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _: &cpal::OutputCallbackInfo| render(data),
            move |err| {
                error!("Audio output error: {}", err);
                stream_error.store(true, Ordering::Release);
            },
            None,
        )
        .map_err(|e| {
            EngineError::from_detail(
                ErrorCode::Internal,
                format!("Failed to build audio stream: {}", e),
            )
        })
}
