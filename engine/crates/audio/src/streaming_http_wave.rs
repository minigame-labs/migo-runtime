//! WAVE/MP3/Vorbis replay reuses the pinned Reader and common media transition.
use super::*;
impl IndexedHttp {
    pub(super) async fn resolve_request(
        &mut self,
        request: HttpRequest,
        decoder: OffWorker<Option<AutoDecoder>>,
        state: &Arc<StreamingState>,
        deadline: Instant,
    ) -> EngineResult<SourceResume<AutoDecoder>> {
        let offset = match request {
            HttpRequest::M4a(request) => {
                return self.resolve(request, decoder, state, deadline).await;
            }
            HttpRequest::Wave(offset) | HttpRequest::Mp3(offset) | HttpRequest::Vorbis(offset) => {
                offset
            }
        };
        check(state, deadline)?;
        self.index.charge(state).map_err(error)?;
        let length = self
            .control()?
            .length()
            .ok_or_else(|| error("HTTP replay source length missing"))?;
        if offset >= length {
            return Err(error("HTTP replay source request outside pinned extent"));
        }
        self.restart_at(offset, decoder, state, deadline, move |decoder| {
            if decoder.http_request() != Some(request) {
                return Err(error("HTTP replay source request changed"));
            }
            match request {
                HttpRequest::Wave(_) => decoder.resume_wave(offset),
                HttpRequest::Mp3(_) => decoder.resume_mp3(offset),
                HttpRequest::Vorbis(_) => decoder.resume_vorbis(offset),
                HttpRequest::M4a(_) => unreachable!(),
            }
        })
        .await
    }
}
