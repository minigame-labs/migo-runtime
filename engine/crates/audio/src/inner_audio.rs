//! InnerAudioContext implementation - simple audio playback without WebAudio graph complexity.
//!
//! This is designed for simple media playback (background music, sound effects)
//! where the full WebAudio API is not needed.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use shared::audio_load::AudioLoadState;
pub(crate) use shared::audio_load::InnerAudioLoadToken;
use shared::error::{EngineError, EngineResult, ErrorCode};
use shared::protocol::audio_cmd::{InnerAudioEvent, InnerAudioEventType, InnerAudioId};
use tokio::sync::Notify;
use tokio::sync::mpsc::Receiver;

use crate::cache::CachedAudio;
use crate::decoder::DecodedAudio;
use crate::limits::{AudioAggregateLedger, AudioAggregatePermit};
use crate::stream_control::{self, Kind as ControlKind, PendingControls, Reply as ControlReply};
use crate::streaming::{STREAM_CHANNEL_CAPACITY, StreamMsg, StreamRevision, StreamingState};

// Use the shared rate's exact resolution for the cursor too. Converting through
// binary fixed point introduces an error on every output frame (e.g. at 1.1x).
const PLAYBACK_RATE_SCALE: u32 = 1000;
const NO_PRESENTATION_TIME: u64 = u64::MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PlaybackState {
    /// Initial state, no audio loaded or stopped
    Stopped = 0,
    /// Audio is playing
    Playing = 1,
    /// Audio is paused
    Paused = 2,
}

impl From<u8> for PlaybackState {
    fn from(v: u8) -> Self {
        match v {
            1 => PlaybackState::Playing,
            2 => PlaybackState::Paused,
            _ => PlaybackState::Stopped,
        }
    }
}

/// Shared state between InnerAudioPlayer and the mixer
/// All fields are atomic for lock-free access from audio callback
pub struct InnerAudioSharedState {
    /// Current playback state
    state: AtomicU32,
    /// Current playback position in sample frames (atomic u64 for precision)
    position_frames: AtomicU64,
    /// Total duration in sample frames
    duration_frames: AtomicU64,
    /// Zero means unknown; known source duration is independent of output rounding.
    source_duration_bits: AtomicU64,
    /// A cyclic media time is published as one atomic value, so readers never
    /// combine an absolute PCM cursor with half-updated loop clock fields.
    presentation_time_bits: AtomicU64,
    /// Volume (0.0 - 1.0, stored as u32 with fixed point: value * 1000)
    volume: AtomicU32,
    /// Playback rate (stored as u32 with fixed point: rate * 1000)
    playback_rate: AtomicU32,
    /// Loop enabled
    loop_enabled: AtomicBool,
    /// Sample rate of the audio
    sample_rate: AtomicU32,
    /// Number of channels
    channels: AtomicU32,
    /// Autoplay flag
    autoplay: AtomicBool,
    /// Whether audio data is loaded
    loaded: AtomicBool,
    /// Zero is retired; accepted loads use monotonically increasing generations.
    source_generation: AtomicU64,
    source_id: AtomicU32,
    source_changed: Notify,
}

impl InnerAudioSharedState {
    pub fn new() -> Self {
        Self {
            state: AtomicU32::new(PlaybackState::Stopped as u32),
            position_frames: AtomicU64::new(0),
            duration_frames: AtomicU64::new(0),
            source_duration_bits: AtomicU64::new(0),
            presentation_time_bits: AtomicU64::new(NO_PRESENTATION_TIME),
            volume: AtomicU32::new(1000),                       // 1.0
            playback_rate: AtomicU32::new(PLAYBACK_RATE_SCALE), // 1.0
            loop_enabled: AtomicBool::new(false),
            sample_rate: AtomicU32::new(0),
            channels: AtomicU32::new(0),
            autoplay: AtomicBool::new(false),
            loaded: AtomicBool::new(false),
            source_generation: AtomicU64::new(0),
            source_id: AtomicU32::new(0),
            source_changed: Notify::new(),
        }
    }

    #[inline]
    pub fn state(&self) -> PlaybackState {
        PlaybackState::from(self.state.load(Ordering::Acquire) as u8)
    }

    #[inline]
    pub fn set_state(&self, state: PlaybackState) {
        self.state.store(state as u32, Ordering::Release);
    }

