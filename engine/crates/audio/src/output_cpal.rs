//! The cpal device backend: ALSA on Linux, WASAPI on Windows, CoreAudio on Apple.
//!
//! Only the device half lives here. The ring buffer, the watermarks and the real-time
//! render logic are in the parent module, shared with the OpenHarmony backend.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crossbeam_utils::atomic::AtomicCell;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, SampleFormat, Stream, StreamConfig};
use ringbuf::traits::{Observer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};
use shared::error::{EngineError, EngineResult, ErrorCode};
use tracing::info;

use super::{AudioSync, FromNormalizedF32, OutputCallback, RING_BUFFER_FRAMES};

/// Audio output handle
pub struct AudioOutput {
    stream: Stream,
    producer: HeapProd<f32>,
    sample_rate: u32,
    channels: u32,
    sync: AudioSync,
    stream_errors: Arc<StreamErrors>,
}

impl AudioOutput {
    /// Create a new audio output
    pub fn new() -> EngineResult<Self> {
        // Migo supplies the mini-game category; CPAL activates the session
        // when starting and owns subsequent system interruption recovery.
        #[cfg(target_os = "ios")]
        crate::apple_session::ensure_configured();

        let host = cpal::default_host();

        let device = host.default_output_device().ok_or_else(|| {
            EngineError::from_detail(ErrorCode::NotFound, "No audio output device found")
        })?;

        info!("Audio output device: {:?}", device.description());

        let config = device.default_output_config().map_err(|e| {
            EngineError::from_detail(
                ErrorCode::Internal,
                format!("Failed to get default output config: {}", e),
            )
        })?;

        let sample_rate = config.sample_rate();
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
        let stream_errors = Arc::new(StreamErrors::new()?);

        let stream = match config.sample_format() {
            SampleFormat::F32 => build_stream_f32(
                &device,
                config.into(),
                consumer,
                sync.clone(),
                stream_errors.clone(),
            )?,
            SampleFormat::I16 => build_stream_converted::<i16>(
                &device,
                config.into(),
                consumer,
                sync.clone(),
                stream_errors.clone(),
            )?,
            SampleFormat::U16 => build_stream_converted::<u16>(
                &device,
                config.into(),
                consumer,
                sync.clone(),
                stream_errors.clone(),
            )?,
            format => {
                return Err(EngineError::from_detail(
                    ErrorCode::Unsupported,
                    format!("Unsupported sample format: {:?}", format),
                ));
            }
        };

        sync.configure_device(
            channels as usize,
            stream.buffer_size().unwrap_or(0) as usize,
        );

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
            stream_errors,
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

    /// Device-derived, quantum-aligned software target, bounded by the ring.
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

    /// Drain coalesced diagnostics on the owner thread and test stream health.
    #[inline]
    pub fn is_alive(&self) -> bool {
        if let Some(kind) = self.stream_errors.pending.swap(None) {
            if kind == cpal::ErrorKind::Xrun {
                // Do not turn sustained underruns into warning-level IO on
                // the producer that must refill the queue.
                tracing::debug!("Audio output xrun");
            } else {
                tracing::warn!(?kind, "Audio output event");
            }
        }
        !self.stream_errors.failed.load(Ordering::Acquire)
    }

    /// A new explicit playback API request may resume a completed interruption
    /// even if the power gate still records requested playback. CPAL makes the
    /// conditional transition under its own lock, independent of notification
    /// ordering with Migo's host-event observer.
    #[cfg(target_os = "ios")]
    pub fn resume_interrupted_stream(&self) {
        let cpal::platform::StreamInner::CoreAudio(stream) = self.stream.as_inner();
        if let Err(error) = stream.resume_after_interruption() {
            self.stream_errors.record(error.kind());
        }
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
                self.stream_errors.failed.store(true, Ordering::Release);
                false
            }
        }
    }
}

