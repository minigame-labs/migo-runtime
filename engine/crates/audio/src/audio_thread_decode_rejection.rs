//! Decoder admission used to drop refused bytes before the command returned.
//! Return that owner to the original delivery and its bounded CPU cleanup.
use super::*;

pub(super) fn forward(
    delivery: AudioCommandDelivery,
    contexts: &impl ContextView,
    players: &mut impl PlayerAccess,
    pool: &mut LazyDecodePool,
    inbox: &mut crate::command_inbox::Inbox,
) -> Option<AudioCommandDelivery> {
    if !delivery.has_queue_permit()
        || delivery.command().queued_payload_bytes() < 4096
        || !matches!(
            delivery.command(),
            AudioCmd::DecodeAudioData { .. }
                | AudioCmd::InnerAudioLoad { .. }
                | AudioCmd::InnerAudioCompleteLoad { .. }
        )
    {
        return Some(delivery);
    }
    inbox.submit_decode(delivery, |command| submit(command, contexts, players, pool));
    None
}
enum Origin {
    Buffer,
    Inner,
    Prepared,
}
fn submit(
    command: AudioCmd,
    contexts: &impl ContextView,
    players: &mut impl PlayerAccess,
    pool: &mut LazyDecodePool,
) -> Option<(AudioCmd, EngineError)> {
    let (job, origin) = match command {
        AudioCmd::DecodeAudioData { ctx_id, data, resp } => {
            let Some(context) = contexts.get(&ctx_id) else {
                return Some((
                    AudioCmd::DecodeAudioData { ctx_id, data, resp },
                    EngineError::from_detail(
                        ErrorCode::NotFound,
                        format!("AudioContext {} not found", ctx_id),
                    ),
                ));
            };
            (
                DecodeJob::AudioBuffer {
                    ctx_id,
                    target_rate: context.sample_rate(),
                    data,
                    resp,
                },
                Origin::Buffer,
            )
        }
        AudioCmd::InnerAudioLoad { id, data, resp } => {
            let Some(player) = players.get_mut(&id) else {
                return Some((
                    AudioCmd::InnerAudioLoad { id, data, resp },
                    EngineError::from_detail(
                        ErrorCode::NotFound,
                        format!("InnerAudioContext {} not found", id),
                    ),
                ));
            };
            // begin_load must still retire the old generation before even a
            // saturated decoder admission, just as the original actor did.
            let load = match player.begin_load() {
                Ok(load) => load,
                Err(error) => return Some((AudioCmd::InnerAudioLoad { id, data, resp }, error)),
            };
            (
                DecodeJob::InnerAudio {
                    id,
                    load,
                    target_rate: pool.sample_rate,
                    data,
                    resp,
                },
                Origin::Inner,
            )
        }
        AudioCmd::InnerAudioCompleteLoad {
            id,
            load,
            data,
            resp,
        } => {
            if !players
                .get(&id)
                .is_some_and(|player| player.shared.owns_load(&load))
            {
                return Some((
                    AudioCmd::InnerAudioCompleteLoad {
                        id,
                        load,
                        data,
                        resp,
                    },
                    crate::inner_audio::InnerAudioLoadToken::cancelled_error(),
                ));
            }
            (
                DecodeJob::InnerAudio {
                    id,
                    load,
                    target_rate: pool.sample_rate,
                    data,
                    resp,
                },
                Origin::Prepared,
            )
        }
        _ => unreachable!("decode dispatcher selected an unrelated command"),
    };
    pool.try_submit(job)
        .map(|(job, code, detail)| (restore(job, origin), EngineError::from_detail(code, detail)))
}
fn restore(job: DecodeJob, origin: Origin) -> AudioCmd {
    match (origin, job) {
        (
            Origin::Buffer,
            DecodeJob::AudioBuffer {
                ctx_id, data, resp, ..
            },
        ) => AudioCmd::DecodeAudioData { ctx_id, data, resp },
        (
            Origin::Inner,
            DecodeJob::InnerAudio {
                id,
                load,
                data,
                resp,
                ..
            },
        ) => {
            drop(load);
            AudioCmd::InnerAudioLoad { id, data, resp }
        }
        (
            Origin::Prepared,
            DecodeJob::InnerAudio {
                id,
                load,
                data,
                resp,
                ..
            },
        ) => AudioCmd::InnerAudioCompleteLoad {
            id,
            load,
            data,
            resp,
        },
        _ => unreachable!("decoder refusal changed its origin"),
    }
}
#[cfg(test)]
#[path = "audio_thread_decode_rejection_tests.rs"]
pub(super) mod tests;