    #[inline]
    pub fn position_frames(&self) -> u64 {
        self.position_frames.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn set_position_frames(&self, frames: u64) {
        self.position_frames.store(frames, Ordering::Relaxed);
    }

    #[inline]
    pub fn duration_frames(&self) -> u64 {
        self.duration_frames.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn set_duration_frames(&self, frames: u64) {
        self.duration_frames.store(frames, Ordering::Relaxed);
        self.source_duration_bits.store(0, Ordering::Relaxed);
    }

    fn set_source_duration(&self, timeline: crate::streaming::StreamTimeline) {
        self.duration_frames
            .store(timeline.output_frames(), Ordering::Relaxed);
        self.source_duration_bits
            .store(timeline.seconds().to_bits(), Ordering::Relaxed);
    }

    /// Get volume as f32 (0.0 - 1.0)
    #[inline]
    pub fn volume(&self) -> f32 {
        self.volume.load(Ordering::Relaxed) as f32 / 1000.0
    }

    /// Set volume (0.0 - 1.0)
    #[inline]
    pub fn set_volume(&self, vol: f32) {
        let clamped = vol.clamp(0.0, 1.0);
        self.volume
            .store((clamped * 1000.0) as u32, Ordering::Relaxed);
    }

    /// Get playback rate as f32
    #[inline]
    pub fn playback_rate(&self) -> f32 {
        self.playback_rate_units() as f32 / PLAYBACK_RATE_SCALE as f32
    }

    #[inline]
    fn playback_rate_units(&self) -> u32 {
        self.playback_rate.load(Ordering::Relaxed)
    }

    /// Set playback rate (0.5 - 2.0), rounded to the nearest thousandth.
    /// A NaN must not replace a valid rate with a zero-step, endless playback.
    #[inline]
    pub fn set_playback_rate(&self, rate: f32) {
        if rate.is_nan() {
            return;
        }
        let clamped = rate.clamp(0.5, 2.0);
        self.playback_rate.store(
            (clamped * PLAYBACK_RATE_SCALE as f32).round() as u32,
            Ordering::Relaxed,
        );
    }

    #[inline]
    pub fn loop_enabled(&self) -> bool {
        self.loop_enabled.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn set_loop_enabled(&self, enabled: bool) {
        self.loop_enabled.store(enabled, Ordering::Relaxed);
    }

    #[inline]
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn set_sample_rate(&self, rate: u32) {
        self.sample_rate.store(rate, Ordering::Relaxed);
    }

    #[inline]
    pub fn channels(&self) -> u32 {
        self.channels.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn set_channels(&self, ch: u32) {
        self.channels.store(ch, Ordering::Relaxed);
    }

    #[inline]
    pub fn autoplay(&self) -> bool {
        self.autoplay.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn set_autoplay(&self, enabled: bool) {
        self.autoplay.store(enabled, Ordering::Relaxed);
    }

    #[inline]
    pub fn is_loaded(&self) -> bool {
        self.loaded.load(Ordering::Acquire)
    }

    #[inline]
    pub fn set_loaded(&self, loaded: bool) {
        self.loaded.store(loaded, Ordering::Release);
    }

    /// Get current time in seconds
    pub fn current_time(&self) -> f64 {
        let presentation = self.presentation_time_bits.load(Ordering::Relaxed);
        if presentation != NO_PRESENTATION_TIME {
            return f64::from_bits(presentation);
        }
        let sr = self.sample_rate();
        if sr == 0 {
            return 0.0;
        }
        let time = self.position_frames() as f64 / sr as f64;
        let exact = self.source_duration_bits.load(Ordering::Relaxed);
        if exact != 0 {
            time.min(f64::from_bits(exact))
        } else {
            time
        }
    }

    /// Get duration in seconds
    pub fn duration(&self) -> f64 {
        let sr = self.sample_rate();
        if sr == 0 {
            return 0.0;
        }
        let exact = self.source_duration_bits.load(Ordering::Relaxed);
        if exact != 0 {
            f64::from_bits(exact)
        } else {
            self.duration_frames() as f64 / sr as f64
        }
    }

    pub(crate) fn owns_load(self: &Arc<Self>, load: &InnerAudioLoadToken) -> bool {
        load.matches(self)
    }

    pub(crate) fn begin_source(
        self: &Arc<Self>,
        source_id: u32,
    ) -> EngineResult<InnerAudioLoadToken> {
        let previous = self
            .source_generation
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |generation| {
                generation.checked_add(1)
            })
            .map_err(|_| {
                EngineError::from_detail(
                    ErrorCode::InputSaturated,
                    "InnerAudio source generation exhausted",
                )
            })?;
        self.source_id.store(source_id, Ordering::Release);
        self.source_changed.notify_waiters();
        Ok(InnerAudioLoadToken::new(self, previous + 1, source_id))
    }
}

impl AudioLoadState for InnerAudioSharedState {
    fn generation(&self) -> u64 {
        self.source_generation.load(Ordering::Acquire)
    }
    fn generation_changed(&self) -> &Notify {
        &self.source_changed
    }
}

impl Default for InnerAudioSharedState {
    fn default() -> Self {
        Self::new()
    }
}

/// Audio data source - either owned (streaming) or shared (cached)
enum AudioSource {
    /// Owned samples (used during streaming, can grow).
    Owned(Vec<f32>),
    /// Shared cached samples (immutable, from cache).
    Cached(Arc<CachedAudio>),
}
impl AudioSource {
    fn samples(&self) -> &[f32] {
        match self {
            AudioSource::Owned(v) => v,
            AudioSource::Cached(a) => &a.samples,
        }
    }

    fn len(&self) -> usize {
        match self {
            AudioSource::Owned(v) => v.len(),
            AudioSource::Cached(a) => a.samples.len(),
        }
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Reserve exactly the requested growth before copying decoder-owned PCM.
    ///
    /// `Vec`'s normal growth policy doubles capacity, so checking only length
    /// after `extend_from_slice` allowed a stream just below the logical limit
    /// to retain substantially more physical PCM. The reserve is performed
    /// before the copy and callers reject a capacity that would exceed the
    /// stream ceiling.
    fn try_reserve_for(&mut self, additional: usize) -> EngineResult<()> {
        let AudioSource::Owned(samples) = self else {
            return Ok(());
        };
        let required = samples.len().checked_add(additional).ok_or_else(|| {
            EngineError::from_detail(ErrorCode::InvalidArgument, "stream PCM length overflow")
        })?;
        if required as u64 > crate::limits::MAX_AUDIO_PCM_SAMPLES {
            return Err(EngineError::from_detail(
                ErrorCode::InputSaturated,
                "stream PCM exceeds the decoded window",
            ));
        }
        if required > samples.capacity() {
            samples
                .try_reserve_exact(required - samples.len())
                .map_err(|_| {
                    EngineError::from_detail(ErrorCode::OutOfMemory, "stream PCM allocation failed")
                })?;
        }
        let capacity_bytes = samples
            .capacity()
            .checked_mul(std::mem::size_of::<f32>())
            .ok_or_else(|| {
                EngineError::from_detail(ErrorCode::InvalidArgument, "stream PCM capacity overflow")
            })?;
        if capacity_bytes as u64 > crate::limits::MAX_AUDIO_PCM_BYTES {
            return Err(EngineError::from_detail(
                ErrorCode::InputSaturated,
                "stream PCM capacity exceeds the decoded window",
            ));
        }
        Ok(())
    }
    #[cfg(test)]
    fn capacity_bytes(&self) -> usize {
        match self {
            AudioSource::Owned(samples) => samples.capacity() * std::mem::size_of::<f32>(),
            AudioSource::Cached(audio) => audio.samples.capacity() * std::mem::size_of::<f32>(),
        }
    }

    /// Append to owned samples (only works for Owned variant).
    fn extend_from_slice(&mut self, samples: &[f32]) {
        if let AudioSource::Owned(v) = self {
            v.extend_from_slice(samples);
        }
    }
}

/// Linearly interpolate channel `ch` between two interleaved source frames.
///
/// A free function rather than a method so it can be called while `samples` is
/// borrowed out of `self.source` on a path that also touches `&mut self`, and so
/// the interpolation rule has exactly one definition across the three channel
/// mappings below.
#[inline]
fn lerp_frame(samples: &[f32], base0: usize, base1: usize, ch: usize, frac: f32) -> f32 {
    let a = samples[base0 + ch];
    let b = samples[base1 + ch];
    a + (b - a) * frac
}

#[derive(Default)]
struct LoadingControls {
    playback: Option<PlaybackState>,
    seek: Option<f64>,
}

/// Audio-thread owner of one full-load or streaming source.
pub struct InnerAudioPlayer {
    /// Unique ID for this player.
    pub id: InnerAudioId,
    /// Shared state for communication with JS.
    pub shared: Arc<InnerAudioSharedState>,
    source: AudioSource,
    /// Host callback IDs are monotonic. Keep their high-water mark even when a
    /// legacy untagged load runs, so delayed preparation cannot reverse src order.
    last_source_id: u32,
    position: usize,
    /// Fractional source frame in thousandths, always below PLAYBACK_RATE_SCALE.
    position_phase: u16,
    /// Completed buffered traversals distinguish real past from zero padding.
    buffered_loops: usize,
    output_channels: u32,
    pending_events: Vec<InnerAudioEvent>,
    stream_rx: Option<Receiver<StreamMsg>>,
    file_load: Option<crate::stream_open::Load>,
    stream_state: Option<Arc<StreamingState>>,
    stream_revision: StreamRevision,
    stream_complete: bool,
    stream_base_frames: u64,
    /// Immutable presentation metadata remains with PCM after transport EOF.
    stream_timeline: Option<crate::streaming::StreamTimeline>,
    stream_presentation: Option<crate::streaming::StreamPresentation>,
    stream_windowed: bool,
    stream_canplay_sent: bool,
    /// Audio-thread-owned target beyond the decoded prefix. No mixer mailbox.
    pending_seek: Option<u64>,
    stream_controls: PendingControls,
    loop_intent: Option<bool>,
    /// Owned by the current load generation, never the mixer or a later src.
    loading_controls: Option<LoadingControls>,
    /// Persistent configuration, applied only to a fresh playback, never resume.
    start_time: f64,
    start_pending: bool,
    replay_needed: bool,
    waiting_notified: bool,
    frames_since_time_update: u64,
    loading_url: Option<String>,
    aggregate: Arc<AudioAggregateLedger>,
    stream_permit: Option<AudioAggregatePermit>,
    rate_converter: Option<crate::variable_rate::Converter>,
}
impl InnerAudioPlayer {
    pub fn new(id: InnerAudioId, output_channels: u32) -> Self {
        Self::new_with_aggregate(id, output_channels, AudioAggregateLedger::process_global())
    }

    pub(crate) fn new_with_aggregate(
        id: InnerAudioId,
        output_channels: u32,
        aggregate: Arc<AudioAggregateLedger>,
    ) -> Self {
        Self {
            id,
            shared: Arc::new(InnerAudioSharedState::new()),
            source: AudioSource::Owned(Vec::new()),
            last_source_id: 0,
            position: 0,
            position_phase: 0,
            buffered_loops: 0,
            output_channels,
            pending_events: Vec::with_capacity(4),
            stream_rx: None,
            file_load: None,
            stream_state: None,
            stream_revision: StreamRevision::INITIAL,
            stream_complete: false,
            stream_base_frames: 0,
            stream_timeline: None,
            stream_presentation: None,
            stream_windowed: false,
            stream_canplay_sent: false,
            pending_seek: None,
            stream_controls: PendingControls::new(),
            loop_intent: None,
            loading_controls: None,
            start_time: 0.0,
            start_pending: true,
            replay_needed: false,
            waiting_notified: false,
            frames_since_time_update: 0,
            loading_url: None,
            aggregate,
            stream_permit: None,
            rate_converter: None,
        }
    }

    fn prepare_rate_converter(
        &self,
        channels: usize,
        empty: bool,
    ) -> EngineResult<Option<crate::variable_rate::Converter>> {
        if empty || self.shared.playback_rate_units() == PLAYBACK_RATE_SCALE {
            return Ok(None);
        }
        crate::variable_rate::Converter::new(channels).map(Some)
    }

    pub(crate) fn set_playback_rate(&mut self, rate: f32) -> EngineResult<()> {
        if rate.is_nan() {
            return Ok(());
        }
        let units = (rate.clamp(0.5, 2.0) * PLAYBACK_RATE_SCALE as f32).round() as u32;
        if units == PLAYBACK_RATE_SCALE {
            self.rate_converter = None;
        } else if self.rate_converter.is_none()
            && self.shared.channels() != 0
            && (self.shared.is_loaded() || self.is_streaming())
        {
            let channels = self.shared.channels() as usize;
            let mut converter = crate::variable_rate::Converter::new(channels)?;
            if !self.stream_complete {
                converter.reset_stream(self.position / channels, self.position_phase)?;
            } else {
                converter.reset(
                    self.source.samples(),
                    self.position / channels,
                    self.position_phase,
                    self.shared.loop_enabled(),
                    self.buffered_loops
                        .saturating_mul(self.source.len() / channels)
                        .saturating_add(self.position / channels),
                )?;
            }
            self.rate_converter = Some(converter);
        }
        self.shared.set_playback_rate(rate);
        Ok(())
    }

    fn reset_rate_converter(&mut self) {
        let streaming = !self.stream_complete;
        if let Some(converter) = &mut self.rate_converter {
            let channels = self.shared.channels() as usize;
            let result = if streaming {
                converter.reset_stream(self.position / channels, self.position_phase)
            } else {
                converter.reset(
                    self.source.samples(),
                    self.position / channels,
                    self.position_phase,
                    self.shared.loop_enabled(),
                    self.buffered_loops
                        .saturating_mul(self.source.len() / channels)
                        .saturating_add(self.position / channels),
                )
            };
            if let Err(error) = result {
                tracing::error!("InnerAudioPlayer {} rate reset failed: {error}", self.id);
                self.shared.set_state(PlaybackState::Stopped);
                self.push_event(InnerAudioEventType::Error);
            }
        }
    }
    /// Change source storage and its existing physical permit as one owner.
    fn replace_source(&mut self, source: AudioSource, permit: Option<AudioAggregatePermit>) {
        self.rate_converter = None;
        self.buffered_loops = 0;
        // Release the old PCM before its aggregate permit.
        drop(std::mem::replace(&mut self.source, source));
        drop(std::mem::replace(&mut self.stream_permit, permit));
    }
    /// The raw cursor remains absolute for trimming, interpolation and
    /// monotonic progress. Only the public media time wraps, before conversion
    /// to f64 and without accumulating a rounded duration per cycle.
    fn publish_position(&self, frame: u64) {
        if let Some(presentation) = self.stream_presentation.filter(|p| p.is_cyclic()) {
            self.shared
                .presentation_time_bits
                .store(presentation.seconds_at(frame).to_bits(), Ordering::Relaxed);
            self.shared.set_position_frames(frame);
        } else {
            self.shared.set_position_frames(frame);
            self.shared
                .presentation_time_bits
                .store(NO_PRESENTATION_TIME, Ordering::Relaxed);
        }
    }

    /// Helper to push an event with current time
    #[inline]
    fn push_event(&mut self, event_type: InnerAudioEventType) {
        self.pending_events.push(InnerAudioEvent {
            id: self.id,
            source_id: self.shared.source_id.load(Ordering::Acquire),
            duration: self.shared.duration(),
            buffered: self.shared.is_loaded(),
            paused: self.shared.state() != PlaybackState::Playing,
            event_type,
            current_time: self.shared.current_time(),
        });
    }

    /// Accumulate rendered output frames and emit a throttled `TimeUpdate`
    /// (~4/sec). Emits at most one event per call and reduces the accumulator
    /// modulo the interval, so a block spanning several intervals fires once and
    /// leaves no backlog (which would over-fire on the next small block). For
    /// normal blocks smaller than the interval this is identical to subtracting
    /// it, keeping the long-term cadence ~4/sec regardless of block size.
    /// Callable on both the normal and early-return (stall) paths.
    #[inline]
    fn accumulate_time_update(&mut self, frames_rendered: usize) {
        let sr = self.shared.sample_rate() as u64;
        if sr == 0 {
            return;
        }
        let interval = (sr / 4).max(1);
        self.frames_since_time_update += frames_rendered as u64;
        if self.frames_since_time_update >= interval {
            self.frames_since_time_update %= interval;
            self.push_event(InnerAudioEventType::TimeUpdate);
        }
    }

    /// Load decoded audio data (full load mode, owned).
    #[cfg(test)]
    pub fn load_audio(&mut self, audio: DecodedAudio) -> EngineResult<()> {
        let load = self.begin_load()?;
        self.complete_load(load, audio)
    }

    /// Retire a source before any asynchronous preparation starts, including a
    /// preparation that later fails. Never replay old PCM, seeks or events while
    /// a new load is pending. Release storage before returning its byte permit.
    pub(crate) fn begin_load(&mut self) -> EngineResult<InnerAudioLoadToken> {
        self.begin_load_for(0)
    }

    pub(crate) fn begin_load_for(&mut self, source_id: u32) -> EngineResult<InnerAudioLoadToken> {
        if source_id != 0 && source_id <= self.last_source_id {
            return Err(InnerAudioLoadToken::cancelled_error());
        }
        let load = self.shared.begin_source(source_id)?;
        if source_id != 0 {
            self.last_source_id = source_id;
        }
        self.cancel_stream();
        self.loop_intent = None;
        self.loading_controls = Some(LoadingControls::default());
        self.start_pending = true;
        self.stream_revision = StreamRevision::INITIAL;
        self.replace_source(AudioSource::Owned(Vec::new()), None);
        self.position = 0;
        self.position_phase = 0;
        self.frames_since_time_update = 0;
        self.stream_presentation = None;
        self.publish_position(0);
        self.shared.set_duration_frames(0);
        self.shared.set_sample_rate(0);
        self.shared.set_channels(0);
        self.shared.set_loaded(false);
        self.shared.set_state(PlaybackState::Stopped);
        self.stream_complete = false;
        self.stream_base_frames = 0;
        self.stream_timeline = None;
        self.stream_presentation = None;
        self.stream_windowed = false;
        self.stream_canplay_sent = false;
        self.pending_seek = None;
        self.waiting_notified = false;
        self.loading_url = None;
        self.replay_needed = false;
        self.pending_events.clear();
        Ok(load)
    }

    #[cfg(test)]
    pub(crate) fn complete_load(
        &mut self,
        load: InnerAudioLoadToken,
        mut audio: DecodedAudio,
    ) -> EngineResult<()> {
        self.try_complete_load(&load, &mut audio)
    }
    pub(crate) fn try_complete_load(
        &mut self,
        load: &InnerAudioLoadToken,
        audio: &mut DecodedAudio,
    ) -> EngineResult<()> {
        if !self.shared.owns_load(load) || self.shared.is_loaded() {
            return Err(InnerAudioLoadToken::cancelled_error());
        }
        let converter =
            self.prepare_rate_converter(audio.channels as usize, audio.samples.is_empty())?;
        let permit = match crate::limits::pcm_bytes(audio.samples.capacity())
            .and_then(|bytes| self.aggregate.try_reserve(bytes, "InnerAudio"))
        {
            Ok(permit) => permit,
            Err(error) => {
                self.discard_loading_controls();
                return Err(error);
            }
        };
        let frame_count = audio.frame_count();
        self.shared.set_sample_rate(audio.sample_rate);
        self.shared.set_channels(audio.channels);
        self.shared.set_duration_frames(frame_count as u64);
        self.stream_timeline = None;
        self.stream_presentation = None;
        self.publish_position(0);
        self.replace_source(
            AudioSource::Owned(std::mem::take(&mut audio.samples)),
            Some(permit),
        );
        self.rate_converter = converter;
        self.position = 0;
        self.position_phase = 0;
        self.frames_since_time_update = 0;
        self.stream_complete = true;
        self.stream_base_frames = 0;
        self.stream_windowed = false;
        self.loading_url = None;
        self.shared.set_loaded(true);
        self.push_event(InnerAudioEventType::CanPlay);

        self.apply_loading_controls();
        Ok(())
    }

    /// Load cached audio data (shared reference, no copy).
    pub(crate) fn complete_cached_load(
        &mut self,
        load: InnerAudioLoadToken,
        audio: Arc<CachedAudio>,
    ) -> EngineResult<()> {
        if !self.shared.owns_load(&load) || self.shared.is_loaded() {
            return Err(InnerAudioLoadToken::cancelled_error());
        }
        let converter =
            self.prepare_rate_converter(audio.channels as usize, audio.samples.is_empty())?;
        let frame_count = audio.frame_count();
        self.shared.set_sample_rate(audio.sample_rate);
        self.shared.set_channels(audio.channels);
        self.shared.set_duration_frames(frame_count as u64);
        self.stream_timeline = audio.source_timeline();
        self.stream_presentation = None;
        if let Some(timeline) = self.stream_timeline {
            self.shared.set_source_duration(timeline);
        }
        self.publish_position(0);
        self.replace_source(AudioSource::Cached(audio), None);
        self.rate_converter = converter;
        self.position = 0;
        self.position_phase = 0;
        self.frames_since_time_update = 0;
        self.stream_complete = true;
        self.stream_base_frames = 0;
        self.stream_windowed = false;
        self.loading_url = None;
        self.shared.set_loaded(true);
        self.push_event(InnerAudioEventType::CanPlay);

        self.apply_loading_controls();
        Ok(())
    }

    /// Start streaming playback from URL.
    #[cfg(test)]
    pub fn start_streaming(
        &mut self,
        url: String,
        rx: Receiver<StreamMsg>,
        state: Arc<StreamingState>,
    ) -> EngineResult<()> {
        let load = self.begin_load().inspect_err(|_| state.cancel())?;
        self.complete_stream_load(load, url, rx, state)
    }

    pub(crate) fn complete_stream_load(
        &mut self,
        load: InnerAudioLoadToken,
        url: String,
        rx: Receiver<StreamMsg>,
        state: Arc<StreamingState>,
    ) -> EngineResult<()> {
        if !self.shared.owns_load(&load) || self.shared.is_loaded() {
            state.cancel();
            return Err(InnerAudioLoadToken::cancelled_error());
        }
        const ESTIMATED_CAPACITY: usize = 44_100 * 2 * 5;
        let initial_bytes = crate::limits::pcm_bytes(ESTIMATED_CAPACITY)?;
        let permit = self
            .aggregate
            .try_reserve(initial_bytes, "InnerAudio")
            .inspect_err(|_| {
                state.cancel();
                self.discard_loading_controls();
            })?;
        self.replace_source(
            AudioSource::Owned(Vec::with_capacity(ESTIMATED_CAPACITY)),
            Some(permit),
        );
        self.position = 0;
        self.position_phase = 0;
        self.frames_since_time_update = 0;
        self.shared.set_duration_frames(0);
        self.shared.set_sample_rate(0);
        self.shared.set_channels(0);
        self.shared.set_loaded(false);
        self.stream_complete = false;
        self.stream_base_frames = 0;
        self.stream_windowed = false;
        self.stream_canplay_sent = false;
        self.loading_url = Some(url);
        self.stream_rx = Some(rx);
        self.stream_state = Some(state);
        self.loop_intent = self
            .stream_state
            .as_ref()
            .filter(|s| s.accepts_controls())
            .map(|_| self.shared.loop_enabled());
        self.stream_timeline = None;
        self.stream_presentation = None;
        self.stream_revision = StreamRevision::INITIAL;
        tracing::debug!("InnerAudioPlayer {} started streaming", self.id);
        Ok(())
    }
    pub(crate) fn complete_file_stream_load(
        &mut self,
        load: InnerAudioLoadToken,
        rx: Receiver<StreamMsg>,
        state: Arc<StreamingState>,
        pending: crate::stream_open::Load,
    ) -> bool {
        match self.complete_stream_load(load, String::new(), rx, state) {
            Ok(()) => {
                self.loading_url = None;
                self.file_load = Some(pending);
                true
            }
            Err(error) => {
                let _ = pending.reply.send(Err(error));
                false
            }
        }
    }
    fn complete_file_canplay(&mut self) {
        if let Some(load) = self.file_load.take() {
            let _ = load.reply.send(Ok(()));
        }
    }

    const STREAM_WINDOW_SAMPLES: usize = 44_100 * 2 * 5;

    /// Reclaim consumed stream prefix without allocating.
    ///
    /// A streaming track is fed at roughly playback rate. Keeping a bounded
    /// look-back window lets the receiver reuse its existing allocation instead
    /// of retaining the complete decoded track. Completed streams are never
    /// trimmed here: their full backing may still be handed to the cache.
    fn trim_consumed_stream_prefix(&mut self) {
        if self.stream_complete || self.position <= Self::STREAM_WINDOW_SAMPLES {
            return;
        }
        let channels = self.shared.channels() as usize;
        if channels == 0 {
            return;
        }
        let trim = ((self.position - Self::STREAM_WINDOW_SAMPLES) / channels) * channels;
        if trim == 0 {
            return;
        }
        if let AudioSource::Owned(samples) = &mut self.source {
            samples.drain(..trim);
            self.position -= trim;
            self.stream_base_frames = self
                .stream_base_frames
                .saturating_add((trim / channels) as u64);
            self.stream_windowed = true;
            if let Some(converter) = &mut self.rate_converter {
                converter.trim_stream_prefix(trim / channels);
            }
        }
    }

    /// Get the URL currently being loaded (for cache key)
    pub fn loading_url(&self) -> Option<&str> {
        self.loading_url.as_deref()
    }

    /// Take ownership of streamed samples and their physical ownership permit.
    #[cfg(test)]
    pub fn take_streamed_audio(&mut self) -> Option<(DecodedAudio, AudioAggregatePermit)> {
        // The legacy PCM-only transfer cannot represent a source clock.
        if self.stream_timeline.is_some() {
            return None;
        }
        self.take_streamed_audio_with_source()
            .map(|(audio, permit, _)| (audio, permit))
    }

    pub(crate) fn take_streamed_audio_with_source(
        &mut self,
    ) -> Option<(
        DecodedAudio,
        AudioAggregatePermit,
        Option<crate::streaming::StreamTimeline>,
    )> {
        if !self.stream_complete
            || self.stream_windowed
            || self.retains_stream_source()
            || self.stream_presentation.is_some_and(|p| p.is_cyclic())
        {
            return None;
        }
        let permit = self.stream_permit.take()?;
        let samples = match std::mem::replace(&mut self.source, AudioSource::Owned(Vec::new())) {
            AudioSource::Owned(samples) => samples,
            AudioSource::Cached(_) => return None,
        };
        if samples.is_empty() {
            return None;
        }
        Some((
            DecodedAudio {
                samples,
                sample_rate: self.shared.sample_rate(),
                channels: self.shared.channels(),
            },
            permit,
            self.stream_timeline,
        ))
    }

    /// Replace a completed stream's owned backing with its cached Arc.
    pub fn attach_cached_backing(&mut self, audio: Arc<CachedAudio>) {
        let converter = self.rate_converter.take();
        self.stream_timeline = audio.source_timeline();
        self.stream_presentation = None;
        self.replace_source(AudioSource::Cached(audio.clone()), None);
        self.rate_converter = converter;
        self.shared.set_duration_frames(audio.frame_count() as u64);
        if let Some(timeline) = self.stream_timeline {
            self.shared.set_source_duration(timeline);
        }
        self.stream_complete = true;
        self.stream_base_frames = 0;
        self.stream_windowed = false;
        self.loading_url = None;
    }

    /// Cancel ongoing stream
    fn cancel_stream(&mut self) {
        if let Some(load) = self.file_load.take() {
            let _ = load.reply.send(Err(InnerAudioLoadToken::cancelled_error()));
        }
        self.stream_controls.clear();
        if let Some(state) = self.stream_state.take() {
            state.cancel();
        }
        self.stream_rx = None;
    }

    pub(crate) fn retains_stream_source(&self) -> bool {
        self.stream_state
            .as_ref()
            .is_some_and(|state| state.retains_source())
    }

    /// The retained PCM's absolute end is independent of the track duration.
    fn stream_decoded_end(&self) -> u64 {
        self.stream_base_frames.saturating_add(
            self.source
                .len()
                .checked_div(self.shared.channels() as usize)
                .unwrap_or(0) as u64,
        )
    }

    fn update_stream_duration(&mut self) -> EngineResult<()> {
        let frames = self
            .source
            .len()
            .checked_div(self.shared.channels() as usize)
            .unwrap_or(0) as u64;
        let end = self.stream_base_frames.checked_add(frames).ok_or_else(|| {
            EngineError::from_detail(ErrorCode::IoError, "stream PCM cursor overflow")
        })?;
        let presentation = self
            .stream_presentation
            .or_else(|| self.stream_state.as_ref().and_then(|s| s.presentation()));
        if let Some(timeline) = self
            .stream_state
            .as_ref()
            .and_then(|s| s.timeline())
            .or(self.stream_timeline)
        {
            if timeline.format() != (self.shared.sample_rate(), self.shared.channels())
                || presentation
                    .map_or(Some(timeline.output_frames()), |p| p.output_end())
                    .is_some_and(|limit| end > limit)
            {
                return Err(EngineError::from_detail(
                    ErrorCode::IoError,
                    "PCM exceeds source timeline or changes its format",
                ));
            }
            self.shared.set_source_duration(timeline);
            self.stream_timeline = Some(timeline);
            self.stream_presentation = presentation;
            self.publish_position(self.shared.position_frames());
        } else {
            self.shared.set_duration_frames(end);
        }
        Ok(())
    }

    /// Commit a source-accepted reposition on the audio thread. The caller has
    /// already validated the target and mapped it to output frames. Merely
    /// requesting a seek must not clear PCM or admit another revision.
    #[cfg(test)]
    pub(crate) fn accept_stream_revision(
        &mut self,
        source: &Arc<StreamingState>,
        revision: StreamRevision,
        target_frame: u64,
    ) -> EngineResult<()> {
        self.accept_stream_revision_as(source, revision, target_frame, true, None, true)
    }

    fn accept_stream_revision_as(
        &mut self,
        source: &Arc<StreamingState>,
        revision: StreamRevision,
        target_frame: u64,
        notify_seek: bool,
        traversal: Option<crate::streaming::StreamTraversal>,
        expose_position: bool,
    ) -> EngineResult<()> {
        if revision <= self.stream_revision
            || self
                .stream_state
                .as_ref()
                .is_none_or(|current| !Arc::ptr_eq(current, source))
            || !self.retains_stream_source()
            || self.stream_state.as_ref().is_none_or(|s| s.is_cancelled())
            || self.stream_rx.as_ref().is_none_or(|rx| rx.is_closed())
            || !self.shared.is_loaded()
            || self.shared.sample_rate() == 0
            || self.shared.channels() == 0
            || !matches!(self.source, AudioSource::Owned(_))
            || traversal.is_some_and(|p| {
                Some(p.presentation().timeline()) != source.timeline()
                    || p.output_start() != target_frame
            })
            || source.timeline().is_some_and(|t| {
                t.format() != (self.shared.sample_rate(), self.shared.channels())
                    || traversal
                        .map_or(Some(t.output_frames()), |p| p.presentation().output_end())
                        .is_some_and(|end| target_frame > end)
            })
        {
            return Err(EngineError::from_detail(
                ErrorCode::InvalidArgument,
                "inactive or nonadvancing stream revision",
            ));
        }
        if notify_seek {
            self.push_event(InnerAudioEventType::Seeking);
        }
        if let AudioSource::Owned(samples) = &mut self.source {
            samples.clear();
        }
        self.stream_revision = revision;
        self.stream_base_frames = target_frame;
        self.stream_windowed = true;
        self.stream_complete = false;
        self.position = 0;
        self.position_phase = 0;
        self.pending_seek = notify_seek.then_some(target_frame);
        self.replay_needed = false;
        self.waiting_notified = false;
        self.frames_since_time_update = 0;
        if let Some(traversal) = traversal {
            self.stream_presentation = Some(traversal.presentation());
            self.shared
                .set_loop_enabled(traversal.presentation().output_end().is_none());
        }
        // A queued stop already exposed zero. Keep it zero on the first
        // atomic store too: a reader can run before the outer poll resumes.
        self.publish_position(if expose_position { target_frame } else { 0 });
        if let Some(timeline) = source.timeline() {
            self.shared.set_source_duration(timeline);
            self.stream_timeline = Some(timeline);
        } else {
            self.shared.set_duration_frames(target_frame);
        }
        self.reset_rate_converter();
        Ok(())
    }

    fn fail_stream(&mut self) {
        let file_load = self.file_load.take();
        let error = file_load.as_ref().map(|load| {
            load.error.lock().unwrap().take().unwrap_or_else(|| {
                EngineError::from_detail(
                    ErrorCode::Internal,
                    "local audio stream preparation failed",
                )
            })
        });
        self.cancel_stream();
        self.discard_loading_controls();
        self.pending_seek = None;
        // Release physical storage before returning its aggregate permit.
        self.replace_source(AudioSource::Owned(Vec::new()), None);
        self.loading_url = None;
        self.stream_complete = false;
        self.shared.set_loaded(false);
        self.shared.set_state(PlaybackState::Stopped);
        if let Some(load) = file_load {
            let _ = load.reply.send(Err(error.unwrap()));
        } else {
            self.push_event(InnerAudioEventType::Error);
        }
    }

    /// Preserve producer backpressure after startup, including for a paused
    /// player. One second of media ahead, plus at most the last admitted chunk,
    /// is independent of track duration and the retained seek-back window.
    fn stream_prefetch_full(&self) -> bool {
        if !self.stream_canplay_sent {
            return false;
        }
        let samples =
            (self.shared.sample_rate() as usize).saturating_mul(self.shared.channels() as usize);
        samples != 0 && self.source.len().saturating_sub(self.position) >= samples
    }

    #[cfg(test)]
    pub(crate) fn queued_stream_messages(&self) -> usize {
        self.stream_rx.as_ref().map_or(0, Receiver::len)
    }

    #[cfg(test)]
    pub(crate) fn retained_stream_start(&self) -> u64 {
        self.stream_base_frames
    }

    /// Process incoming stream messages (call from audio thread loop)
    pub fn poll_stream(&mut self) {
        if self
            .file_load
            .as_ref()
            .is_some_and(|load| load.reply.is_closed())
        {
            self.cancel_stream();
            self.discard_loading_controls();
            return;
        }
        self.poll_stream_controls();
        if !self.stream_controls.is_empty() {
            return;
        }
        if self.stream_complete {
            return;
        }
        // Reclaim consumed data before appending the next decoder loan. This
        // keeps the retained window bounded while preserving a contiguous slice
        // for interpolation.
        self.trim_consumed_stream_prefix();
        // Take the receiver out to split the borrow on self,
        // allowing inline processing without a temporary Vec.
        let mut rx = match self.stream_rx.take() {
            Some(rx) => rx,
            None => return,
        };

        let mut stream_ended = false;

        // A producer may refill the queue concurrently while a seek discards
        // PCM. Bound each pass even when no forward prefetch is retained.
        for _ in 0..STREAM_CHANNEL_CAPACITY {
            if self.stream_prefetch_full() {
                break;
            }
            let msg = match rx.try_recv() {
                Ok(msg) => msg,
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    // Every producer ends a traversal with Done or Error before
                    // it exits; a channel closed with neither (a panicked or
                    // lost producer) can never complete this stream.
                    tracing::error!("InnerAudioPlayer {} stream producer vanished", self.id);
                    self.fail_stream();
                    stream_ended = true;
                    break;
                }
            };
            if msg.revision() != self.stream_revision {
                continue;
            }
            match msg {
                StreamMsg::Ready {
                    sample_rate,
                    channels,
                    ..
                } => {
                    self.shared.set_sample_rate(sample_rate);
                    self.shared.set_channels(channels);
                    if self.update_stream_duration().is_err() {
                        self.fail_stream();
                        stream_ended = true;
                        break;
                    }
                    if self.rate_converter.is_none() {
                        match self.prepare_rate_converter(channels as usize, false) {
                            Ok(converter) => self.rate_converter = converter,
                            Err(_) => {
                                self.fail_stream();
                                stream_ended = true;
                                break;
                            }
                        }
                    }
                    tracing::debug!(
                        "InnerAudioPlayer {} stream ready: {} Hz, {} channels",
                        self.id,
                        sample_rate,
                        channels
                    );
                }
                StreamMsg::Samples {
                    pcm: new_samples, ..
                } => {
                    let samples = match self.seek_stream_prefix(&new_samples) {
                        Ok(samples) => samples,
                        Err(error) => {
                            tracing::error!(
                                "InnerAudioPlayer {} invalid seek PCM: {}",
                                self.id,
                                error
                            );
                            self.fail_stream();
                            stream_ended = true;
                            break;
                        }
                    };
                    let required_samples =
                        self.source.len().checked_add(samples.len()).ok_or_else(|| {
                            EngineError::from_detail(
                                ErrorCode::InvalidArgument,
                                "stream PCM length overflow",
                            )
                        });
                    let required_bytes = required_samples.and_then(crate::limits::pcm_bytes);
                    let admit = match required_bytes {
                        Ok(bytes) => match &mut self.stream_permit {
                            Some(permit) => permit.try_grow_to(bytes, "InnerAudio"),
                            None => self
                                .aggregate
                                .try_reserve(bytes, "InnerAudio")
                                .map(|permit| {
                                    self.stream_permit = Some(permit);
                                }),
                        },
                        Err(error) => Err(error),
                    };
                    if let Err(error) = admit {
                        tracing::error!(
                            "InnerAudioPlayer {} cannot grow streaming PCM: {}",
                            self.id,
                            error
                        );
                        self.fail_stream();
                        stream_ended = true;
                        break;
                    }
                    if let Err(error) = self.source.try_reserve_for(samples.len()) {
                        tracing::error!(
                            "InnerAudioPlayer {} cannot grow streaming PCM: {}",
                            self.id,
                            error
                        );
                        self.fail_stream();
                        stream_ended = true;
                        break;
                    }
                    self.source.extend_from_slice(samples);
                    // Dropped at the end of this arm, which returns the buffer to
                    // the decoder that lent it.
                    drop(new_samples);
                    // Fresh data arrived: re-arm the stall notification so a
                    // subsequent underrun emits `Waiting` again.
                    if self.pending_seek.is_none() {
                        self.waiting_notified = false;
                    }

                    if self.update_stream_duration().is_err() {
                        self.fail_stream();
                        stream_ended = true;
                        break;
                    }
                    if self
                        .pending_seek
                        .is_some_and(|target| target < self.stream_decoded_end())
                    {
                        self.pending_seek = None;
                        self.waiting_notified = false;
                        self.push_event(InnerAudioEventType::Seeked);
                    }

                    // Send canplay once we have enough data (~0.5 seconds)
                    if !self.stream_canplay_sent {
                        let sr = self.shared.sample_rate();
                        let ch = self.shared.channels();
                        if sr > 0 && ch > 0 {
                            let buffered_frames = self.source.len() / ch as usize;
                            let buffered_seconds = buffered_frames as f64 / sr as f64;

                            tracing::trace!(
                                "InnerAudioPlayer {} buffered: {:.2}s ({} frames, {} samples)",
                                self.id,
                                buffered_seconds,
                                buffered_frames,
                                self.source.len()
                            );

                            // Can play once we have 0.5 seconds buffered
                            if buffered_seconds >= 0.5 {
                                self.stream_canplay_sent = true;
                                self.shared.set_loaded(true);
                                tracing::debug!(
                                    "InnerAudioPlayer {} canplay: {:.2}s buffered",
                                    self.id,
                                    buffered_seconds
                                );
                                self.push_event(InnerAudioEventType::CanPlay);
                                self.complete_file_canplay();
                            }
                        }
                    }
                }
                StreamMsg::Done { .. } => {
                    if self
                        .stream_state
                        .as_ref()
                        .and_then(|s| s.timeline())
                        .is_some_and(|t| {
                            self.stream_presentation
                                .map_or(Some(t.output_frames()), |p| p.output_end())
                                != Some(self.stream_decoded_end())
                        })
                    {
                        self.fail_stream();
                        stream_ended = true;
                        break;
                    }
                    self.stream_complete = true;
                    if !self.retains_stream_source() {
                        self.stream_state = None;
                        stream_ended = true;
                    }
                    if self.pending_seek.take().is_some() {
                        // Unknown duration becomes authoritative only at EOF.
                        self.publish_position(
                            self.stream_presentation
                                .and_then(|p| p.output_end())
                                .unwrap_or(self.shared.duration_frames()),
                        );
                        self.push_event(InnerAudioEventType::Seeked);
                    }
                    let ch = self.shared.channels() as usize;
                    let sr = self.shared.sample_rate();
                    let duration = if ch > 0 && sr > 0 {
                        (self.source.len() / ch) as f64 / sr as f64
                    } else {
                        0.0
                    };
                    tracing::debug!(
                        "InnerAudioPlayer {} stream complete: {} samples, {:.2}s duration",
                        self.id,
                        self.source.len(),
                        duration
                    );

                    // If we haven't sent canplay yet (very short audio), send it now
                    if !self.stream_canplay_sent && !self.source.is_empty() {
                        self.stream_canplay_sent = true;
                        self.shared.set_loaded(true);
                        tracing::debug!("InnerAudioPlayer {} canplay (short audio)", self.id);
                        self.push_event(InnerAudioEventType::CanPlay);
                        self.complete_file_canplay();
                    }
                    if self.file_load.is_some() {
                        if let Some(load) = &self.file_load {
                            *load.error.lock().unwrap() = Some(EngineError::from_detail(
                                ErrorCode::InvalidArgument,
                                "local audio contains no samples",
                            ));
                        }
                        self.fail_stream();
                    }
                    break; // No more messages after Done
                }
                StreamMsg::Error { message: err, .. } => {
                    tracing::error!("InnerAudioPlayer {} stream error: {}", self.id, err);
                    self.fail_stream();
                    stream_ended = true;
                    break; // No more messages after Error
                }
            }
        }

        // Put receiver back unless the stream has ended
        if !stream_ended {
            self.stream_rx = Some(rx);
        }
        if self.shared.is_loaded()
            && let Some(enabled) = self.loop_intent.take()
        {
            self.set_loop_enabled(enabled);
        }
        if self.stream_canplay_sent && self.shared.is_loaded() {
            self.apply_loading_controls();
        }
    }

    /// Retire only this preparation's commands. A rejected old completion must
    /// never call this for the replacement generation.
    pub(crate) fn discard_loading_controls(&mut self) {
        self.loading_controls = None;
    }

    fn apply_loading_controls(&mut self) {
        let Some(controls) = self.loading_controls.take() else {
            return;
        };
        if let Some(target) = controls.seek {
            self.seek(target);
        }
        // A closed source port or invalid preparation can fail during seek.
        if !self.shared.is_loaded() {
            return;
        }
        match controls.playback.unwrap_or_else(|| {
            if self.shared.autoplay() {
                PlaybackState::Playing
            } else {
                self.shared.state()
            }
        }) {
            PlaybackState::Playing => self.play(),
            state => self.shared.set_state(state),
        }
    }

    /// Keep the original codec/DSP history and source while skipping forward.
    /// Decoder loans before the target return without copying into the player.
    fn seek_stream_prefix<'a>(&mut self, samples: &'a [f32]) -> EngineResult<&'a [f32]> {
        let Some(target) = self.pending_seek else {
            return Ok(samples);
        };
        let channels = self.shared.channels() as usize;
        if channels == 0 || !samples.len().is_multiple_of(channels) {
            return Err(EngineError::from_detail(
                ErrorCode::IoError,
                "invalid seek PCM format",
            ));
        }
        let base = self.stream_decoded_end();
        let frames = (samples.len() / channels) as u64;
        let end = base.checked_add(frames).ok_or_else(|| {
            EngineError::from_detail(ErrorCode::IoError, "seek PCM duration overflow")
        })?;
        let skip = target.min(end).saturating_sub(base);
        if let AudioSource::Owned(pcm) = &mut self.source {
            pcm.clear();
        }
        self.position = 0;
        self.stream_base_frames = base + skip;
        self.stream_windowed = true;
        self.reset_rate_converter();
        Ok(&samples[skip as usize * channels..])
    }

    /// Submit without blocking the audio thread. The source clock, rather than
    /// the output sample rate, defines the decoder's target.
    fn request_stream_control(&mut self, time: f64, kind: ControlKind) -> bool {
        let managed = self.stream_presentation.is_some_and(|p| p.is_cyclic())
            || self.stream_controls.has_cutover();
        let cutover = match kind {
            ControlKind::Loop(enabled) => Some(stream_control::LoopRequest {
                output_start: self.shared.position_frames(),
                enabled,
                seek_target: None,
            }),
            ControlKind::Restart if managed => Some(stream_control::LoopRequest {
                output_start: 0,
                enabled: self
                    .stream_controls
                    .desired_loop(self.shared.loop_enabled()),
                seek_target: None,
            }),
            _ => None,
        };
        let Some(source) = self.stream_state.as_ref() else {
            return false;
        };
        let Some(sender) = source.control_sender() else {
            return false;
        };
        let result = (|| {
            if self.stream_controls.is_full() {
                return Err(EngineError::from_detail(
                    ErrorCode::InputSaturated,
                    "player source controls full",
                ));
            }
            let timeline = source.timeline().ok_or_else(|| {
                EngineError::from_detail(
                    ErrorCode::InvalidArgument,
                    "source control has no presentation clock",
                )
            })?;
            let target = if cutover.is_some() {
                0
            } else {
                timeline.seek_frame(time)?
            };
            let cutover = if matches!(kind, ControlKind::Seek | ControlKind::Start) && managed {
                let position = timeline.position(target as u64)?;
                Some(stream_control::LoopRequest {
                    output_start: position.frame + u64::from(position.phase != 0),
                    enabled: self
                        .stream_controls
                        .desired_loop(self.shared.loop_enabled()),
                    seek_target: Some(target as u64),
                })
            } else {
                cutover
            };
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
            let reply = stream_control::request_as(sender, target, cutover, deadline)?;
            Ok(stream_control::Pending {
                reply,
                kind,
                deadline,
                target,
                cutover,
            })
        })();
        match result {
            Ok(pending) => self.stream_controls.push(pending),
            Err(error) => {
                tracing::warn!(
                    "InnerAudioPlayer {} source control refused: {}",
                    self.id,
                    error
                );
                if matches!(kind, ControlKind::Restart | ControlKind::Start) {
                    self.fail_stream();
                } else {
                    self.push_event(InnerAudioEventType::Error);
                }
            }
        }
        true
    }

    fn poll_stream_controls(&mut self) {
        use tokio::sync::oneshot::error::TryRecvError;
        for _ in 0..STREAM_CHANNEL_CAPACITY {
            let Some(pending) = self.stream_controls.front() else {
                break;
            };
            let reply = match pending.reply.try_recv() {
                Ok(reply) => reply,
                Err(TryRecvError::Empty) if tokio::time::Instant::now() < pending.deadline => break,
                Err(_) => {
                    self.fail_stream();
                    break;
                }
            };
            let quiet = self.stream_controls.has_restart();
            let pending = self.stream_controls.pop().expect("observed control");
            match reply {
                ControlReply::Prepared(prepared) => {
                    if prepared.target != pending.target || prepared.deadline != pending.deadline {
                        prepared.source.cancel();
                        self.fail_stream();
                        break;
                    }
                    let expected = pending
                        .cutover
                        .map(|request| {
                            let clock = prepared.source.timeline().ok_or_else(|| {
                                EngineError::from_detail(
                                    ErrorCode::InvalidArgument,
                                    "loop receipt has no source clock",
                                )
                            })?;
                            request.traversal(clock)
                        })
                        .transpose();
                    if expected.as_ref().is_err() || expected.ok().flatten() != prepared.traversal {
                        prepared.source.cancel();
                        self.fail_stream();
                        break;
                    }
                    let result = prepared.mapped_output().and_then(|output| {
                        prepared.accept_as(
                            self,
                            output,
                            !quiet
                                && matches!(pending.kind, ControlKind::Seek | ControlKind::Start),
                            quiet,
                        )
                    });
                    if result.is_err() {
                        self.fail_stream();
                        break;
                    }
                }
                ControlReply::Rejected(code) | ControlReply::NotStarted(code) => {
                    tracing::debug!(
                        "InnerAudioPlayer {} source target rejected: {:?}",
                        self.id,
                        code
                    );
                    if matches!(pending.kind, ControlKind::Restart | ControlKind::Start) {
                        self.fail_stream();
                        break;
                    }
                    if matches!(pending.kind, ControlKind::Loop(_)) {
                        self.shared.set_loop_enabled(
                            self.stream_presentation
                                .is_some_and(|p| p.is_cyclic() && p.output_end().is_none()),
                        );
                    }
                    self.push_event(InnerAudioEventType::Error);
                }
            }
        }
    }

    /// Reconfigure a retained source before publishing its next traversal.
    pub fn set_loop_enabled(&mut self, enabled: bool) {
        let controlled = self
            .stream_state
            .as_ref()
            .is_some_and(|s| s.control_sender().is_some());
        if !controlled {
            if !self.shared.is_loaded()
                && self
                    .stream_state
                    .as_ref()
                    .is_some_and(|s| s.accepts_controls())
            {
                self.loop_intent = Some(enabled);
            }
            let changed = self.shared.loop_enabled() != enabled;
            self.shared.set_loop_enabled(enabled);
            if changed {
                self.reset_rate_converter();
            }
            return;
        }
        if !self.shared.is_loaded() {
            self.shared.set_loop_enabled(enabled);
            self.loop_intent = Some(enabled);
            return;
        }
        let current = self
            .stream_presentation
            .or_else(|| self.stream_state.as_ref().and_then(|s| s.presentation()))
            .is_some_and(|p| p.is_cyclic() && p.output_end().is_none());
        if self.stream_controls.is_empty() && current == enabled {
            return;
        }
        self.request_stream_control(0.0, ControlKind::Loop(enabled));
    }

    /// Configure a future start without seeking or changing playback state.
    pub fn set_start_time(&mut self, time: f64) {
        if time.is_finite() {
            self.start_time = time.max(0.0);
        }
    }

    /// Play the audio
    pub fn play(&mut self) {
        if !self.shared.is_loaded() {
            if let Some(controls) = &mut self.loading_controls {
                controls.playback = Some(PlaybackState::Playing);
            }
            tracing::trace!("InnerAudioPlayer {} play() ignored: not loaded", self.id);
            return;
        }
        let prev_state = self.shared.state();
        if prev_state == PlaybackState::Stopped && self.position == 0 {
            self.buffered_loops = 0;
            self.reset_rate_converter();
        }
        let initial_seek = self.start_pending && self.start_time > 0.0;
        self.start_pending = false;
        if initial_seek {
            if !self.request_stream_control(self.start_time, ControlKind::Start) {
                self.seek(self.start_time);
            }
            if !self.shared.is_loaded() {
                return;
            }
        }
        if self.replay_needed
            && !initial_seek
            && self.stream_controls.is_empty()
            && !self.stream_controls.has_restart()
        {
            self.request_stream_control(0.0, ControlKind::Restart);
            if !self.shared.is_loaded() {
                return;
            }
        }
        if prev_state != PlaybackState::Playing {
            tracing::debug!(
                "InnerAudioPlayer {} play: {:?} -> Playing",
                self.id,
                prev_state
            );
            self.shared.set_state(PlaybackState::Playing);
            self.waiting_notified = false;
            self.push_event(InnerAudioEventType::Play);
        }
    }

    /// Pause the audio
    pub fn pause(&mut self) {
        if !self.shared.is_loaded()
            && let Some(controls) = &mut self.loading_controls
        {
            controls.playback = Some(PlaybackState::Paused);
            return;
        }
        if self.shared.state() == PlaybackState::Playing {
            tracing::debug!(
                "InnerAudioPlayer {} pause at {:.2}s",
                self.id,
                self.shared.current_time()
            );
            self.shared.set_state(PlaybackState::Paused);
            self.push_event(InnerAudioEventType::Pause);
        }
    }

    /// Apply a buffered seek on the owning audio thread, even while paused or
    /// stopped. Deferring this to process() both stalled idle players and let
    /// later commands overtake the seek. No seek mailbox belongs in the mixer.
    pub fn seek(&mut self, time: f64) {
        if !time.is_finite() {
            return;
        }
        if !self.shared.is_loaded() {
            if let Some(controls) = &mut self.loading_controls {
                controls.seek = Some(time.max(0.0));
            }
            return;
        }
        let sample_rate = self.shared.sample_rate();
        if !time.is_finite() || sample_rate == 0 || !self.shared.is_loaded() {
            return;
        }
        // An explicit target, including zero, takes precedence over startTime.
        self.start_pending = false;
        if self.request_stream_control(time, ControlKind::Seek) {
            return;
        }
        let target_frame = (time.max(0.0) * sample_rate as f64) as u64;
        let target_frame = self.stream_timeline.map_or(target_frame, |t| {
            if time >= t.seconds() {
                t.output_frames()
            } else {
                target_frame.min(t.output_frames())
            }
        });
        if self.is_streaming() && !self.stream_complete && target_frame >= self.stream_decoded_end()
        {
            self.push_event(InnerAudioEventType::Seeking);
            self.pending_seek = Some(target_frame);
            self.position = self.source.len();
            self.position_phase = 0;
            self.frames_since_time_update = 0;
            self.waiting_notified = false;
            self.publish_position(target_frame);
            return;
        }
        self.seek_frame(target_frame.min(self.shared.duration_frames()));
    }

    fn seek_frame(&mut self, target_frame: u64) {
        let channels = self.shared.channels() as usize;
        if channels == 0 || self.source.is_empty() {
            return;
        }
        self.pending_seek = None;
        self.push_event(InnerAudioEventType::Seeking);
        // This is the buffered path. Source/decoder repositioning outside the
        // retained streaming window is handled separately from this cursor.
        let relative_frame = target_frame.saturating_sub(self.stream_base_frames) as usize;
        self.position = relative_frame
            .saturating_mul(channels)
            .min(self.source.len());
        self.position_phase = 0;
        self.buffered_loops = 0;
        self.publish_position(self.stream_base_frames + (self.position / channels) as u64);
        self.reset_rate_converter();
        // A seek into buffered data may resume a stalled player. Let the next
        // underrun notify, and start a fresh TimeUpdate interval at the target.
        self.waiting_notified = false;
        self.frames_since_time_update = 0;
        self.push_event(InnerAudioEventType::Seeked);
    }

    /// Stop the audio and reset position
    pub fn stop(&mut self) {
        self.start_pending = true;
        if let Some(controls) = &mut self.loading_controls {
            controls.playback = Some(PlaybackState::Stopped);
            controls.seek = None;
        }
        tracing::debug!("InnerAudioPlayer {} stop", self.id);
        self.shared.set_state(PlaybackState::Stopped);
        self.pending_seek = None;
        self.position = 0;
        self.position_phase = 0;
        self.buffered_loops = 0;
        self.publish_position(0);
        self.frames_since_time_update = 0;
        self.reset_rate_converter();
        if self.shared.is_loaded() {
            self.request_stream_control(0.0, ControlKind::Restart);
        }
    }

    /// Drain pending events (call from audio thread loop).
    ///
    /// Draining rather than handing the vector away is what keeps the audio
    /// thread off the heap. `mem::take` left the player holding a zero-capacity
    /// vector, so the next `push_event` allocated a fresh one — and a playing
    /// player raises a throttled `TimeUpdate` about four times a second forever,
    /// which made this a permanent steady-state allocation on the one thread that
    /// must never be late. Measured at six allocation events over 64 ticks before
    /// the change. `Vec::drain` keeps the capacity, so the buffer is bought once.
    pub fn drain_events(&mut self) -> std::vec::Drain<'_, InnerAudioEvent> {
        self.pending_events.drain(..)
    }

    /// Check if player is active (playing)
    #[inline]
    pub fn is_active(&self) -> bool {
        self.shared.state() == PlaybackState::Playing && self.shared.is_loaded()
    }

    /// Returns `true` if this player has an active streaming download
    /// (receiving decoded chunks from the network), including a receiver
    /// whose forward prefetch is currently full.
    #[inline]
    pub fn is_streaming(&self) -> bool {
        self.stream_rx.is_some()
    }

    pub(crate) fn needs_stream_poll(&self) -> bool {
        !self.stream_controls.is_empty()
            || (self.is_streaming() && !self.stream_complete && !self.stream_prefetch_full())
    }

    fn finish_playback(&mut self, final_frame: usize, rendered_frames: usize) {
        self.start_pending = true;
        self.replay_needed = self.stream_windowed && self.retains_stream_source();
        self.publish_position(self.stream_base_frames.saturating_add(final_frame as u64));
        self.accumulate_time_update(rendered_frames);
        self.shared.set_state(PlaybackState::Stopped);
        self.push_event(InnerAudioEventType::Ended);
        self.position = 0;
        self.position_phase = 0;
        self.buffered_loops = 0;
        self.publish_position(0);
        self.frames_since_time_update = 0;
    }

    /// Process and mix samples into output buffer
    /// Returns true if still playing, false if finished
    pub fn process(&mut self, output: &mut [f32]) -> bool {
        if !self.stream_controls.is_empty() {
            return self.shared.state() != PlaybackState::Stopped;
        }
        if !self.is_active() {
            return self.shared.state() != PlaybackState::Stopped;
        }

        if self.pending_seek.is_some() {
            if !self.waiting_notified {
                self.waiting_notified = true;
                self.push_event(InnerAudioEventType::Waiting);
            }
            return true;
        }

        let channels = self.shared.channels() as usize;
        if channels == 0 {
            return false;
        }
        if self.source.is_empty() {
            if self.stream_complete {
                self.finish_playback(0, 0);
            }
            return false;
        }

        if self.rate_converter.is_some() {
            return self.process_rate_converted(output);
        }

        let volume = self.shared.volume();
        let rate_units = self.shared.playback_rate_units();
        let loop_enabled =
            self.shared.loop_enabled() && !self.stream_presentation.is_some_and(|p| p.is_cyclic());
        let output_channels = self.output_channels as usize;

        // Get samples reference after seek handling
        let samples = self.source.samples();

        // Keep an integer source frame and a phase in the shared rate's units.
        // Split the step once per block: advancing a frame needs only addition
        // and a carry, with no division or floating-point clock accumulation.
        let step_frames = (rate_units / PLAYBACK_RATE_SCALE) as usize;
        let step_phase = (rate_units % PLAYBACK_RATE_SCALE) as u16;
        let mut source_frame = self.position / channels;
        let mut phase = self.position_phase;

        let total_frames = samples.len() / channels;
        let output_frames = output.len() / output_channels;
        let waiting_for_input = !self.stream_complete && self.stream_rx.is_some();

        for frame_idx in 0..output_frames {
            let mut src_frame = source_frame;

            // Check if we've reached the end of available data
            let missing_neighbor = waiting_for_input
                && src_frame < total_frames
                && src_frame + 1 == total_frames
                && phase != 0;
            if src_frame >= total_frames || missing_neighbor {
                // In streaming mode, we might just need to wait for more data
                if waiting_for_input {
                    // Streaming but caught up - output silence for remaining
                    tracing::trace!(
                        "InnerAudioPlayer {} waiting for stream data (frame {} >= available {})",
                        self.id,
                        src_frame,
                        total_frames
                    );
                    // Contribute nothing for the rest of the block. `output` is the
                    // shared mix bus that every context and every other player has
                    // already added into, so writing silence here would delete their
                    // audio for up to a whole block, not just mute this player.
                    // Preserve the exact next source position, including an
                    // interpolation fraction or playbackRate overshoot. Moving
                    // backward repeats already consumed frames after every stall.
                    let final_frame = source_frame;
                    self.position = final_frame * channels;
                    self.position_phase = phase;
                    self.publish_position(self.stream_base_frames + final_frame as u64);
                    if !self.waiting_notified {
                        self.waiting_notified = true;
                        self.push_event(InnerAudioEventType::Waiting);
                    }
                    // Count frames actually rendered before the stall (and emit a
                    // TimeUpdate if the throttle threshold is now crossed).
                    self.accumulate_time_update(frame_idx);
                    return true; // Still playing, waiting for data
                }

                if loop_enabled {
                    // Carry overshoot and phase across the seam, including
                    // multiple wraps when a very short clip plays above 1x.
                    tracing::trace!("InnerAudioPlayer {} looping back to start", self.id);
                    self.buffered_loops = self
                        .buffered_loops
                        .saturating_add(source_frame / total_frames);
                    source_frame %= total_frames;
                    src_frame = source_frame;
                } else {
                    // Playback finished
                    tracing::debug!("InnerAudioPlayer {} playback ended", self.id);
                    // Stop contributing; do not clear. See the streaming-stall
                    // branch above: `output` is the shared additive mix bus.
                    self.finish_playback(total_frames, frame_idx);
                    return false;
                }
            }

            // Linear interpolation uses the exact cursor phase. This is not an
            // anti-aliasing filter; playback-rate DSP quality is a separate gate.
            // Wrap the successor at a completed loop's seam.
            let frac = phase as f32 / PLAYBACK_RATE_SCALE as f32;
            let next_frame = if src_frame + 1 < total_frames {
                src_frame + 1
            } else if loop_enabled && !waiting_for_input {
                0
            } else {
                src_frame
            };
            let base0 = src_frame * channels;
            let base1 = next_frame * channels;
            let dst_start = frame_idx * output_channels;

            // `src_frame` and `next_frame` are both below `total_frames`, and
            // `frame_idx` below `output_frames`, so every index below is in range
            // by construction.
            if channels == 1 {
                // Mono up-mixes to every output channel by duplication.
                let sample = lerp_frame(samples, base0, base1, 0, frac) * volume;
                for ch in 0..output_channels {
                    output[dst_start + ch] += sample;
                }
            } else if output_channels == 1 {
                // Down-mix to mono is the average of the source channels.
                let mut sum = 0.0;
                for ch in 0..channels {
                    sum += lerp_frame(samples, base0, base1, ch, frac);
                }
                output[dst_start] += sum / channels as f32 * volume;
            } else {
                for ch in 0..channels.min(output_channels) {
                    output[dst_start + ch] += lerp_frame(samples, base0, base1, ch, frac) * volume;
                }
            }

            // Advance frame position by playback rate
            source_frame += step_frames;
            phase += step_phase;
            if phase >= PLAYBACK_RATE_SCALE as u16 {
                phase -= PLAYBACK_RATE_SCALE as u16;
                source_frame += 1;
            }
        }

        // Update position (convert frame back to sample index). When looping,
        // normalize a final overshoot so the stored currentTime doesn't briefly
        // pin at duration until the next callback wraps.
        if loop_enabled && !waiting_for_input {
            self.buffered_loops = self
                .buffered_loops
                .saturating_add(source_frame / total_frames);
            source_frame %= total_frames;
        }
        if !waiting_for_input && !loop_enabled && source_frame >= total_frames {
            self.finish_playback(total_frames, output_frames);
            return false;
        }
        let final_frame = source_frame;
        let final_frame = if waiting_for_input {
            final_frame
        } else {
            final_frame.min(total_frames)
        };
        self.position = final_frame * channels;
        self.position_phase = phase;
        self.publish_position(self.stream_base_frames + final_frame as u64);

        // Throttled TimeUpdate (~4x/sec of playback) so JS currentTime advances
        // and onTimeUpdate fires between the discrete lifecycle events.
        self.accumulate_time_update(output_frames);

        true
    }

    fn process_rate_converted(&mut self, output: &mut [f32]) -> bool {
        let channels = self.shared.channels() as usize;
        let output_channels = self.output_channels as usize;
        let total = self.source.len() / channels;
        if total == 0 {
            self.finish_playback(0, 0);
            return false;
        }
        let looping = self.shared.loop_enabled()
            && self.stream_complete
            && !self.stream_presentation.is_some_and(|p| p.is_cyclic());
        let units = self.shared.playback_rate_units();
        let volume = self.shared.volume();
        let mut frame = self.position / channels;
        let mut phase = u32::from(self.position_phase);
        let mut written = 0;
        let requested = output.len() / output_channels;
        let mut scratch = [0.; crate::variable_rate::BLOCK * 32];
        if looping && frame >= total {
            self.buffered_loops = self.buffered_loops.saturating_add(frame / total);
            frame %= total;
        }
        while written < requested && (looping || frame < total) {
            let n = (requested - written).min(crate::variable_rate::BLOCK);
            let n = if looping || !self.stream_complete {
                n
            } else {
                let left =
                    (total - frame) as u64 * u64::from(PLAYBACK_RATE_SCALE) - u64::from(phase);
                n.min(left.div_ceil(u64::from(units)) as usize)
            };
            let converter = self.rate_converter.as_mut().unwrap();
            let result = if looping {
                converter
                    .render(
                        self.source.samples(),
                        units,
                        true,
                        &mut scratch[..n * channels],
                    )
                    .map(|()| n)
            } else {
                converter.render_stream(
                    self.source.samples(),
                    units,
                    self.stream_complete,
                    &mut scratch[..n * channels],
                )
            };
            let n = match result {
                Ok(n) => n,
                Err(error) => {
                    tracing::error!("InnerAudioPlayer {} rate render failed: {error}", self.id);
                    self.shared.set_state(PlaybackState::Stopped);
                    self.push_event(InnerAudioEventType::Error);
                    return false;
                }
            };
            if n == 0 {
                break;
            }
            for i in 0..n {
                let source = &scratch[i * channels..(i + 1) * channels];
                let destination = &mut output
                    [(written + i) * output_channels..(written + i + 1) * output_channels];
                if channels == 1 {
                    for value in destination {
                        *value += source[0] * volume;
                    }
                } else if output_channels == 1 {
                    let sum = source.iter().copied().sum::<f32>();
                    destination[0] += sum / channels as f32 * volume;
                } else {
                    for ch in 0..channels.min(output_channels) {
                        destination[ch] += source[ch] * volume;
                    }
                }
            }
            let advance = phase + n as u32 * units;
            frame += (advance / PLAYBACK_RATE_SCALE) as usize;
            phase = advance % PLAYBACK_RATE_SCALE;
            if looping {
                self.buffered_loops = self.buffered_loops.saturating_add(frame / total);
                frame %= total;
            }
            written += n;
        }
        if self.stream_complete && !looping && frame >= total {
            self.finish_playback(total, written);
            return false;
        }
        self.position = frame * channels;
        self.position_phase = phase as u16;
        self.publish_position(self.stream_base_frames + frame as u64);
        self.accumulate_time_update(written);
        if !self.stream_complete && written < requested && !self.waiting_notified {
            self.waiting_notified = true;
            self.push_event(InnerAudioEventType::Waiting);
        }
        true
    }

    /// Check if stream download is complete (for caching)
    pub fn is_stream_complete(&self) -> bool {
        self.stream_complete
    }
    #[cfg(test)]
    pub(crate) fn physical_pcm_bytes(&self) -> usize {
        self.source.capacity_bytes()
    }
}

impl stream_control::Prepared {
    fn accept_as(
        self,
        player: &mut InnerAudioPlayer,
        output_frame: u64,
        notify_seek: bool,
        quiet: bool,
    ) -> EngineResult<()> {
        if tokio::time::Instant::now() >= self.deadline {
            self.source.cancel();
            return Err(EngineError::from_detail(
                ErrorCode::Timeout,
                "source acceptance deadline exceeded",
            ));
        }
        if self.accept.is_closed() {
            return Err(EngineError::from_detail(
                ErrorCode::IoError,
                "source no longer awaits acceptance",
            ));
        }
        if self.source.timeline().is_some() && self.mapped_output()? != output_frame {
            self.source.cancel();
            return Err(EngineError::from_detail(
                ErrorCode::InvalidArgument,
                "acceptance does not match source output clock",
            ));
        }
        if let Err(error) = player.accept_stream_revision_as(
            &self.source,
            self.revision,
            output_frame,
            notify_seek,
            self.traversal,
            !quiet && (notify_seek || self.traversal.is_some()),
        ) {
            self.source.cancel();
            return Err(error);
        }
        self.accept.send(()).map_err(|_| {
            self.source.cancel();
            EngineError::from_detail(ErrorCode::IoError, "source acceptance expired")
        })
    }
}

impl Drop for InnerAudioPlayer {
    fn drop(&mut self) {
        self.shared.source_generation.store(0, Ordering::Release);
        self.shared.source_changed.notify_waiters();
        // Cancel any in-flight streaming download so its background task stops
        // promptly instead of downloading/decoding into a dropped channel
        // (happens on DestroyInnerAudio, which removes the player from the map).
        self.cancel_stream();
        self.replace_source(AudioSource::Owned(Vec::new()), None);
    }
}

#[cfg(test)]
#[path = "inner_rate_tests.rs"]
mod rate_tests;

#[cfg(test)]
#[path = "loading_control_tests.rs"]
mod loading_control_tests;

#[cfg(test)]
#[path = "stream_revision_tests.rs"]
mod stream_revision_tests;

#[cfg(test)]
#[path = "public_control_tests.rs"]
mod public_control_tests;

#[cfg(test)]
#[path = "live_loop_tests.rs"]
mod live_loop_tests;

#[cfg(test)]
#[path = "cyclic_clock_tests.rs"]
mod cyclic_clock_tests;

#[cfg(test)]
#[path = "stream_timeline_tests.rs"]
mod stream_timeline_tests;

#[cfg(test)]
#[path = "seek_tests.rs"]
mod seek_tests;

#[cfg(test)]
#[path = "forward_seek_tests.rs"]
mod forward_seek_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::streaming::{StreamMsg, StreamingState};

