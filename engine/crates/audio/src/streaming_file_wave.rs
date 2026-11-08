//! Sequential replay reads reuse the sealed capability, CPU and control deadline.
use super::*;
use tokio::time::Instant;
impl FileSource {
    pub(super) async fn resolve_request(
        &mut self,
        request: FileRequest,
        decoder: OffWorker<Option<FileDecoder>>,
        tx: &mpsc::Sender<StreamMsg>,
        state: &Arc<StreamingState>,
        deadline: Instant,
    ) -> EngineResult<Option<SourceResume<FileDecoder>>> {
        let offset = match request {
            FileRequest::M4a(request) => {
                return self
                    .resolve_before(request, decoder, tx, state, deadline)
                    .await;
            }
            FileRequest::Wave(offset) | FileRequest::Mp3(offset) | FileRequest::Vorbis(offset) => {
                offset
            }
        };
        self.index.charge(state)?;
        let count = self
            .file
            .length()
            .checked_sub(offset)
            .filter(|n| *n > 0)
            .ok_or_else(|| invalid("audio source request outside sealed extent"))?
            .min(migo_io::read_at_file::FILE_CHUNK_BYTES as u64) as usize;
        let first = self.loan(offset, count, false, state, deadline).await?;
        let file = self.file.clone();
        let active = state.clone();
        let operation = decoder.with(move |slot| {
            let result = (|| {
                index::check(file.as_ref(), &active, deadline)?;
                match (request, slot) {
                    (FileRequest::Vorbis(_), Some(FileDecoder::Vorbis(decoder, _))) => {
                        if decoder.vorbis_request() != Some(offset) {
                            return Err(invalid("Vorbis source request changed"));
                        }
                        decoder.resume_vorbis(offset)
                    }
                    (FileRequest::Wave(_), Some(FileDecoder::Wave(decoder, _))) => {
                        if decoder.wave_request() != Some(offset) {
                            return Err(invalid("WAVE source request changed"));
                        }
                        decoder.resume_wave(offset)
                    }
                    (FileRequest::Mp3(_), Some(FileDecoder::Sequential(decoder, _))) => {
                        if decoder.mp3_request() != Some(offset) {
                            return Err(invalid("MP3 replay source request changed"));
                        }
                        decoder.resume_mp3(offset)
                    }
                    _ => Err(invalid("sealed replay decoder missing")),
                }
            })();
            (result, first)
        });
        let Some(result) =
            transport::while_active(tx, state, tokio::time::timeout_at(deadline, operation)).await
        else {
            return Ok(None);
        };
        let (decoder, (result, first)) =
            result.map_err(|_| EngineError::new(ErrorCode::Timeout))??;
        result?;
        self.position = offset + count as u64;
        self.pending = Some(first);
        Ok(Some(SourceResume {
            decoder,
            restart: true,
        }))
    }
}
