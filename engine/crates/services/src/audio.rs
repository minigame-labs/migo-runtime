//! Web Audio, `InnerAudioContext` and `MediaAudioPlayer`, as the host runs them.
//!
//! Every audio op is the same three steps: check the arguments, build the
//! command the audio thread takes, send it -- and, for an op that answers,
//! wait for the answer the audio thread sends back. None of that needs a
//! script runtime, and all of it has to be identical whichever execution is
//! asking: a game that schedules a ramp with a negative time is refused with
//! the same words whether its JavaScript runs beside the ops or in WebKit's
//! WebContent process. So it lives here once; the embedded runtime's ops take
//! their arguments out of V8 and call these, and the external session's
//! service dispatcher takes them out of a service message and calls the same.
//!
//! What stays with each caller is what only it can do: the embedded op copies
//! and detaches a V8 `ArrayBuffer`, and checks a streamed source against its
//! network layer; the external dispatcher reads bytes off the wire.
//!
//! # Where PCM lives
//!
//! A decode is answered with its samples and adopted into the host's
//! [`AudioResourceRegistry`] as the `AudioBuffer`'s frozen snapshot
//! ([`Decoding::answer`]). Playing it reuses that snapshot; only reading its
//! channels makes a copy. So PCM crosses to JavaScript when, and only when,
//! content asks to read it -- which on iOS Performance+ means it does not
//! cross the process boundary at all for a game that only plays its sounds.
//!
//! # Errors
//!
//! Every failure is an `AudioError`, with the message the embedded op has
//! always thrown: a check's own sentence, or an engine error as
//! `[Code] summary (detail)`.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use shared::audio_channel::{AudioCommandPermit, AudioCommandReserveError, AudioCommandSendError};
use shared::audio_resources::{
    AudioBufferFormat, AudioBufferKey, AudioResourceRegistry, PreparedAudioSnapshot,
};
use shared::error::{EngineError, ErrorCode};
use shared::op_state::AudioSender;
use shared::protocol::audio_cmd::{
    AudioBufferId, AudioBufferInfo, AudioCmd, AudioContextId, AudioNodeId, AudioResp, DecodedPcm,
    InnerAudioId, InnerAudioInfo, InnerAudioState,
};
use shared::services::AudioPlatformService;
use shared::vfs::VirtualFS;
use tokio::io::AsyncReadExt;
use tokio::sync::oneshot;

use crate::ServiceError;

/// The URL type a caller's admission check for a streamed source takes.
pub use url::Url;

/// The class every audio failure is thrown as.
pub const CLASS_AUDIO_ERROR: &str = "AudioError";

/// Maximum compressed/encoded bytes one decode or load takes.
pub const MAX_ENCODED_AUDIO_BYTES: usize = 16 * 1024 * 1024;
/// Maximum bytes of one WaveShaper curve.
pub const MAX_WAVE_SHAPER_CURVE_BYTES: usize = 16 * 1024 * 1024;
/// Names and enum-like protocol strings never need to approach queue scale.
pub const MAX_AUDIO_CONTROL_STRING_BYTES: usize = 4 * 1024;
/// Frequency-response arrays are proportional to DSP work as well as queue use.
pub const MAX_FREQUENCY_RESPONSE_POINTS: usize = 16 * 1024;
/// Maximum bytes one `copyToChannel` writes.
pub const MAX_COPY_TO_CHANNEL_BYTES: usize = 64 * 1024 * 1024;
const LOCAL_AUDIO_READ_CHUNK_BYTES: usize = 8 * 1024;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// A failure with a sentence of its own.
pub fn audio_error(message: impl Into<String>) -> ServiceError {
    ServiceError::classed(CLASS_AUDIO_ERROR, message)
}

/// An engine error, in the form the audio ops have always shown it.
pub fn engine_error(error: EngineError) -> ServiceError {
    audio_error(match &error.detail {
        Some(detail) => format!("[{:?}] {} ({})", error.code, error.msg, detail),
        None => format!("[{:?}] {}", error.code, error.msg),
    })
}

/// A send the audio queue refused.
pub fn send_error(error: AudioCommandSendError) -> ServiceError {
    engine_error(match error {
        AudioCommandSendError::Full(_) => {
            EngineError::from_detail(ErrorCode::InputSaturated, "audio command queue is full")
        }
        AudioCommandSendError::ByteLimit(_) => EngineError::from_detail(
            ErrorCode::InputSaturated,
            "audio command queue byte limit exceeded",
        ),
        AudioCommandSendError::Disconnected(_) => {
            EngineError::from_detail(ErrorCode::Disconnected, "audio thread disconnected")
        }
    })
}

/// A reservation the audio queue refused.
pub fn reserve_error(error: AudioCommandReserveError) -> ServiceError {
    engine_error(match error {
        AudioCommandReserveError::Full => {
            EngineError::from_detail(ErrorCode::InputSaturated, "audio command queue is full")
        }
        AudioCommandReserveError::ByteLimit => EngineError::from_detail(
            ErrorCode::InputSaturated,
            "audio command queue byte limit exceeded",
        ),
        AudioCommandReserveError::Disconnected => {
            EngineError::from_detail(ErrorCode::Disconnected, "audio thread disconnected")
        }
    })
}

fn response_closed() -> ServiceError {
    audio_error("Response channel closed")
}

// ---------------------------------------------------------------------------
// Checks
//
// Public because a producer in another process mirrors the ones that guard a
// command -- a command has no answer, so its refusal has to happen before it
// is sent -- and the mirror is held to these by generated answers.
// ---------------------------------------------------------------------------

/// A time on the context's timeline: finite and not negative.
pub fn validate_scheduled_time(name: &str, value: f64) -> Result<f64, ServiceError> {
    if !value.is_finite() || value < 0.0 {
        return Err(audio_error(format!(
            "{name} must be a finite, non-negative number"
        )));
    }
    Ok(value)
}

/// A `start()` duration, where -1 is "until the end".
pub fn validate_optional_duration(value: f64) -> Result<Option<f64>, ServiceError> {
    if value == -1.0 {
        return Ok(None);
    }
    validate_scheduled_time("duration", value).map(Some)
}

/// A name or enum-like string, bounded so it cannot fill the queue.
pub fn validate_audio_control_string(field: &str, value: &str) -> Result<(), ServiceError> {
    if value.len() > MAX_AUDIO_CONTROL_STRING_BYTES {
        return Err(engine_error(EngineError::from_detail(
            ErrorCode::InputSaturated,
            format!(
                "{field} is {} bytes; limit is {MAX_AUDIO_CONTROL_STRING_BYTES}",
                value.len()
            ),
        )));
    }
    Ok(())
}

