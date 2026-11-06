//! One source ACK/revision handshake, shared by sealed File and pinned HTTP.
use crate::off_worker::OffWorker;
use crate::streaming::{self, StreamingState};
use crate::streaming_transport::{self as transport, Decoder, Source, Step};
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::{future::Future, sync::Arc};
use tokio::sync::mpsc;
pub(crate) type ErrorCell = Arc<std::sync::Mutex<Option<EngineError>>>;
pub(crate) enum Repositioned<D> {
    Rejected {
        decoder: OffWorker<Option<D>>,
        code: ErrorCode,
    },
    Prepared {
        decoder: OffWorker<Option<D>>,
        start: u64,
        restart: bool,
    },
}
pub(crate) trait ControlledSource<D: Decoder>: Source<D> + InterruptibleSource {
    fn can_reposition(&self, state: &Arc<StreamingState>) -> bool;
    fn check(&self, state: &StreamingState, deadline: Instant) -> EngineResult<()>;
    fn reposition(
        &mut self,
        target: i64,
        cutover: Option<crate::stream_control::LoopRequest>,
        decoder: OffWorker<Option<D>>,
        context: ControlContext<'_>,
        deadline: Instant,
    ) -> impl Future<Output = EngineResult<Option<Repositioned<D>>>> + Send;
}
fn invalid(message: &str) -> EngineError {
    EngineError::from_detail(ErrorCode::InvalidArgument, message)
}

use crate::stream_control::{Command, Prepared, Reply};
use crate::streaming_transport::{
    ControlContext, ControlResult, InterruptibleSource, SourceControl,
};
use tokio::{sync::oneshot, time::Instant};

pub(crate) struct Controls {
    requests: mpsc::Receiver<Command>,
    error: Option<ErrorCell>,
    deferred: Option<mpsc::Sender<Command>>,
}
impl Controls {
    pub(crate) fn deferred(error: Option<ErrorCell>) -> Self {
        let (sender, requests) = crate::stream_control::channel();
        Self {
            requests,
            error,
            deferred: Some(sender),
        }
    }
    pub(crate) fn for_source(
        state: &Arc<StreamingState>,
        error: Option<ErrorCell>,
    ) -> EngineResult<Self> {
        let mut control = Self::deferred(error);
        state.install_controls(control.deferred.take().unwrap())?;
        Ok(control)
    }
}
impl<D: Decoder, S: ControlledSource<D>> SourceControl<D, S> for Controls {
    type Request = Command;
    fn enabled(&self) -> bool {
        true
    }
    fn format_ready(&mut self, state: &Arc<StreamingState>, ready: bool) -> EngineResult<()> {
        if ready && let Some(sender) = self.deferred.take() {
            if state.timeline().is_some() {
                state.install_controls(sender)?;
            } else {
                self.requests.close();
            }
        }
        Ok(())
    }
    fn failed(&self, error: &EngineError) {
        if let Some(cell) = &self.error {
            *cell.lock().unwrap() = Some(error.clone());
        }
    }
    fn recover_read(&mut self, source: &mut S) -> EngineResult<()> {
        source.resume_after_read_interruption()
    }
    async fn next(&mut self) -> Option<Command> {
        self.requests.recv().await
    }
    async fn apply<'a>(
        &'a mut self,
        request: Command,
        source: &'a mut S,
        decoder: OffWorker<Option<D>>,
        pcm: streaming::PcmChunk,
        context: ControlContext<'a>,
    ) -> EngineResult<Option<ControlResult<D>>> {
        let Command {
            target,
            cutover,
            deadline,
            reply,
        } = request;
        if reply.is_closed() {
            return Ok(Some(ControlResult::Unchanged(decoder)));
        }
        if Instant::now() >= deadline || !context.initialized {
            let _ = reply.send(Reply::NotStarted(if Instant::now() >= deadline {
                ErrorCode::Timeout
            } else {
                ErrorCode::InvalidArgument
            }));
            return Ok(Some(ControlResult::Unchanged(decoder)));
        }
        let traversal = match cutover
            .map(|r| {
                if r.seek_target
                    .is_some_and(|n| i64::try_from(n).ok() != Some(target))
                {
                    return Err(invalid("source seek target changes its traversal"));
                }
                r.traversal(
                    context
                        .state
                        .timeline()
                        .ok_or_else(|| invalid("source has no source clock"))?,
                )
            })
            .transpose()
        {
            Ok(t) => t,
            Err(_) => {
                let _ = reply.send(Reply::Rejected(ErrorCode::InvalidArgument));
                return Ok(Some(ControlResult::Unchanged(decoder)));
            }
        };
        if !source.can_reposition(context.state) {
            let _ = reply.send(Reply::Rejected(ErrorCode::InputSaturated));
            return Ok(Some(ControlResult::Unchanged(decoder)));
        }
        let revision = context.revision.checked_next()?;
        let Some(prepared) = source
            .reposition(target, cutover, decoder, context, deadline)
            .await?
        else {
            return Ok(None);
        };
        let (decoder, start, restart) = match prepared {
            Repositioned::Rejected { decoder, code } => {
                let _ = reply.send(Reply::Rejected(code));
                return Ok(Some(ControlResult::Unchanged(decoder)));
            }
            Repositioned::Prepared {
                decoder,
                start,
                restart,
            } => (decoder, start, restart),
        };
        source.check(context.state, deadline)?;
        let (accept, receipt) = oneshot::channel();
        let prepared = Prepared {
            revision,
            target,
            traversal,
            source: context.state.clone(),
            deadline,
            accept,
        };
        if prepared.mapped_output()? != start {
            return Err(invalid("source seek changed its output clock"));
        }
        if reply.send(Reply::Prepared(prepared)).is_err() {
            context.state.cancel();
            return Ok(None);
        }
        let accepted = transport::while_active(
            context.tx,
            context.state,
            tokio::time::timeout_at(deadline, receipt),
        )
        .await;
        if !matches!(accepted, Some(Ok(Ok(())))) || Instant::now() >= deadline {
            context.state.cancel();
            return Ok(None);
        }
        source.check(context.state, deadline)?;
        let (_, channels) = context
            .state
            .timeline()
            .ok_or_else(|| invalid("source lost its clock"))?
            .format();
        let rate = context.state.timeline().unwrap().format().0;
        Ok(Some(ControlResult::Applied {
            decoder,
            revision,
            pcm,
            step: Step {
                consumed: 0,
                progress: transport::Progress::Yield,
                sample_rate: rate,
                channels,
            },
            restart,
            output_start: Some(start),
            traversal,
        }))
    }
}
