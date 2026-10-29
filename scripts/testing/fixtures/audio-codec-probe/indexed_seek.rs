//! One bounded off-worker seek step, then the existing pinned source recovery.
//! The caller owns publication and the old input suffix; restart invalidates
//! that suffix. Rejected targets retain the exact decoder/source owner.
use crate::{
    off_worker::OffWorker,
    push_decoder::{M4aSeekDecoder, M4aSourceRequest},
    streaming::{PcmChunk, StreamMsg, StreamingState},
    streaming_transport::{Decoder, Progress, Source, SourceResume, Step, while_active},
};
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::{future::Future, sync::Arc};
use symphonia::core::{
    errors::{Error, SeekErrorKind},
    formats::SeekedTo,
    units::Timestamp,
};
use tokio::{sync::mpsc, time::Instant};

#[derive(Clone, Copy)]
pub(crate) struct SeekContext<'tx, 'state> {
    pub tx: &'tx mpsc::Sender<StreamMsg>,
    pub state: &'state Arc<StreamingState>,
    pub deadline: Instant,
}

pub(crate) trait SeekSource<D: Decoder>: Source<D, Request = M4aSourceRequest> {
    fn resume_before(
        &mut self,
        request: M4aSourceRequest,
        decoder: OffWorker<Option<D>>,
        context: SeekContext<'_, '_>,
    ) -> impl Future<Output = EngineResult<Option<SourceResume<D>>>> + Send;
}

pub(crate) struct SeekResult<D> {
    pub decoder: OffWorker<Option<D>>,
    pub status: SeekStatus,
}
pub(crate) enum SeekStatus {
    Rejected(SeekErrorKind),
    Applied {
        seeked: SeekedTo,
        step: Step,
        pcm: PcmChunk,
        restart: bool,
    },
}
enum CpuResult {
    Rejected(SeekErrorKind),
    Applied(SeekedTo, Step, Option<M4aSourceRequest>),
}
fn invalid(cause: impl std::fmt::Display) -> EngineError {
    EngineError::from_detail(ErrorCode::IoError, cause.to_string())
}
fn timeout() -> EngineError {
    EngineError::from_detail(ErrorCode::Timeout, "indexed seek deadline exceeded")
}

/// No Ready/Done/Seeked publication occurs here. One CPU ownership transfer
/// resets/seeks and tries the retained cache, with no old body bytes offered.
/// A true miss reuses the source's existing verified ReadAt resume path.
/// Targets use the track's presentation frames. A cached result preserves the
/// caller's input suffix and EOF flag; this step does not mark seek completion.
pub(crate) fn seek<'a, D: Decoder + M4aSeekDecoder, S: SeekSource<D>>(
    source: &'a mut S,
    decoder: OffWorker<Option<D>>,
    target: Timestamp,
    mut pcm: PcmChunk,
    context: SeekContext<'a, 'a>,
) -> impl Future<Output = EngineResult<Option<SeekResult<D>>>> + Send + 'a {
    let SeekContext {
        tx,
        state,
        deadline,
    } = context;
    let invalid_loan = !pcm.is_empty();
    let active = state.clone();
    let operation = decoder.with(move |slot| {
        if active.is_cancelled() {
            return Ok((pcm, None));
        }
        if Instant::now() >= deadline {
            return Err(timeout());
        }
        let seeked = match slot.seek(target) {
            Ok(seeked) => seeked,
            Err(Error::SeekError(kind)) => return Ok((pcm, Some(CpuResult::Rejected(kind)))),
            Err(error) => return Err(invalid(error)),
        };
        if Instant::now() >= deadline {
            return Err(timeout());
        }
        let value = slot
            .as_mut()
            .ok_or_else(|| invalid("seek decoder was not initialized"))?;
        let step = value.step(&[], false, pcm.buffer_mut(), &active.cancelled)?;
        if step.consumed != 0 {
            return Err(invalid("seek step consumed unoffered bytes"));
        }
        let request = if step.progress == Progress::NeedSource {
            let request = S::pending(value)?;
            if !matches!(request, M4aSourceRequest::ReadAt(_)) {
                return Err(invalid("seek tried to replace its retained index"));
            }
            Some(request)
        } else {
            None
        };
        Ok((pcm, Some(CpuResult::Applied(seeked, step, request))))
    });
    // Construct the CPU future before capture: the outer future does not
    // retain another full-sized, already-moved typed decoder parameter.
    async move {
        if state.is_cancelled() || tx.is_closed() {
            return Ok(None);
        }
        if Instant::now() >= deadline {
            return Err(timeout());
        }
        if invalid_loan {
            return Err(invalid("seek output loan must be empty"));
        }
        tokio::pin!(operation);
        let Some(result) =
            while_active(tx, state, tokio::time::timeout_at(deadline, &mut operation)).await
        else {
            return Ok(None);
        };
        let (decoder, outcome) = result.map_err(|_| timeout())??;
        let (pcm, result) = outcome?;
        let Some(result) = result else {
            return Ok(None);
        };
        if state.is_cancelled() || tx.is_closed() {
            return Ok(None);
        }
        if Instant::now() >= deadline {
            return Err(timeout());
        }
        match result {
            CpuResult::Rejected(kind) => Ok(Some(SeekResult {
                decoder,
                status: SeekStatus::Rejected(kind),
            })),
            CpuResult::Applied(seeked, mut step, request) => {
                if step.progress == Progress::Cancelled {
                    return Ok(None);
                }
                let (decoder, restart) = if let Some(request) = request {
                    let resume = source.resume_before(request, decoder, context);
                    // Pass a pinned borrow through the generic cancellation
                    // wrapper instead of embedding another entire IO future.
                    tokio::pin!(resume);
                    let Some(resumed) =
                        while_active(tx, state, tokio::time::timeout_at(deadline, &mut resume))
                            .await
                    else {
                        return Ok(None);
                    };
                    let Some(resumed) = resumed.map_err(|_| timeout())?? else {
                        return Ok(None);
                    };
                    if state.is_cancelled() || tx.is_closed() {
                        return Ok(None);
                    }
                    if Instant::now() >= deadline {
                        return Err(timeout());
                    }
                    if !resumed.restart {
                        return Err(invalid("seek ReadAt retained the previous body suffix"));
                    }
                    step.progress = Progress::Yield;
                    (resumed.decoder, true)
                } else {
                    (decoder, false)
                };
                Ok(Some(SeekResult {
                    decoder,
                    status: SeekStatus::Applied {
                        seeked,
                        step,
                        pcm,
                        restart,
                    },
                }))
            }
        }
    }
}
