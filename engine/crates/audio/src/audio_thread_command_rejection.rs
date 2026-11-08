//! Refuse invalid owners while the original delivery still owns its payload.
//! Executable commands keep their exact original dispatch and DSP behavior.
use super::*;

pub(super) fn forward(
    delivery: AudioCommandDelivery,
    contexts: &mut impl ContextAccess,
    index: &NodeContextIndex,
    players: &impl PlayerView,
    inbox: &mut crate::command_inbox::Inbox,
) -> Option<AudioCommandDelivery> {
    if let Some(error) = rejection(delivery.command(), contexts, index, players) {
        inbox.reject(delivery, error);
        None
    } else {
        Some(delivery)
    }
}

fn rejection(
    command: &AudioCmd,
    contexts: &mut impl ContextAccess,
    index: &NodeContextIndex,
    players: &impl PlayerView,
) -> Option<EngineError> {
    let missing_context =
        |id| EngineError::from_detail(ErrorCode::NotFound, format!("AudioContext {id} not found"));
    let missing_inner = |id| {
        EngineError::from_detail(
            ErrorCode::NotFound,
            format!("InnerAudioContext {id} not found"),
        )
    };
    let missing_node = || EngineError::new(ErrorCode::NotFound);
    match command {
        AudioCmd::DecodeAudioData { ctx_id, resp, .. } => {
            if !contexts.contains_key(ctx_id) {
                Some(missing_context(ctx_id))
            } else if resp.is_closed() {
                Some(EngineError::from_detail(
                    ErrorCode::Cancelled,
                    "audio decode request cancelled",
                ))
            } else {
                None
            }
        }
        AudioCmd::CopyToChannel { ctx_id, .. } | AudioCmd::CreateIIRFilter { ctx_id, .. } => {
            (!contexts.contains_key(ctx_id)).then(|| missing_context(ctx_id))
        }
        AudioCmd::SetStartedBuffer {
            ctx_id, node_id, ..
        }
        | AudioCmd::StartBuffer {
            ctx_id, node_id, ..
        } => {
            let owned = index.get_context(*node_id) == Some(*ctx_id)
                && contexts
                    .get_mut(ctx_id)
                    .is_some_and(|ctx| ctx.node_is::<crate::nodes::BufferSourceNode>(*node_id));
            (!owned).then(missing_node)
        }
        AudioCmd::SetWaveShaperCurve { node_id, .. } => {
            let owned = index
                .get_context(*node_id)
                .and_then(|id| contexts.get_mut(&id))
                .is_some_and(|ctx| ctx.node_is::<WaveShaperNode>(*node_id));
            (!owned).then(missing_node)
        }
        AudioCmd::GetFrequencyResponse { node_id, .. } => {
            let owned = index
                .get_context(*node_id)
                .and_then(|id| contexts.get_mut(&id))
                .is_some_and(|ctx| {
                    ctx.node_is::<BiquadFilterNode>(*node_id)
                        || ctx.node_is::<IIRFilterNode>(*node_id)
                });
            (!owned).then(|| {
                EngineError::from_detail(
                    ErrorCode::NotFound,
                    format!("Filter node {node_id} not found"),
                )
            })
        }
        AudioCmd::InnerAudioLoad { id, .. } => {
            (!players.contains_key(id)).then(|| missing_inner(id))
        }
        AudioCmd::InnerAudioCompleteLoad { id, load, resp, .. } => {
            (!players.get(id).is_some_and(|p| p.shared.owns_load(load)) || resp.is_closed())
                .then(crate::inner_audio::InnerAudioLoadToken::cancelled_error)
        }
        AudioCmd::InnerAudioLoadFile { id, load, resp, .. } => {
            if let Some(player) = players.get(id) {
                if !player.shared.owns_load(load) || resp.is_closed() {
                    Some(crate::inner_audio::InnerAudioLoadToken::cancelled_error())
                } else {
                    None
                }
            } else {
                Some(missing_inner(id))
            }
        }
        AudioCmd::InnerAudioLoadUrl { id, load, .. } => {
            if let Some(player) = players.get(id) {
                (!player.shared.owns_load(load))
                    .then(crate::inner_audio::InnerAudioLoadToken::cancelled_error)
            } else {
                Some(missing_inner(id))
            }
        }
        AudioCmd::SetNodeParam { node_id, .. }
        | AudioCmd::AudioParamSetValueAtTime { node_id, .. }
        | AudioCmd::AudioParamLinearRamp { node_id, .. }
        | AudioCmd::AudioParamExponentialRamp { node_id, .. }
        | AudioCmd::AudioParamSetTarget { node_id, .. }
        | AudioCmd::AudioParamCancelScheduled { node_id, .. }
        | AudioCmd::SetOscillatorType { node_id, .. }
        | AudioCmd::SetBiquadFilterType { node_id, .. }
        | AudioCmd::SetWaveShaperOversample { node_id, .. }
        | AudioCmd::SetPanningModel { node_id, .. }
        | AudioCmd::SetDistanceModel { node_id, .. }
        | AudioCmd::SetPannerScalar { node_id, .. }
        | AudioCmd::SetAnalyserScalar { node_id, .. } => (!index
            .get_context(*node_id)
            .is_some_and(|id| contexts.contains_key(&id)))
        .then(missing_node),
        _ => None,
    }
}
