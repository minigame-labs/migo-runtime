//! # Audio Command Protocol
//!
//! Defines the command protocol for audio operations in the Migo engine.
//! All audio operations are performed asynchronously through message passing.
//!
//! ## API Categories
//!
//! ### WebAudio API
//!
//! Implements a subset of the W3C WebAudio API:
//! - `AudioContext` management
//! - `AudioBufferSourceNode` for sample-accurate playback
//! - `GainNode` for volume control
//! - Audio graph connections
//!
//! ### InnerAudioContext API
//!
//! Implements the Mini Program audio API:
//! - Simple play/pause/stop control
//! - URL-based loading with streaming support
//! - Volume, loop, and playback rate control
//! - Event callbacks (onPlay, onPause, onEnded, etc.)
//!
//! ## Thread Model
//!
//! Commands are sent from the JS runtime thread to the dedicated audio thread
//! via a bounded, non-blocking channel. Saturated sends are returned to the
//! caller, and responses (where needed) use oneshot channels.

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use crate::audio_resources::AudioSnapshot;
use crate::error::EngineResult;

/// Unique identifier for an AudioContext instance.
pub type AudioContextId = u32;

/// Unique identifier for an AudioBuffer.
pub type AudioBufferId = u32;

/// Unique identifier for an AudioNode (source, gain, etc.).
pub type AudioNodeId = u32;

/// Unique identifier for an InnerAudioContext instance.
pub type InnerAudioId = u32;

/// Response sender for async audio commands.
///
/// Commands that need a response (e.g., `CreateContext`, `DecodeAudioData`)
/// include a oneshot sender for the result.
pub type AudioResp<T> = oneshot::Sender<EngineResult<T>>;

/// Information about a decoded audio buffer.
///
/// What `op_audio_decode_audio_data` answers once the decode is adopted: `id`
/// is then the buffer's `AudioResourceRegistry` serial, the id every other
/// `AudioBuffer` op takes. (`CreateBuffer`'s answer has the same shape with a
/// context-local id.)
///
/// # Example Response
///
/// ```json
/// {
///   "id": 1,
///   "duration": 3.5,
///   "sample_rate": 48000,
///   "channels": 2,
///   "length": 168000
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioBufferInfo {
    /// Unique buffer identifier (used with `SetBuffer`).
    pub id: AudioBufferId,
    /// Duration in seconds.
    pub duration: f64,
    /// Sample rate in Hz (e.g., 44100, 48000).
    pub sample_rate: u32,
    /// Number of channels (1 = mono, 2 = stereo).
    pub channels: u32,
    /// Total number of sample frames.
    pub length: u32,
}

/// A finished decode: interleaved samples at the context's rate.
///
/// `frames * channels == samples.len()`, which the decoder guarantees and the
/// registry checks again when it adopts them.
#[derive(Debug)]
pub struct DecodedPcm {
    pub sample_rate: u32,
    pub channels: u32,
    pub frames: u32,
    pub samples: Vec<f32>,
}

/// Playback state of an AudioContext.
///
/// Follows the W3C AudioContext state machine:
/// - `Suspended` → `Running` (via resume)
/// - `Running` → `Suspended` (via suspend)
/// - Any → `Closed` (via close, terminal state)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioContextState {
    /// Audio processing is suspended (no output).
    Suspended,
    /// Audio is actively processing and producing output.
    Running,
    /// Context is closed and cannot be reused.
    Closed,
}

/// Audio command protocol.
///
/// All audio operations are expressed as commands sent to the audio thread.
/// Commands are categorized into:
///
/// - **Context**: Create, close, suspend, resume AudioContext
/// - **Buffer**: Decode audio data, release buffers
/// - **Nodes**: Create and configure AudioBufferSourceNode, GainNode
/// - **Graph**: Connect and disconnect audio nodes
/// - **InnerAudio**: InnerAudioContext operations
/// - **Lifecycle**: Thread management (shutdown)
///
/// # Fire-and-Forget vs Request-Response
///
/// - Commands without `resp` field are fire-and-forget (best effort)
/// - Commands with `resp: AudioResp<T>` require a response
pub enum AudioCmd {
    // ==================== Context ====================
    /// Create a new AudioContext with a JS-allocated id (fire-and-forget).
    /// The id is generated on the JS side so the context is usable synchronously
    /// (browser semantics); FIFO channel ordering guarantees this command is
    /// processed before any node op that references `ctx_id`.
    CreateContext {
        ctx_id: AudioContextId,
        sample_rate: Option<u32>,
    },

