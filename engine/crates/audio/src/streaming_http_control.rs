//! Pinned HTTP target preparation over the same reader, budgets and CPU owner.
use super::*;
use crate::streaming_source_control::{ControlledSource, Controls, Repositioned};
use crate::streaming_transport::{self as transport, ControlContext, InterruptibleSource};
use symphonia_core::errors::{Error as CodecError, SeekErrorKind};
impl IndexedHttp {
    pub(crate) async fn run_selected(
        self,
        cpu: migo_io::audio_cpu::AudioCpu,
        length: Option<u64>,
        tx: &mpsc::Sender<StreamMsg>,
        state: &Arc<StreamingState>,
        rate: u32,
    ) -> EngineResult<()> {
        if state.accepts_controls() {
            let length = length
                .or_else(|| match &self.owner {
                    Owner::Control(reader) => reader.length(),
                    _ => None,
                })
                .ok_or_else(|| error("HTTP control source length missing"))?;
            // Retained codecs bind their authoritative clock before
            // advertising the source port. Loading intents already wait for it.
            let controls = Controls::deferred(None);
            // The common controlled driver publishes its own revision-bound error.
            let _ = transport::run_source_controlled(
                cpu,
                self,
                Some(length),
                tx.clone(),
                state.clone(),
                move |_| AutoDecoder::http_retained(rate, length),
                controls,
            )
            .await;
            Ok(())
        } else {
            transport::run_source(cpu, self, length, tx, state, move |_| {
                AutoDecoder::http(rate)
            })
            .await
        }
    }
    fn control_check(&self, state: &StreamingState, deadline: Instant) -> EngineResult<()> {
        check(state, deadline)?;
        self.owner_check()
    }
    fn owner_check(&self) -> EngineResult<()> {
        let retired = match &self.owner {
            Owner::Failed => true,
            Owner::Control(reader) => reader.is_failed(),
            Owner::Media(stream) => stream.reader().is_failed(),
            Owner::Initial(identity) => identity.is_err(),
        };
        if self.failed || retired {
            return Err(error("HTTP controlled source retired"));
        }
        Ok(())
    }
}
impl InterruptibleSource for IndexedHttp {
    fn resume_after_read_interruption(&mut self) -> EngineResult<()> {
        if self.interrupted {
            // Only a dropped pending media read is recoverable. Returned errors
            // clear interrupted but keep failed; control/CPU handoffs are never soft-interrupted.
            match &mut self.owner {
                Owner::Media(stream) => stream.resume_after_interruption().map_err(error)?,
                Owner::Control(reader) if self.initial.is_some() && !reader.is_failed() => {}
                _ => return Err(error("HTTP interrupted source unavailable")),
            }
            self.interrupted = false;
            self.failed = false;
        }
        self.owner_check()
    }
}
impl ControlledSource<AutoDecoder> for IndexedHttp {
    fn can_reposition(&self, state: &Arc<StreamingState>) -> bool {
        self.index.can_reposition(state)
    }
    fn check(&self, state: &StreamingState, deadline: Instant) -> EngineResult<()> {
        self.control_check(state, deadline)
    }
    async fn reposition(
        &mut self,
        target: i64,
        cutover: Option<crate::stream_control::LoopRequest>,
        decoder: OffWorker<Option<AutoDecoder>>,
        context: ControlContext<'_>,
        deadline: Instant,
    ) -> EngineResult<Option<Repositioned<AutoDecoder>>> {
        self.control_check(context.state, deadline)?;
        let active = context.state.clone();
        let operation = decoder.with(move |slot| {
            check(&active, deadline)?;
            let Some(slot) = slot else {
                return Ok(Err(ErrorCode::InvalidArgument));
            };
            let result = match slot.reposition(target, cutover)? {
                Ok(result) => Ok(result),
                Err(CodecError::SeekError(kind)) => {
                    return Ok(Err(match kind {
                        SeekErrorKind::OutOfRange => ErrorCode::InvalidArgument,
                        _ => ErrorCode::IoError,
                    }));
                }
                Err(cause) => return Err(error(cause)),
            };
            check(&active, deadline)?;
            Ok(result)
        });
        let Some(result) = transport::while_active(
            context.tx,
            context.state,
            tokio::time::timeout_at(deadline, operation),
        )
        .await
        else {
            return Ok(None);
        };
        let (mut decoder, result) = result.map_err(|_| EngineError::new(ErrorCode::Timeout))??;
        let (request, start, terminal) = match result? {
            Ok(value) => value,
            Err(code) => return Ok(Some(Repositioned::Rejected { decoder, code })),
        };
        let restart = request.is_some() || terminal;
        if let Some(request) = request {
            // Keep the original public-control deadline through metadata, media
            // validation and the first real body chunk; resolve never resets it.
            self.failed = true;
            let result = transport::while_active(
                context.tx,
                context.state,
                self.resolve_request(request, decoder, context.state, deadline),
            )
            .await;
            let Some(resumed) = result else {
                return Ok(None);
            };
            let resumed = resumed?;
            if !resumed.restart {
                return Err(error("HTTP seek retained old encoded suffix"));
            }
            self.failed = false;
            decoder = resumed.decoder;
        } else if terminal {
            self.control()?;
            self.initial = None;
            self.prefix.clear();
            self.pending = None;
            self.terminal = true;
        }
        self.control_check(context.state, deadline)?;
        Ok(Some(Repositioned::Prepared {
            decoder,
            start,
            restart,
        }))
    }
}
