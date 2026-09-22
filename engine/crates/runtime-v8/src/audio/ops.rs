//! The audio ops: arguments out of V8, into `migo_services::audio`.
//!
//! What each op does -- the checks, the command, the answer -- is in
//! `migo_services::audio`, which the external session's service dispatcher
//! calls too. What stays here is what only an op beside V8 can do: take a
//! `v8::ArrayBuffer`'s bytes and detach it, and hold a streamed source to this
//! runtime's network gate.

use deno_core::{JsBuffer, OpState, op2, v8};
use migo_services::audio::{
    self as service, Automation, BufferScope, InnerAudioCommand, InnerAudioSources, NodeKind,
    NodeSetting, PlanarBacking, PlayerCommand, ScheduledSource, StartTiming,
};
use shared::{
    audio_resources::{AudioBufferKey, AudioResourceRegistry, PreparedAudioSnapshot},
    error::EngineError,
    op_state::{AudioSender, HostOpState},
    protocol::audio_cmd::{
        AudioBufferId, AudioBufferInfo, AudioCmd, AudioContextId, AudioNodeId, InnerAudioId,
        InnerAudioInfo, InnerAudioState,
    },
    services::Scope,
};
use std::{cell::RefCell, rc::Rc};
use tokio::sync::oneshot;

#[derive(Debug, thiserror::Error, deno_error::JsError)]
pub enum AudioError {
    #[class("AudioError")]
    #[error("{0}")]
    Message(String),
}

impl From<&str> for AudioError {
    #[inline]
    fn from(value: &str) -> Self {
        AudioError::Message(value.to_string())
    }
}

impl From<String> for AudioError {
    #[inline]
    fn from(value: String) -> Self {
        AudioError::Message(value)
    }
}

impl From<EngineError> for AudioError {
    #[inline]
    fn from(e: EngineError) -> Self {
        service::engine_error(e).into()
    }
}

impl From<migo_services::ServiceError> for AudioError {
    /// Every audio service failure is already an `AudioError`; the class is
    /// this type's, and the message is carried as it is.
    #[inline]
    fn from(e: migo_services::ServiceError) -> Self {
        debug_assert_eq!(e.class, service::CLASS_AUDIO_ERROR);
        AudioError::Message(e.message)
    }
}

impl From<shared::audio_channel::AudioCommandSendError> for AudioError {
    fn from(error: shared::audio_channel::AudioCommandSendError) -> Self {
        service::send_error(error).into()
    }
}

impl From<shared::audio_channel::AudioCommandReserveError> for AudioError {
    fn from(error: shared::audio_channel::AudioCommandReserveError) -> Self {
        service::reserve_error(error).into()
    }
}

impl From<shared::protocol::error::ServiceError> for AudioError {
    #[inline]
    fn from(e: shared::protocol::error::ServiceError) -> Self {
        AudioError::Message(e.message)
    }
}

#[inline]
fn audio_err(msg: impl Into<String>) -> AudioError {
    AudioError::Message(msg.into())
}

#[inline]
fn get_audio_tx(state: Rc<RefCell<OpState>>) -> AudioSender {
    let st = state.borrow();
    st.borrow::<HostOpState>().audio_tx.clone()
}

/// This runtime's audio sender and generation, which scope its buffer ids.
#[inline]
fn audio_scope(state: &Rc<RefCell<OpState>>) -> (AudioSender, i64) {
    let st = state.borrow();
    let host = st.borrow::<HostOpState>();
    (host.audio_tx.clone(), host.runtime_generation)
}

// ============================================================================
// Context Operations
// ============================================================================

/// Create an AudioContext with a JS-allocated id (fire-and-forget).
///
/// The id is generated on the JS side so `new AudioContext()` is usable
/// synchronously (browser semantics) instead of racing an async round-trip —
/// otherwise `createGain()` on the next line runs with a null context id and
/// the smi decode fails with "expected i32". Ordering is safe: this command and
/// every later node op share one FIFO channel, so the context is created before
/// any node that references it.
#[op2(fast)]
pub fn op_audio_create_context(
    state: Rc<RefCell<OpState>>,
    #[smi] ctx_id: AudioContextId,
    #[smi] sample_rate: u32,
) -> Result<(), AudioError> {
    Ok(service::create_context(
        &get_audio_tx(state),
        ctx_id,
        sample_rate,
    )?)
}

#[op2(async(lazy), fast)]
pub async fn op_audio_close_context(
    state: Rc<RefCell<OpState>>,
    #[smi] ctx_id: AudioContextId,
) -> Result<(), AudioError> {
    let tx = get_audio_tx(state);
    Ok(service::close_context(&tx, ctx_id)?.answer().await?)
}

/// Release an abandoned AudioContext from a `FinalizationRegistry` callback.
///
/// Unlike the explicit close operation this is fire-and-forget, idempotent,
/// and carries no response sender that could outlive the collected wrapper.
#[op2(fast)]
pub fn op_audio_release_context(
    state: Rc<RefCell<OpState>>,
    #[smi] ctx_id: AudioContextId,
) -> Result<(), AudioError> {
    Ok(service::release_context(&get_audio_tx(state), ctx_id)?)
}

/// Mark an AudioNode as unreachable from JavaScript, from its GC finalizer.
///
/// Fire-and-forget and idempotent. The audio thread treats it as a request, not
/// a removal: JavaScript drops a `source -> gain -> destination` chain as one
/// unreachable object graph, so this can arrive while the source is still
/// playing through the node. The node is dropped once nothing upstream can feed
/// it, so this cannot cut a sound short.
#[op2(fast)]
pub fn op_audio_release_node(
    state: Rc<RefCell<OpState>>,
    #[smi] ctx_id: AudioContextId,
    #[smi] node_id: AudioNodeId,
) -> Result<(), AudioError> {
    Ok(service::release_node(
        &get_audio_tx(state),
        ctx_id,
        node_id,
    )?)
}

/// Resume a suspended AudioContext
#[op2(async(lazy), fast)]
pub async fn op_audio_resume_context(
    state: Rc<RefCell<OpState>>,
    #[smi] ctx_id: AudioContextId,
) -> Result<(), AudioError> {
    let tx = get_audio_tx(state);
    Ok(service::resume_context(&tx, ctx_id)?.answer().await?)
}

/// Suspend an AudioContext
#[op2(async(lazy), fast)]
pub async fn op_audio_suspend_context(
    state: Rc<RefCell<OpState>>,
    #[smi] ctx_id: AudioContextId,
) -> Result<(), AudioError> {
    let tx = get_audio_tx(state);
    Ok(service::suspend_context(&tx, ctx_id)?.answer().await?)
}

// ============================================================================
// Buffer Operations
// ============================================================================

/// Decode `data` and adopt the result as a frozen `AudioBuffer`.
///
/// The input is copied out and detached before the future exists, as
/// `decodeAudioData` transfers it; the answer's `id` names a buffer whose PCM
/// stays on the host until JavaScript reads its channels.
#[op2(async(lazy), fast)]
#[serde]
pub fn op_audio_decode_audio_data(
    state: Rc<RefCell<OpState>>,
    #[smi] ctx_id: AudioContextId,
    data: v8::Local<v8::ArrayBuffer>,
) -> Result<
    impl std::future::Future<Output = Result<AudioBufferInfo, AudioError>> + use<>,
    AudioError,
> {
    if data.was_detached() || !data.is_detachable() {
        return Err(audio_err("audioData is detached or cannot be detached"));
    }
    let len = data.byte_length();
    let (tx, runtime_generation) = audio_scope(&state);
    // Bounded and admitted before a byte is copied.
    let permit = service::reserve_encoded(&tx, len)?;

    let backing = data.get_backing_store();
    if backing.is_shared() {
        return Err(audio_err("audioData must not be shared"));
    }
    if backing.byte_length() < len {
        return Err(audio_err(
            "audioData backing is shorter than its visible length",
        ));
    }
    let mut owned = Vec::new();
    owned
        .try_reserve_exact(len)
        .map_err(|_| audio_err("encoded audio input allocation failed"))?;
    if len != 0 {
        let ptr = backing
            .data()
            .ok_or_else(|| audio_err("audioData backing has no data"))?;
        // JavaScript cannot resize or detach this buffer while the synchronous
        // op entry point is executing. The backing-store handle keeps the
        // allocation alive until the copy completes.
        let source = unsafe { std::slice::from_raw_parts(ptr.as_ptr().cast::<u8>(), len) };
        owned.extend_from_slice(source);
    }
    drop(backing);
    if data.detach(None) != Some(true) {
        return Err(audio_err("audioData could not be detached"));
    }

    let decoding = service::decode_audio_data(
        BufferScope {
            tx: &tx,
            runtime_generation,
        },
        ctx_id,
        owned,
        permit,
    )?;
    Ok(async move { Ok(decoding.answer().await?) })
}