    /// Idempotently release an AudioContext from a GC finalizer.
    ReleaseContext { ctx_id: AudioContextId },

    /// Host restart barrier: discard older WebAudio work and release every
    /// native WebAudio context. Delivery and acknowledgement use the audio
    /// transport's reserved cleanup lane, not an in-band response.
    ReleaseAllContexts,

    /// Close an AudioContext and release resources
    CloseContext {
        ctx_id: AudioContextId,
        resp: AudioResp<()>,
    },

    /// Get the current state of an AudioContext
    GetContextState {
        ctx_id: AudioContextId,
        resp: AudioResp<AudioContextState>,
    },

    /// Resume a suspended AudioContext
    ResumeContext {
        ctx_id: AudioContextId,
        resp: AudioResp<()>,
    },

    /// Suspend an AudioContext
    SuspendContext {
        ctx_id: AudioContextId,
        resp: AudioResp<()>,
    },

    // ==================== Buffer ====================
    /// Decode audio data for `decodeAudioData`, at the context's rate.
    ///
    /// The samples are answered, not kept: the caller adopts them into the
    /// host's `AudioResourceRegistry` as the `AudioBuffer`'s frozen snapshot,
    /// so a decoded clip lives in one allocation that playback shares and
    /// JavaScript only copies if it reads the channels. The context is still
    /// named because a decode belongs to it -- closing it refuses the answer.
    /// Data is Arc-wrapped to avoid copying the entire compressed file from
    /// the JS thread to the audio/decode thread.
    DecodeAudioData {
        ctx_id: AudioContextId,
        data: std::sync::Arc<Vec<u8>>,
        resp: AudioResp<DecodedPcm>,
    },

    /// Idempotently release this context's map reference to an AudioBuffer.
    /// BufferSource nodes may retain the underlying PCM until they clear,
    /// replace, or finish. Fire-and-forget so it is safe from a GC finalizer.
    ReleaseBuffer {
        ctx_id: AudioContextId,
        buffer_id: AudioBufferId,
    },

    // ==================== Nodes ====================
    /// Idempotently mark an AudioNode as unreachable from JavaScript.
    ///
    /// Fire-and-forget so it is safe from a GC finalizer, and a *request* rather
    /// than a removal: JavaScript drops a whole `source -> gain -> destination`
    /// chain as one unreachable object graph, so this can arrive while the source
    /// is still playing through the node. The audio thread drops the node once
    /// nothing upstream can still feed it.
    ReleaseNode {
        ctx_id: AudioContextId,
        node_id: AudioNodeId,
    },

    /// Create an AudioBufferSourceNode (JS provides node_id, fire-and-forget)
    CreateBufferSource {
        ctx_id: AudioContextId,
        node_id: AudioNodeId,
    },

    /// Set or clear the buffer for an AudioBufferSourceNode (fire-and-forget).
    /// `ctx_id` is the claimed owner and is validated on the audio thread.
    SetBuffer {
        ctx_id: AudioContextId,
        node_id: AudioNodeId,
        buffer_id: Option<AudioBufferId>,
    },

    /// Atomically replace the native snapshot held by a source node.
    ///
    /// The snapshot is Arc-backed and already owns its PCM accounting permit;
    /// this command only transfers a reference to the audio thread.
    SetStartedBuffer {
        ctx_id: AudioContextId,
        node_id: AudioNodeId,
        buffer: Option<std::sync::Arc<AudioSnapshot>>,
    },

