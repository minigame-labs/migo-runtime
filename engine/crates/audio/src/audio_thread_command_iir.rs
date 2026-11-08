//! IIR normalization owns new arrays, but used to free large command inputs
//! on the audio caller. Borrow them and retain the original transport ticket.
use super::*;

pub(super) fn forward(
    delivery: AudioCommandDelivery,
    contexts: &mut impl ContextAccess,
    index: &mut NodeContextIndex,
    inbox: &mut crate::command_inbox::Inbox,
) -> Option<AudioCommandDelivery> {
    let AudioCmd::CreateIIRFilter {
        ctx_id,
        node_id,
        feedforward,
        feedback,
    } = delivery.command()
    else {
        return Some(delivery);
    };
    if !delivery.has_queue_permit() || delivery.command().queued_payload_bytes() < 4096 {
        return Some(delivery);
    }
    if let Some(ctx) = contexts.get_mut(ctx_id) {
        index.create(*node_id, *ctx_id, || {
            ctx.create_iir_filter(*node_id, feedforward, feedback)
        });
    }
    inbox.finish_parameters(delivery);
    None
}
