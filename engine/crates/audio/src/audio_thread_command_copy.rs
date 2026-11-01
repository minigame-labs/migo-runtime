//! A completed borrowed copy still owned a large input on the audio caller.
//! Keep its original delivery until the copy answers and CPU drops that input.
use super::*;

pub(super) fn forward(
    delivery: AudioCommandDelivery,
    contexts: &mut HashMap<AudioContextId, AudioContext>,
    inbox: &mut crate::command_inbox::Inbox,
) -> Option<AudioCommandDelivery> {
    let AudioCmd::CopyToChannel {
        ctx_id,
        buffer_id,
        data,
        channel,
        start,
        ..
    } = delivery.command()
    else {
        return Some(delivery);
    };
    if !delivery.has_queue_permit()
        || data.capacity().saturating_mul(std::mem::size_of::<f32>()) < 4096
    {
        return Some(delivery);
    }
    let result = copy(contexts, *ctx_id, *buffer_id, data, *channel, *start);
    inbox.finish_copy(delivery, result);
    None
}

pub(super) fn copy(
    contexts: &mut HashMap<AudioContextId, AudioContext>,
    ctx_id: AudioContextId,
    buffer_id: shared::protocol::audio_cmd::AudioBufferId,
    data: &[f32],
    channel: u32,
    start: u32,
) -> EngineResult<()> {
    let Some(ctx) = contexts.get_mut(&ctx_id) else {
        return Err(EngineError::from_detail(
            ErrorCode::NotFound,
            format!("AudioContext {} not found", ctx_id),
        ));
    };
    match ctx.copy_to_channel(buffer_id, data, channel, start) {
        Ok(true) => Ok(()),
        Ok(false) => Err(EngineError::from_detail(
            ErrorCode::NotFound,
            format!("Buffer {} channel {} not found", buffer_id, channel),
        )),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
#[path = "audio_thread_command_copy_tests.rs"]
pub(super) mod tests;