/// A writable `AudioBuffer`'s backing: a V8 `ArrayBuffer` holding its planar
/// PCM, validated and read while JavaScript is paused on this op.
struct V8Planar<'s>(v8::Local<'s, v8::ArrayBuffer>);

impl PlanarBacking for V8Planar<'_> {
    fn prepare(
        self,
        resources: &AudioResourceRegistry,
        key: AudioBufferKey,
        expected_bytes: usize,
    ) -> Result<PreparedAudioSnapshot, migo_services::ServiceError> {
        let backing = self.0;
        if backing.was_detached()
            || !backing.is_detachable()
            || backing.byte_length() != expected_bytes
        {
            return Err(service::audio_error(
                "AudioBuffer backing is detached, non-detachable, or has an invalid length",
            ));
        }
        let store = backing.get_backing_store();
        if store.is_shared() || store.is_resizable_by_user_javascript() {
            return Err(service::audio_error(
                "AudioBuffer backing must be non-shared and fixed-length",
            ));
        }
        let sample_count = expected_bytes / std::mem::size_of::<f32>();
        let data = store
            .data()
            .ok_or_else(|| service::audio_error("AudioBuffer backing has no data"))?;
        if (data.as_ptr() as usize) % std::mem::align_of::<f32>() != 0 {
            return Err(service::audio_error(
                "AudioBuffer backing is not f32-aligned",
            ));
        }

        // This is a non-shared, fixed-length ArrayBuffer and JavaScript cannot
        // run concurrently while this synchronous op is executing. `store`
        // keeps the allocation alive for the full borrow; it is dropped before
        // publication.
        let planar =
            unsafe { std::slice::from_raw_parts(data.as_ptr().cast::<f32>(), sample_count) };
        let prepared = resources
            .prepare_snapshot(key, Some(planar))
            .map_err(service::engine_error)?;
        drop(store);
        Ok(prepared)
    }
}

fn publish_snapshot(
    state: Rc<RefCell<OpState>>,
    ctx_id: AudioContextId,
    node_id: AudioNodeId,
    buffer_id: AudioBufferId,
    backing: Option<v8::Local<v8::ArrayBuffer>>,
    timing: Option<StartTiming>,
) -> Result<(), AudioError> {
    let (tx, runtime_generation) = audio_scope(&state);
    Ok(service::publish_buffer(
        BufferScope {
            tx: &tx,
            runtime_generation,
        },
        ctx_id,
        node_id,
        buffer_id,
        backing.map(V8Planar),
        timing,
        move || {
            if let Some(backing) = backing {
                if backing.detach(None) != Some(true) {
                    // The buffer was validated while JavaScript was paused, so
                    // this indicates a V8 invariant failure. Never unwind
                    // through the op/JNI boundary after the audio command was
                    // accepted.
                    tracing::error!("validated AudioBuffer backing failed to detach");
                }
            }
        },
    )?)
}

#[op2]
pub fn op_audio_reserve_buffer(
    state: Rc<RefCell<OpState>>,
    #[smi] channels: u32,
    #[smi] length: u32,
    #[smi] sample_rate: u32,
) -> Result<AudioBufferId, AudioError> {
    let (tx, runtime_generation) = audio_scope(&state);
    Ok(service::reserve_buffer(
        BufferScope {
            tx: &tx,
            runtime_generation,
        },
        channels,
        length,
        sample_rate,
    )?)
}

fn release_global_audio_buffer(state: &Rc<RefCell<OpState>>, buffer_id: AudioBufferId) {
    let (tx, runtime_generation) = audio_scope(state);
    service::release_buffer(
        BufferScope {
            tx: &tx,
            runtime_generation,
        },
        buffer_id,
    );
}

#[op2(fast)]
pub fn op_audio_abort_buffer(state: Rc<RefCell<OpState>>, #[smi] buffer_id: AudioBufferId) {
    release_global_audio_buffer(&state, buffer_id);
}

/// Release a global AudioBuffer backing from its finalizer. Missing/stale keys
/// are intentionally idempotent no-ops.
#[op2(fast)]
pub fn op_audio_release_buffer(state: Rc<RefCell<OpState>>, #[smi] buffer_id: AudioBufferId) {
    release_global_audio_buffer(&state, buffer_id);
}

#[op2]
#[arraybuffer]
pub fn op_audio_materialize_buffer(
    state: Rc<RefCell<OpState>>,
    #[smi] buffer_id: AudioBufferId,
) -> Result<Vec<f32>, AudioError> {
    let (tx, runtime_generation) = audio_scope(&state);
    Ok(service::materialize_buffer(
        BufferScope {
            tx: &tx,
            runtime_generation,
        },
        buffer_id,
    )?)
}

#[op2(fast)]
pub fn op_audio_start_buffer(
    state: Rc<RefCell<OpState>>,
    #[smi] ctx_id: AudioContextId,
    #[smi] node_id: AudioNodeId,
    #[smi] buffer_id: AudioBufferId,
    backing: Option<v8::Local<v8::ArrayBuffer>>,
    when: f64,
    offset: f64,
    duration: f64,
) -> Result<(), AudioError> {
    publish_snapshot(
        state,
        ctx_id,
        node_id,
        buffer_id,
        backing,
        Some(StartTiming {
            when,
            offset,
            duration,
        }),
    )
}

#[op2(fast)]
pub fn op_audio_set_started_buffer(
    state: Rc<RefCell<OpState>>,
    #[smi] ctx_id: AudioContextId,
    #[smi] node_id: AudioNodeId,
    #[smi] buffer_id: AudioBufferId,
    backing: Option<v8::Local<v8::ArrayBuffer>>,
) -> Result<(), AudioError> {
    publish_snapshot(state, ctx_id, node_id, buffer_id, backing, None)
}

// ============================================================================
// Node creation
// ============================================================================

fn create_node(
    state: Rc<RefCell<OpState>>,
    kind: NodeKind,
    ctx_id: AudioContextId,
    node_id: AudioNodeId,
) -> Result<(), AudioError> {
    Ok(service::create_node(
        &get_audio_tx(state),
        kind,
        ctx_id,
        node_id,
    )?)
}

/// Create a buffer source node with JS-provided node_id (fire and forget)
#[op2(fast)]
pub fn op_audio_create_buffer_source(
    state: Rc<RefCell<OpState>>,
    #[smi] ctx_id: AudioContextId,
    #[smi] node_id: AudioNodeId,
) -> Result<(), AudioError> {
    create_node(state, NodeKind::BufferSource, ctx_id, node_id)
}

/// Create a gain node with JS-provided node_id (fire and forget)
#[op2(fast)]
pub fn op_audio_create_gain(
    state: Rc<RefCell<OpState>>,
    #[smi] ctx_id: AudioContextId,
    #[smi] node_id: AudioNodeId,
) -> Result<(), AudioError> {
    create_node(state, NodeKind::Gain, ctx_id, node_id)
}

#[op2(fast)]
pub fn op_audio_create_oscillator(
    state: Rc<RefCell<OpState>>,
    #[smi] ctx_id: AudioContextId,
    #[smi] node_id: AudioNodeId,
) -> Result<(), AudioError> {
    create_node(state, NodeKind::Oscillator, ctx_id, node_id)
}

#[op2(fast)]
pub fn op_audio_create_biquad_filter(
    state: Rc<RefCell<OpState>>,
    #[smi] ctx_id: AudioContextId,
    #[smi] node_id: AudioNodeId,
) -> Result<(), AudioError> {
    create_node(state, NodeKind::BiquadFilter, ctx_id, node_id)
}

#[op2(fast)]
pub fn op_audio_create_wave_shaper(
    state: Rc<RefCell<OpState>>,
    #[smi] ctx_id: AudioContextId,
    #[smi] node_id: AudioNodeId,
) -> Result<(), AudioError> {
    create_node(state, NodeKind::WaveShaper, ctx_id, node_id)
}

#[op2(fast)]
pub fn op_audio_create_analyser(
    state: Rc<RefCell<OpState>>,
    #[smi] ctx_id: AudioContextId,
    #[smi] node_id: AudioNodeId,
) -> Result<(), AudioError> {
    create_node(state, NodeKind::Analyser, ctx_id, node_id)
}