    /// Set a native snapshot and start the source in one FIFO command.
    /// Fire-and-forget so a queued command carries no PCM byte charge.
    StartBuffer {
        ctx_id: AudioContextId,
        node_id: AudioNodeId,
        buffer: Option<std::sync::Arc<AudioSnapshot>>,
        when: f64,
        offset: f64,
        duration: Option<f64>,
    },

    /// Start playback of an AudioBufferSourceNode
    Start {
        node_id: AudioNodeId,
        when: f64,
        offset: f64,
        duration: Option<f64>,
        resp: AudioResp<()>,
    },

    /// Stop playback of an AudioBufferSourceNode
    Stop {
        node_id: AudioNodeId,
        when: f64,
        resp: AudioResp<()>,
    },

    /// Set loop property (fire-and-forget)
    SetLoop {
        node_id: AudioNodeId,
        loop_enabled: bool,
        loop_start: f64,
        loop_end: f64,
    },

    /// Set playback rate
    SetPlaybackRate {
        node_id: AudioNodeId,
        rate: f32,
        resp: AudioResp<()>,
    },

    /// Create a GainNode (JS provides node_id, fire-and-forget)
    CreateGain {
        ctx_id: AudioContextId,
        node_id: AudioNodeId,
    },

    /// Set gain value (fire-and-forget)
    SetGainValue { node_id: AudioNodeId, value: f32 },

    /// Set an AudioParam's current value now, by node + param name (fire-and-forget).
    /// Used for direct `param.value = x` writes on bound params; sets the intrinsic
    /// value without inserting a timeline event.
    SetNodeParam {
        node_id: AudioNodeId,
        param_name: String,
        value: f32,
    },

    // ==================== Graph ====================
    /// Connect two nodes
    Connect {
        src: AudioNodeId,
        /// Output port on `src`. Only a ChannelSplitter has more than one.
        src_output: u32,
        dst: AudioNodeId,
        /// Input port on `dst`. Only a ChannelMerger has more than one.
        dst_input: u32,
        resp: AudioResp<()>,
    },

    /// Disconnect a node from all outputs or a specific destination
    Disconnect {
        node_id: AudioNodeId,
        dst: Option<AudioNodeId>,
        resp: AudioResp<()>,
    },

    // ==================== Phase 2 Nodes ====================
    /// Create an OscillatorNode (fire-and-forget)
    CreateOscillator {
        ctx_id: AudioContextId,
        node_id: AudioNodeId,
    },

    /// Set OscillatorNode type (fire-and-forget)
    SetOscillatorType {
        node_id: AudioNodeId,
        osc_type: String,
    },

    /// Start an OscillatorNode
    StartOscillator { node_id: AudioNodeId, when: f64 },

    /// Stop an OscillatorNode
    StopOscillator { node_id: AudioNodeId, when: f64 },

    /// Create a DelayNode (fire-and-forget)
    CreateDelay {
        ctx_id: AudioContextId,
        node_id: AudioNodeId,
        max_delay_time: f32,
    },

    /// Create a BiquadFilterNode (fire-and-forget)
    CreateBiquadFilter {
        ctx_id: AudioContextId,
        node_id: AudioNodeId,
    },

    /// Set BiquadFilterNode type (fire-and-forget)
    SetBiquadFilterType {
        node_id: AudioNodeId,
        filter_type: String,
    },

    /// Create a WaveShaperNode (fire-and-forget)
    CreateWaveShaper {
        ctx_id: AudioContextId,
        node_id: AudioNodeId,
    },

    /// Set WaveShaperNode curve (fire-and-forget)
    SetWaveShaperCurve {
        node_id: AudioNodeId,
        curve: Option<Vec<f32>>,
    },

    /// Set WaveShaperNode oversample (fire-and-forget)
    SetWaveShaperOversample {
        node_id: AudioNodeId,
        oversample: String,
    },

    /// Create an AnalyserNode (fire-and-forget)
    CreateAnalyser {
        ctx_id: AudioContextId,
        node_id: AudioNodeId,
    },

    /// Set AnalyserNode fft_size (fire-and-forget)
    SetAnalyserFftSize { node_id: AudioNodeId, fft_size: u32 },

