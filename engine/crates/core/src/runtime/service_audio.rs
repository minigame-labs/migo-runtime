//! The audio service ops, as the external session answers them.
//!
//! Content's Web Audio graph and its InnerAudioContexts play here, on the
//! session's audio thread; what crosses from WebContent is the control stream
//! that drives them. Each op is the embedded runtime's op of the same name --
//! arguments converted by deno_core's rule for each parameter (see
//! `service_args`), the same `migo_services::audio` function the embedded op
//! calls -- and each answer is encoded as the value serde_v8 would have handed
//! JavaScript.
//!
//! PCM does not cross unless content asks for it: a decode is answered with the
//! buffer's shape and the id of the entry it was adopted as, and a buffer that
//! is only played is published from that entry. A buffer's samples come back
//! only for `getChannelData` (`op_audio_materialize_buffer`), and go out only
//! when content starts one it wrote (`op_audio_start_buffer` with a backing).

use frame_wire::value::OwnedValue;
use futures::future::BoxFuture;
use migo_services::ServiceError;
use migo_services::audio::{
    self as service, AnalyserData, AnalyserRead, Automation, BufferScope, InnerAudioCommand,
    InnerAudioSources, LittleEndianPlanar, NodeKind, NodeSetting, PlayerCommand, ScheduledSource,
    StartTiming,
};
use shared::op_state::AudioSender;
use shared::protocol::audio_cmd::{AudioBufferInfo, InnerAudioState};

use super::service_args::{
    boolean, bytes, exactly, f32_bits_of, f64_of, f64s, not_a, optional_bytes, string, u32_of,
};
use super::service_ops::id;

/// The session's audio: the sender its thread reads, and the runtime
/// generation that scopes its buffer ids. Bound once the session thread has
/// built the audio service.
pub(crate) struct AudioBinding {
    pub(crate) sender: AudioSender,
    pub(crate) runtime_generation: i64,
    /// What `setInnerAudioOption` acts on, where the platform has one.
    pub(crate) platform: Option<std::sync::Arc<dyn shared::services::AudioPlatformService>>,
}

impl AudioBinding {
    fn scope(&self) -> BufferScope<'_> {
        BufferScope {
            tx: &self.sender,
            runtime_generation: self.runtime_generation,
        }
    }
}

/// Where an InnerAudioContext reads a packaged sound from.
pub(crate) struct LocalSources {
    pub(crate) code_dir: Option<String>,
    pub(crate) vfs: Option<std::sync::Arc<shared::vfs::VirtualFS>>,
}

/// `AudioBufferInfo`, in its field order: id, duration, sample_rate, channels,
/// length.
fn buffer_info(info: AudioBufferInfo) -> OwnedValue {
    OwnedValue::Array(vec![
        OwnedValue::U32(info.id),
        OwnedValue::F64(info.duration),
        OwnedValue::U32(info.sample_rate),
        OwnedValue::U32(info.channels),
        OwnedValue::U32(info.length),
    ])
}

/// `InnerAudioState`, in its field order: current_time, duration, paused,
/// volume, loop_enabled, playback_rate, buffered. An `f32` is the Number
/// serde_v8 makes of it: the same value, widened.
fn inner_audio_state(state: InnerAudioState) -> OwnedValue {
    OwnedValue::Array(vec![
        OwnedValue::F64(state.current_time),
        OwnedValue::F64(state.duration),
        OwnedValue::Bool(state.paused),
        OwnedValue::F64(f64::from(state.volume)),
        OwnedValue::Bool(state.loop_enabled),
        OwnedValue::F64(f64::from(state.playback_rate)),
        OwnedValue::Bool(state.buffered),
    ])
}

/// `f32`s as their little-endian bytes: how every float array an audio op
/// answers crosses, whatever shape the producer rebuilds from it. One byte
/// buffer, rather than a Number value per sample.
fn le_bytes(floats: &[f32]) -> OwnedValue {
    OwnedValue::Bytes(floats.iter().flat_map(|f| f.to_le_bytes()).collect())
}

fn analyser_data(data: AnalyserData) -> OwnedValue {
    match data {
        AnalyserData::Bytes(bytes) => OwnedValue::Bytes(bytes),
        AnalyserData::Floats(floats) => le_bytes(&floats),
    }
}