#[op2(fast)]
pub fn op_audio_create_dynamics_compressor(
    state: Rc<RefCell<OpState>>,
    #[smi] ctx_id: AudioContextId,
    #[smi] node_id: AudioNodeId,
) -> Result<(), AudioError> {
    create_node(state, NodeKind::DynamicsCompressor, ctx_id, node_id)
}

#[op2(fast)]
pub fn op_audio_create_panner(
    state: Rc<RefCell<OpState>>,
    #[smi] ctx_id: AudioContextId,
    #[smi] node_id: AudioNodeId,
) -> Result<(), AudioError> {
    create_node(state, NodeKind::Panner, ctx_id, node_id)
}

#[op2(fast)]
pub fn op_audio_create_constant_source(
    state: Rc<RefCell<OpState>>,
    #[smi] ctx_id: AudioContextId,
    #[smi] node_id: AudioNodeId,
) -> Result<(), AudioError> {
    create_node(state, NodeKind::ConstantSource, ctx_id, node_id)
}

#[op2(fast)]
pub fn op_audio_create_delay(
    state: Rc<RefCell<OpState>>,
    #[smi] ctx_id: AudioContextId,
    #[smi] node_id: AudioNodeId,
    max_delay_time: f32,
) -> Result<(), AudioError> {
    Ok(service::create_delay(
        &get_audio_tx(state),
        ctx_id,
        node_id,
        max_delay_time,
    )?)
}

#[op2(fast)]
pub fn op_audio_create_channel_merger(
    state: Rc<RefCell<OpState>>,
    #[smi] ctx_id: AudioContextId,
    #[smi] node_id: AudioNodeId,
    #[smi] number_of_inputs: u32,
) -> Result<(), AudioError> {
    Ok(service::create_channel_merger(
        &get_audio_tx(state),
        ctx_id,
        node_id,
        number_of_inputs,
    )?)
}

#[op2(fast)]
pub fn op_audio_create_channel_splitter(
    state: Rc<RefCell<OpState>>,
    #[smi] ctx_id: AudioContextId,
    #[smi] node_id: AudioNodeId,
    #[smi] number_of_outputs: u32,
) -> Result<(), AudioError> {
    Ok(service::create_channel_splitter(
        &get_audio_tx(state),
        ctx_id,
        node_id,
        number_of_outputs,
    )?)
}

#[op2]
pub fn op_audio_create_iir_filter(
    state: Rc<RefCell<OpState>>,
    #[smi] ctx_id: AudioContextId,
    #[smi] node_id: AudioNodeId,
    #[serde] feedforward: Vec<f64>,
    #[serde] feedback: Vec<f64>,
) -> Result<(), AudioError> {
    Ok(service::create_iir_filter(
        &get_audio_tx(state),
        ctx_id,
        node_id,
        feedforward,
        feedback,
    )?)
}

// ============================================================================
// BufferSourceNode Operations
// ============================================================================

#[op2(async(lazy), fast)]
pub async fn op_audio_stop(
    state: Rc<RefCell<OpState>>,
    #[smi] node_id: AudioNodeId,
    when: f64,
) -> Result<(), AudioError> {
    let tx = get_audio_tx(state);
    Ok(service::stop(&tx, node_id, when)?.answer().await?)
}

/// Set loop property (fire and forget)
#[op2(fast)]
pub fn op_audio_set_loop(
    state: Rc<RefCell<OpState>>,
    #[smi] node_id: AudioNodeId,
    loop_enabled: bool,
    loop_start: f64,
    loop_end: f64,
) -> Result<(), AudioError> {
    Ok(service::set_loop(
        &get_audio_tx(state),
        node_id,
        loop_enabled,
        loop_start,
        loop_end,
    )?)
}

// ============================================================================
// Parameters
// ============================================================================

/// Set gain value (fire and forget)
#[op2(fast)]
pub fn op_audio_set_gain_value(
    state: Rc<RefCell<OpState>>,
    #[smi] node_id: AudioNodeId,
    value: f32,
) -> Result<(), AudioError> {
    Ok(service::set_gain_value(
        &get_audio_tx(state),
        node_id,
        value,
    )?)
}

/// Set an AudioParam's current value now by node + param name (fire and forget).
#[op2(fast)]
pub fn op_audio_set_node_param(
    state: Rc<RefCell<OpState>>,
    #[smi] node_id: AudioNodeId,
    #[string] param_name: &str,
    value: f32,
) -> Result<(), AudioError> {
    Ok(service::set_node_param(
        &get_audio_tx(state),
        node_id,
        param_name,
        value,
    )?)
}

fn automate(
    state: Rc<RefCell<OpState>>,
    node_id: AudioNodeId,
    param_name: &str,
    automation: Automation,
) -> Result<(), AudioError> {
    Ok(service::automate(
        &get_audio_tx(state),
        node_id,
        param_name,
        automation,
    )?)
}

#[op2(fast)]
pub fn op_audio_param_set_value_at_time(
    state: Rc<RefCell<OpState>>,
    #[smi] node_id: AudioNodeId,
    #[string] param_name: &str,
    value: f32,
    time: f64,
) -> Result<(), AudioError> {
    automate(
        state,
        node_id,
        param_name,
        Automation::SetValueAtTime { value, time },
    )
}

#[op2(fast)]
pub fn op_audio_param_linear_ramp(
    state: Rc<RefCell<OpState>>,
    #[smi] node_id: AudioNodeId,
    #[string] param_name: &str,
    value: f32,
    end_time: f64,
) -> Result<(), AudioError> {
    automate(
        state,
        node_id,
        param_name,
        Automation::LinearRamp { value, end_time },
    )
}

#[op2(fast)]
pub fn op_audio_param_exponential_ramp(
    state: Rc<RefCell<OpState>>,
    #[smi] node_id: AudioNodeId,
    #[string] param_name: &str,
    value: f32,
    end_time: f64,
) -> Result<(), AudioError> {
    automate(
        state,
        node_id,
        param_name,
        Automation::ExponentialRamp { value, end_time },
    )
}

#[op2(fast)]
pub fn op_audio_param_set_target(
    state: Rc<RefCell<OpState>>,
    #[smi] node_id: AudioNodeId,
    #[string] param_name: &str,
    target: f32,
    start_time: f64,
    time_constant: f64,
) -> Result<(), AudioError> {
    automate(
        state,
        node_id,
        param_name,
        Automation::SetTarget {
            target,
            start_time,
            time_constant,
        },
    )
}

#[op2(fast)]
pub fn op_audio_param_cancel_scheduled(
    state: Rc<RefCell<OpState>>,
    #[smi] node_id: AudioNodeId,
    #[string] param_name: &str,
    cancel_time: f64,
) -> Result<(), AudioError> {
    automate(
        state,
        node_id,
        param_name,
        Automation::CancelScheduled { cancel_time },
    )
}

// ============================================================================
// Graph Operations
// ============================================================================

#[op2(async(lazy), fast)]
pub async fn op_audio_connect(
    state: Rc<RefCell<OpState>>,
    #[smi] src: AudioNodeId,
    #[smi] dst: AudioNodeId,
    #[smi] src_output: u32,
    #[smi] dst_input: u32,
) -> Result<(), AudioError> {
    let tx = get_audio_tx(state);
    Ok(service::connect(&tx, src, dst, src_output, dst_input)?
        .answer()
        .await?)
}

#[op2(async(lazy), fast)]
pub async fn op_audio_disconnect(
    state: Rc<RefCell<OpState>>,
    #[smi] node_id: AudioNodeId,
) -> Result<(), AudioError> {
    let tx = get_audio_tx(state);
    Ok(service::disconnect(&tx, node_id)?.answer().await?)
}

// ============================================================================
// Scheduled sources and string-valued settings
// ============================================================================

#[op2(fast)]
pub fn op_audio_start_oscillator(
    state: Rc<RefCell<OpState>>,
    #[smi] node_id: AudioNodeId,
    when: f64,
) -> Result<(), AudioError> {
    Ok(service::start_source(
        &get_audio_tx(state),
        ScheduledSource::Oscillator,
        node_id,
        when,
    )?)
}

#[op2(fast)]
pub fn op_audio_stop_oscillator(
    state: Rc<RefCell<OpState>>,
    #[smi] node_id: AudioNodeId,
    when: f64,
) -> Result<(), AudioError> {
    Ok(service::stop_source(
        &get_audio_tx(state),
        ScheduledSource::Oscillator,
        node_id,
        when,
    )?)
}