    /// Set one of an AnalyserNode's scalar properties.
    ///
    /// `minDecibels`, `maxDecibels` and `smoothingTimeConstant` were JavaScript-only
    /// state: the setters stored a value that never reached the node doing the
    /// analysis, so the dB window and the smoothing the spec defines the frequency
    /// data in terms of were all ignored.
    SetAnalyserScalar {
        node_id: AudioNodeId,
        prop: String,
        value: f32,
    },

    /// Get AnalyserNode time domain data (byte)
    GetAnalyserByteTimeDomainData {
        node_id: AudioNodeId,
        resp: AudioResp<Vec<u8>>,
    },

    /// Get AnalyserNode time domain data (float)
    GetAnalyserFloatTimeDomainData {
        node_id: AudioNodeId,
        resp: AudioResp<Vec<f32>>,
    },

    // ==================== Phase 3 Nodes ====================
    /// Create a DynamicsCompressorNode (fire-and-forget)
    CreateDynamicsCompressor {
        ctx_id: AudioContextId,
        node_id: AudioNodeId,
    },

    /// Create a PannerNode (fire-and-forget)
    CreatePanner {
        ctx_id: AudioContextId,
        node_id: AudioNodeId,
    },

    /// Set PannerNode panning model (fire-and-forget)
    SetPanningModel { node_id: AudioNodeId, model: String },

    /// Set PannerNode distance model (fire-and-forget)
    SetDistanceModel { node_id: AudioNodeId, model: String },

    /// Set PannerNode scalar properties (fire-and-forget)
    SetPannerScalar {
        node_id: AudioNodeId,
        prop: String,
        value: f64,
    },

    /// Create a ChannelMergerNode (fire-and-forget)
    CreateChannelMerger {
        ctx_id: AudioContextId,
        node_id: AudioNodeId,
        number_of_inputs: u32,
    },

    /// Create a ChannelSplitterNode (fire-and-forget)
    CreateChannelSplitter {
        ctx_id: AudioContextId,
        node_id: AudioNodeId,
        number_of_outputs: u32,
    },

    /// Create a ConstantSourceNode (fire-and-forget)
    CreateConstantSource {
        ctx_id: AudioContextId,
        node_id: AudioNodeId,
    },

    /// Start a ConstantSourceNode
    StartConstantSource { node_id: AudioNodeId, when: f64 },

    /// Stop a ConstantSourceNode
    StopConstantSource { node_id: AudioNodeId, when: f64 },

    /// Create an IIRFilterNode (fire-and-forget)
    CreateIIRFilter {
        ctx_id: AudioContextId,
        node_id: AudioNodeId,
        feedforward: Vec<f64>,
        feedback: Vec<f64>,
    },

    // ==================== Frequency Response & Analysis ====================
    /// Get frequency response from BiquadFilterNode or IIRFilterNode
    GetFrequencyResponse {
        node_id: AudioNodeId,
        frequencies: Vec<f32>,
        resp: AudioResp<(Vec<f32>, Vec<f32>)>,
    },

    /// Get current compression reduction from DynamicsCompressorNode
    GetReduction {
        node_id: AudioNodeId,
        resp: AudioResp<f32>,
    },

    /// Get frequency domain data from AnalyserNode (byte, after FFT)
    GetAnalyserByteFrequencyData {
        node_id: AudioNodeId,
        resp: AudioResp<Vec<u8>>,
    },

    /// Get frequency domain data from AnalyserNode (float, after FFT)
    GetAnalyserFloatFrequencyData {
        node_id: AudioNodeId,
        resp: AudioResp<Vec<f32>>,
    },

    // ==================== AudioParam Automation ====================
    /// Schedule a value change at a specific time
    AudioParamSetValueAtTime {
        node_id: AudioNodeId,
        param_name: String,
        value: f32,
        time: f64,
    },

    /// Schedule a linear ramp to a value
    AudioParamLinearRamp {
        node_id: AudioNodeId,
        param_name: String,
        value: f32,
        end_time: f64,
    },

