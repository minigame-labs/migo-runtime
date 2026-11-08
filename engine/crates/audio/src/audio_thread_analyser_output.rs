//! Analyser getters for the audio thread's command dispatch.
use super::*;
use shared::audio_analyser_samples::AnalyserSamples;

pub(super) fn bytes(
    contexts: &mut impl ContextAccess,
    index: &NodeContextIndex,
    node_id: AudioNodeId,
    frequency: bool,
) -> EngineResult<AnalyserSamples<u8>> {
    read(contexts, index, node_id, |node| {
        if frequency {
            node.get_byte_frequency_data()
        } else {
            node.get_byte_time_domain_data()
        }
    })
}
pub(super) fn floats(
    contexts: &mut impl ContextAccess,
    index: &NodeContextIndex,
    node_id: AudioNodeId,
    frequency: bool,
) -> EngineResult<AnalyserSamples<f32>> {
    read(contexts, index, node_id, |node| {
        if frequency {
            node.get_float_frequency_data()
        } else {
            node.get_float_time_domain_data()
        }
    })
}
fn read<T: Send + 'static>(
    contexts: &mut impl ContextAccess,
    index: &NodeContextIndex,
    node_id: AudioNodeId,
    getter: impl FnOnce(&mut AnalyserNode) -> Vec<T>,
) -> EngineResult<AnalyserSamples<T>> {
    let missing = || {
        EngineError::from_detail(
            ErrorCode::NotFound,
            format!("AnalyserNode {} not found", node_id),
        )
    };
    let ctx = index
        .get_context(node_id)
        .and_then(|id| contexts.get_mut(&id))
        .ok_or_else(missing)?;
    let data = ctx
        .with_node_typed::<AnalyserNode, _>(node_id, getter)
        .ok_or_else(missing)?;
    Ok(data.into())
}