    #[test]
    fn source_generation_exhaustion_never_wraps_or_revalidates_a_token() {
        let mut player = InnerAudioPlayer::new(1, 1);
        player
            .shared
            .source_generation
            .store(u64::MAX - 1, Ordering::Release);
        let last = player.begin_load().unwrap();
        assert_eq!(
            player.shared.source_generation.load(Ordering::Acquire),
            u64::MAX
        );
        assert!(last.is_current());
        let error = player.begin_load().expect_err("generation exhaustion");
        assert_eq!(error.code, ErrorCode::InputSaturated);
        assert!(last.is_current());
        assert_eq!(
            player.shared.source_generation.load(Ordering::Acquire),
            u64::MAX
        );
        drop(player);
        assert!(!last.is_current());
    }

    fn send_pcm(tx: &tokio::sync::mpsc::Sender<StreamMsg>, samples: &[f32]) {
        let mut pool = crate::streaming::PcmPool::new();
        let mut pcm = pool.take();
        pcm.buffer_mut().extend_from_slice(samples);
        assert!(
            tx.try_send(StreamMsg::Samples {
                revision: crate::streaming::StreamRevision::INITIAL,
                pcm
            })
            .is_ok()
        );
    }

    fn short_stream(samples: &[f32]) -> (InnerAudioPlayer, tokio::sync::mpsc::Sender<StreamMsg>) {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let mut player = InnerAudioPlayer::new(1, 1);
        player
            .start_streaming(
                "https://example.test/music".into(),
                rx,
                StreamingState::new(),
            )
            .unwrap();
        assert!(
            tx.try_send(StreamMsg::Ready {
                revision: crate::streaming::StreamRevision::INITIAL,
                sample_rate: 8000,
                channels: 1
            })
            .is_ok()
        );
        send_pcm(&tx, samples);
        player.poll_stream();
        // These playback-cursor tests deliberately use less than the startup
        // buffer threshold. The transport/player integration covers CanPlay.
        player.shared.set_loaded(true);
        player.play();
        (player, tx)
    }