/// Encoded audio a decode or a load takes.
pub fn validate_encoded_audio_size(len: usize) -> Result<(), ServiceError> {
    if len > MAX_ENCODED_AUDIO_BYTES {
        return Err(engine_error(EngineError::from_detail(
            ErrorCode::InputSaturated,
            format!("encoded audio input is {len} bytes; limit is {MAX_ENCODED_AUDIO_BYTES} bytes"),
        )));
    }
    Ok(())
}

/// A WaveShaper curve's bytes: whole `f32`s, bounded.
pub fn validate_wave_shaper_curve_size(len: usize) -> Result<(), ServiceError> {
    if !len.is_multiple_of(size_of::<f32>()) {
        return Err(engine_error(EngineError::from_detail(
            ErrorCode::InvalidArgument,
            "WaveShaper curve bytes must be f32 aligned",
        )));
    }
    if len > MAX_WAVE_SHAPER_CURVE_BYTES {
        return Err(engine_error(EngineError::from_detail(
            ErrorCode::InputSaturated,
            format!("WaveShaper curve is {len} bytes; limit is {MAX_WAVE_SHAPER_CURVE_BYTES}"),
        )));
    }
    Ok(())
}

/// A frequency-response query's frequencies, as bytes: whole `f32`s, bounded.
pub fn validate_frequency_response_bytes(bytes: usize) -> Result<(), ServiceError> {
    if !bytes.is_multiple_of(size_of::<f32>()) {
        return Err(engine_error(EngineError::from_detail(
            ErrorCode::InvalidArgument,
            "frequency response bytes must be f32 aligned",
        )));
    }
    let points = bytes / size_of::<f32>();
    if points > MAX_FREQUENCY_RESPONSE_POINTS {
        return Err(engine_error(EngineError::from_detail(
            ErrorCode::InputSaturated,
            format!(
                "frequency response has {points} points; limit is {MAX_FREQUENCY_RESPONSE_POINTS}"
            ),
        )));
    }
    Ok(())
}

/// `copyToChannel`'s data, as bytes: whole `f32`s, bounded.
pub fn validate_copy_to_channel_size(len: usize) -> Result<(), ServiceError> {
    if !len.is_multiple_of(size_of::<f32>()) {
        return Err(engine_error(EngineError::from_detail(
            ErrorCode::InvalidArgument,
            "copyToChannel data bytes must be f32 aligned",
        )));
    }
    if len > MAX_COPY_TO_CHANNEL_BYTES {
        return Err(engine_error(EngineError::from_detail(
            ErrorCode::InputSaturated,
            format!("copyToChannel data is {len} bytes; limit is {MAX_COPY_TO_CHANNEL_BYTES}"),
        )));
    }
    Ok(())
}

/// Loop points: any finite numbers.
pub fn validate_loop_points(loop_start: f64, loop_end: f64) -> Result<(), ServiceError> {
    if !loop_start.is_finite() || !loop_end.is_finite() {
        return Err(audio_error("loopStart and loopEnd must be finite numbers"));
    }
    Ok(())
}

/// IIR coefficients, as Web Audio requires them -- non-empty, at most 20 long,
/// `feedback[0]` not zero, all finite -- so the audio thread never divides by
/// an empty length.
pub fn validate_iir_coefficients(
    feedforward: &[f64],
    feedback: &[f64],
) -> Result<(), ServiceError> {
    if feedforward.is_empty() || feedback.is_empty() {
        return Err(audio_error(
            "createIIRFilter: feedforward and feedback must be non-empty",
        ));
    }
    if feedforward.len() > 20 || feedback.len() > 20 {
        return Err(audio_error(
            "createIIRFilter: coefficient arrays must have at most 20 elements",
        ));
    }
    if feedback[0] == 0.0 {
        return Err(audio_error("createIIRFilter: feedback[0] must not be zero"));
    }
    if feedforward
        .iter()
        .chain(feedback.iter())
        .any(|v| !v.is_finite())
    {
        return Err(audio_error("createIIRFilter: coefficients must be finite"));
    }
    Ok(())
}

