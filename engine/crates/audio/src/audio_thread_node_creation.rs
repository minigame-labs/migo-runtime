//! All ordinary node factories enter through the actor index admission.
use super::*;

pub(super) fn apply(
    command: &AudioCmd,
    contexts: &mut impl ContextAccess,
    index: &mut NodeContextIndex,
    sample_rate: u32,
) -> bool {
    let (ctx_id, node_id) = match command {
        AudioCmd::CreateBufferSource { ctx_id, node_id }
        | AudioCmd::CreateGain { ctx_id, node_id }
        | AudioCmd::CreateOscillator { ctx_id, node_id }
        | AudioCmd::CreateDelay {
            ctx_id, node_id, ..
        }
        | AudioCmd::CreateBiquadFilter { ctx_id, node_id }
        | AudioCmd::CreateWaveShaper { ctx_id, node_id }
        | AudioCmd::CreateAnalyser { ctx_id, node_id }
        | AudioCmd::CreateDynamicsCompressor { ctx_id, node_id }
        | AudioCmd::CreatePanner { ctx_id, node_id }
        | AudioCmd::CreateChannelMerger {
            ctx_id, node_id, ..
        }
        | AudioCmd::CreateChannelSplitter {
            ctx_id, node_id, ..
        }
        | AudioCmd::CreateConstantSource { ctx_id, node_id }
        | AudioCmd::CreateIIRFilter {
            ctx_id, node_id, ..
        } => (*ctx_id, *node_id),
        _ => return false,
    };
    let Some(ctx) = contexts.get_mut(&ctx_id) else {
        match command {
            AudioCmd::CreateBufferSource { .. } => {
                tracing::warn!("CreateBufferSource: AudioContext {} not found", ctx_id);
            }
            AudioCmd::CreateGain { .. } => {
                tracing::warn!("CreateGain: AudioContext {} not found", ctx_id);
            }
            _ => {}
        }
        return false;
    };
    index.create(node_id, ctx_id, || match command {
        AudioCmd::CreateBufferSource { .. } => ctx.create_buffer_source(node_id),
        AudioCmd::CreateGain { .. } => ctx.create_gain(node_id),
        AudioCmd::CreateOscillator { .. } => {
            ctx.add_node(Box::new(OscillatorNode::new(node_id, sample_rate)))
        }
        AudioCmd::CreateDelay { max_delay_time, .. } => {
            let sr = ctx.sample_rate();
            let ch = ctx.channels();
            ctx.add_node(Box::new(DelayNode::new(node_id, *max_delay_time, sr, ch)))
        }
        AudioCmd::CreateBiquadFilter { .. } => ctx.create_biquad_filter(node_id),
        AudioCmd::CreateWaveShaper { .. } => ctx.add_node(Box::new(WaveShaperNode::new(node_id))),
        AudioCmd::CreateAnalyser { .. } => ctx.create_analyser(node_id),
        AudioCmd::CreateDynamicsCompressor { .. } => {
            let ch = ctx.channels();
            ctx.add_node(Box::new(DynamicsCompressorNode::new(node_id, ch)))
        }
        AudioCmd::CreatePanner { .. } => ctx.add_node(Box::new(PannerNode::new(node_id))),
        AudioCmd::CreateChannelMerger {
            number_of_inputs, ..
        } => ctx.add_node(Box::new(ChannelMergerNode::new(node_id, *number_of_inputs))),
        AudioCmd::CreateChannelSplitter {
            number_of_outputs, ..
        } => ctx.add_node(Box::new(ChannelSplitterNode::new(
            node_id,
            *number_of_outputs,
        ))),
        AudioCmd::CreateConstantSource { .. } => {
            ctx.add_node(Box::new(ConstantSourceNode::new(node_id)))
        }
        AudioCmd::CreateIIRFilter {
            feedforward,
            feedback,
            ..
        } => ctx.create_iir_filter(node_id, feedforward, feedback),
        _ => unreachable!(),
    })
}