    /// Schedule an exponential ramp to a value
    AudioParamExponentialRamp {
        node_id: AudioNodeId,
        param_name: String,
        value: f32,
        end_time: f64,
    },

    /// Asymptotically approach a target value
    AudioParamSetTarget {
        node_id: AudioNodeId,
        param_name: String,
        target: f32,
        start_time: f64,
        time_constant: f64,
    },

    /// Cancel all scheduled events after a specific time
    AudioParamCancelScheduled {
        node_id: AudioNodeId,
        param_name: String,
        cancel_time: f64,
    },

    // ==================== Buffer Data Access ====================
    /// Create an empty audio buffer
    CreateBuffer {
        ctx_id: AudioContextId,
        channels: u32,
        length: u32,
        sample_rate: u32,
        resp: AudioResp<AudioBufferInfo>,
    },

    /// Get channel data from a buffer (single channel)
    GetChannelData {
        ctx_id: AudioContextId,
        buffer_id: AudioBufferId,
        channel: u32,
        resp: AudioResp<Vec<f32>>,
    },

    /// Copy data to a buffer channel
    CopyToChannel {
        ctx_id: AudioContextId,
        buffer_id: AudioBufferId,
        data: Vec<f32>,
        channel: u32,
        start: u32,
        resp: AudioResp<()>,
    },

    // ==================== MediaAudioPlayer ====================
    /// Create a MediaAudioPlayer
    CreateMediaAudioPlayer { id: u32 },

    /// Add an InnerAudioContext source to a MediaAudioPlayer
    MediaAudioPlayerAddSource {
        player_id: u32,
        source_id: InnerAudioId,
    },

    /// Remove an InnerAudioContext source from a MediaAudioPlayer
    MediaAudioPlayerRemoveSource {
        player_id: u32,
        source_id: InnerAudioId,
    },

    /// Start a MediaAudioPlayer
    MediaAudioPlayerStart { player_id: u32 },

    /// Stop a MediaAudioPlayer
    MediaAudioPlayerStop { player_id: u32 },

    /// Destroy a MediaAudioPlayer
    MediaAudioPlayerDestroy { player_id: u32 },

    // ==================== Lifecycle ====================
    /// Shutdown the audio thread
    Shutdown,

    /// Pause all audio processing (app going to background).
    ///
    /// Stops audio output and timeline advancement for all contexts and
    /// InnerAudioContext players. The audio thread stays alive and continues
    /// processing commands so it can receive `ResumeAll` or `Shutdown`.
    PauseAll,

    /// Resume all audio processing (app returning to foreground).
    ///
    /// Restarts audio output and timeline advancement.
    ResumeAll,

    // ==================== InnerAudioContext ====================
    /// Create an InnerAudioContext (JS provides id, fire-and-forget)
    CreateInnerAudio { id: InnerAudioId },

    /// Destroy an InnerAudioContext
    DestroyInnerAudio { id: InnerAudioId },

    /// Load audio data into InnerAudioContext (full load mode)
    InnerAudioLoad {
        id: InnerAudioId,
        data: Vec<u8>,
        resp: AudioResp<InnerAudioInfo>,
    },

    /// Load audio from URL with streaming (edge-download-edge-play)
    InnerAudioLoadUrl {
        id: InnerAudioId,
        url: String,
        resp: AudioResp<()>,
    },

    /// Play InnerAudioContext
    InnerAudioPlay { id: InnerAudioId },

    /// Pause InnerAudioContext
    InnerAudioPause { id: InnerAudioId },

    /// Stop InnerAudioContext
    InnerAudioStop { id: InnerAudioId },

    /// Seek InnerAudioContext to position (in seconds)
    InnerAudioSeek { id: InnerAudioId, position: f64 },

    /// Set InnerAudioContext volume (0.0 - 1.0)
    InnerAudioSetVolume { id: InnerAudioId, volume: f32 },

    /// Set InnerAudioContext loop
    InnerAudioSetLoop {
        id: InnerAudioId,
        loop_enabled: bool,
    },