    #[test]
    fn paused_stream_backpressure_reaches_the_producer_and_resumes() {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let mut player = InnerAudioPlayer::new(1, 1);
        player
            .start_streaming(
                "https://example.test/music".into(),
                rx,
                StreamingState::new(),
            )
            .unwrap();
        assert!(
            tx.try_send(StreamMsg::Ready {
                revision: crate::streaming::StreamRevision::INITIAL,
                sample_rate: 8000,
                channels: 1
            })
            .is_ok()
        );
        let mut pool = crate::streaming::PcmPool::new();
        for tick in 0..64 {
            let mut pcm = pool.take();
            pcm.buffer_mut().resize(2000, 0.25);
            let _ = tx.try_send(StreamMsg::Samples {
                revision: crate::streaming::StreamRevision::INITIAL,
                pcm,
            });
            player.poll_stream();
            if tick == 2 {
                player.play();
                player.pause();
            }
        }
        assert_eq!(player.shared.state(), PlaybackState::Paused);
        assert!(
            player.source.len() <= 10_000,
            "paused player drained the whole track into PCM: {}",
            player.source.len()
        );
        assert_eq!(
            tx.capacity(),
            0,
            "forward prefetch must stop consuming the producer queue"
        );
        assert!(
            !player.needs_stream_poll(),
            "a paused full prefetch kept the audio management loop active"
        );
        player.play();
        let mut output = [0.0; 1024];
        assert!(player.process(&mut output));
        assert!(output.iter().all(|sample| *sample == 0.25));
        player.poll_stream();
        assert!(
            tx.capacity() > 0,
            "playback must release upstream backpressure"
        );
        assert!(player.source.len() - player.position <= 10_000);
    }