#[op2(fast)]
pub fn op_audio_start_constant_source(
    state: Rc<RefCell<OpState>>,
    #[smi] node_id: AudioNodeId,
    when: f64,
) -> Result<(), AudioError> {
    Ok(service::start_source(
        &get_audio_tx(state),
        ScheduledSource::ConstantSource,
        node_id,
        when,
    )?)
}

#[op2(fast)]
pub fn op_audio_stop_constant_source(
    state: Rc<RefCell<OpState>>,
    #[smi] node_id: AudioNodeId,
    when: f64,
) -> Result<(), AudioError> {
    Ok(service::stop_source(
        &get_audio_tx(state),
        ScheduledSource::ConstantSource,
        node_id,
        when,
    )?)
}

fn set_node_setting(
    state: Rc<RefCell<OpState>>,
    setting: NodeSetting,
    node_id: AudioNodeId,
    value: &str,
) -> Result<(), AudioError> {
    Ok(service::set_node_setting(
        &get_audio_tx(state),
        setting,
        node_id,
        value,
    )?)
}

#[op2(fast)]
pub fn op_audio_set_oscillator_type(
    state: Rc<RefCell<OpState>>,
    #[smi] node_id: AudioNodeId,
    #[string] osc_type: &str,
) -> Result<(), AudioError> {
    set_node_setting(state, NodeSetting::OscillatorType, node_id, osc_type)
}

#[op2(fast)]
pub fn op_audio_set_biquad_filter_type(
    state: Rc<RefCell<OpState>>,
    #[smi] node_id: AudioNodeId,
    #[string] filter_type: &str,
) -> Result<(), AudioError> {
    set_node_setting(state, NodeSetting::BiquadFilterType, node_id, filter_type)
}

#[op2(fast)]
pub fn op_audio_set_wave_shaper_oversample(
    state: Rc<RefCell<OpState>>,
    #[smi] node_id: AudioNodeId,
    #[string] oversample: &str,
) -> Result<(), AudioError> {
    set_node_setting(
        state,
        NodeSetting::WaveShaperOversample,
        node_id,
        oversample,
    )
}

#[op2(fast)]
pub fn op_audio_set_panning_model(
    state: Rc<RefCell<OpState>>,
    #[smi] node_id: AudioNodeId,
    #[string] model: &str,
) -> Result<(), AudioError> {
    set_node_setting(state, NodeSetting::PanningModel, node_id, model)
}

#[op2(fast)]
pub fn op_audio_set_distance_model(
    state: Rc<RefCell<OpState>>,
    #[smi] node_id: AudioNodeId,
    #[string] model: &str,
) -> Result<(), AudioError> {
    set_node_setting(state, NodeSetting::DistanceModel, node_id, model)
}

#[op2]
pub fn op_audio_set_wave_shaper_curve(
    state: Rc<RefCell<OpState>>,
    #[smi] node_id: AudioNodeId,
    #[buffer] curve_bytes: Option<JsBuffer>,
) -> Result<(), AudioError> {
    Ok(service::set_wave_shaper_curve(
        &get_audio_tx(state),
        node_id,
        curve_bytes.as_deref(),
    )?)
}

// ============================================================================
// AnalyserNode, PannerNode, DynamicsCompressorNode
// ============================================================================

#[op2(fast)]
pub fn op_audio_set_analyser_fft_size(
    state: Rc<RefCell<OpState>>,
    #[smi] node_id: AudioNodeId,
    #[smi] fft_size: u32,
) -> Result<(), AudioError> {
    Ok(service::set_analyser_fft_size(
        &get_audio_tx(state),
        node_id,
        fft_size,
    )?)
}

/// Set one of an AnalyserNode's scalar properties.
///
/// These used to live only in JavaScript, so the analysis the node performed
/// ignored the dB window and the smoothing the spec defines its output in terms of.
#[op2(fast)]
pub fn op_audio_set_analyser_scalar(
    state: Rc<RefCell<OpState>>,
    #[smi] node_id: AudioNodeId,
    #[string] prop: &str,
    value: f32,
) -> Result<(), AudioError> {
    Ok(service::set_analyser_scalar(
        &get_audio_tx(state),
        node_id,
        prop,
        value,
    )?)
}

#[op2(fast)]
pub fn op_audio_set_panner_scalar(
    state: Rc<RefCell<OpState>>,
    #[smi] node_id: AudioNodeId,
    #[string] prop: &str,
    value: f64,
) -> Result<(), AudioError> {
    Ok(service::set_panner_scalar(
        &get_audio_tx(state),
        node_id,
        prop,
        value,
    )?)
}

async fn read_analyser(
    state: Rc<RefCell<OpState>>,
    read: service::AnalyserRead,
    node_id: AudioNodeId,
) -> Result<service::AnalyserData, AudioError> {
    let tx = get_audio_tx(state);
    Ok(service::read_analyser(&tx, read, node_id)?.await?)
}

fn floats_as_le_bytes(data: service::AnalyserData) -> Vec<u8> {
    match data {
        service::AnalyserData::Bytes(bytes) => bytes,
        service::AnalyserData::Floats(floats) => {
            floats.iter().flat_map(|f| f.to_le_bytes()).collect()
        }
    }
}

#[op2(async(lazy), fast)]
#[buffer]
pub async fn op_audio_analyser_byte_time_domain(
    state: Rc<RefCell<OpState>>,
    #[smi] node_id: AudioNodeId,
) -> Result<Vec<u8>, AudioError> {
    read_analyser(state, service::AnalyserRead::ByteTimeDomain, node_id)
        .await
        .map(floats_as_le_bytes)
}

#[op2(async(lazy), fast)]
#[buffer]
pub async fn op_audio_analyser_float_time_domain(
    state: Rc<RefCell<OpState>>,
    #[smi] node_id: AudioNodeId,
) -> Result<Vec<u8>, AudioError> {
    // The samples as raw little-endian bytes, for a Float32Array over them.
    read_analyser(state, service::AnalyserRead::FloatTimeDomain, node_id)
        .await
        .map(floats_as_le_bytes)
}

/// Get byte frequency data from AnalyserNode (FFT output, 0-255)
#[op2(async(lazy), fast)]
#[serde]
pub async fn op_audio_analyser_byte_frequency(
    state: Rc<RefCell<OpState>>,
    #[smi] node_id: AudioNodeId,
) -> Result<Vec<u8>, AudioError> {
    match read_analyser(state, service::AnalyserRead::ByteFrequency, node_id).await? {
        service::AnalyserData::Bytes(bytes) => Ok(bytes),
        service::AnalyserData::Floats(_) => unreachable!("a byte read answers bytes"),
    }
}

/// Get float frequency data from AnalyserNode (FFT output, dB values)
#[op2(async(lazy), fast)]
#[serde]
pub async fn op_audio_analyser_float_frequency(
    state: Rc<RefCell<OpState>>,
    #[smi] node_id: AudioNodeId,
) -> Result<Vec<f32>, AudioError> {
    match read_analyser(state, service::AnalyserRead::FloatFrequency, node_id).await? {
        service::AnalyserData::Floats(floats) => Ok(floats),
        service::AnalyserData::Bytes(_) => unreachable!("a float read answers floats"),
    }
}

/// Get frequency response from BiquadFilterNode or IIRFilterNode.
/// Returns the magnitude and phase response vectors.
#[op2(async(lazy))]
#[serde]
pub async fn op_audio_get_frequency_response(
    state: Rc<RefCell<OpState>>,
    #[smi] node_id: AudioNodeId,
    #[buffer] frequencies: JsBuffer,
) -> Result<(Vec<f32>, Vec<f32>), AudioError> {
    let tx = get_audio_tx(state);
    Ok(service::get_frequency_response(&tx, node_id, &frequencies)?
        .answer()
        .await?)
}

/// Get current reduction value from DynamicsCompressorNode
#[op2(async(lazy), fast)]
pub async fn op_audio_get_reduction(
    state: Rc<RefCell<OpState>>,
    #[smi] node_id: AudioNodeId,
) -> Result<f32, AudioError> {
    let tx = get_audio_tx(state);
    Ok(service::get_reduction(&tx, node_id)?.answer().await?)
}

// ============================================================================
// Legacy context-local buffer ops
//
// Registered, but called by none of the engine's JavaScript: `AudioBuffer`
// is a global registry entry now (see `00_audio_buffer.js`), and these name
// buffers by a context-local id. No lane classifies them, so only the
// embedded runtime has them.
// ============================================================================