    /// Set InnerAudioContext playback rate (0.5 - 2.0)
    InnerAudioSetPlaybackRate { id: InnerAudioId, rate: f32 },

    /// Set InnerAudioContext autoplay
    InnerAudioSetAutoplay { id: InnerAudioId, autoplay: bool },

    /// Get InnerAudioContext current state
    InnerAudioGetState {
        id: InnerAudioId,
        resp: AudioResp<InnerAudioState>,
    },

    /// Poll for pending InnerAudioContext events
    InnerAudioPollEvents {
        resp: AudioResp<Vec<InnerAudioEvent>>,
    },
}

impl AudioCmd {
    /// Heap payload retained while this command waits in the audio queue.
    /// Fixed-size command metadata is bounded by the queue's item limit.
    pub fn queued_payload_bytes(&self) -> usize {
        fn vector_bytes<T>(values: &Vec<T>) -> usize {
            values.capacity().saturating_mul(std::mem::size_of::<T>())
        }

        match self {
            Self::DecodeAudioData { data, .. } => data.capacity(),
            Self::SetNodeParam { param_name, .. }
            | Self::AudioParamSetValueAtTime { param_name, .. }
            | Self::AudioParamLinearRamp { param_name, .. }
            | Self::AudioParamExponentialRamp { param_name, .. }
            | Self::AudioParamSetTarget { param_name, .. }
            | Self::AudioParamCancelScheduled { param_name, .. } => param_name.capacity(),
            Self::SetOscillatorType { osc_type, .. } => osc_type.capacity(),
            Self::SetBiquadFilterType { filter_type, .. } => filter_type.capacity(),
            Self::SetWaveShaperCurve { curve, .. } => curve.as_ref().map_or(0, vector_bytes),
            Self::SetWaveShaperOversample { oversample, .. } => oversample.capacity(),
            Self::SetPanningModel { model, .. } | Self::SetDistanceModel { model, .. } => {
                model.capacity()
            }
            Self::SetPannerScalar { prop, .. } | Self::SetAnalyserScalar { prop, .. } => {
                prop.capacity()
            }
            Self::CreateIIRFilter {
                feedforward,
                feedback,
                ..
            } => vector_bytes(feedforward).saturating_add(vector_bytes(feedback)),
            Self::GetFrequencyResponse { frequencies, .. } => vector_bytes(frequencies),
            Self::CopyToChannel { data, .. } => vector_bytes(data),
            Self::InnerAudioLoad { data, .. } => data.capacity(),
            Self::InnerAudioLoadUrl { url, .. } => url.capacity(),
            Self::CreateContext { .. }
            | Self::ReleaseContext { .. }
            | Self::ReleaseNode { .. }
            | Self::ReleaseAllContexts
            | Self::CloseContext { .. }
            | Self::GetContextState { .. }
            | Self::ResumeContext { .. }
            | Self::SuspendContext { .. }
            | Self::ReleaseBuffer { .. }
            | Self::CreateBufferSource { .. }
            | Self::SetBuffer { .. }
            | Self::SetStartedBuffer { .. }
            | Self::StartBuffer { .. }
            | Self::Start { .. }
            | Self::Stop { .. }
            | Self::SetLoop { .. }
            | Self::SetPlaybackRate { .. }
            | Self::CreateGain { .. }
            | Self::SetGainValue { .. }
            | Self::Connect { .. }
            | Self::Disconnect { .. }
            | Self::CreateOscillator { .. }
            | Self::StartOscillator { .. }
            | Self::StopOscillator { .. }
            | Self::CreateDelay { .. }
            | Self::CreateBiquadFilter { .. }
            | Self::CreateWaveShaper { .. }
            | Self::CreateAnalyser { .. }
            | Self::SetAnalyserFftSize { .. }
            | Self::GetAnalyserByteTimeDomainData { .. }
            | Self::GetAnalyserFloatTimeDomainData { .. }
            | Self::CreateDynamicsCompressor { .. }
            | Self::CreatePanner { .. }
            | Self::CreateChannelMerger { .. }
            | Self::CreateChannelSplitter { .. }
            | Self::CreateConstantSource { .. }
            | Self::StartConstantSource { .. }
            | Self::StopConstantSource { .. }
            | Self::GetReduction { .. }
            | Self::GetAnalyserByteFrequencyData { .. }
            | Self::GetAnalyserFloatFrequencyData { .. }
            | Self::CreateBuffer { .. }
            | Self::GetChannelData { .. }
            | Self::CreateMediaAudioPlayer { .. }
            | Self::MediaAudioPlayerAddSource { .. }
            | Self::MediaAudioPlayerRemoveSource { .. }
            | Self::MediaAudioPlayerStart { .. }
            | Self::MediaAudioPlayerStop { .. }
            | Self::MediaAudioPlayerDestroy { .. }
            | Self::Shutdown
            | Self::PauseAll
            | Self::ResumeAll
            | Self::CreateInnerAudio { .. }
            | Self::DestroyInnerAudio { .. }
            | Self::InnerAudioPlay { .. }
            | Self::InnerAudioPause { .. }
            | Self::InnerAudioStop { .. }
            | Self::InnerAudioSeek { .. }
            | Self::InnerAudioSetVolume { .. }
            | Self::InnerAudioSetLoop { .. }
            | Self::InnerAudioSetPlaybackRate { .. }
            | Self::InnerAudioSetAutoplay { .. }
            | Self::InnerAudioGetState { .. }
            | Self::InnerAudioPollEvents { .. } => 0,
        }
    }
}