    #[test]
    fn stalled_stream_never_replays_the_last_rendered_frame() {
        let (mut player, tx) = short_stream(&[1.0, 2.0, 3.0]);
        let mut output = [0.0; 5];
        assert!(player.process(&mut output));
        assert_eq!(output, [1.0, 2.0, 3.0, 0.0, 0.0]);
        assert_eq!(player.shared.position_frames(), 3);
        for _ in 0..3 {
            let mut empty = [0.0; 2];
            assert!(player.process(&mut empty));
            assert_eq!(empty, [0.0; 2], "underflow replayed a consumed sample");
        }
        send_pcm(&tx, &[4.0, 5.0]);
        player.poll_stream();
        let mut resumed = [0.0; 2];
        assert!(player.process(&mut resumed));
        assert_eq!(resumed, [4.0, 5.0]);
    }

    #[test]
    fn fractional_stream_waits_for_the_missing_interpolation_neighbor() {
        let (mut player, tx) = short_stream(&[0.0, 1.0]);
        player.shared.set_playback_rate(0.5);
        let mut first = [0.0; 3];
        player.process(&mut first);
        assert_eq!(first, [0.0, 0.5, 1.0]);
        let mut waiting = [0.0; 2];
        player.process(&mut waiting);
        assert_eq!(
            waiting, [0.0; 2],
            "missing successor must not be fabricated from the previous frame"
        );
        assert_eq!(player.position_phase, 500);
        send_pcm(&tx, &[2.0, 3.0]);
        player.poll_stream();
        let mut resumed = [0.0; 2];
        player.process(&mut resumed);
        assert_eq!(resumed, [1.5, 2.0]);
    }