/// Little-endian `f32`s, the way typed-array bytes arrive. The length has been
/// checked to be whole samples.
fn le_f32s(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(size_of::<f32>())
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

// ---------------------------------------------------------------------------
// Sending
// ---------------------------------------------------------------------------

/// An answer the audio thread owes.
///
/// The command has been sent -- in call order, with every command sent before
/// it -- when this exists; awaiting it only collects the answer.
#[must_use = "the command is sent; dropping this discards its answer"]
pub struct Pending<T>(oneshot::Receiver<shared::error::EngineResult<T>>);

impl<T> Pending<T> {
    pub async fn answer(self) -> Result<T, ServiceError> {
        self.0
            .await
            .map_err(|_| response_closed())?
            .map_err(engine_error)
    }
}

fn send(tx: &AudioSender, command: AudioCmd) -> Result<(), ServiceError> {
    tx.send(command).map_err(send_error)
}

fn request<T>(
    tx: &AudioSender,
    command: impl FnOnce(AudioResp<T>) -> AudioCmd,
) -> Result<Pending<T>, ServiceError> {
    let (resp, answer) = oneshot::channel();
    send(tx, command(resp))?;
    Ok(Pending(answer))
}

/// Reserve queue room for `bytes` of payload before copying it.
pub fn reserve(tx: &AudioSender, bytes: usize) -> Result<AudioCommandPermit, ServiceError> {
    tx.try_reserve_data(bytes).map_err(reserve_error)
}

fn send_reserved(
    tx: &AudioSender,
    command: AudioCmd,
    permit: AudioCommandPermit,
) -> Result<(), ServiceError> {
    tx.send_reserved(command, permit).map_err(send_error)
}

/// A command carrying one bounded string: checked, reserved, then sent.
fn send_named(
    tx: &AudioSender,
    field: &str,
    value: &str,
    command: impl FnOnce(String) -> AudioCmd,
) -> Result<(), ServiceError> {
    validate_audio_control_string(field, value)?;
    let permit = reserve(tx, value.len())?;
    send_reserved(tx, command(value.to_owned()), permit)
}

// ---------------------------------------------------------------------------
// AudioContext
// ---------------------------------------------------------------------------

/// Create a context with the caller's id; 0 asks for the device's rate.
///
/// The id is the caller's so `new AudioContext()` is usable at once, and the
/// command shares one FIFO with every later node command, so the context is
/// created before any node that names it.
pub fn create_context(
    tx: &AudioSender,
    ctx_id: AudioContextId,
    sample_rate: u32,
) -> Result<(), ServiceError> {
    send(
        tx,
        AudioCmd::CreateContext {
            ctx_id,
            sample_rate: (sample_rate != 0).then_some(sample_rate),
        },
    )
}

pub fn close_context(
    tx: &AudioSender,
    ctx_id: AudioContextId,
) -> Result<Pending<()>, ServiceError> {
    request(tx, |resp| AudioCmd::CloseContext { ctx_id, resp })
}

/// Release a context whose wrapper was collected: idempotent, and carrying no
/// answer that could outlive it.
pub fn release_context(tx: &AudioSender, ctx_id: AudioContextId) -> Result<(), ServiceError> {
    send(tx, AudioCmd::ReleaseContext { ctx_id })
}

/// Mark a node unreachable from JavaScript. A request, not a removal: it is
/// dropped once nothing upstream can feed it, so this cannot cut a sound short.
pub fn release_node(
    tx: &AudioSender,
    ctx_id: AudioContextId,
    node_id: AudioNodeId,
) -> Result<(), ServiceError> {
    send(tx, AudioCmd::ReleaseNode { ctx_id, node_id })
}

pub fn resume_context(
    tx: &AudioSender,
    ctx_id: AudioContextId,
) -> Result<Pending<()>, ServiceError> {
    request(tx, |resp| AudioCmd::ResumeContext { ctx_id, resp })
}

pub fn suspend_context(
    tx: &AudioSender,
    ctx_id: AudioContextId,
) -> Result<Pending<()>, ServiceError> {
    request(tx, |resp| AudioCmd::SuspendContext { ctx_id, resp })
}

// ---------------------------------------------------------------------------
// AudioBuffer
// ---------------------------------------------------------------------------

/// The runtime an `AudioBuffer` belongs to, and the registry that owns it.
///
/// Buffer ids are registry serials scoped to a runtime generation, so a
/// retired runtime's finalizer can never name its replacement's buffer.
#[derive(Clone, Copy)]
pub struct BufferScope<'a> {
    pub tx: &'a AudioSender,
    pub runtime_generation: i64,
}

impl BufferScope<'_> {
    fn resources(&self) -> Result<&AudioResourceRegistry, ServiceError> {
        self.tx.resources().map_err(engine_error)
    }

    fn key(&self, serial: AudioBufferId) -> AudioBufferKey {
        AudioBufferKey {
            runtime_generation: self.runtime_generation,
            serial,
        }
    }
}

/// Room in the queue for `len` bytes of encoded audio -- a decode's input or
/// a load's -- taken before they are copied: bounded first, so an oversized
/// input is refused before it is allocated.
pub fn reserve_encoded(tx: &AudioSender, len: usize) -> Result<AudioCommandPermit, ServiceError> {
    validate_encoded_audio_size(len)?;
    reserve(tx, len)
}

/// A decode in flight. See [`Decoding::answer`].
#[must_use = "the decode is sent; dropping this discards the buffer"]
pub struct Decoding {
    pcm: Pending<DecodedPcm>,
    resources: AudioResourceRegistry,
    runtime_generation: i64,
}

/// Decode `data` for `ctx_id`. `permit` is [`reserve_encoded`]'s, for exactly
/// these bytes.
pub fn decode_audio_data(
    scope: BufferScope<'_>,
    ctx_id: AudioContextId,
    data: Vec<u8>,
    permit: AudioCommandPermit,
) -> Result<Decoding, ServiceError> {
    let resources = scope.resources()?.clone();
    let (resp, answer) = oneshot::channel();
    send_reserved(
        scope.tx,
        AudioCmd::DecodeAudioData {
            ctx_id,
            data: Arc::new(data),
            resp,
        },
        permit,
    )?;
    Ok(Decoding {
        pcm: Pending(answer),
        resources,
        runtime_generation: scope.runtime_generation,
    })
}

impl Decoding {
    /// The decoded buffer, adopted as a frozen registry entry: its `id` is the
    /// serial every `AudioBuffer` op takes, and it has no JavaScript backing
    /// until content reads it.
    pub async fn answer(self) -> Result<AudioBufferInfo, ServiceError> {
        let pcm = self.pcm.answer().await?;
        let format = AudioBufferFormat {
            channels: pcm.channels,
            frames: pcm.frames,
            sample_rate: pcm.sample_rate,
        };
        let lease = self
            .resources
            .adopt_frozen(self.runtime_generation, format, pcm.samples)
            .map_err(engine_error)?;
        Ok(AudioBufferInfo {
            id: lease.key().serial,
            duration: f64::from(pcm.frames) / f64::from(pcm.sample_rate),
            sample_rate: pcm.sample_rate,
            channels: pcm.channels,
            length: pcm.frames,
        })
    }
}

/// Admit a new writable `AudioBuffer` before JavaScript allocates its backing.
pub fn reserve_buffer(
    scope: BufferScope<'_>,
    channels: u32,
    length: u32,
    sample_rate: u32,
) -> Result<AudioBufferId, ServiceError> {
    scope
        .resources()?
        .reserve_backing(
            scope.runtime_generation,
            AudioBufferFormat {
                channels,
                frames: length,
                sample_rate,
            },
        )
        .map(|lease| lease.key().serial)
        .map_err(engine_error)
}

/// Release a buffer: after a failed allocation, or from its finalizer. A stale
/// or unknown id is a no-op by design, as is a runtime with no registry.
pub fn release_buffer(scope: BufferScope<'_>, buffer_id: AudioBufferId) {
    if let Ok(resources) = scope.resources() {
        resources.release_buffer(scope.key(buffer_id));
    }
}

/// A frozen buffer's channels, as one exact channel-major allocation that
/// becomes its writable backing.
pub fn materialize_buffer(
    scope: BufferScope<'_>,
    buffer_id: AudioBufferId,
) -> Result<Vec<f32>, ServiceError> {
    scope
        .resources()?
        .prepare_materialize(scope.key(buffer_id))
        .map(|prepared| prepared.commit())
        .map_err(engine_error)
}