#[op2(fast)]
pub fn op_audio_set_buffer(
    state: Rc<RefCell<OpState>>,
    #[smi] ctx_id: AudioContextId,
    #[smi] node_id: AudioNodeId,
    #[smi] buffer_id: AudioBufferId,
) -> Result<(), AudioError> {
    let tx = get_audio_tx(state);
    tx.send(AudioCmd::SetBuffer {
        ctx_id,
        node_id,
        buffer_id: (buffer_id != 0).then_some(buffer_id),
    })
    .map_err(AudioError::from)
}

#[op2(async(lazy), fast)]
pub async fn op_audio_start(
    state: Rc<RefCell<OpState>>,
    #[smi] node_id: AudioNodeId,
    when: f64,
    offset: f64,
    duration: f64,
) -> Result<(), AudioError> {
    let when = service::validate_scheduled_time("when", when)?;
    let offset = service::validate_scheduled_time("offset", offset)?;
    let duration = service::validate_optional_duration(duration)?;
    let tx = get_audio_tx(state);
    let (resp, answer) = oneshot::channel();
    tx.send(AudioCmd::Start {
        node_id,
        when,
        offset,
        duration,
        resp,
    })
    .map_err(AudioError::from)?;
    answer
        .await
        .map_err(|_| audio_err("Response channel closed"))?
        .map_err(AudioError::from)
}

/// Create an empty context-local buffer
#[op2(async(lazy), fast)]
#[serde]
pub async fn op_audio_create_buffer(
    state: Rc<RefCell<OpState>>,
    #[smi] ctx_id: AudioContextId,
    #[smi] channels: u32,
    #[smi] length: u32,
    #[smi] sample_rate: u32,
) -> Result<AudioBufferInfo, AudioError> {
    let tx = get_audio_tx(state);
    let (resp, answer) = oneshot::channel();
    tx.send(AudioCmd::CreateBuffer {
        ctx_id,
        channels,
        length,
        sample_rate,
        resp,
    })
    .map_err(AudioError::from)?;
    answer
        .await
        .map_err(|_| audio_err("Response channel closed"))?
        .map_err(AudioError::from)
}

/// Get one channel of a context-local buffer
#[op2(async(lazy), fast)]
#[arraybuffer]
pub async fn op_audio_get_channel_data(
    state: Rc<RefCell<OpState>>,
    #[smi] ctx_id: AudioContextId,
    #[smi] buffer_id: AudioBufferId,
    #[smi] channel: u32,
) -> Result<Vec<f32>, AudioError> {
    let tx = get_audio_tx(state);
    let (resp, answer) = oneshot::channel();
    tx.send(AudioCmd::GetChannelData {
        ctx_id,
        buffer_id,
        channel,
        resp,
    })
    .map_err(AudioError::from)?;
    answer
        .await
        .map_err(|_| audio_err("Response channel closed"))?
        .map_err(AudioError::from)
}

/// Copy data to a context-local buffer channel
#[op2(async(lazy))]
pub async fn op_audio_copy_to_channel(
    state: Rc<RefCell<OpState>>,
    #[smi] ctx_id: AudioContextId,
    #[smi] buffer_id: AudioBufferId,
    #[buffer] data_bytes: JsBuffer,
    #[smi] channel: u32,
    #[smi] start: u32,
) -> Result<(), AudioError> {
    service::validate_copy_to_channel_size(data_bytes.len())?;
    let tx = get_audio_tx(state);
    let permit = service::reserve(&tx, data_bytes.len())?;
    let data: Vec<f32> = data_bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect();
    let (resp, answer) = oneshot::channel();
    tx.send_reserved(
        AudioCmd::CopyToChannel {
            ctx_id,
            buffer_id,
            data,
            channel,
            start,
            resp,
        },
        permit,
    )
    .map_err(AudioError::from)?;
    answer
        .await
        .map_err(|_| audio_err("Response channel closed"))?
        .map_err(AudioError::from)
}

// ============================================================================
// Global Audio Options
// ============================================================================

fn audio_platform(
    state: &OpState,
) -> Option<std::sync::Arc<dyn shared::services::AudioPlatformService>> {
    state
        .borrow::<HostOpState>()
        .device_services
        .as_ref()
        .and_then(|services| services.audio_platform())
}

/// Set inner audio options via platform AudioManager.
/// Routes through DeviceServices for platform-specific audio configuration:
/// - mixWithOther: Android audio focus behavior (duck vs abandon)
/// - obeyMuteSwitch: Respect device ringer/mute mode
/// - speakerOn: Route audio output to speaker
#[op2(fast)]
pub fn op_audio_set_inner_audio_option(
    state: &mut OpState,
    mix_with_other: bool,
    obey_mute_switch: bool,
    speaker_on: bool,
) -> Result<(), AudioError> {
    Ok(service::set_inner_audio_option(
        audio_platform(state).as_deref(),
        mix_with_other,
        obey_mute_switch,
        speaker_on,
    )?)
}

/// Get available audio input sources from platform.
/// Queries Android AudioManager/MediaRecorder for supported audio sources.
/// Returns source identifiers matching RecorderManager.start() audioSource param.
#[op2]
#[serde]
pub fn op_audio_get_available_audio_sources(
    state: &mut OpState,
) -> Result<Vec<String>, AudioError> {
    Ok(service::get_available_audio_sources(
        audio_platform(state).as_deref(),
    )?)
}

// ============================================================================
// Recorder Operations
// ============================================================================

/// Start recording with the given options (JSON string).
/// Routes through DeviceServices RecorderService.
#[op2(fast)]
pub fn op_recorder_start(
    state: &mut OpState,
    #[string] options_json: &str,
) -> Result<(), AudioError> {
    crate::permission::require_scope(state, Scope::Record)
        .map_err(|denied| audio_err(denied.to_string()))?;
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(recorder) = services.recorder() {
            return recorder.start(options_json).map_err(AudioError::from);
        }
    }
    Err(audio_err("recorderManager.start:fail not supported"))
}

/// Pause recording.
#[op2(fast)]
pub fn op_recorder_pause(state: &mut OpState) -> Result<(), AudioError> {
    crate::permission::require_scope(state, Scope::Record)
        .map_err(|denied| audio_err(denied.to_string()))?;
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(recorder) = services.recorder() {
            return recorder.pause().map_err(AudioError::from);
        }
    }
    Err(audio_err("recorderManager.pause:fail not supported"))
}

/// Resume recording after pause.
#[op2(fast)]
pub fn op_recorder_resume(state: &mut OpState) -> Result<(), AudioError> {
    crate::permission::require_scope(state, Scope::Record)
        .map_err(|denied| audio_err(denied.to_string()))?;
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(recorder) = services.recorder() {
            return recorder.resume().map_err(AudioError::from);
        }
    }
    Err(audio_err("recorderManager.resume:fail not supported"))
}

/// Stop recording. Results delivered asynchronously via RecorderEvent.
#[op2(fast)]
pub fn op_recorder_stop(state: &mut OpState) -> Result<(), AudioError> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(recorder) = services.recorder() {
            return recorder.stop().map_err(AudioError::from);
        }
    }
    Err(audio_err("recorderManager.stop:fail not supported"))
}

// ============================================================================
// MediaAudioPlayer Operations
// ============================================================================

fn media_audio_player(
    state: Rc<RefCell<OpState>>,
    player_id: u32,
    command: PlayerCommand,
) -> Result<(), AudioError> {
    Ok(service::media_audio_player(
        &get_audio_tx(state),
        player_id,
        command,
    )?)
}

/// Create a MediaAudioPlayer (fire and forget)
#[op2(fast)]
pub fn op_media_audio_player_create(
    state: Rc<RefCell<OpState>>,
    #[smi] player_id: u32,
) -> Result<(), AudioError> {
    media_audio_player(state, player_id, PlayerCommand::Create)
}

/// Add source to MediaAudioPlayer (fire and forget)
#[op2(fast)]
pub fn op_media_audio_player_add_source(
    state: Rc<RefCell<OpState>>,
    #[smi] player_id: u32,
    #[smi] source_id: InnerAudioId,
) -> Result<(), AudioError> {
    media_audio_player(state, player_id, PlayerCommand::AddSource(source_id))
}

/// Remove source from MediaAudioPlayer (fire and forget)
#[op2(fast)]
pub fn op_media_audio_player_remove_source(
    state: Rc<RefCell<OpState>>,
    #[smi] player_id: u32,
    #[smi] source_id: InnerAudioId,
) -> Result<(), AudioError> {
    media_audio_player(state, player_id, PlayerCommand::RemoveSource(source_id))
}