/// Information about loaded InnerAudioContext
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InnerAudioInfo {
    pub duration: f64,
    pub sample_rate: u32,
    pub channels: u32,
}

/// State of InnerAudioContext
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InnerAudioState {
    /// Current playback position in seconds
    pub current_time: f64,
    /// Total duration in seconds
    pub duration: f64,
    /// Whether audio is paused
    pub paused: bool,
    /// Current volume (0.0 - 1.0)
    pub volume: f32,
    /// Whether loop is enabled
    pub loop_enabled: bool,
    /// Current playback rate
    pub playback_rate: f32,
    /// Whether audio is loaded and ready to play
    pub buffered: bool,
}

// Re-export the unified InnerAudioEventType from host_cmd (single source of truth).
pub use super::host_cmd::InnerAudioEventType;

/// Event from InnerAudioContext
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InnerAudioEvent {
    /// ID of the InnerAudioContext that generated this event
    pub id: InnerAudioId,
    /// Type of event
    pub event_type: InnerAudioEventType,
    /// Current playback time when event occurred
    pub current_time: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_lifecycle_commands_are_context_scoped_and_fire_and_forget() {
        let release_context = AudioCmd::ReleaseContext { ctx_id: 7 };
        let release_all_contexts = AudioCmd::ReleaseAllContexts;
        let release = AudioCmd::ReleaseBuffer {
            ctx_id: 7,
            buffer_id: 11,
        };
        let bind = AudioCmd::SetBuffer {
            ctx_id: 7,
            node_id: 13,
            buffer_id: Some(11),
        };
        let clear = AudioCmd::SetBuffer {
            ctx_id: 7,
            node_id: 13,
            buffer_id: None,
        };

        assert_eq!(release_context.queued_payload_bytes(), 0);
        assert_eq!(release_all_contexts.queued_payload_bytes(), 0);
        assert_eq!(release.queued_payload_bytes(), 0);
        assert_eq!(bind.queued_payload_bytes(), 0);
        assert_eq!(clear.queued_payload_bytes(), 0);
    }

    #[test]
    fn snapshot_commands_are_fire_and_forget_and_unmetered() {
        let start = AudioCmd::StartBuffer {
            ctx_id: 7,
            node_id: 13,
            buffer: None,
            when: 0.0,
            offset: 0.0,
            duration: None,
        };
        let replace = AudioCmd::SetStartedBuffer {
            ctx_id: 7,
            node_id: 13,
            buffer: None,
        };

        assert_eq!(start.queued_payload_bytes(), 0);
        assert_eq!(replace.queued_payload_bytes(), 0);
    }
}