/// A writable buffer's planar PCM, wherever the caller holds it.
///
/// The embedded op holds a V8 `ArrayBuffer` it has to validate while
/// JavaScript is paused; the external one holds bytes from a message. Either
/// way what the registry needs is the snapshot prepared from them, for the
/// byte length the entry's format implies.
pub trait PlanarBacking {
    fn prepare(
        self,
        resources: &AudioResourceRegistry,
        key: AudioBufferKey,
        expected_bytes: usize,
    ) -> Result<PreparedAudioSnapshot, ServiceError>;
}

/// Planar PCM that crossed as little-endian bytes.
pub struct LittleEndianPlanar<'a>(pub &'a [u8]);

impl PlanarBacking for LittleEndianPlanar<'_> {
    fn prepare(
        self,
        resources: &AudioResourceRegistry,
        key: AudioBufferKey,
        expected_bytes: usize,
    ) -> Result<PreparedAudioSnapshot, ServiceError> {
        if self.0.len() != expected_bytes {
            return Err(audio_error(
                "AudioBuffer backing is detached, non-detachable, or has an invalid length",
            ));
        }
        resources
            .prepare_snapshot_from_le_bytes(key, self.0)
            .map_err(engine_error)
    }
}

/// When a started source plays: `when`, `offset` and `duration` as `start()`
/// takes them, duration -1 meaning to the end.
#[derive(Clone, Copy, Debug)]
pub struct StartTiming {
    pub when: f64,
    pub offset: f64,
    pub duration: f64,
}

/// Start a buffer source (`timing` given) or give a started one its buffer.
///
/// The buffer's PCM is published as an immutable snapshot the node shares: a
/// frozen buffer's own, or one frozen now from `backing`, the writable planar
/// PCM JavaScript holds. `on_published` runs only once the command is
/// accepted, and in the same step: it is where the caller gives up that
/// backing (the embedded op detaches it), so a refused command leaves the
/// buffer entirely usable.
pub fn publish_buffer(
    scope: BufferScope<'_>,
    ctx_id: AudioContextId,
    node_id: AudioNodeId,
    buffer_id: AudioBufferId,
    backing: Option<impl PlanarBacking>,
    timing: Option<StartTiming>,
    on_published: impl FnOnce(),
) -> Result<(), ServiceError> {
    let timing = match timing {
        Some(StartTiming {
            when,
            offset,
            duration,
        }) => Some((
            validate_scheduled_time("when", when)?,
            validate_scheduled_time("offset", offset)?,
            validate_optional_duration(duration)?,
        )),
        None => None,
    };
    let key = (buffer_id != 0).then(|| scope.key(buffer_id));
    if key.is_none() && backing.is_some() {
        return Err(audio_error(
            "a null AudioBuffer id cannot carry a backing store",
        ));
    }
    // Claim the queue slot before copying as much as 64 MiB of PCM. A failed
    // preparation drops this permit, and a later restart fence prevents the
    // commit below from running.
    let permit = reserve(scope.tx, 0)?;
    let prepared = match key {
        None => None,
        Some(key) => {
            let resources = scope.resources()?;
            Some(match backing {
                Some(backing) => {
                    let format = resources
                        .format(key)
                        .ok_or_else(|| audio_error("unknown AudioBuffer id"))?;
                    let expected_bytes = usize::try_from(format.channels)
                        .ok()
                        .and_then(|channels| {
                            usize::try_from(format.frames)
                                .ok()
                                .and_then(|frames| channels.checked_mul(frames))
                        })
                        .and_then(|samples| samples.checked_mul(size_of::<f32>()))
                        .ok_or_else(|| audio_error("AudioBuffer backing size overflow"))?;
                    backing.prepare(resources, key, expected_bytes)?
                }
                None => resources
                    .prepare_snapshot(key, None)
                    .map_err(engine_error)?,
            })
        }
    };
    let snapshot = prepared.as_ref().map(PreparedAudioSnapshot::snapshot);
    let command = match timing {
        Some((when, offset, duration)) => AudioCmd::StartBuffer {
            ctx_id,
            node_id,
            buffer: snapshot,
            when,
            offset,
            duration,
        },
        None => AudioCmd::SetStartedBuffer {
            ctx_id,
            node_id,
            buffer: snapshot,
        },
    };
    scope
        .tx
        .send_reserved_committing(command, permit, move || {
            on_published();
            if let Some(prepared) = prepared {
                let _ = prepared.commit();
            }
        })
        .map_err(send_error)
}

// ---------------------------------------------------------------------------
// Source and graph nodes
// ---------------------------------------------------------------------------

/// Create one of the nodes that take nothing but their ids.
pub fn create_node(
    tx: &AudioSender,
    kind: NodeKind,
    ctx_id: AudioContextId,
    node_id: AudioNodeId,
) -> Result<(), ServiceError> {
    send(
        tx,
        match kind {
            NodeKind::BufferSource => AudioCmd::CreateBufferSource { ctx_id, node_id },
            NodeKind::Gain => AudioCmd::CreateGain { ctx_id, node_id },
            NodeKind::Oscillator => AudioCmd::CreateOscillator { ctx_id, node_id },
            NodeKind::BiquadFilter => AudioCmd::CreateBiquadFilter { ctx_id, node_id },
            NodeKind::WaveShaper => AudioCmd::CreateWaveShaper { ctx_id, node_id },
            NodeKind::Analyser => AudioCmd::CreateAnalyser { ctx_id, node_id },
            NodeKind::DynamicsCompressor => AudioCmd::CreateDynamicsCompressor { ctx_id, node_id },
            NodeKind::Panner => AudioCmd::CreatePanner { ctx_id, node_id },
            NodeKind::ConstantSource => AudioCmd::CreateConstantSource { ctx_id, node_id },
        },
    )
}

/// The nodes [`create_node`] makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeKind {
    BufferSource,
    Gain,
    Oscillator,
    BiquadFilter,
    WaveShaper,
    Analyser,
    DynamicsCompressor,
    Panner,
    ConstantSource,
}

pub fn create_delay(
    tx: &AudioSender,
    ctx_id: AudioContextId,
    node_id: AudioNodeId,
    max_delay_time: f32,
) -> Result<(), ServiceError> {
    send(
        tx,
        AudioCmd::CreateDelay {
            ctx_id,
            node_id,
            max_delay_time,
        },
    )
}