    #[test]
    fn streamed_duration_includes_the_reclaimed_prefix() {
        let (mut player, tx) = short_stream(&[]);
        player
            .source
            .extend_from_slice(&vec![0.0; InnerAudioPlayer::STREAM_WINDOW_SAMPLES + 20]);
        player.position = InnerAudioPlayer::STREAM_WINDOW_SAMPLES + 5;
        send_pcm(&tx, &[0.0; 8]);
        assert!(
            tx.try_send(StreamMsg::Done {
                revision: crate::streaming::StreamRevision::INITIAL
            })
            .is_ok()
        );
        player.poll_stream();
        assert_eq!(
            player.shared.duration_frames(),
            InnerAudioPlayer::STREAM_WINDOW_SAMPLES as u64 + 28
        );
        assert!(player.stream_windowed);
        assert!(player.stream_complete);
    }

    #[test]
    fn ended_is_emitted_on_the_exact_final_output_boundary_once() {
        let mut player = InnerAudioPlayer::new(1, 1);
        player
            .load_audio(DecodedAudio {
                samples: vec![1.0, 2.0, 3.0],
                sample_rate: 8000,
                channels: 1,
            })
            .unwrap();
        player.play();
        let mut output = [0.0; 3];
        assert!(
            !player.process(&mut output),
            "ended waited for another mix quantum"
        );
        assert_eq!(output, [1.0, 2.0, 3.0]);
        assert_eq!(
            player
                .drain_events()
                .filter(|event| event.event_type == InnerAudioEventType::Ended)
                .count(),
            1
        );
        assert!(!player.process(&mut [0.0; 3]));
        assert!(
            !player
                .drain_events()
                .any(|event| event.event_type == InnerAudioEventType::Ended)
        );
    }