/// Whether `op` is a synchronous audio op this module answers.
pub(crate) fn is_sync(op: u32) -> bool {
    matches!(
        op,
        id::op_audio_create_context
            | id::op_audio_reserve_buffer
            | id::op_audio_materialize_buffer
            | id::op_audio_set_inner_audio_option
            | id::op_audio_get_available_audio_sources
            | id::op_media_audio_player_create
            | id::op_inner_audio_create
    )
}

/// Whether `op` is an awaited audio op this module answers.
pub(crate) fn is_async(op: u32) -> bool {
    matches!(
        op,
        id::op_audio_close_context
            | id::op_audio_resume_context
            | id::op_audio_suspend_context
            | id::op_audio_decode_audio_data
            | id::op_audio_stop
            | id::op_audio_connect
            | id::op_audio_disconnect
            | id::op_audio_analyser_byte_time_domain
            | id::op_audio_analyser_float_time_domain
            | id::op_audio_analyser_byte_frequency
            | id::op_audio_analyser_float_frequency
            | id::op_audio_get_frequency_response
            | id::op_audio_get_reduction
            | id::op_inner_audio_load_url
            | id::op_inner_audio_get_state
    )
}

/// Run a synchronous audio op. Called on the synchronous endpoint's thread,
/// once the call's turn has come: every command content sent before it has
/// been applied.
pub(crate) fn call_sync(
    audio: &AudioBinding,
    op: u32,
    args: Vec<OwnedValue>,
) -> Result<OwnedValue, ServiceError> {
    let tx = &audio.sender;
    match op {
        id::op_audio_create_context => {
            let [ctx_id, sample_rate] = exactly(op, args)?;
            service::create_context(tx, u32_of(op, 0, ctx_id)?, u32_of(op, 1, sample_rate)?)
                .map(|()| OwnedValue::Null)
        }
        id::op_audio_reserve_buffer => {
            let [channels, length, sample_rate] = exactly(op, args)?;
            service::reserve_buffer(
                audio.scope(),
                u32_of(op, 0, channels)?,
                u32_of(op, 1, length)?,
                u32_of(op, 2, sample_rate)?,
            )
            .map(OwnedValue::U32)
        }
        id::op_audio_materialize_buffer => {
            let [buffer_id] = exactly(op, args)?;
            service::materialize_buffer(audio.scope(), u32_of(op, 0, buffer_id)?)
                .map(|planar| le_bytes(&planar))
        }
        id::op_audio_set_inner_audio_option => {
            let [mix_with_other, obey_mute_switch, speaker_on] = exactly(op, args)?;
            service::set_inner_audio_option(
                audio.platform.as_deref(),
                boolean(op, 0, mix_with_other)?,
                boolean(op, 1, obey_mute_switch)?,
                boolean(op, 2, speaker_on)?,
            )
            .map(|()| OwnedValue::Null)
        }
        id::op_audio_get_available_audio_sources => {
            let [] = exactly(op, args)?;
            service::get_available_audio_sources(audio.platform.as_deref()).map(|sources| {
                OwnedValue::Array(sources.into_iter().map(OwnedValue::Str).collect())
            })
        }
        id::op_media_audio_player_create => {
            let [player_id] = exactly(op, args)?;
            service::media_audio_player(tx, u32_of(op, 0, player_id)?, PlayerCommand::Create)
                .map(|()| OwnedValue::Null)
        }
        id::op_inner_audio_create => {
            let [inner_id] = exactly(op, args)?;
            service::inner_audio(tx, u32_of(op, 0, inner_id)?, InnerAudioCommand::Create)
                .map(|()| OwnedValue::Null)
        }
        other => Err(not_a(other, "synchronous audio")),
    }
}