pub fn create_channel_merger(
    tx: &AudioSender,
    ctx_id: AudioContextId,
    node_id: AudioNodeId,
    number_of_inputs: u32,
) -> Result<(), ServiceError> {
    send(
        tx,
        AudioCmd::CreateChannelMerger {
            ctx_id,
            node_id,
            number_of_inputs,
        },
    )
}

pub fn create_channel_splitter(
    tx: &AudioSender,
    ctx_id: AudioContextId,
    node_id: AudioNodeId,
    number_of_outputs: u32,
) -> Result<(), ServiceError> {
    send(
        tx,
        AudioCmd::CreateChannelSplitter {
            ctx_id,
            node_id,
            number_of_outputs,
        },
    )
}

pub fn create_iir_filter(
    tx: &AudioSender,
    ctx_id: AudioContextId,
    node_id: AudioNodeId,
    feedforward: Vec<f64>,
    feedback: Vec<f64>,
) -> Result<(), ServiceError> {
    validate_iir_coefficients(&feedforward, &feedback)?;
    send(
        tx,
        AudioCmd::CreateIIRFilter {
            ctx_id,
            node_id,
            feedforward,
            feedback,
        },
    )
}

pub fn stop(
    tx: &AudioSender,
    node_id: AudioNodeId,
    when: f64,
) -> Result<Pending<()>, ServiceError> {
    let when = validate_scheduled_time("when", when)?;
    request(tx, |resp| AudioCmd::Stop {
        node_id,
        when,
        resp,
    })
}

pub fn set_loop(
    tx: &AudioSender,
    node_id: AudioNodeId,
    loop_enabled: bool,
    loop_start: f64,
    loop_end: f64,
) -> Result<(), ServiceError> {
    validate_loop_points(loop_start, loop_end)?;
    send(
        tx,
        AudioCmd::SetLoop {
            node_id,
            loop_enabled,
            loop_start,
            loop_end,
        },
    )
}

pub fn set_gain_value(
    tx: &AudioSender,
    node_id: AudioNodeId,
    value: f32,
) -> Result<(), ServiceError> {
    send(tx, AudioCmd::SetGainValue { node_id, value })
}

/// Set an `AudioParam`'s value now, by node and parameter name.
pub fn set_node_param(
    tx: &AudioSender,
    node_id: AudioNodeId,
    param_name: &str,
    value: f32,
) -> Result<(), ServiceError> {
    send_named(tx, "AudioParam name", param_name, |param_name| {
        AudioCmd::SetNodeParam {
            node_id,
            param_name,
            value,
        }
    })
}

pub fn connect(
    tx: &AudioSender,
    src: AudioNodeId,
    dst: AudioNodeId,
    src_output: u32,
    dst_input: u32,
) -> Result<Pending<()>, ServiceError> {
    request(tx, |resp| AudioCmd::Connect {
        src,
        src_output,
        dst,
        dst_input,
        resp,
    })
}

pub fn disconnect(tx: &AudioSender, node_id: AudioNodeId) -> Result<Pending<()>, ServiceError> {
    request(tx, |resp| AudioCmd::Disconnect {
        node_id,
        dst: None,
        resp,
    })
}

/// One of `AudioParam`'s automation calls.
#[derive(Clone, Copy, Debug)]
pub enum Automation {
    SetValueAtTime {
        value: f32,
        time: f64,
    },
    LinearRamp {
        value: f32,
        end_time: f64,
    },
    ExponentialRamp {
        value: f32,
        end_time: f64,
    },
    SetTarget {
        target: f32,
        start_time: f64,
        time_constant: f64,
    },
    CancelScheduled {
        cancel_time: f64,
    },
}

pub fn automate(
    tx: &AudioSender,
    node_id: AudioNodeId,
    param_name: &str,
    automation: Automation,
) -> Result<(), ServiceError> {
    send_named(
        tx,
        "AudioParam name",
        param_name,
        |param_name| match automation {
            Automation::SetValueAtTime { value, time } => AudioCmd::AudioParamSetValueAtTime {
                node_id,
                param_name,
                value,
                time,
            },
            Automation::LinearRamp { value, end_time } => AudioCmd::AudioParamLinearRamp {
                node_id,
                param_name,
                value,
                end_time,
            },
            Automation::ExponentialRamp { value, end_time } => {
                AudioCmd::AudioParamExponentialRamp {
                    node_id,
                    param_name,
                    value,
                    end_time,
                }
            }
            Automation::SetTarget {
                target,
                start_time,
                time_constant,
            } => AudioCmd::AudioParamSetTarget {
                node_id,
                param_name,
                target,
                start_time,
                time_constant,
            },
            Automation::CancelScheduled { cancel_time } => AudioCmd::AudioParamCancelScheduled {
                node_id,
                param_name,
                cancel_time,
            },
        },
    )
}

/// The scheduled sources: an oscillator or a constant source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScheduledSource {
    Oscillator,
    ConstantSource,
}

pub fn start_source(
    tx: &AudioSender,
    source: ScheduledSource,
    node_id: AudioNodeId,
    when: f64,
) -> Result<(), ServiceError> {
    let when = validate_scheduled_time("when", when)?;
    send(
        tx,
        match source {
            ScheduledSource::Oscillator => AudioCmd::StartOscillator { node_id, when },
            ScheduledSource::ConstantSource => AudioCmd::StartConstantSource { node_id, when },
        },
    )
}

pub fn stop_source(
    tx: &AudioSender,
    source: ScheduledSource,
    node_id: AudioNodeId,
    when: f64,
) -> Result<(), ServiceError> {
    let when = validate_scheduled_time("when", when)?;
    send(
        tx,
        match source {
            ScheduledSource::Oscillator => AudioCmd::StopOscillator { node_id, when },
            ScheduledSource::ConstantSource => AudioCmd::StopConstantSource { node_id, when },
        },
    )
}

/// The string-valued node settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeSetting {
    OscillatorType,
    BiquadFilterType,
    WaveShaperOversample,
    PanningModel,
    DistanceModel,
}

impl NodeSetting {
    /// The name a refusal gives the value.
    pub fn field(self) -> &'static str {
        match self {
            Self::OscillatorType => "oscillator type",
            Self::BiquadFilterType => "biquad filter type",
            Self::WaveShaperOversample => "WaveShaper oversample",
            Self::PanningModel => "panning model",
            Self::DistanceModel => "distance model",
        }
    }
}

