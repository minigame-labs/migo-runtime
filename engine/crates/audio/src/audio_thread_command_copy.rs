//! `copyToChannel` for the audio thread's command dispatch.
use super::*;

pub(super) fn copy(
    contexts: &mut impl ContextAccess,
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