/// Start an awaited audio op. Its command is sent now, in the order the
/// session dispatches; the future only collects the answer.
pub(crate) fn call_async(
    audio: &AudioBinding,
    sources: LocalSources,
    op: u32,
    args: Vec<OwnedValue>,
) -> Result<BoxFuture<'static, Result<OwnedValue, ServiceError>>, ServiceError> {
    let tx = &audio.sender;
    let nothing = |()| OwnedValue::Null;
    Ok(match op {
        id::op_audio_close_context => {
            let [ctx_id] = exactly(op, args)?;
            let pending = service::close_context(tx, u32_of(op, 0, ctx_id)?)?;
            Box::pin(async move { pending.answer().await.map(nothing) })
        }
        id::op_audio_resume_context => {
            let [ctx_id] = exactly(op, args)?;
            let pending = service::resume_context(tx, u32_of(op, 0, ctx_id)?)?;
            Box::pin(async move { pending.answer().await.map(nothing) })
        }
        id::op_audio_suspend_context => {
            let [ctx_id] = exactly(op, args)?;
            let pending = service::suspend_context(tx, u32_of(op, 0, ctx_id)?)?;
            Box::pin(async move { pending.answer().await.map(nothing) })
        }
        id::op_audio_decode_audio_data => {
            // The producer detached its ArrayBuffer and sent its bytes; they
            // are moved into the decode, not copied again.
            let [ctx_id, data] = exactly(op, args)?;
            let ctx_id = u32_of(op, 0, ctx_id)?;
            let mut data = bytes(op, 1, data)?;
            data.shrink_to_fit();
            let permit = service::reserve_encoded(tx, data.len())?;
            let decoding = service::decode_audio_data(audio.scope(), ctx_id, data, permit)?;
            Box::pin(async move { decoding.answer().await.map(buffer_info) })
        }
        id::op_audio_stop => {
            let [node_id, when] = exactly(op, args)?;
            let pending = service::stop(tx, u32_of(op, 0, node_id)?, f64_of(op, 1, when)?)?;
            Box::pin(async move { pending.answer().await.map(nothing) })
        }
        id::op_audio_connect => {
            let [src, dst, src_output, dst_input] = exactly(op, args)?;
            let pending = service::connect(
                tx,
                u32_of(op, 0, src)?,
                u32_of(op, 1, dst)?,
                u32_of(op, 2, src_output)?,
                u32_of(op, 3, dst_input)?,
            )?;
            Box::pin(async move { pending.answer().await.map(nothing) })
        }
        id::op_audio_disconnect => {
            let [node_id] = exactly(op, args)?;
            let pending = service::disconnect(tx, u32_of(op, 0, node_id)?)?;
            Box::pin(async move { pending.answer().await.map(nothing) })
        }
        id::op_audio_analyser_byte_time_domain
        | id::op_audio_analyser_float_time_domain
        | id::op_audio_analyser_byte_frequency
        | id::op_audio_analyser_float_frequency => {
            let read = match op {
                id::op_audio_analyser_byte_time_domain => AnalyserRead::ByteTimeDomain,
                id::op_audio_analyser_float_time_domain => AnalyserRead::FloatTimeDomain,
                id::op_audio_analyser_byte_frequency => AnalyserRead::ByteFrequency,
                _ => AnalyserRead::FloatFrequency,
            };
            let [node_id] = exactly(op, args)?;
            let reading = service::read_analyser(tx, read, u32_of(op, 0, node_id)?)?;
            Box::pin(async move { reading.await.map(analyser_data) })
        }
        id::op_audio_get_frequency_response => {
            let [node_id, frequencies] = exactly(op, args)?;
            let node_id = u32_of(op, 0, node_id)?;
            let frequencies = bytes(op, 1, frequencies)?;
            let pending = service::get_frequency_response(tx, node_id, &frequencies)?;
            Box::pin(async move {
                pending.answer().await.map(|(magnitude, phase)| {
                    OwnedValue::Array(vec![le_bytes(&magnitude), le_bytes(&phase)])
                })
            })
        }
        id::op_audio_get_reduction => {
            let [node_id] = exactly(op, args)?;
            let pending = service::get_reduction(tx, u32_of(op, 0, node_id)?)?;
            Box::pin(async move {
                pending
                    .answer()
                    .await
                    .map(|reduction| OwnedValue::F64(f64::from(reduction)))
            })
        }
        id::op_inner_audio_load_url => {
            let [inner_id, src] = exactly(op, args)?;
            let inner_id = u32_of(op, 0, inner_id)?;
            let src = service::prepare_inner_audio_src(&string(op, 1, src)?)?;
            let sources = InnerAudioSources {
                code_dir: sources.code_dir,
                vfs: sources.vfs,
                admit_remote: remote_audio_unavailable,
            };
            let tx = tx.clone();
            Box::pin(async move {
                service::inner_audio_load_url(tx, sources, inner_id, src)
                    .await
                    .map(nothing)
            })
        }
        id::op_inner_audio_get_state => {
            let [inner_id] = exactly(op, args)?;
            let pending = service::inner_audio_get_state(tx, u32_of(op, 0, inner_id)?)?;
            Box::pin(async move { pending.answer().await.map(inner_audio_state) })
        }
        other => return Err(not_a(other, "awaited audio")),
    })
}