/// Start MediaAudioPlayer (fire and forget)
#[op2(fast)]
pub fn op_media_audio_player_start(
    state: Rc<RefCell<OpState>>,
    #[smi] player_id: u32,
) -> Result<(), AudioError> {
    media_audio_player(state, player_id, PlayerCommand::Start)
}

/// Stop MediaAudioPlayer (fire and forget)
#[op2(fast)]
pub fn op_media_audio_player_stop(
    state: Rc<RefCell<OpState>>,
    #[smi] player_id: u32,
) -> Result<(), AudioError> {
    media_audio_player(state, player_id, PlayerCommand::Stop)
}

/// Destroy MediaAudioPlayer (fire and forget)
#[op2(fast)]
pub fn op_media_audio_player_destroy(
    state: Rc<RefCell<OpState>>,
    #[smi] player_id: u32,
) -> Result<(), AudioError> {
    media_audio_player(state, player_id, PlayerCommand::Destroy)
}

// ============================================================================
// InnerAudioContext Operations
// ============================================================================

fn inner_audio(
    state: Rc<RefCell<OpState>>,
    id: InnerAudioId,
    command: InnerAudioCommand,
) -> Result<(), AudioError> {
    Ok(service::inner_audio(&get_audio_tx(state), id, command)?)
}

/// Create an InnerAudioContext with JS-provided id (fire and forget)
#[op2(fast)]
pub fn op_inner_audio_create(
    state: Rc<RefCell<OpState>>,
    #[smi] id: InnerAudioId,
) -> Result<(), AudioError> {
    inner_audio(state, id, InnerAudioCommand::Create)
}

/// Destroy an InnerAudioContext (fire and forget)
#[op2(fast)]
pub fn op_inner_audio_destroy(
    state: Rc<RefCell<OpState>>,
    #[smi] id: InnerAudioId,
) -> Result<(), AudioError> {
    inner_audio(state, id, InnerAudioCommand::Destroy)
}

/// Load audio data into InnerAudioContext (full load mode - deprecated, use op_inner_audio_load_url)
#[op2(async(lazy))]
#[serde]
pub async fn op_inner_audio_load(
    state: Rc<RefCell<OpState>>,
    #[smi] id: InnerAudioId,
    #[buffer] data: JsBuffer,
) -> Result<InnerAudioInfo, AudioError> {
    let tx = get_audio_tx(state);
    // Bounded and admitted before the bytes are copied out of V8.
    let permit = service::reserve_encoded(&tx, data.len())?;
    Ok(service::inner_audio_load(&tx, id, data.to_vec(), permit)?
        .answer()
        .await?)
}

/// Load audio from URL or local path
/// - HTTP/HTTPS URLs: streaming download (edge-download-edge-play), admitted by
///   this runtime's network gate first
/// - Local paths: read through the game's sandbox and decoded
#[op2(async(lazy), fast)]
pub fn op_inner_audio_load_url(
    state: Rc<RefCell<OpState>>,
    #[smi] id: InnerAudioId,
    #[string] src: &str,
) -> Result<impl std::future::Future<Output = Result<(), AudioError>> + use<>, AudioError> {
    let src = service::prepare_inner_audio_src(src)?;
    let (tx, sources) = {
        let st = state.borrow();
        let host = st.borrow::<HostOpState>();
        (
            host.audio_tx.clone(),
            (host.code_dir.clone(), host.vfs.clone()),
        )
    };
    let (code_dir, vfs) = sources;
    let sources = InnerAudioSources {
        code_dir,
        vfs,
        admit_remote: move |url: &deno_core::url::Url| {
            let st = state.borrow();
            crate::network::gate::enforce_from_state(
                url,
                &st,
                crate::network::gate::GateKind::AudioStream,
            )
            .map_err(|error| service::audio_error(error.to_string()))
        },
    };
    Ok(async move { Ok(service::inner_audio_load_url(tx, sources, id, src).await?) })
}

/// Play InnerAudioContext (fire and forget)
#[op2(fast)]
pub fn op_inner_audio_play(
    state: Rc<RefCell<OpState>>,
    #[smi] id: InnerAudioId,
) -> Result<(), AudioError> {
    inner_audio(state, id, InnerAudioCommand::Play)
}

/// Pause InnerAudioContext (fire and forget)
#[op2(fast)]
pub fn op_inner_audio_pause(
    state: Rc<RefCell<OpState>>,
    #[smi] id: InnerAudioId,
) -> Result<(), AudioError> {
    inner_audio(state, id, InnerAudioCommand::Pause)
}

/// Stop InnerAudioContext (fire and forget)
#[op2(fast)]
pub fn op_inner_audio_stop(
    state: Rc<RefCell<OpState>>,
    #[smi] id: InnerAudioId,
) -> Result<(), AudioError> {
    inner_audio(state, id, InnerAudioCommand::Stop)
}

/// Seek InnerAudioContext (fire and forget)
#[op2(fast)]
pub fn op_inner_audio_seek(
    state: Rc<RefCell<OpState>>,
    #[smi] id: InnerAudioId,
    position: f64,
) -> Result<(), AudioError> {
    inner_audio(state, id, InnerAudioCommand::Seek(position))
}

/// Set volume (fire and forget)
#[op2(fast)]
pub fn op_inner_audio_set_volume(
    state: Rc<RefCell<OpState>>,
    #[smi] id: InnerAudioId,
    volume: f32,
) -> Result<(), AudioError> {
    inner_audio(state, id, InnerAudioCommand::SetVolume(volume))
}

/// Set loop (fire and forget)
#[op2(fast)]
pub fn op_inner_audio_set_loop(
    state: Rc<RefCell<OpState>>,
    #[smi] id: InnerAudioId,
    loop_enabled: bool,
) -> Result<(), AudioError> {
    inner_audio(state, id, InnerAudioCommand::SetLoop(loop_enabled))
}

/// Set playback rate (fire and forget)
#[op2(fast)]
pub fn op_inner_audio_set_playback_rate(
    state: Rc<RefCell<OpState>>,
    #[smi] id: InnerAudioId,
    rate: f32,
) -> Result<(), AudioError> {
    inner_audio(state, id, InnerAudioCommand::SetPlaybackRate(rate))
}

/// Set autoplay (fire and forget)
#[op2(fast)]
pub fn op_inner_audio_set_autoplay(
    state: Rc<RefCell<OpState>>,
    #[smi] id: InnerAudioId,
    autoplay: bool,
) -> Result<(), AudioError> {
    inner_audio(state, id, InnerAudioCommand::SetAutoplay(autoplay))
}

/// Get current state
#[op2(async(lazy), fast)]
#[serde]
pub async fn op_inner_audio_get_state(
    state: Rc<RefCell<OpState>>,
    #[smi] id: InnerAudioId,
) -> Result<InnerAudioState, AudioError> {
    let tx = get_audio_tx(state);
    Ok(service::inner_audio_get_state(&tx, id)?.answer().await?)
}

#[cfg(test)]
mod tests {
    //! The adapters and the engine JavaScript they serve. What the ops do --
    //! checks, limits, ordering of admission against copies -- is tested with
    //! the work, in `migo_services::audio`.

    use super::{AudioCmd, AudioError, AudioSender};
    use migo_services::audio as service;