pub fn set_node_setting(
    tx: &AudioSender,
    setting: NodeSetting,
    node_id: AudioNodeId,
    value: &str,
) -> Result<(), ServiceError> {
    send_named(tx, setting.field(), value, |value| match setting {
        NodeSetting::OscillatorType => AudioCmd::SetOscillatorType {
            node_id,
            osc_type: value,
        },
        NodeSetting::BiquadFilterType => AudioCmd::SetBiquadFilterType {
            node_id,
            filter_type: value,
        },
        NodeSetting::WaveShaperOversample => AudioCmd::SetWaveShaperOversample {
            node_id,
            oversample: value,
        },
        NodeSetting::PanningModel => AudioCmd::SetPanningModel {
            node_id,
            model: value,
        },
        NodeSetting::DistanceModel => AudioCmd::SetDistanceModel {
            node_id,
            model: value,
        },
    })
}

/// Set a WaveShaper's curve from its `f32` bytes, or clear it.
pub fn set_wave_shaper_curve(
    tx: &AudioSender,
    node_id: AudioNodeId,
    curve_bytes: Option<&[u8]>,
) -> Result<(), ServiceError> {
    let byte_len = curve_bytes.map_or(0, <[u8]>::len);
    validate_wave_shaper_curve_size(byte_len)?;
    let permit = reserve(tx, byte_len)?;
    send_reserved(
        tx,
        AudioCmd::SetWaveShaperCurve {
            node_id,
            curve: curve_bytes.map(le_f32s),
        },
        permit,
    )
}

pub fn set_analyser_fft_size(
    tx: &AudioSender,
    node_id: AudioNodeId,
    fft_size: u32,
) -> Result<(), ServiceError> {
    send(tx, AudioCmd::SetAnalyserFftSize { node_id, fft_size })
}

/// Set one of an AnalyserNode's scalar properties.
pub fn set_analyser_scalar(
    tx: &AudioSender,
    node_id: AudioNodeId,
    prop: &str,
    value: f32,
) -> Result<(), ServiceError> {
    send_named(tx, "analyser property", prop, |prop| {
        AudioCmd::SetAnalyserScalar {
            node_id,
            prop,
            value,
        }
    })
}

/// Set one of a PannerNode's scalar properties.
pub fn set_panner_scalar(
    tx: &AudioSender,
    node_id: AudioNodeId,
    prop: &str,
    value: f64,
) -> Result<(), ServiceError> {
    send_named(tx, "panner property", prop, |prop| {
        AudioCmd::SetPannerScalar {
            node_id,
            prop,
            value,
        }
    })
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

/// The analyser reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnalyserRead {
    ByteTimeDomain,
    FloatTimeDomain,
    ByteFrequency,
    FloatFrequency,
}

/// What an analyser read answers: bytes, or `f32`s.
pub enum AnalyserData {
    Bytes(Vec<u8>),
    Floats(Vec<f32>),
}

pub fn read_analyser(
    tx: &AudioSender,
    read: AnalyserRead,
    node_id: AudioNodeId,
) -> Result<impl Future<Output = Result<AnalyserData, ServiceError>> + use<>, ServiceError> {
    enum Reading {
        Bytes(Pending<Vec<u8>>),
        Floats(Pending<Vec<f32>>),
    }
    let reading = match read {
        AnalyserRead::ByteTimeDomain => Reading::Bytes(request(tx, |resp| {
            AudioCmd::GetAnalyserByteTimeDomainData { node_id, resp }
        })?),
        AnalyserRead::ByteFrequency => Reading::Bytes(request(tx, |resp| {
            AudioCmd::GetAnalyserByteFrequencyData { node_id, resp }
        })?),
        AnalyserRead::FloatTimeDomain => Reading::Floats(request(tx, |resp| {
            AudioCmd::GetAnalyserFloatTimeDomainData { node_id, resp }
        })?),
        AnalyserRead::FloatFrequency => Reading::Floats(request(tx, |resp| {
            AudioCmd::GetAnalyserFloatFrequencyData { node_id, resp }
        })?),
    };
    Ok(async move {
        Ok(match reading {
            Reading::Bytes(pending) => AnalyserData::Bytes(pending.answer().await?),
            Reading::Floats(pending) => AnalyserData::Floats(pending.answer().await?),
        })
    })
}

/// A filter's magnitude and phase response at `frequencies` (`f32` bytes).
pub fn get_frequency_response(
    tx: &AudioSender,
    node_id: AudioNodeId,
    frequencies: &[u8],
) -> Result<Pending<(Vec<f32>, Vec<f32>)>, ServiceError> {
    validate_frequency_response_bytes(frequencies.len())?;
    let permit = reserve(tx, frequencies.len())?;
    let (resp, answer) = oneshot::channel();
    send_reserved(
        tx,
        AudioCmd::GetFrequencyResponse {
            node_id,
            frequencies: le_f32s(frequencies),
            resp,
        },
        permit,
    )?;
    Ok(Pending(answer))
}

/// A DynamicsCompressor's current gain reduction.
pub fn get_reduction(tx: &AudioSender, node_id: AudioNodeId) -> Result<Pending<f32>, ServiceError> {
    request(tx, |resp| AudioCmd::GetReduction { node_id, resp })
}

// ---------------------------------------------------------------------------
// Platform audio options
// ---------------------------------------------------------------------------

/// `setInnerAudioOption`, through the platform's audio service. Without one the
/// option cannot be honoured, and saying so is the answer.
pub fn set_inner_audio_option(
    platform: Option<&dyn AudioPlatformService>,
    mix_with_other: bool,
    obey_mute_switch: bool,
    speaker_on: bool,
) -> Result<(), ServiceError> {
    match platform {
        Some(platform) => platform
            .set_inner_audio_option(mix_with_other, obey_mute_switch, speaker_on)
            .map_err(|error| audio_error(error.message)),
        None => Err(audio_error("setInnerAudioOption:fail not supported")),
    }
}

/// The recorder's input sources, from the platform's audio service.
pub fn get_available_audio_sources(
    platform: Option<&dyn AudioPlatformService>,
) -> Result<Vec<String>, ServiceError> {
    match platform {
        Some(platform) => platform
            .get_available_audio_sources()
            .map_err(|error| audio_error(error.message)),
        None => Err(audio_error("getAvailableAudioSources:fail not supported")),
    }
}

// ---------------------------------------------------------------------------
// MediaAudioPlayer
// ---------------------------------------------------------------------------