/// A streamed `src`, before the network service exists on this lane: refused
/// with the reason, as the embedded runtime refuses a source its network
/// policy blocks -- never fetched past the policy every other request is held
/// to.
fn remote_audio_unavailable(url: &service::Url) -> Result<(), ServiceError> {
    Err(service::audio_error(format!(
        "{url}: this session has no network service to stream audio with"
    )))
}

/// Whether `op` is an audio command this module applies.
pub(crate) fn is_command(op: u32) -> bool {
    matches!(
        op,
        id::op_audio_release_context
            | id::op_audio_release_node
            | id::op_audio_abort_buffer
            | id::op_audio_release_buffer
            | id::op_audio_start_buffer
            | id::op_audio_set_started_buffer
            | id::op_audio_create_buffer_source
            | id::op_audio_create_gain
            | id::op_audio_create_oscillator
            | id::op_audio_create_delay
            | id::op_audio_create_biquad_filter
            | id::op_audio_create_wave_shaper
            | id::op_audio_create_analyser
            | id::op_audio_create_dynamics_compressor
            | id::op_audio_create_panner
            | id::op_audio_create_channel_merger
            | id::op_audio_create_channel_splitter
            | id::op_audio_create_constant_source
            | id::op_audio_create_iir_filter
            | id::op_audio_set_loop
            | id::op_audio_set_gain_value
            | id::op_audio_set_node_param
            | id::op_audio_param_set_value_at_time
            | id::op_audio_param_linear_ramp
            | id::op_audio_param_exponential_ramp
            | id::op_audio_param_set_target
            | id::op_audio_param_cancel_scheduled
            | id::op_audio_set_oscillator_type
            | id::op_audio_start_oscillator
            | id::op_audio_stop_oscillator
            | id::op_audio_set_biquad_filter_type
            | id::op_audio_set_wave_shaper_curve
            | id::op_audio_set_wave_shaper_oversample
            | id::op_audio_set_analyser_fft_size
            | id::op_audio_set_panning_model
            | id::op_audio_set_distance_model
            | id::op_audio_set_analyser_scalar
            | id::op_audio_set_panner_scalar
            | id::op_audio_start_constant_source
            | id::op_audio_stop_constant_source
            | id::op_media_audio_player_add_source
            | id::op_media_audio_player_remove_source
            | id::op_media_audio_player_start
            | id::op_media_audio_player_stop
            | id::op_media_audio_player_destroy
            | id::op_inner_audio_destroy
            | id::op_inner_audio_play
            | id::op_inner_audio_pause
            | id::op_inner_audio_stop
            | id::op_inner_audio_seek
            | id::op_inner_audio_set_volume
            | id::op_inner_audio_set_loop
            | id::op_inner_audio_set_playback_rate
            | id::op_inner_audio_set_autoplay
    )
}

