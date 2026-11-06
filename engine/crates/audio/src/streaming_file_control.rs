//! File-specific target preparation; the source handshake is shared with HTTP.
use super::*;
pub(super) use crate::streaming_source_control::Controls;
use crate::streaming_source_control::{ControlledSource, Repositioned};
use crate::streaming_transport::{ControlContext, InterruptibleSource};
use symphonia_core::errors::{Error as CodecError, SeekErrorKind};
use tokio::time::Instant;
impl InterruptibleSource for FileSource {
    fn resume_after_read_interruption(&mut self) -> EngineResult<()> {
        if self.file.is_closed() {
            return Err(io_error());
        }
        // read() changes the cursor only after a completed chunk; interrupting
        // its future leaves the same sealed capability and exact cursor live.
        Ok(())
    }
}
impl ControlledSource<FileDecoder> for FileSource {
    fn can_reposition(&self, state: &Arc<StreamingState>) -> bool {
        self.index.can_reposition(state)
    }
    fn check(&self, state: &StreamingState, deadline: Instant) -> EngineResult<()> {
        index::check(self.file.as_ref(), state, deadline)
    }
    async fn reposition(
        &mut self,
        target: i64,
        cutover: Option<crate::stream_control::LoopRequest>,
        decoder: OffWorker<Option<FileDecoder>>,
        context: ControlContext<'_>,
        deadline: Instant,
    ) -> EngineResult<Option<Repositioned<FileDecoder>>> {
        let file = self.file.clone();
        let active = context.state.clone();
        let operation = decoder.with(move |slot| {
            index::check(file.as_ref(), &active, deadline)?;
            let reposition = match slot {
                Some(FileDecoder::Vorbis(decoder, _)) => decoder
                    .reposition_vorbis(target, cutover)
                    .map(|(r, s, t)| (r.map(FileRequest::Vorbis), s, t)),
                Some(FileDecoder::M4a(decoder, _)) => decoder
                    .reposition(target, cutover)
                    .map(|(r, s, t)| (r.map(FileRequest::M4a), s, t)),
                Some(FileDecoder::Wave(decoder, _)) => decoder
                    .reposition_wave(target, cutover)
                    .map(|(r, s, t)| (r.map(FileRequest::Wave), s, t)),
                Some(FileDecoder::Sequential(decoder, _)) => decoder
                    .reposition_mp3(target, cutover)
                    .map(|(r, s, t)| (r.map(FileRequest::Mp3), s, t)),
                _ => return Ok(Err(ErrorCode::InvalidArgument)),
            };
            let result = match reposition {
                Ok(result) => Ok(result),
                Err(CodecError::SeekError(kind)) => {
                    return Ok(Err(match kind {
                        SeekErrorKind::OutOfRange => ErrorCode::InvalidArgument,
                        _ => ErrorCode::IoError,
                    }));
                }
                Err(error) => return Err(invalid(&error.to_string())),
            };
            index::check(file.as_ref(), &active, deadline)?;
            Ok(result)
        });
        tokio::pin!(operation);
        let Some(result) = transport::while_active(
            context.tx,
            context.state,
            tokio::time::timeout_at(deadline, &mut operation),
        )
        .await
        else {
            return Ok(None);
        };
        let (mut decoder, result) = result.map_err(|_| EngineError::new(ErrorCode::Timeout))??;
        let (request, start, terminal) = match result? {
            Ok(value) => value,
            Err(code) => {
                return Ok(Some(Repositioned::Rejected { decoder, code }));
            }
        };
        let restart = request.is_some() || terminal;
        if let Some(request) = request {
            let result = self
                .resolve_request(request, decoder, context.tx, context.state, deadline)
                .await?;
            let Some(resumed) = result else {
                return Ok(None);
            };
            if !resumed.restart {
                return Err(invalid("file seek retained its old encoded suffix"));
            }
            decoder = resumed.decoder;
        } else if terminal {
            self.position = self.file.length();
            self.pending = None;
        }
        Ok(Some(Repositioned::Prepared {
            decoder,
            start,
            restart,
        }))
    }
}