    /// The body of `name` in `source`, up to `end`.
    fn section<'a>(source: &'a str, name: &str, end: &str) -> &'a str {
        let start = source.find(name).unwrap_or_else(|| panic!("{name}"));
        let stop = source[start..]
            .find(end)
            .map(|offset| start + offset)
            .unwrap_or_else(|| panic!("end of {name}"));
        &source[start..stop]
    }

    #[test]
    fn audio_queue_full_is_not_reported_as_disconnected() {
        let (tx, _rx) = shared::audio_channel::channel();
        for ctx_id in 0..shared::audio_channel::AUDIO_COMMAND_CAPACITY as u32 {
            tx.try_send(AudioCmd::CreateContext {
                ctx_id,
                sample_rate: None,
            })
            .expect("fixture fills the data queue");
        }
        let sender = AudioSender::new(tx, shared::channel::ThreadWakeup::new());
        let (resp, mut response) = tokio::sync::oneshot::channel();

        let error = sender
            .send(AudioCmd::CloseContext { ctx_id: 9, resp })
            .map_err(AudioError::from)
            .expect_err("limit + 1 must fail immediately");

        assert!(error.to_string().contains("InputSaturated"));
        assert!(error.to_string().contains("audio command queue is full"));
        assert!(matches!(
            response.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed)
        ));
    }

    #[test]
    fn audio_buffer_constructor_owns_the_pcm_limit_and_validates_before_reserving() {
        const LIMIT: &str = "64 * 1024 * 1024";
        let buffer = include_str!("00_audio_buffer.js");

        assert!(buffer.contains(LIMIT));
        assert!(!buffer.contains("512 * 1024 * 1024"));
        assert!(buffer.contains(
            "constructor({ numberOfChannels = 1, length, sampleRate }, internal = undefined)"
        ));
        assert!(buffer.contains("op_audio_reserve_buffer"));
        assert!(buffer.contains("op_audio_abort_buffer"));
        assert!(buffer.contains("DECODED_BUFFER_TOKEN"));
        assert!(buffer.contains("internal.token !== DECODED_BUFFER_TOKEN"));
        assert!(!buffer.contains("static _fromDecoded"));
        let reserve = buffer.find("op_audio_reserve_buffer").unwrap();
        let allocate = buffer.find("new ArrayBuffer").unwrap();
        let abort = buffer.find("op_audio_abort_buffer(id)").unwrap();
        assert!(
            reserve < allocate && allocate < abort,
            "reserve/allocate/abort ordering"
        );
        assert!(
            buffer
                .contains("new Float32Array(this.#backing, channel * channelBytes, this.#length)")
        );
    }

    #[test]
    fn web_audio_registries_do_not_strongly_retain_contexts_buffers_or_nodes() {
        let context = include_str!("01_audio_context.js");
        let node = include_str!("00_audio_node.js");
        let buffer = include_str!("00_audio_buffer.js");

        assert!(
            !context.contains("const BUFFER_REGISTRY = new Map()"),
            "AudioBuffer PCM must not be pinned by a module-global strong registry"
        );
        assert!(
            context.contains("new WeakRef(this)"),
            "the lifecycle registry must not keep abandoned AudioContexts alive"
        );
        assert!(
            context.contains("new FinalizationRegistry"),
            "abandoned AudioContexts need a native cleanup path"
        );
        assert!(
            !node.contains("NODE_REGISTRY.set"),
            "an unused registry must not pin every AudioNode and its context"
        );
        assert!(
            buffer.contains("new FinalizationRegistry")
                && buffer.contains("op_audio_release_buffer(id)"),
            "unreachable AudioBuffers must release their native retained PCM"
        );
    }

    #[test]
    fn audio_buffers_are_not_context_scoped_and_setters_only_associate_before_start() {
        let source = include_str!("00_buffer_source_node.js");
        let setter = source.find("set buffer(value)").expect("buffer setter");
        let end = source[setter..]
            .find("get loop()")
            .map(|offset| setter + offset)
            .expect("end of buffer setter");
        let body = &source[setter..end];

        assert!(
            !body.contains("_ctxId"),
            "AudioBuffer must be usable in another context"
        );
        assert!(
            !body.contains("op_audio_set_buffer"),
            "pre-start association stays in JS"
        );
        assert!(body.contains("if (value !== null && this.#bufferWasSet)"));
        assert!(body.contains("if (value !== null) this.#bufferWasSet = true"));
        assert!(body.contains("if (!this.#started)"));
        assert!(body.contains("op_audio_set_started_buffer"));
    }

    #[test]
    fn buffer_source_uses_the_new_synchronous_buffer_ops() {
        let source = include_str!("00_buffer_source_node.js");
        assert!(source.contains("op_audio_start_buffer"));
        assert!(source.contains("op_audio_set_started_buffer"));
        assert!(!source.contains("op_audio_set_buffer"));
        assert!(!source.contains("op_audio_start("));

        let start = source
            .find("start(when = 0, offset = 0, duration)")
            .unwrap();
        let body = &source[start..source[start..].find("stop(when = 0)").unwrap() + start];
        let native = body
            .find("op_audio_start_buffer")
            .expect("synchronous native start");
        let commit = body
            .find("this.#started = true")
            .expect("started-state commit");
        let acquire = body
            .find("buffer._acquireBacking()")
            .expect("backing acquisition");
        assert!(
            acquire < native && native < commit,
            "a failed native start must leave the node startable"
        );
    }

    #[test]
    fn web_audio_ids_are_allocated_by_the_host_across_runtime_restarts() {
        let source = include_str!("01_audio_context.js");

        assert!(source.contains("allocateHostCallbackId"));
        assert!(!source.contains("let nextContextId"));
        assert!(!source.contains("let nextNodeId"));
        assert!(!source.contains("nextContextId++"));
        assert!(!source.contains("nextNodeId++"));
        assert!(
            source.matches("allocateHostCallbackId()").count() >= 2,
            "both contexts and nodes must use the host-lifetime allocator"
        );
    }

    #[test]
    fn audio_buffer_finalizer_releases_the_global_buffer_id_directly() {
        let buffer = include_str!("00_audio_buffer.js");
        let context = include_str!("01_audio_context.js");

        assert!(!buffer.contains("PENDING_BUFFER_RELEASES"));
        assert!(!buffer.contains("setTimeout(flushPendingBufferReleases"));
        assert!(buffer.contains("AUDIO_BUFFER_FINALIZER"));
        assert!(buffer.contains("op_audio_release_buffer(id)"));

        assert!(context.contains("op_audio_release_context"));
        assert!(
            context.contains("createReleaseQueue(op_audio_release_context)"),
            "the context release must go through the shared retrying queue"
        );
    }

    #[test]
    fn unreachable_audio_nodes_release_their_native_node() {
        let node = include_str!("00_audio_node.js");

        assert!(
            node.contains("new FinalizationRegistry"),
            "an unreachable AudioNode needs a native cleanup path"
        );
        assert!(
            node.contains("op_audio_release_node(ctxId, nodeId)"),
            "the finalizer must release the native node"
        );
        assert!(
            node.contains("NODE_FINALIZER.register(this"),
            "every node must be registered when it is constructed"
        );
        // The retry must never hold the object it is trying to free.
        assert!(
            !node.contains("pending.set(key, this)"),
            "a queued release must retain ids, not nodes"
        );
    }

    #[test]
    fn native_releases_share_one_retry_queue() {
        let node = include_str!("00_audio_node.js");
        let context = include_str!("01_audio_context.js");

        assert!(
            node.contains("function createReleaseQueue("),
            "the retry queue must be defined once"
        );
        assert!(
            !context.contains("function flushPendingContextReleases"),
            "the context module must not carry a second copy of the backoff"
        );
        assert!(
            context.contains("createReleaseQueue"),
            "the context release must use the shared queue"
        );
    }

    #[test]
    fn buffer_source_enforces_first_non_null_assignment_as_set_once() {
        let source = include_str!("00_buffer_source_node.js");
        let setter = source.find("set buffer(value)").expect("buffer setter");
        let end = source[setter..]
            .find("get loop()")
            .map(|offset| setter + offset)
            .expect("end of setter");
        let body = &source[setter..end];

        assert!(source.contains("#bufferWasSet = false"));
        let guard = body
            .find("value !== null && this.#bufferWasSet")
            .expect("set-once guard");
        let commit = body
            .find("this.#bufferWasSet = true")
            .expect("successful first assignment commit");
        assert!(guard < commit);
        assert!(
            body.contains("value !== null"),
            "null must not consume the one assignment"
        );
    }

    #[test]
    fn create_buffer_is_synchronous_and_has_no_legacy_native_create_or_copy_ops() {
        let context = include_str!("01_audio_context.js");
        let buffer = include_str!("00_audio_buffer.js");
        let create = context
            .find("  createBuffer(")
            .expect("createBuffer method");
        let body =
            &context[create..context[create..].find("  createBufferSource()").unwrap() + create];

        assert!(!body.contains("async createBuffer"));
        assert!(body.contains("return new AudioBuffer({ numberOfChannels, length, sampleRate })"));
        assert!(!context.contains("op_audio_create_buffer("));
        assert!(!buffer.contains("op_audio_copy_to_channel"));
        assert!(!buffer.contains("op_audio_get_channel_data"));
    }

    #[test]
    fn buffer_reacquires_backing_after_native_acquisition_or_freeze() {
        let buffer = include_str!("00_audio_buffer.js");
        assert!(buffer.contains("_acquireBacking()"));
        assert!(buffer.contains("this.#backing = null"));
        assert!(buffer.contains("this.#channelData = null"));
        assert!(buffer.contains("op_audio_materialize_buffer(this.#id)"));
        assert!(buffer.contains("copyToChannel(source, channelNumber, startInChannel = 0)"));
        assert!(buffer.contains("data.set(source.subarray(0, len), start)"));
    }

    #[test]
    fn decode_builds_each_channel_once_without_legacy_get_channel_data() {
        let context = include_str!("01_audio_context.js");
        let start = context.find("async decodeAudioData").unwrap();
        let body = &context[start..context[start..].find("  createBuffer(").unwrap() + start];
        assert_eq!(body.matches("channelData.push(").count(), 0);
        assert!(!body.contains("op_audio_get_channel_data"));
        assert!(body.contains("createDecodedAudioBuffer"));
    }

    #[test]
    fn v8_encoded_audio_is_capped_and_admitted_before_copying() {
        let source = include_str!("ops.rs");
        for (name, end_marker, copy_marker) in [
            (
                "pub fn op_audio_decode_audio_data",
                "struct V8Planar",
                "extend_from_slice",
            ),
            (
                "pub async fn op_inner_audio_load",
                "/// Load audio from URL or local path",
                ".to_vec()",
            ),
        ] {
            let body = section(source, name, end_marker);
            // `reserve_encoded` bounds the size, then takes queue room.
            let cap = body
                .find("service::reserve_encoded")
                .expect("encoded-size cap must be checked in the op");
            let copy = body
                .find(copy_marker)
                .expect("the V8 backing store is copied by this op");
            assert!(cap < copy, "{name} checked the cap after allocating");
        }
    }

    #[test]
    fn decode_audio_data_detaches_the_exact_input_before_returning_the_future() {
        let rust = include_str!("ops.rs");
        let body = section(rust, "pub fn op_audio_decode_audio_data", "struct V8Planar");

        assert!(body.contains("v8::Local<v8::ArrayBuffer>"));
        assert!(!body.contains("#[buffer] data: JsBuffer"));
        let reserve = body.find("reserve_encoded").expect("queue reservation");
        let copy = body.find("extend_from_slice").expect("bounded input copy");
        let detach = body.find("detach(None)").expect("synchronous detach");
        let future = body
            .find("async move")
            .expect("response future must start after ownership transfer");
        assert!(reserve < copy && copy < detach && detach < future);

        let js = include_str!("01_audio_context.js");
        let body = section(js, "async decodeAudioData", "  createBuffer(");
        assert!(
            body.contains(
                "op_audio_decode_audio_data(\n        this.#nativeId,\n        audioData"
            )
        );
        assert!(!body.contains("new Uint8Array(audioData)\n      )"));
        assert!(body.contains("new DOMException") && body.contains("\"DataCloneError\""));
        assert!(body.contains("const decodePromise"));
        assert!(body.contains("decodePromise.then"));
        assert!(
            !body.contains("if (errorCallback) {\n        errorCallback(error);\n        return;")
        );
    }

    /// A decode is adopted natively as the buffer's frozen PCM: nothing is
    /// reserved for a JavaScript copy, nothing is taken back into one, and an
    /// AudioBuffer that cannot be built gives the entry back.
    #[test]
    fn a_decoded_buffer_is_the_native_entry_with_no_javascript_backing() {
        let context = include_str!("01_audio_context.js");
        let body = section(context, "async decodeAudioData", "  createBuffer(");
        let decode = body
            .find("op_audio_decode_audio_data")
            .expect("native decode");
        let adopt = body
            .find("createDecodedAudioBuffer(info.id, info)")
            .expect("the answer names the adopted entry");
        let abort = body
            .find("op_audio_abort_buffer(info.id)")
            .expect("a failed construction releases the entry");
        assert!(decode < adopt && adopt < abort);
        assert!(!body.contains("op_audio_reserve_buffer"));
        assert!(!context.contains("op_audio_take_decoded_buffer_data"));

        let buffer = include_str!("00_audio_buffer.js");
        let factory = section(buffer, "function createDecodedAudioBuffer", "\n}\n");
        assert!(
            !factory.contains("ArrayBuffer"),
            "a decode carries no backing"
        );
        assert!(
            buffer.contains(
                "this.#initialize(internal.id, numberOfChannels, length, sampleRate, null)"
            )
        );
    }

    #[test]
    fn scheduled_source_time_validation_is_uniform_and_native_defended() {
        for source in [
            include_str!("00_buffer_source_node.js"),
            include_str!("00_constant_source_node.js"),
            include_str!("00_oscillator_node.js"),
        ] {
            assert!(source.contains("InvalidStateError"));
            assert!(source.contains("validateScheduledTime"));
        }
        let node = include_str!("00_audio_node.js");
        assert!(node.contains("Number.isFinite"));
        assert!(node.contains("RangeError"));
    }

    /// Every op that takes a string borrows it from V8 and hands the borrow to
    /// the service, which checks it before it is copied.
    #[test]
    fn string_ops_borrow_their_v8_string() {
        let source = include_str!("ops.rs");
        for name in [
            "op_audio_set_node_param",
            "op_audio_param_set_value_at_time",
            "op_audio_param_linear_ramp",
            "op_audio_param_exponential_ramp",
            "op_audio_param_set_target",
            "op_audio_param_cancel_scheduled",
            "op_audio_set_oscillator_type",
            "op_audio_set_biquad_filter_type",
            "op_audio_set_wave_shaper_oversample",
            "op_audio_set_panning_model",
            "op_audio_set_distance_model",
            "op_audio_set_analyser_scalar",
            "op_audio_set_panner_scalar",
        ] {
            let body = section(source, &format!("pub fn {name}"), "#[op2");
            assert!(body.contains("#[string]"), "{name}");
            assert!(body.contains(": &str"), "{name} must borrow its V8 string");
            assert!(
                !body.contains(".to_owned()") && !body.contains(".to_string()"),
                "{name}"
            );
        }
    }

    #[test]
    fn buffer_ops_hand_the_service_a_borrow_of_the_js_buffer() {
        let source = include_str!("ops.rs");
        let curve = section(source, "pub fn op_audio_set_wave_shaper_curve", "#[op2");
        assert!(curve.contains("#[buffer] curve_bytes: Option<JsBuffer>"));
        assert!(curve.contains("curve_bytes.as_deref()"));
        let response = section(
            source,
            "pub async fn op_audio_get_frequency_response",
            "/// Get current reduction",
        );
        assert!(response.contains("#[buffer] frequencies: JsBuffer"));
        assert!(response.contains("&frequencies"));
    }

    #[test]
    fn local_audio_source_is_borrowed_and_checked_before_the_future_exists() {
        let source = include_str!("ops.rs");
        let entry = section(
            source,
            "pub fn op_inner_audio_load_url",
            "/// Play InnerAudioContext",
        );

        assert!(entry.contains("#[string] src: &str"));
        assert!(entry.contains("Result<impl std::future::Future"));
        let prepare = entry
            .find("service::prepare_inner_audio_src(src)?")
            .unwrap();
        let future = entry.find("async move").unwrap();
        assert!(prepare < future);
    }

    #[test]
    fn oversized_local_audio_source_does_not_allocate_in_proportion_to_input() {
        let oversized = "x".repeat(8 * 1024 * 1024);
        let before = migo_alloc_probe::thread_counts();
        let error = match service::prepare_inner_audio_src(&oversized) {
            Ok(_) => panic!("an oversized source unexpectedly passed validation"),
            Err(error) => error,
        };
        let after = migo_alloc_probe::thread_counts();
        let allocated = after.bytes_allocated - before.bytes_allocated;

        assert!(error.message.contains("InputSaturated"));
        assert!(
            allocated < 64 * 1024,
            "the native guard allocated {allocated} bytes for an oversized borrowed source"
        );
    }

    /// A streamed source is admitted by this runtime's network gate: the
    /// service calls it before the command is queued (see its tests), and the
    /// gate the op gives it is the audio-stream one.
    #[test]
    fn remote_audio_is_admitted_by_the_audio_stream_gate() {
        let source = include_str!("ops.rs");
        let entry = section(
            source,
            "pub fn op_inner_audio_load_url",
            "/// Play InnerAudioContext",
        );
        let admit = entry
            .find("admit_remote")
            .expect("the op supplies the gate");
        let gate = entry
            .find("GateKind::AudioStream")
            .expect("remote audio must use the shared gate");
        assert!(admit < gate);
    }
}