/// Apply an audio command, in dispatch order. Nothing answers it: the
/// producer has already made every check a well-formed call can fail, so what
/// is left to fail here -- a full queue, a malformed call -- is logged by the
/// caller, as the embedded runtime's failed fire-and-forget op is.
pub(crate) fn command(
    audio: &AudioBinding,
    op: u32,
    args: Vec<OwnedValue>,
) -> Result<(), ServiceError> {
    let tx = &audio.sender;
    let node = |kind: NodeKind, args: Vec<OwnedValue>| -> Result<(), ServiceError> {
        let [ctx_id, node_id] = exactly(op, args)?;
        service::create_node(tx, kind, u32_of(op, 0, ctx_id)?, u32_of(op, 1, node_id)?)
    };
    let setting = |setting: NodeSetting, args: Vec<OwnedValue>| -> Result<(), ServiceError> {
        let [node_id, value] = exactly(op, args)?;
        service::set_node_setting(tx, setting, u32_of(op, 0, node_id)?, &string(op, 1, value)?)
    };
    let scheduled =
        |source: ScheduledSource, start: bool, args: Vec<OwnedValue>| -> Result<(), ServiceError> {
            let [node_id, when] = exactly(op, args)?;
            let (node_id, when) = (u32_of(op, 0, node_id)?, f64_of(op, 1, when)?);
            if start {
                service::start_source(tx, source, node_id, when)
            } else {
                service::stop_source(tx, source, node_id, when)
            }
        };
    let player = |args: Vec<OwnedValue>, command: PlayerCommand| -> Result<(), ServiceError> {
        let [player_id] = exactly(op, args)?;
        service::media_audio_player(tx, u32_of(op, 0, player_id)?, command)
    };
    let player_source = |args: Vec<OwnedValue>, add: bool| -> Result<(), ServiceError> {
        let [player_id, source_id] = exactly(op, args)?;
        let source_id = u32_of(op, 1, source_id)?;
        service::media_audio_player(
            tx,
            u32_of(op, 0, player_id)?,
            if add {
                PlayerCommand::AddSource(source_id)
            } else {
                PlayerCommand::RemoveSource(source_id)
            },
        )
    };
    let inner = |args: Vec<OwnedValue>, command: InnerAudioCommand| -> Result<(), ServiceError> {
        let [inner_id] = exactly(op, args)?;
        service::inner_audio(tx, u32_of(op, 0, inner_id)?, command)
    };
    let inner_with = |args: Vec<OwnedValue>,
                      command: &dyn Fn(OwnedValue) -> Result<InnerAudioCommand, ServiceError>|
     -> Result<(), ServiceError> {
        let [inner_id, value] = exactly(op, args)?;
        let inner_id = u32_of(op, 0, inner_id)?;
        service::inner_audio(tx, inner_id, command(value)?)
    };
    let automation = |args: Vec<OwnedValue>| -> Result<(), ServiceError> {
        let (node_id, param_name, automation) = match op {
            id::op_audio_param_set_value_at_time => {
                let [node_id, name, value, time] = exactly(op, args)?;
                (
                    node_id,
                    name,
                    Automation::SetValueAtTime {
                        value: f32_bits_of(op, 2, value)?,
                        time: f64_of(op, 3, time)?,
                    },
                )
            }
            id::op_audio_param_linear_ramp => {
                let [node_id, name, value, end_time] = exactly(op, args)?;
                (
                    node_id,
                    name,
                    Automation::LinearRamp {
                        value: f32_bits_of(op, 2, value)?,
                        end_time: f64_of(op, 3, end_time)?,
                    },
                )
            }
            id::op_audio_param_exponential_ramp => {
                let [node_id, name, value, end_time] = exactly(op, args)?;
                (
                    node_id,
                    name,
                    Automation::ExponentialRamp {
                        value: f32_bits_of(op, 2, value)?,
                        end_time: f64_of(op, 3, end_time)?,
                    },
                )
            }
            id::op_audio_param_set_target => {
                let [node_id, name, target, start_time, time_constant] = exactly(op, args)?;
                (
                    node_id,
                    name,
                    Automation::SetTarget {
                        target: f32_bits_of(op, 2, target)?,
                        start_time: f64_of(op, 3, start_time)?,
                        time_constant: f64_of(op, 4, time_constant)?,
                    },
                )
            }
            _ => {
                let [node_id, name, cancel_time] = exactly(op, args)?;
                (
                    node_id,
                    name,
                    Automation::CancelScheduled {
                        cancel_time: f64_of(op, 2, cancel_time)?,
                    },
                )
            }
        };
        service::automate(
            tx,
            u32_of(op, 0, node_id)?,
            &string(op, 1, param_name)?,
            automation,
        )
    };

    match op {
        id::op_audio_release_context => {
            let [ctx_id] = exactly(op, args)?;
            service::release_context(tx, u32_of(op, 0, ctx_id)?)
        }
        id::op_audio_release_node => {
            let [ctx_id, node_id] = exactly(op, args)?;
            service::release_node(tx, u32_of(op, 0, ctx_id)?, u32_of(op, 1, node_id)?)
        }
        id::op_audio_abort_buffer | id::op_audio_release_buffer => {
            let [buffer_id] = exactly(op, args)?;
            service::release_buffer(audio.scope(), u32_of(op, 0, buffer_id)?);
            Ok(())
        }
        id::op_audio_start_buffer => {
            let [ctx_id, node_id, buffer_id, backing, when, offset, duration] = exactly(op, args)?;
            let (ctx_id, node_id) = (u32_of(op, 0, ctx_id)?, u32_of(op, 1, node_id)?);
            let buffer_id = u32_of(op, 2, buffer_id)?;
            let backing = optional_bytes(op, 3, backing)?;
            let timing = StartTiming {
                when: f64_of(op, 4, when)?,
                offset: f64_of(op, 5, offset)?,
                duration: f64_of(op, 6, duration)?,
            };
            service::publish_buffer(
                audio.scope(),
                ctx_id,
                node_id,
                buffer_id,
                backing.as_deref().map(LittleEndianPlanar),
                Some(timing),
                || {},
            )
        }
        id::op_audio_set_started_buffer => {
            let [ctx_id, node_id, buffer_id, backing] = exactly(op, args)?;
            let (ctx_id, node_id) = (u32_of(op, 0, ctx_id)?, u32_of(op, 1, node_id)?);
            let buffer_id = u32_of(op, 2, buffer_id)?;
            let backing = optional_bytes(op, 3, backing)?;
            service::publish_buffer(
                audio.scope(),
                ctx_id,
                node_id,
                buffer_id,
                backing.as_deref().map(LittleEndianPlanar),
                None,
                || {},
            )
        }
        id::op_audio_create_buffer_source => node(NodeKind::BufferSource, args),
        id::op_audio_create_gain => node(NodeKind::Gain, args),
        id::op_audio_create_oscillator => node(NodeKind::Oscillator, args),
        id::op_audio_create_biquad_filter => node(NodeKind::BiquadFilter, args),
        id::op_audio_create_wave_shaper => node(NodeKind::WaveShaper, args),
        id::op_audio_create_analyser => node(NodeKind::Analyser, args),
        id::op_audio_create_dynamics_compressor => node(NodeKind::DynamicsCompressor, args),
        id::op_audio_create_panner => node(NodeKind::Panner, args),
        id::op_audio_create_constant_source => node(NodeKind::ConstantSource, args),
        id::op_audio_create_delay => {
            let [ctx_id, node_id, max_delay_time] = exactly(op, args)?;
            service::create_delay(
                tx,
                u32_of(op, 0, ctx_id)?,
                u32_of(op, 1, node_id)?,
                f32_bits_of(op, 2, max_delay_time)?,
            )
        }
        id::op_audio_create_channel_merger => {
            let [ctx_id, node_id, inputs] = exactly(op, args)?;
            service::create_channel_merger(
                tx,
                u32_of(op, 0, ctx_id)?,
                u32_of(op, 1, node_id)?,
                u32_of(op, 2, inputs)?,
            )
        }
        id::op_audio_create_channel_splitter => {
            let [ctx_id, node_id, outputs] = exactly(op, args)?;
            service::create_channel_splitter(
                tx,
                u32_of(op, 0, ctx_id)?,
                u32_of(op, 1, node_id)?,
                u32_of(op, 2, outputs)?,
            )
        }
        id::op_audio_create_iir_filter => {
            let [ctx_id, node_id, feedforward, feedback] = exactly(op, args)?;
            service::create_iir_filter(
                tx,
                u32_of(op, 0, ctx_id)?,
                u32_of(op, 1, node_id)?,
                f64s(op, 2, feedforward)?,
                f64s(op, 3, feedback)?,
            )
        }
        id::op_audio_set_loop => {
            let [node_id, loop_enabled, loop_start, loop_end] = exactly(op, args)?;
            service::set_loop(
                tx,
                u32_of(op, 0, node_id)?,
                boolean(op, 1, loop_enabled)?,
                f64_of(op, 2, loop_start)?,
                f64_of(op, 3, loop_end)?,
            )
        }
        id::op_audio_set_gain_value => {
            let [node_id, value] = exactly(op, args)?;
            service::set_gain_value(tx, u32_of(op, 0, node_id)?, f32_bits_of(op, 1, value)?)
        }
        id::op_audio_set_node_param => {
            let [node_id, name, value] = exactly(op, args)?;
            service::set_node_param(
                tx,
                u32_of(op, 0, node_id)?,
                &string(op, 1, name)?,
                f32_bits_of(op, 2, value)?,
            )
        }
        id::op_audio_param_set_value_at_time
        | id::op_audio_param_linear_ramp
        | id::op_audio_param_exponential_ramp
        | id::op_audio_param_set_target
        | id::op_audio_param_cancel_scheduled => automation(args),
        id::op_audio_set_oscillator_type => setting(NodeSetting::OscillatorType, args),
        id::op_audio_set_biquad_filter_type => setting(NodeSetting::BiquadFilterType, args),
        id::op_audio_set_wave_shaper_oversample => setting(NodeSetting::WaveShaperOversample, args),
        id::op_audio_set_panning_model => setting(NodeSetting::PanningModel, args),
        id::op_audio_set_distance_model => setting(NodeSetting::DistanceModel, args),
        id::op_audio_start_oscillator => scheduled(ScheduledSource::Oscillator, true, args),
        id::op_audio_stop_oscillator => scheduled(ScheduledSource::Oscillator, false, args),
        id::op_audio_start_constant_source => {
            scheduled(ScheduledSource::ConstantSource, true, args)
        }
        id::op_audio_stop_constant_source => {
            scheduled(ScheduledSource::ConstantSource, false, args)
        }
        id::op_audio_set_wave_shaper_curve => {
            let [node_id, curve] = exactly(op, args)?;
            let node_id = u32_of(op, 0, node_id)?;
            service::set_wave_shaper_curve(tx, node_id, optional_bytes(op, 1, curve)?.as_deref())
        }
        id::op_audio_set_analyser_fft_size => {
            let [node_id, fft_size] = exactly(op, args)?;
            service::set_analyser_fft_size(tx, u32_of(op, 0, node_id)?, u32_of(op, 1, fft_size)?)
        }
        id::op_audio_set_analyser_scalar => {
            let [node_id, prop, value] = exactly(op, args)?;
            service::set_analyser_scalar(
                tx,
                u32_of(op, 0, node_id)?,
                &string(op, 1, prop)?,
                f32_bits_of(op, 2, value)?,
            )
        }
        id::op_audio_set_panner_scalar => {
            let [node_id, prop, value] = exactly(op, args)?;
            service::set_panner_scalar(
                tx,
                u32_of(op, 0, node_id)?,
                &string(op, 1, prop)?,
                f64_of(op, 2, value)?,
            )
        }
        id::op_media_audio_player_add_source => player_source(args, true),
        id::op_media_audio_player_remove_source => player_source(args, false),
        id::op_media_audio_player_start => player(args, PlayerCommand::Start),
        id::op_media_audio_player_stop => player(args, PlayerCommand::Stop),
        id::op_media_audio_player_destroy => player(args, PlayerCommand::Destroy),
        id::op_inner_audio_destroy => inner(args, InnerAudioCommand::Destroy),
        id::op_inner_audio_play => inner(args, InnerAudioCommand::Play),
        id::op_inner_audio_pause => inner(args, InnerAudioCommand::Pause),
        id::op_inner_audio_stop => inner(args, InnerAudioCommand::Stop),
        id::op_inner_audio_seek => inner_with(args, &|value| {
            f64_of(op, 1, value).map(InnerAudioCommand::Seek)
        }),
        id::op_inner_audio_set_volume => inner_with(args, &|value| {
            f32_bits_of(op, 1, value).map(InnerAudioCommand::SetVolume)
        }),
        id::op_inner_audio_set_loop => inner_with(args, &|value| {
            boolean(op, 1, value).map(InnerAudioCommand::SetLoop)
        }),
        id::op_inner_audio_set_playback_rate => inner_with(args, &|value| {
            f32_bits_of(op, 1, value).map(InnerAudioCommand::SetPlaybackRate)
        }),
        id::op_inner_audio_set_autoplay => inner_with(args, &|value| {
            boolean(op, 1, value).map(InnerAudioCommand::SetAutoplay)
        }),
        other => Err(not_a(other, "audio command")),
    }
}

#[cfg(test)]
#[path = "service_audio_tests.rs"]
mod tests;