/// The MediaAudioPlayer commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayerCommand {
    Create,
    AddSource(InnerAudioId),
    RemoveSource(InnerAudioId),
    Start,
    Stop,
    Destroy,
}

pub fn media_audio_player(
    tx: &AudioSender,
    player_id: u32,
    command: PlayerCommand,
) -> Result<(), ServiceError> {
    send(
        tx,
        match command {
            PlayerCommand::Create => AudioCmd::CreateMediaAudioPlayer { id: player_id },
            PlayerCommand::AddSource(source_id) => AudioCmd::MediaAudioPlayerAddSource {
                player_id,
                source_id,
            },
            PlayerCommand::RemoveSource(source_id) => AudioCmd::MediaAudioPlayerRemoveSource {
                player_id,
                source_id,
            },
            PlayerCommand::Start => AudioCmd::MediaAudioPlayerStart { player_id },
            PlayerCommand::Stop => AudioCmd::MediaAudioPlayerStop { player_id },
            PlayerCommand::Destroy => AudioCmd::MediaAudioPlayerDestroy { player_id },
        },
    )
}

// ---------------------------------------------------------------------------
// InnerAudioContext
// ---------------------------------------------------------------------------

/// The InnerAudioContext commands that answer nothing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum InnerAudioCommand {
    Create,
    Destroy,
    Play,
    Pause,
    Stop,
    Seek(f64),
    SetVolume(f32),
    SetLoop(bool),
    SetPlaybackRate(f32),
    SetAutoplay(bool),
}

pub fn inner_audio(
    tx: &AudioSender,
    id: InnerAudioId,
    command: InnerAudioCommand,
) -> Result<(), ServiceError> {
    send(
        tx,
        match command {
            InnerAudioCommand::Create => AudioCmd::CreateInnerAudio { id },
            InnerAudioCommand::Destroy => AudioCmd::DestroyInnerAudio { id },
            InnerAudioCommand::Play => AudioCmd::InnerAudioPlay { id },
            InnerAudioCommand::Pause => AudioCmd::InnerAudioPause { id },
            InnerAudioCommand::Stop => AudioCmd::InnerAudioStop { id },
            InnerAudioCommand::Seek(position) => AudioCmd::InnerAudioSeek { id, position },
            InnerAudioCommand::SetVolume(volume) => AudioCmd::InnerAudioSetVolume { id, volume },
            InnerAudioCommand::SetLoop(loop_enabled) => {
                AudioCmd::InnerAudioSetLoop { id, loop_enabled }
            }
            InnerAudioCommand::SetPlaybackRate(rate) => {
                AudioCmd::InnerAudioSetPlaybackRate { id, rate }
            }
            InnerAudioCommand::SetAutoplay(autoplay) => {
                AudioCmd::InnerAudioSetAutoplay { id, autoplay }
            }
        },
    )
}

pub fn inner_audio_get_state(
    tx: &AudioSender,
    id: InnerAudioId,
) -> Result<Pending<InnerAudioState>, ServiceError> {
    request(tx, |resp| AudioCmd::InnerAudioGetState { id, resp })
}

/// Load encoded audio already in hand into an InnerAudioContext. `permit` is
/// [`reserve_encoded`]'s, for exactly these bytes.
pub fn inner_audio_load(
    tx: &AudioSender,
    id: InnerAudioId,
    data: Vec<u8>,
    permit: AudioCommandPermit,
) -> Result<Pending<InnerAudioInfo>, ServiceError> {
    let (resp, answer) = oneshot::channel();
    send_reserved(tx, AudioCmd::InnerAudioLoad { id, data, resp }, permit)?;
    Ok(Pending(answer))
}

/// Where an InnerAudioContext's `src` is read from.
pub struct InnerAudioSources<G> {
    /// The package's code directory, for a path outside any sandbox.
    pub code_dir: Option<String>,
    /// The game's sandbox. Present in every product; absent only in tooling.
    pub vfs: Option<Arc<VirtualFS>>,
    /// Admits an `http(s)://` source, or refuses it with the reason: the
    /// caller's network policy, which is the caller's to hold.
    pub admit_remote: G,
}

/// A `src`, checked before anything is started: bounded, like every string
/// the audio queue carries.
pub fn prepare_inner_audio_src(src: &str) -> Result<String, ServiceError> {
    validate_audio_control_string("audio source URL", src)?;
    Ok(src.to_owned())
}

/// Load `src` into an InnerAudioContext: streamed from an `http(s)` URL the
/// caller admits, or read from the game's sandbox and decoded.
pub async fn inner_audio_load_url<G>(
    tx: AudioSender,
    sources: InnerAudioSources<G>,
    id: InnerAudioId,
    src: String,
) -> Result<(), ServiceError>
where
    G: FnOnce(&url::Url) -> Result<(), ServiceError>,
{
    if let Some(url) = parse_remote_audio_url(&src)? {
        (sources.admit_remote)(&url)?;
        return request(&tx, |resp| AudioCmd::InnerAudioLoadUrl {
            id,
            url: src,
            resp,
        })?
        .answer()
        .await;
    }

    let source = resolve_local_src(sources.code_dir.as_deref(), sources.vfs.as_deref(), &src)?;
    // Admit the operation before it reaches the blocking filesystem pool.
    // This bounds concurrent opens as well as the later allocation; every
    // failure releases the RAII permit, and a successful small load shrinks it
    // to the Vec capacity its queued command retains.
    let mut permit = reserve(&tx, MAX_ENCODED_AUDIO_BYTES)?;
    let file = open_local_audio_source(sources.vfs, source).await?;
    let data = read_capped_local_audio(file).await?;
    if data.capacity() > MAX_ENCODED_AUDIO_BYTES {
        return Err(engine_error(EngineError::from_detail(
            ErrorCode::InputSaturated,
            "local audio buffer capacity exceeds its reservation",
        )));
    }
    // The file reader uses metadata only as an allocation hint, so charge the
    // actual retained capacity before publishing its command.
    permit.shrink_to(data.capacity());
    let (resp, answer) = oneshot::channel();
    send_reserved(&tx, AudioCmd::InnerAudioLoad { id, data, resp }, permit)?;
    // Waited for, but what the load learned is not this op's answer.
    Pending(answer).answer().await.map(|_| ())
}