    #[test]
    fn streaming_loop_waits_for_complete_input_before_wrapping() {
        let (mut player, tx) = short_stream(&[1.0, 2.0, 3.0]);
        player.shared.set_loop_enabled(true);
        let mut first = [0.0; 3];
        player.process(&mut first);
        assert_eq!(first, [1.0, 2.0, 3.0]);
        assert_eq!(player.shared.position_frames(), 3);
        let mut waiting = [0.0; 2];
        player.process(&mut waiting);
        assert_eq!(waiting, [0.0; 2]);
        send_pcm(&tx, &[4.0, 5.0]);
        assert!(
            tx.try_send(StreamMsg::Done {
                revision: crate::streaming::StreamRevision::INITIAL
            })
            .is_ok()
        );
        player.poll_stream();
        let mut resumed = [0.0; 4];
        assert!(player.process(&mut resumed));
        assert_eq!(resumed, [4.0, 5.0, 1.0, 2.0]);
    }

    #[test]
    fn a_producer_gone_without_done_or_error_fails_the_stream() {
        let (mut player, tx) = short_stream(&[1.0, 2.0]);
        player.drain_events().for_each(drop);
        drop(tx);
        player.poll_stream();
        assert_eq!(player.shared.state(), PlaybackState::Stopped);
        assert!(!player.shared.is_loaded());
        let events = player
            .drain_events()
            .map(|event| event.event_type)
            .collect::<Vec<_>>();
        assert_eq!(events, [InnerAudioEventType::Error]);
    }

    #[test]
    fn stream_error_stops_playback_without_an_ended_event() {
        let (mut player, tx) = short_stream(&[1.0, 2.0]);
        player.drain_events().for_each(drop);
        assert!(
            tx.try_send(StreamMsg::Error {
                revision: crate::streaming::StreamRevision::INITIAL,
                message: "body truncated".into()
            })
            .is_ok()
        );
        player.poll_stream();
        assert_eq!(player.shared.state(), PlaybackState::Stopped);
        let mut output = [0.0; 4];
        assert!(!player.process(&mut output));
        assert_eq!(output, [0.0; 4]);
        let events = player
            .drain_events()
            .map(|event| event.event_type)
            .collect::<Vec<_>>();
        assert_eq!(events, [InnerAudioEventType::Error]);
    }
    use shared::protocol::audio_cmd::InnerAudioEventType;
    use tokio::sync::mpsc;

    #[test]
    fn dropping_player_cancels_streaming_download() {
        let state = StreamingState::new();
        let (_tx, rx) = mpsc::channel::<StreamMsg>(1);
        let mut player = InnerAudioPlayer::new(1, 2);
        player
            .start_streaming("http://example/x.mp3".into(), rx, state.clone())
            .unwrap();

        assert!(!state.is_cancelled(), "should not be cancelled while alive");
        drop(player);
        assert!(
            state.is_cancelled(),
            "dropping the player must cancel the in-flight streaming download"
        );
    }

    #[test]
    fn waiting_event_emitted_once_per_stall() {
        let state = StreamingState::new();
        let (_tx, rx) = mpsc::channel::<StreamMsg>(1);
        let mut player = InnerAudioPlayer::new(1, 2); // stereo output
        player
            .start_streaming("http://example/x.mp3".into(), rx, state)
            .unwrap();

        // ~0.5 s of stereo data buffered, playback caught up to the end.
        player.shared.set_sample_rate(44_100);
        player.shared.set_channels(2);
        player.shared.set_loaded(true);
        player.source.extend_from_slice(&vec![0.0f32; 44_100]); // 22050 stereo frames
        player.shared.set_state(PlaybackState::Playing);
        player.position = 44_100; // at the end of available data

        let mut out = [0.0f32; 8]; // 4 stereo frames per call
        for _ in 0..5 {
            player.process(&mut out);
        }

        let waiting = player
            .drain_events()
            .filter(|e| matches!(e.event_type, InnerAudioEventType::Waiting))
            .count();
        assert_eq!(
            waiting, 1,
            "Waiting must fire once per stall episode, not every audio tick"
        );
    }

    /// `output` is the mix bus every AudioContext and every other player has
    /// already added into by the time a player runs, so a player that runs out
    /// of audio must stop contributing rather than write silence. Clearing the
    /// tail deleted co-mixed audio for the rest of the block, which is heard as
    /// a hole in unrelated sounds whenever one track happens to end.
    #[test]
    fn a_player_that_ends_mid_block_leaves_co_mixed_audio_alone() {
        const CO_MIXED: f32 = 7.0;

        let mut player = InnerAudioPlayer::new(1, 2);
        player.shared.set_sample_rate(48_000);
        player.shared.set_channels(2);
        player.shared.set_loaded(true);
        // Two stereo frames of audio against a four-frame block.
        player.source.extend_from_slice(&[1.0, 1.0, 1.0, 1.0]);
        player.shared.set_state(PlaybackState::Playing);

        let mut out = [CO_MIXED; 8];
        assert!(!player.process(&mut out), "the player has ended");

        assert_eq!(
            &out[..4],
            &[CO_MIXED + 1.0; 4],
            "the player's own frames must be added, not assigned"
        );
        assert_eq!(
            &out[4..],
            &[CO_MIXED; 4],
            "frames past the end of this player's audio must keep what other \
             sources already mixed there"
        );
    }

    #[test]
    fn a_player_stalled_on_stream_data_leaves_co_mixed_audio_alone() {
        const CO_MIXED: f32 = -3.5;

        let state = StreamingState::new();
        let (_tx, rx) = mpsc::channel::<StreamMsg>(1);
        let mut player = InnerAudioPlayer::new(1, 2);
        player
            .start_streaming("http://example/x.mp3".into(), rx, state)
            .unwrap();
        player.shared.set_sample_rate(48_000);
        player.shared.set_channels(2);
        player.shared.set_loaded(true);
        player.source.extend_from_slice(&[1.0, 1.0]);
        player.shared.set_state(PlaybackState::Playing);
        player.position = 2; // already at the buffered edge

        let mut out = [CO_MIXED; 8];
        assert!(player.process(&mut out), "still playing, waiting for data");

        assert_eq!(
            out, [CO_MIXED; 8],
            "a stalled stream must contribute nothing, not silence the bus"
        );
    }

    /// A fractional playback rate must interpolate. Truncating the 16.16
    /// position is a zero-order hold, which on a ramp repeats samples where the
    /// true signal is a straight line -- broadband aliasing, not a pitch shift.
    #[test]
    fn a_fractional_playback_rate_interpolates_between_source_frames() {
        let mut player = InnerAudioPlayer::new(1, 1); // mono out
        player.shared.set_sample_rate(48_000);
        player.shared.set_channels(1);
        player.shared.set_loaded(true);
        player
            .source
            .extend_from_slice(&(0..16).map(|i| i as f32).collect::<Vec<_>>());
        player.shared.set_state(PlaybackState::Playing);
        player.shared.set_playback_rate(0.5);

        let mut out = [0.0f32; 6];
        assert!(player.process(&mut out));
        assert_eq!(out, [0.0, 0.5, 1.0, 1.5, 2.0, 2.5]);
    }

    #[test]
    fn waiting_event_refires_after_seek() {
        let state = StreamingState::new();
        let (_tx, rx) = mpsc::channel::<StreamMsg>(1);
        let mut player = InnerAudioPlayer::new(1, 2);
        player
            .start_streaming("http://example/x.mp3".into(), rx, state)
            .unwrap();
        player.shared.set_sample_rate(44_100);
        player.shared.set_channels(2);
        player.shared.set_loaded(true);
        player.source.extend_from_slice(&vec![0.0f32; 44_100]); // 22050 stereo frames
        player.shared.set_state(PlaybackState::Playing);

        let mut out = [0.0f32; 8];

        // First stall at the buffered edge -> one Waiting.
        player.position = 44_100;
        player.process(&mut out);

        // Seek back into buffered data (consumes the seek, should re-arm Waiting),
        // then stall again at the edge.
        player.seek_frame(0);
        player.process(&mut out);
        player.position = 44_100;
        player.process(&mut out);

        let waiting = player
            .drain_events()
            .filter(|e| matches!(e.event_type, InnerAudioEventType::Waiting))
            .count();
        assert_eq!(
            waiting, 2,
            "a stall after a seek must emit Waiting again (latch re-armed on seek)"
        );
    }

