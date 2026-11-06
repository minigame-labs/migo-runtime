//! A valid destination can still refuse its old-value retirement quota.
//! Take incoming curve/snapshot storage only after that admission succeeds.
use super::*;

pub(super) enum Forward {
    Next(AudioCommandDelivery),
    Handled(Option<(AudioContextId, AudioNodeId)>),
}
pub(super) fn forward(
    delivery: AudioCommandDelivery,
    contexts: &mut impl ContextAccess,
    index: &NodeContextIndex,
    inbox: &mut crate::command_inbox::Inbox,
) -> Forward {
    // Snapshots retain their separate resource-domain charge; the transport
    // charges only their item ticket. Select bulk cleanup by actual samples.
    let bytes = match delivery.command() {
        AudioCmd::SetStartedBuffer { buffer, .. } | AudioCmd::StartBuffer { buffer, .. } => buffer
            .as_ref()
            .map_or(0, |b| std::mem::size_of_val(b.samples())),
        command => command.queued_payload_bytes(),
    };
    if !delivery.has_queue_permit()
        || bytes < 4096
        || !matches!(
            delivery.command(),
            AudioCmd::SetWaveShaperCurve { .. }
                | AudioCmd::SetStartedBuffer { .. }
                | AudioCmd::StartBuffer { .. }
        )
    {
        return Forward::Next(delivery);
    }
    let started = inbox.apply_replacement(delivery, |command| apply(command, contexts, index));
    Forward::Handled(started)
}
fn apply(
    command: &mut AudioCmd,
    contexts: &mut impl ContextAccess,
    index: &NodeContextIndex,
) -> Option<(AudioContextId, AudioNodeId)> {
    match command {
        AudioCmd::SetWaveShaperCurve { node_id, curve } => {
            if let Some(ctx) = index
                .get_context(*node_id)
                .and_then(|id| contexts.get_mut(&id))
            {
                ctx.try_set_wave_shaper_curve(*node_id, curve);
            }
        }
        AudioCmd::SetStartedBuffer {
            ctx_id,
            node_id,
            buffer,
        } => {
            let accepted = index.get_context(*node_id) == Some(*ctx_id)
                && contexts
                    .get_mut(ctx_id)
                    .is_some_and(|ctx| ctx.try_set_started_buffer(*node_id, buffer));
            if !accepted {
                warn!("SetStartedBuffer rejected: context {ctx_id} does not own node {node_id}");
            }
        }
        AudioCmd::StartBuffer {
            ctx_id,
            node_id,
            buffer,
            when,
            offset,
            duration,
        } => {
            let started = index.get_context(*node_id) == Some(*ctx_id)
                && contexts.get_mut(ctx_id).is_some_and(|ctx| {
                    ctx.try_start_buffer(*node_id, buffer, *when, *offset, *duration)
                });
            if !started {
                warn!("StartBuffer rejected: context {ctx_id} does not own node {node_id}");
            }
            return started.then_some((*ctx_id, *node_id));
        }
        _ => unreachable!("replacement dispatcher selected an unrelated command"),
    }
    None
}
#[cfg(test)]
#[path = "audio_thread_replacement_input_tests.rs"]
pub(super) mod tests;