fn build_stream_f32(
    device: &Device,
    config: StreamConfig,
    consumer: HeapCons<f32>,
    sync: AudioSync,
    stream_errors: Arc<StreamErrors>,
) -> EngineResult<Stream> {
    let mut callback = OutputCallback::new(consumer, sync, 0);
    build_stream(device, config, stream_errors, move |data: &mut [f32]| {
        callback.render_native(data)
    })
}

fn build_stream_converted<T>(
    device: &Device,
    config: StreamConfig,
    consumer: HeapCons<f32>,
    sync: AudioSync,
    stream_errors: Arc<StreamErrors>,
) -> EngineResult<Stream>
where
    T: cpal::SizedSample + FromNormalizedF32 + 'static,
{
    let mut callback = OutputCallback::new(consumer, sync, 0);
    build_stream(device, config, stream_errors, move |data: &mut [T]| {
        callback.render_converted(data)
    })
}

fn build_stream<T, F>(
    device: &Device,
    config: StreamConfig,
    stream_errors: Arc<StreamErrors>,
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
                stream_errors.record(err.kind());
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

/// Coalesce diagnostics for the non-real-time owner. The device/error callback
/// only stores Copy values; it never formats a message or calls a subscriber.
struct StreamErrors {
    failed: AtomicBool,
    pending: AtomicCell<Option<cpal::ErrorKind>>,
}

impl StreamErrors {
    fn new() -> EngineResult<Self> {
        if !AtomicCell::<Option<cpal::ErrorKind>>::is_lock_free() {
            return Err(EngineError::from_detail(
                ErrorCode::Unsupported,
                "CPAL error exchange must be lock-free",
            ));
        }
        Ok(Self {
            failed: AtomicBool::new(false),
            pending: AtomicCell::new(None),
        })
    }

    fn record(&self, kind: cpal::ErrorKind) {
        if !matches!(
            kind,
            cpal::ErrorKind::Xrun
                | cpal::ErrorKind::DeviceChanged
                | cpal::ErrorKind::RealtimeDenied
        ) {
            self.failed.store(true, Ordering::Release);
        }
        self.pending.store(Some(kind));
    }
}

#[cfg(test)]
mod error_tests {
    use super::*;

    #[test]
    fn first_cpal_error_notifications_do_not_format_or_allocate() {
        use migo_alloc_probe::{Burst, assert_no_steady_state_allocation};
        let fleet: Vec<_> = (0..65).map(|_| StreamErrors::new().unwrap()).collect();
        assert_no_steady_state_allocation(
            Burst {
                path: "CPAL first error-kind exchange",
                warmup: 1,
                measured: 64,
            },
            |index| {
                for kind in [
                    cpal::ErrorKind::Xrun,
                    cpal::ErrorKind::DeviceChanged,
                    cpal::ErrorKind::RealtimeDenied,
                    cpal::ErrorKind::StreamInvalidated,
                ] {
                    fleet[index].record(kind);
                }
                fleet[index].pending.swap(None)
            },
        );
    }

    #[test]
    fn cpal_recoverable_notifications_keep_the_live_stream() {
        for kind in [
            cpal::ErrorKind::Xrun,
            cpal::ErrorKind::DeviceChanged,
            cpal::ErrorKind::RealtimeDenied,
        ] {
            let errors = StreamErrors::new().unwrap();
            errors.record(kind);
            assert!(
                !errors.failed.load(Ordering::Acquire),
                "{kind:?} must not reopen an active stream"
            );
            assert_eq!(errors.pending.swap(None), Some(kind));
            assert_eq!(errors.pending.swap(None), None);
        }
    }

    #[test]
    fn cpal_fatal_notifications_require_a_replacement_stream() {
        for kind in [
            cpal::ErrorKind::DeviceNotAvailable,
            cpal::ErrorKind::StreamInvalidated,
            cpal::ErrorKind::BackendError,
        ] {
            let errors = StreamErrors::new().unwrap();
            errors.record(kind);
            assert!(errors.failed.load(Ordering::Acquire));
            // A later recoverable event must never resurrect a dead stream.
            errors.record(cpal::ErrorKind::Xrun);
            assert!(errors.failed.load(Ordering::Acquire));
        }
    }
}