/// A remote audio URL, without reclassifying ordinary local or VFS paths. URL
/// schemes are case-insensitive; a malformed string that explicitly claims
/// HTTP(S) is an error rather than a filesystem path.
pub fn parse_remote_audio_url(src: &str) -> Result<Option<url::Url>, ServiceError> {
    match url::Url::parse(src) {
        Ok(url) if matches!(url.scheme(), "http" | "https") => Ok(Some(url)),
        Ok(_) => Ok(None),
        Err(error) => {
            let is_http = src
                .get(..7)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("http://"));
            let is_https = src
                .get(..8)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://"));
            if is_http || is_https {
                Err(audio_error(format!("Invalid audio URL: {error}")))
            } else {
                Ok(None)
            }
        }
    }
}

/// Resolve a local path against the code directory, for tooling with no VFS.
fn resolve_path(code_dir: Option<&str>, path: &str) -> String {
    if path == "/code" {
        return code_dir.unwrap_or_default().to_string();
    }
    if let Some(stripped) = path.strip_prefix("/code/") {
        let mut full = PathBuf::from(code_dir.unwrap_or_default());
        full.push(stripped);
        return full.to_string_lossy().into_owned();
    }

    let p = PathBuf::from(path);
    if p.is_absolute() {
        return path.to_string();
    }
    match code_dir {
        Some(base) if !base.is_empty() => {
            let mut full = PathBuf::from(base);
            full.push(path);
            full.to_string_lossy().into_owned()
        }
        _ => path.to_string(),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum LocalAudioSource {
    /// A virtual path that must be opened by the VFS itself. Keeping it virtual
    /// prevents callers from accidentally reintroducing resolve-then-open.
    Sandboxed { virtual_path: String },
    /// Tooling/headless mode has no sandbox contract and retains the legacy
    /// code-directory-relative behavior.
    Unconfined { path: String },
}

fn resolve_local_src(
    code_dir: Option<&str>,
    vfs: Option<&VirtualFS>,
    src: &str,
) -> Result<LocalAudioSource, ServiceError> {
    if let Some(vfs) = vfs {
        if !src.starts_with('/') {
            return Ok(LocalAudioSource::Sandboxed {
                virtual_path: format!("/code/{src}"),
            });
        }

        if vfs.is_virtual_path(src) {
            return Ok(LocalAudioSource::Sandboxed {
                virtual_path: src.to_owned(),
            });
        }

        // Never include the rejected input or a host path in a JS-visible
        // error. The caller only needs to know that policy denied it.
        return Err(audio_error("Local audio path is not permitted"));
    }

    // No VFS (headless / tooling): fall back to code_dir-relative resolution.
    Ok(LocalAudioSource::Unconfined {
        path: resolve_path(code_dir, src),
    })
}

async fn open_local_audio_source(
    vfs: Option<Arc<VirtualFS>>,
    source: LocalAudioSource,
) -> Result<tokio::fs::File, ServiceError> {
    match source {
        LocalAudioSource::Sandboxed { virtual_path } => {
            let vfs = vfs.ok_or_else(|| audio_error("Failed to open local audio file"))?;
            let file =
                tokio::task::spawn_blocking(move || vfs.open_regular_for_read(&virtual_path))
                    .await
                    .map_err(|_| audio_error("Failed to open local audio file"))?
                    .map_err(|_| audio_error("Failed to open local audio file"))?;
            Ok(tokio::fs::File::from_std(file))
        }
        LocalAudioSource::Unconfined { path } => tokio::fs::File::open(path)
            .await
            .map_err(|_| audio_error("Failed to open local audio file")),
    }
}

fn next_local_audio_capacity(
    current_capacity: usize,
    current_len: usize,
    incoming: usize,
) -> Result<Option<usize>, ServiceError> {
    let needed = current_len.checked_add(incoming).ok_or_else(|| {
        engine_error(EngineError::from_detail(
            ErrorCode::InputSaturated,
            "encoded audio input size overflow",
        ))
    })?;
    if needed > MAX_ENCODED_AUDIO_BYTES {
        return Err(input_exceeds_limit());
    }
    if needed <= current_capacity {
        return Ok(None);
    }

    // Metadata can become stale while a file grows. Grow geometrically from a
    // small first chunk so that this fallback stays amortized O(n), but never
    // request capacity beyond the input cap.
    let target = needed
        .max(LOCAL_AUDIO_READ_CHUNK_BYTES)
        .max(current_capacity.saturating_mul(2))
        .min(MAX_ENCODED_AUDIO_BYTES);
    Ok(Some(target))
}

fn input_exceeds_limit() -> ServiceError {
    engine_error(EngineError::from_detail(
        ErrorCode::InputSaturated,
        format!("encoded audio input exceeds the {MAX_ENCODED_AUDIO_BYTES} byte limit"),
    ))
}

async fn read_capped_local_audio(file: tokio::fs::File) -> Result<Vec<u8>, ServiceError> {
    let file_len = usize::try_from(
        file.metadata()
            .await
            .map_err(|_| audio_error("Failed to read local audio file"))?
            .len(),
    )
    .unwrap_or(usize::MAX);
    read_capped_local_audio_with_capacity_hint(file, file_len).await
}

async fn read_capped_local_audio_with_capacity_hint(
    mut file: tokio::fs::File,
    file_len: usize,
) -> Result<Vec<u8>, ServiceError> {
    if file_len > MAX_ENCODED_AUDIO_BYTES {
        return Err(input_exceeds_limit());
    }

    let mut data = Vec::new();
    data.try_reserve_exact(file_len)
        .map_err(|_| audio_error("Failed to allocate local audio buffer"))?;
    if data.capacity() > MAX_ENCODED_AUDIO_BYTES {
        return Err(input_exceeds_limit());
    }
    let mut chunk = [0_u8; LOCAL_AUDIO_READ_CHUNK_BYTES];

    loop {
        let count = file
            .read(&mut chunk)
            .await
            .map_err(|_| audio_error("Failed to read local audio file"))?;
        if count == 0 {
            return Ok(data);
        }

        if let Some(target_capacity) =
            next_local_audio_capacity(data.capacity(), data.len(), count)?
        {
            data.try_reserve_exact(target_capacity - data.len())
                .map_err(|_| audio_error("Failed to allocate local audio buffer"))?;
            if data.capacity() > MAX_ENCODED_AUDIO_BYTES {
                return Err(input_exceeds_limit());
            }
        }
        data.extend_from_slice(&chunk[..count]);
    }
}

#[cfg(test)]
#[path = "audio_tests.rs"]
mod tests;
