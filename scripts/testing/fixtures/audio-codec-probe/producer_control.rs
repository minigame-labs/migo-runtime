//! Indexed seek policy for the common, persistent source/CPU/PCM driver.
//! Targets are track-presentation frames. Clocked sources validate their own
//! output mapping at acceptance; the public playback cutover remains open.
use crate::{
    indexed_seek::{self, SeekContext, SeekSource, SeekStatus},
    off_worker::OffWorker,
    push_decoder::M4aSeekDecoder,
    streaming::{PcmChunk, StreamingState},
    streaming_transport::{ControlContext, ControlResult, Decoder, SourceControl, while_active},
};
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::sync::Arc;
use symphonia::core::{errors::SeekErrorKind, units::Timestamp};
use tokio::{
    sync::{mpsc, oneshot},
    time::Instant,
};

pub(crate) use crate::stream_control::{Command, Prepared, Reply};
pub(crate) struct Controls {
    requests: mpsc::Receiver<Command>,
}
pub(crate) fn channel() -> (mpsc::Sender<Command>, Controls) {
    let (tx, requests) = crate::stream_control::channel();
    (tx, Controls { requests })
}
impl Controls {
    pub(crate) fn for_source(state: &Arc<StreamingState>) -> EngineResult<Self> {
        let (sender, requests) = crate::stream_control::channel();
        state.install_controls(sender)?;
        Ok(Self { requests })
    }
}

fn invalid(cause: impl Into<String>) -> EngineError {
    EngineError::from_detail(ErrorCode::IoError, cause.into())
}
pub(crate) fn request(
    tx: &mpsc::Sender<Command>,
    target: Timestamp,
    deadline: Instant,
) -> EngineResult<oneshot::Receiver<Reply>> {
    crate::stream_control::request(tx, target.get(), deadline)
}
impl<D: Decoder + M4aSeekDecoder, S: SeekSource<D>> SourceControl<D, S> for Controls {
    type Request = Command;
    fn enabled(&self) -> bool {
        true
    }
    fn recover_read(&mut self, source: &mut S) -> EngineResult<()> {
        source.resume_after_read_interruption()
    }
    async fn next(&mut self) -> Option<Command> {
        self.requests.recv().await
    }
    fn apply<'a>(
        &'a mut self,
        command: Command,
        source: &'a mut S,
        decoder: OffWorker<Option<D>>,
        pcm: PcmChunk,
        context: ControlContext<'a>,
    ) -> impl std::future::Future<Output = EngineResult<Option<ControlResult<D>>>>
    + Send
    + 'a
    + use<'a, D, S> {
        use futures::{FutureExt, future::Either};
        let Command {
            target,
            cutover,
            deadline,
            reply,
        } = command;
        if reply.is_closed() {
            return Either::Left(std::future::ready(Ok(Some(ControlResult::Unchanged(
                decoder,
            )))));
        }
        let expired = Instant::now() >= deadline;
        if expired || !context.initialized {
            let cause = if expired {
                ErrorCode::Timeout
            } else {
                ErrorCode::InvalidArgument
            };
            let _ = reply.send(Reply::NotStarted(cause));
            return Either::Left(std::future::ready(Ok(Some(ControlResult::Unchanged(
                decoder,
            )))));
        }
        let revision = match context.revision.checked_next() {
            Ok(revision) => revision,
            Err(error) => return Either::Left(std::future::ready(Err(error))),
        };
        let traversal = match cutover
            .map(|request| {
                let clock = context
                    .state
                    .timeline()
                    .ok_or_else(|| invalid("loop request has no source clock"))?;
                if request
                    .seek_target
                    .is_some_and(|frame| i64::try_from(frame).ok() != Some(target))
                {
                    return Err(invalid("cyclic seek target does not match its command"));
                }
                request.traversal(clock)
            })
            .transpose()
        {
            Ok(value) => value,
            Err(_) => {
                let _ = reply.send(Reply::Rejected(ErrorCode::InvalidArgument));
                return Either::Left(std::future::ready(Ok(Some(ControlResult::Unchanged(
                    decoder,
                )))));
            }
        };
        // Construct before capture, as in indexed_seek itself: the async
        // continuation must not retain another already-moved full decoder.
        let operation = indexed_seek::reposition(
            source,
            decoder,
            move |slot| match cutover {
                Some(request) => {
                    let decoder =
                        slot.as_mut()
                            .ok_or(symphonia::core::errors::Error::SeekError(
                                SeekErrorKind::Unseekable,
                            ))?;
                    if let Some(frame) = request.seek_target {
                        decoder.seek_loop(Timestamp::new(frame as i64), request.enabled)
                    } else {
                        decoder.cutover(request.output_start, request.enabled)
                    }
                }
                None => slot.seek(Timestamp::new(target)),
            },
            pcm,
            SeekContext {
                tx: context.tx,
                state: context.state,
                deadline,
            },
        );
        // Then stores the first operation and the acceptance continuation in
        // mutually exclusive phases. Capturing and re-pinning the whole seek
        // future inside async move would retain two full copies.
        Either::Right(operation.then(move |result| async move {
            let Some(result) = result? else {
                return Ok(None);
            };
            match result.status {
                SeekStatus::Rejected(kind) => {
                    let _ = reply.send(Reply::Rejected(match kind {
                        SeekErrorKind::OutOfRange => ErrorCode::InvalidArgument,
                        _ => ErrorCode::IoError,
                    }));
                    Ok(Some(ControlResult::Unchanged(result.decoder)))
                }
                SeekStatus::Applied {
                    seeked,
                    step,
                    pcm,
                    restart,
                } => {
                    let (accept, receipt) = oneshot::channel();
                    let prepared = Prepared {
                        revision,
                        target: if traversal.is_some() {
                            target
                        } else {
                            seeked.required_ts.get()
                        },
                        traversal,
                        source: context.state.clone(),
                        deadline,
                        accept,
                    };
                    if reply.send(Reply::Prepared(prepared)).is_err() {
                        context.state.cancel();
                        return Ok(None);
                    }
                    let accepted = while_active(
                        context.tx,
                        context.state,
                        tokio::time::timeout_at(deadline, receipt),
                    )
                    .await;
                    if !matches!(accepted, Some(Ok(Ok(())))) || Instant::now() >= deadline {
                        context.state.cancel();
                        return Ok(None);
                    }
                    let output_start = if let Some(traversal) = traversal {
                        Some(traversal.output_start())
                    } else {
                        context
                            .state
                            .timeline()
                            .map(|timeline| {
                                let target = u64::try_from(seeked.required_ts.get())
                                    .map_err(|_| invalid("negative accepted clock position"))?;
                                let position = timeline.position(target)?;
                                Ok::<_, EngineError>(
                                    position.frame + u64::from(position.phase != 0),
                                )
                            })
                            .transpose()?
                    };
                    Ok(Some(ControlResult::Applied {
                        decoder: result.decoder,
                        revision,
                        pcm,
                        step,
                        restart,
                        output_start,
                        traversal,
                    }))
                }
            }
        }))
    }
}

