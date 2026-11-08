//! `getFrequencyResponse` for the audio thread's command dispatch.
use super::*;

type Data = (Vec<f32>, Vec<f32>);
use shared::audio_frequency_response::FrequencyResponse;

pub(super) fn query(
    contexts: &mut impl ContextAccess,
    index: &NodeContextIndex,
    node_id: AudioNodeId,
    frequencies: &[f32],
) -> EngineResult<FrequencyResponse> {
    let missing = || {
        EngineError::from_detail(
            ErrorCode::NotFound,
            format!("Filter node {} not found", node_id),
        )
    };
    let ctx = index
        .get_context(node_id)
        .and_then(|id| contexts.get_mut(&id))
        .ok_or_else(missing)?;
    if !ctx.node_is::<BiquadFilterNode>(node_id) && !ctx.node_is::<IIRFilterNode>(node_id) {
        return Err(missing());
    }
    let limit = migo_services::audio::MAX_FREQUENCY_RESPONSE_POINTS;
    if frequencies.len() > limit {
        return Err(EngineError::from_detail(
            ErrorCode::InputSaturated,
            format!(
                "frequency response has {} points; limit is {}",
                frequencies.len(),
                limit
            ),
        ));
    }
    let data = compute(ctx, node_id, frequencies).expect("checked filter node disappeared");
    Ok(FrequencyResponse::new(data))
}

fn compute(ctx: &mut AudioContext, node_id: AudioNodeId, frequencies: &[f32]) -> Option<Data> {
    let sr = ctx.sample_rate() as f64;
    // Try BiquadFilterNode first, preserving its coefficient update.
    if let Some(r) = ctx.with_node_typed::<BiquadFilterNode, _>(node_id, |n| {
        n.get_frequency_response(sr, frequencies)
    }) {
        return Some(r);
    }
    ctx.with_node_typed::<IIRFilterNode, _>(node_id, |n| n.get_frequency_response(sr, frequencies))
}
