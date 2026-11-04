//! Borrowed parameter Strings retained their allocation until the audio match ended.
//! Preserve each original effect and hand only large admitted inputs to CPU drop.
use super::*;

pub(super) fn forward(
    delivery: AudioCommandDelivery,
    contexts: &mut impl ContextAccess,
    index: &NodeContextIndex,
    inbox: &mut crate::command_inbox::Inbox,
) -> Option<AudioCommandDelivery> {
    if !delivery.has_queue_permit() || delivery.command().queued_payload_bytes() < 4096 {
        return Some(delivery);
    }
    if !apply(delivery.command(), contexts, index) {
        return Some(delivery);
    }
    inbox.finish_parameters(delivery);
    None
}

fn owner<'a>(
    contexts: &'a mut impl ContextAccess,
    index: &NodeContextIndex,
    node_id: AudioNodeId,
) -> Option<&'a mut AudioContext> {
    index
        .get_context(node_id)
        .and_then(|id| contexts.get_mut(&id))
}

pub(super) fn apply(
    command: &AudioCmd,
    contexts: &mut impl ContextAccess,
    index: &NodeContextIndex,
) -> bool {
    match command {
        AudioCmd::SetNodeParam {
            node_id,
            param_name,
            value,
        } => {
            let found = index
                .get_context(*node_id)
                .and_then(|ctx_id| contexts.get_mut(&ctx_id))
                .map(|ctx| ctx.set_node_param(*node_id, param_name, *value))
                .unwrap_or(false);

            if !found {
                tracing::warn!(
                    "SetNodeParam: node {} param {} not found",
                    *node_id,
                    param_name
                );
            }
        }
        AudioCmd::SetOscillatorType { node_id, osc_type } => {
            if let Some(ctx) = owner(contexts, index, *node_id) {
                ctx.with_node_typed::<OscillatorNode, _>(*node_id, |osc| {
                    osc.set_type(OscillatorType::from_str(osc_type));
                });
            }
        }
        AudioCmd::SetBiquadFilterType {
            node_id,
            filter_type,
        } => {
            if let Some(ctx) = owner(contexts, index, *node_id) {
                ctx.with_node_typed::<BiquadFilterNode, _>(*node_id, |filt| {
                    filt.set_type(BiquadFilterType::from_str(filter_type));
                });
            }
        }
        AudioCmd::SetWaveShaperOversample {
            node_id,
            oversample,
        } => {
            if let Some(ctx) = owner(contexts, index, *node_id) {
                ctx.set_wave_shaper_oversample(*node_id, OversampleType::from_str(oversample));
            }
        }
        AudioCmd::SetPanningModel { node_id, model } => {
            if let Some(ctx) = owner(contexts, index, *node_id) {
                ctx.with_node_typed::<PannerNode, _>(*node_id, |p| {
                    p.set_panning_model(PanningModel::from_str(model));
                });
            }
        }
        AudioCmd::SetDistanceModel { node_id, model } => {
            if let Some(ctx) = owner(contexts, index, *node_id) {
                ctx.with_node_typed::<PannerNode, _>(*node_id, |p| {
                    p.set_distance_model(DistanceModel::from_str(model));
                });
            }
        }
        AudioCmd::SetAnalyserScalar {
            node_id,
            prop,
            value,
        } => {
            if let Some(ctx) = owner(contexts, index, *node_id) {
                ctx.with_node_typed::<AnalyserNode, _>(*node_id, |an| match prop.as_str() {
                    "minDecibels" => an.set_min_decibels(*value),
                    "maxDecibels" => an.set_max_decibels(*value),
                    "smoothingTimeConstant" => an.set_smoothing_time_constant(*value),
                    _ => {}
                });
            }
        }
        AudioCmd::SetPannerScalar {
            node_id,
            prop,
            value,
        } => {
            if let Some(ctx) = owner(contexts, index, *node_id) {
                ctx.with_node_typed::<PannerNode, _>(*node_id, |p| match prop.as_str() {
                    "refDistance" => p.set_ref_distance(*value),
                    "maxDistance" => p.set_max_distance(*value),
                    "rolloffFactor" => p.set_rolloff_factor(*value),
                    "coneInnerAngle" => p.set_cone_inner_angle(*value),
                    "coneOuterAngle" => p.set_cone_outer_angle(*value),
                    "coneOuterGain" => p.set_cone_outer_gain(*value),
                    _ => {}
                });
            }
        }
        AudioCmd::AudioParamSetValueAtTime {
            node_id,
            param_name,
            value,
            time,
        } => {
            if let Some(ctx) = owner(contexts, index, *node_id) {
                ctx.param_set_value_at_time(*node_id, param_name, *value, *time);
            }
        }
        AudioCmd::AudioParamLinearRamp {
            node_id,
            param_name,
            value,
            end_time,
        } => {
            if let Some(ctx) = owner(contexts, index, *node_id) {
                ctx.param_linear_ramp(*node_id, param_name, *value, *end_time);
            }
        }
        AudioCmd::AudioParamExponentialRamp {
            node_id,
            param_name,
            value,
            end_time,
        } => {
            if let Some(ctx) = owner(contexts, index, *node_id) {
                ctx.param_exponential_ramp(*node_id, param_name, *value, *end_time);
            }
        }
        AudioCmd::AudioParamSetTarget {
            node_id,
            param_name,
            target,
            start_time,
            time_constant,
        } => {
            if let Some(ctx) = owner(contexts, index, *node_id) {
                ctx.param_set_target(*node_id, param_name, *target, *start_time, *time_constant);
            }
        }
        AudioCmd::AudioParamCancelScheduled {
            node_id,
            param_name,
            cancel_time,
        } => {
            if let Some(ctx) = owner(contexts, index, *node_id) {
                ctx.param_cancel_scheduled(*node_id, param_name, *cancel_time);
            }
        }
        _ => return false,
    }
    true
}

#[cfg(test)]
#[path = "audio_thread_command_parameters_tests.rs"]
pub(super) mod tests;