#[cfg(test)]
#[path = "producer_contracts.rs"]
mod tests;

#[cfg(test)]
mod clock_tests {
    use super::*;
    use crate::inner_audio::InnerAudioPlayer;
    use crate::streaming::{PcmPool, StreamMsg, StreamRevision, StreamTimeline, stream_channel};
    fn setup() -> (
        InnerAudioPlayer,
        Arc<StreamingState>,
        mpsc::Sender<StreamMsg>,
    ) {
        let state = StreamingState::new_reusable();
        state
            .bind_timeline(StreamTimeline::new(44100, 44100, 48000, 1).unwrap())
            .unwrap();
        let (tx, rx) = stream_channel();
        let mut player = InnerAudioPlayer::new(904, 1);
        player
            .start_streaming("receipt clock".into(), rx, state.clone())
            .unwrap();
        assert!(
            tx.try_send(StreamMsg::Ready {
                revision: StreamRevision::INITIAL,
                sample_rate: 48000,
                channels: 1
            })
            .is_ok()
        );
        let mut pool = PcmPool::new();
        let mut pcm = pool.take();
        pcm.buffer_mut().resize(24000, 0.25);
        assert!(
            tx.try_send(StreamMsg::Samples {
                revision: StreamRevision::INITIAL,
                pcm
            })
            .is_ok()
        );
        player.poll_stream();
        assert!(player.shared.is_loaded());
        player.drain_events().for_each(drop);
        (player, state, tx)
    }
    fn receipt(source: Arc<StreamingState>) -> (Prepared, oneshot::Receiver<()>) {
        let (accept, waiter) = oneshot::channel();
        (
            Prepared {
                traversal: None,
                revision: StreamRevision::INITIAL.checked_next().unwrap(),
                target: 1,
                source,
                deadline: Instant::now() + std::time::Duration::from_secs(3),
                accept,
            },
            waiter,
        )
    }
    #[test]
    fn clocked_receipt_uses_the_ceil_boundary_instead_of_the_input_frame() {
        let (mut player, state, _tx) = setup();
        let (value, mut waiter) = receipt(state);
        value.accept_clocked(&mut player).unwrap();
        assert_eq!(player.shared.position_frames(), 2);
        assert_eq!(waiter.try_recv(), Ok(()));
    }
    #[test]
    fn mismatched_external_mapping_cannot_mutate_player_or_acknowledge() {
        let (mut player, state, _tx) = setup();
        let before = (
            player.shared.position_frames(),
            player.shared.duration(),
            player.physical_pcm_bytes(),
        );
        let (value, mut waiter) = receipt(state.clone());
        assert_eq!(
            value.accept(&mut player, 1).unwrap_err().code,
            ErrorCode::InvalidArgument
        );
        assert_eq!(
            (
                player.shared.position_frames(),
                player.shared.duration(),
                player.physical_pcm_bytes()
            ),
            before
        );
        assert!(player.drain_events().next().is_none());
        assert!(state.is_cancelled());
        assert!(matches!(
            waiter.try_recv(),
            Err(oneshot::error::TryRecvError::Closed)
        ));
    }
}