    #[test]
    fn time_update_caps_at_one_per_call_without_backlog() {
        // A process block spanning multiple TimeUpdate intervals must emit at
        // most one TimeUpdate and leave no interval-sized backlog, so a later
        // small block cannot immediately over-fire while a backlog drains.
        let mut player = InnerAudioPlayer::new(1, 2);
        player.shared.set_sample_rate(44_100); // interval = 11_025 frames
        let interval = 44_100u64 / 4;

        // One block covering >2 intervals.
        player.accumulate_time_update((interval * 2 + 500) as usize);
        let first = player
            .drain_events()
            .filter(|e| matches!(e.event_type, InnerAudioEventType::TimeUpdate))
            .count();
        assert_eq!(
            first, 1,
            "a multi-interval block emits at most one TimeUpdate"
        );
        assert!(
            player.frames_since_time_update < interval,
            "no interval-sized backlog may remain after emitting"
        );

        // A tiny follow-up block must not fire again from a drained backlog.
        player.accumulate_time_update(1);
        let second = player
            .drain_events()
            .filter(|e| matches!(e.event_type, InnerAudioEventType::TimeUpdate))
            .count();
        assert_eq!(
            second, 0,
            "a small next block must not over-fire from backlog"
        );
    }

    #[test]
    fn fractional_position_is_invariant_to_chunk_splitting() {
        fn make_player() -> InnerAudioPlayer {
            let mut player = InnerAudioPlayer::new(1, 1);
            player.shared.set_sample_rate(48_000);
            player.shared.set_channels(1);
            player.shared.set_loaded(true);
            player.shared.set_state(PlaybackState::Playing);
            player.shared.set_playback_rate(1.1);
            player
                .source
                .extend_from_slice(&(0..256).map(|frame| frame as f32).collect::<Vec<_>>());
            player
        }

        let mut one_quantum = make_player();
        let mut one_output = [0.0; 64];
        assert!(one_quantum.process(&mut one_output));

        let mut split = make_player();
        let mut split_output = [0.0; 64];
        for chunk in split_output.chunks_mut(8) {
            assert!(split.process(chunk));
        }

        assert_eq!(split_output, one_output);
        assert_eq!(split.position, one_quantum.position);
        assert_eq!(split.position_phase, one_quantum.position_phase);
    }

    fn clock_player(samples: &[f32], rate: f32) -> InnerAudioPlayer {
        let mut player = InnerAudioPlayer::new(1, 1);
        player.source.extend_from_slice(samples);
        player.shared.set_sample_rate(48_000);
        player.shared.set_channels(1);
        player.shared.set_duration_frames(samples.len() as u64);
        player.shared.set_loaded(true);
        player.shared.set_playback_rate(rate);
        player.play();
        player
    }

    #[test]
    fn playback_rate_preserves_every_supported_thousandth() {
        let shared = InnerAudioSharedState::new();
        for units in 500..=2000 {
            let rate = units as f32 / 1000.0;
            shared.set_playback_rate(rate);
            assert_eq!(shared.playback_rate(), rate, "rate {units}/1000");
        }
        shared.set_playback_rate(1.2346);
        assert_eq!(shared.playback_rate(), 1.235);
        shared.set_playback_rate(f32::NEG_INFINITY);
        assert_eq!(shared.playback_rate(), 0.5);
        shared.set_playback_rate(f32::INFINITY);
        assert_eq!(shared.playback_rate(), 2.0);
    }

    #[test]
    fn nan_rate_retains_the_previous_valid_playback_speed() {
        let mut player = clock_player(&[0.0; 8], 1.1);
        player.shared.set_playback_rate(f32::NAN);
        assert_eq!(player.shared.playback_rate(), 1.1);
        assert!(player.process(&mut [0.0; 5]));
        assert_eq!(player.shared.position_frames(), 5);
        assert!(!player.process(&mut [0.0; 3]));
    }

    #[test]
    fn decimal_rate_tracks_exact_time_over_many_callbacks() {
        let samples: Vec<_> = (0..60_000).map(|frame| frame as f32).collect();
        let mut player = clock_player(&samples, 1.1);
        let mut rendered = 0;
        let mut output = [0.0; 509];
        for block in [127, 1, 64, 311, 509].into_iter().cycle() {
            let count = block.min(48_000 - rendered);
            output[..count].fill(0.0);
            assert!(player.process(&mut output[..count]));
            for (offset, actual) in output[..count].iter().enumerate() {
                let expected = ((rendered + offset) as f64 * 1.1) as f32;
                assert!(
                    (actual - expected).abs() <= 0.004,
                    "at {}",
                    rendered + offset
                );
            }
            rendered += count;
            if rendered == 48_000 {
                break;
            }
        }
        assert_eq!(player.shared.position_frames(), 52_800);
        assert_eq!(player.position_phase, 0);
    }

    #[test]
    fn decimal_rate_ends_on_the_exact_final_output_frame() {
        for units in [503_u64, 1100, 1333, 1999] {
            let mut player = clock_player(&[0.25; 11], units as f32 / 1000.0);
            player.drain_events().for_each(drop);
            let output_frames = (11 * 1000_u64).div_ceil(units);
            for frame in 0..output_frames {
                let mut output = [0.0];
                assert_eq!(player.process(&mut output), frame + 1 < output_frames);
                assert_eq!(output, [0.25]);
            }
            assert_eq!(player.shared.position_frames(), 0);
            assert_eq!(player.position_phase, 0);
            assert!(!player.process(&mut [0.0; 3]));
            assert_eq!(
                player
                    .drain_events()
                    .filter(|event| matches!(event.event_type, InnerAudioEventType::Ended))
                    .count(),
                1
            );
        }
    }

    #[test]
    fn rate_changes_and_pause_preserve_the_fractional_media_clock() {
        let samples: Vec<_> = (0..32).map(|frame| frame as f32).collect();
        let mut player = clock_player(&samples, 1.1);
        assert!(player.process(&mut [0.0; 3]));
        player.pause();
        let mut paused_mix = [7.0; 8];
        assert!(player.process(&mut paused_mix));
        assert_eq!(paused_mix, [7.0; 8]);
        player.shared.set_playback_rate(0.503);
        player.play();
        let mut resumed = [0.0; 2];
        assert!(player.process(&mut resumed));
        assert!((resumed[0] - 3.3).abs() < 1e-6);
        assert!((resumed[1] - 3.803).abs() < 1e-6);
        player.shared.set_playback_rate(1.333);
        assert!(player.process(&mut [0.0; 3]));
        // Three at 1.1 + two at .503 + three at 1.333 = 8.305 frames.
        assert_eq!(player.shared.position_frames(), 8);
        assert_eq!(player.position_phase, 305);
    }

    #[test]
    fn buffered_seek_and_stop_reset_the_fractional_media_clock() {
        let samples: Vec<_> = (0..32).map(|frame| frame as f32).collect();
        let mut player = clock_player(&samples, 1.1);
        assert!(player.process(&mut [0.0; 3]));
        player.seek_frame(10);
        let mut output = [0.0];
        assert!(player.process(&mut output));
        assert_eq!(output, [10.0]);
        assert_eq!(player.position_phase, 100);
        player.stop();
        assert_eq!(player.position_phase, 0);
        player.play();
        output.fill(0.0);
        assert!(player.process(&mut output));
        assert_eq!(output, [0.0]);
        assert_eq!(player.position_phase, 100);
    }

    #[test]
    fn short_decimal_loops_match_an_absolute_clock_across_partitions() {
        for samples in [&[0.25][..], &[0.0, 1.0, -1.0][..]] {
            for units in [503_u64, 1100, 1333, 1999, 2000] {
                let mut whole = clock_player(samples, units as f32 / 1000.0);
                let mut split = clock_player(samples, units as f32 / 1000.0);
                whole.shared.set_loop_enabled(true);
                split.shared.set_loop_enabled(true);
                let mut whole_output = [0.0; 1001];
                let mut split_output = [0.0; 1001];
                assert!(whole.process(&mut whole_output));
                for chunk in split_output.chunks_mut(7) {
                    assert!(split.process(chunk));
                }
                assert_eq!(split_output, whole_output);
                for (frame, actual) in whole_output.iter().enumerate() {
                    let position = (frame as u64 * units) % (samples.len() as u64 * 1000);
                    let index = (position / 1000) as usize;
                    let next = (index + 1) % samples.len();
                    let fraction = (position % 1000) as f64 / 1000.0;
                    let expected =
                        samples[index] as f64 * (1.0 - fraction) + samples[next] as f64 * fraction;
                    assert!(
                        (*actual as f64 - expected).abs() < 2e-7,
                        "rate {units}/1000 at frame {frame}: {actual} != {expected}"
                    );
                }
                let end = 1001 * units;
                assert_eq!(
                    split.shared.position_frames(),
                    (end / 1000) % samples.len() as u64
                );
                assert_eq!(split.position_phase as u64, end % 1000);
            }
        }
    }

    #[test]
    fn decimal_stream_stalls_preserve_phase_and_the_additive_mix() {
        let (mut player, tx) = short_stream(&[0.0, 1.0, 2.0, 3.0]);
        player.shared.set_playback_rate(1.1);
        let mut first = [0.0; 8];
        assert!(player.process(&mut first));
        assert_eq!(&first[..3], &[0.0, 1.1, 2.2]);
        assert_eq!(&first[3..], &[0.0; 5]);
        for _ in 0..3 {
            let mut other_players = [7.0; 4];
            assert!(player.process(&mut other_players));
            assert_eq!(other_players, [7.0; 4]);
        }
        let more: Vec<_> = (4..20).map(|frame| frame as f32).collect();
        send_pcm(&tx, &more);
        player.poll_stream();
        let mut resumed = [0.0; 7];
        assert!(player.process(&mut resumed));
        for (offset, actual) in resumed.iter().enumerate() {
            let expected = 3.3 + offset as f32 * 1.1;
            assert!((actual - expected).abs() < 1e-6);
        }
        assert_eq!(player.shared.position_frames(), 11);
        assert_eq!(player.position_phase, 0);
    }

    #[test]
    fn decimal_rate_mixing_has_no_steady_state_heap_activity() {
        use migo_alloc_probe::{Burst, assert_no_steady_state_allocation};
        let mut player = clock_player(&[0.0, 0.25, -0.25], 1.1);
        player.shared.set_loop_enabled(true);
        let mut output = [0.0; 128];
        assert_no_steady_state_allocation(
            Burst {
                path: "InnerAudio decimal-rate mix, loop and event drain",
                warmup: 4,
                measured: 256,
            },
            |_| {
                output.fill(0.0);
                assert!(player.process(&mut output));
                player.drain_events().for_each(drop);
                std::hint::black_box(&output);
            },
        );
    }

    #[test]
    fn attaching_cached_backing_preserves_stream_playback_state() {
        let mut player = InnerAudioPlayer::new(1, 2);
        player.shared.set_sample_rate(48_000);
        player.shared.set_channels(2);
        player.shared.set_loaded(true);
        player.shared.set_state(PlaybackState::Playing);
        player.shared.set_loop_enabled(true);
        player.position = 20;
        player.position_phase = 234;
        player.frames_since_time_update = 17;
        player.loading_url = Some("https://example.test/audio.mp3".into());
        player.source.extend_from_slice(&[0.0; 20]);

        let mut cache = crate::cache::AudioCache::with_max_size_and_aggregate(
            usize::MAX,
            AudioAggregateLedger::process_global(),
        );
        let cached = cache
            .insert(
                "https://example.test/audio.mp3".into(),
                DecodedAudio {
                    samples: vec![0.25; 40],
                    sample_rate: 48_000,
                    channels: 2,
                },
            )
            .unwrap();
        player.attach_cached_backing(cached);

        assert_eq!(player.position, 20);
        assert_eq!(player.position_phase, 234);
        assert_eq!(player.shared.state(), PlaybackState::Playing);
        assert!(player.shared.loop_enabled());
        assert_eq!(player.frames_since_time_update, 17);
        assert!(player.loading_url.is_none());
        assert_eq!(player.source.len(), 40);
        assert_eq!(player.shared.duration_frames(), 20);
    }
    #[test]
    fn streaming_rejects_before_capacity_can_cross_pcm_ceiling() {
        let state = StreamingState::new();
        let (tx, rx) = mpsc::channel(2);
        let mut player = InnerAudioPlayer::new(1, 1);
        player
            .start_streaming("https://example.test/long".into(), rx, state)
            .unwrap();
        player.shared.set_sample_rate(48_000);
        player.shared.set_channels(1);
        player.source =
            AudioSource::Owned(vec![0.0; crate::limits::MAX_AUDIO_PCM_SAMPLES as usize - 4]);

        let mut pool = crate::streaming::PcmPool::new();
        let mut pcm = pool.take();
        pcm.buffer_mut().resize(8, 0.0);
        tx.try_send(StreamMsg::Samples {
            revision: crate::streaming::StreamRevision::INITIAL,
            pcm,
        })
        .unwrap();
        player.poll_stream();

        assert!(
            player.physical_pcm_bytes() <= crate::limits::MAX_AUDIO_PCM_BYTES as usize,
            "rejected stream growth must not leave capacity above the PCM ceiling"
        );
        assert_eq!(player.shared.state(), PlaybackState::Stopped);
        assert!(
            player
                .drain_events()
                .any(|event| event.event_type == InnerAudioEventType::Error)
        );
    }
}
