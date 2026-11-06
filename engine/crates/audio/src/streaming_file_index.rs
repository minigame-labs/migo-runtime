//! Fixed-size library framing and bounded index IO over the original sealed file.
//! Full moov parsing and decoder resume run on the original AudioCpu.
use super::*;
use crate::decoder::aac::m4a::SourceRequest;
use shared::audio_file::AudioFileChunk;
use symphonia_format_isomp4::streaming::{
    AtomKind, AtomSpan, AudioIndex, IndexOptions, IndexedPacket,
};
use tokio::time::Instant;
const MAX_ATOMS: usize = 32;
const MAX_SKIPPED: u64 = 4 * 1024 * 1024;
const RESUME_BURST: u64 = 64;
pub(crate) struct Index {
    media: [Option<AtomSpan>; MAX_ATOMS],
    owner: Option<std::sync::Weak<StreamingState>>,
    observed: u64,
    remaining: u64,
    failed: bool,
}
impl Index {
    pub(crate) fn new() -> Self {
        Self {
            media: [None; MAX_ATOMS],
            owner: None,
            observed: 0,
            remaining: RESUME_BURST,
            failed: false,
        }
    }
    pub(crate) fn remember(&mut self, media: AtomSpan) -> EngineResult<()> {
        if self.media.contains(&Some(media)) {
            return Ok(());
        }
        *self
            .media
            .iter_mut()
            .find(|m| m.is_none())
            .ok_or_else(|| invalid("local M4A media extent budget exceeded"))? = Some(media);
        Ok(())
    }
    pub(crate) fn charge(&mut self, state: &Arc<StreamingState>) -> EngineResult<()> {
        if self.failed {
            return Err(invalid("local M4A reposition budget retired"));
        }
        self.failed = true;
        if state.is_cancelled() {
            return Err(invalid("cancelled local M4A reposition"));
        }
        let rendered = state.rendered_frames();
        if let Some(owner) = &self.owner {
            if owner.as_ptr() != Arc::as_ptr(state) {
                return Err(invalid("local M4A reposition owner changed"));
            }
            self.remaining = self
                .remaining
                .saturating_add(rendered.saturating_sub(self.observed))
                .min(RESUME_BURST);
        } else {
            self.owner = Some(Arc::downgrade(state));
        }
        self.observed = rendered;
        self.remaining = self
            .remaining
            .checked_sub(1)
            .ok_or_else(|| invalid("local M4A reposition budget exceeded"))?;
        self.failed = false;
        Ok(())
    }
    pub(crate) fn media_for(&self, packet: IndexedPacket) -> Option<AtomSpan> {
        let end = packet.offset.checked_add(u64::from(packet.size))?;
        self.media
            .iter()
            .flatten()
            .find(|s| s.body_offset <= packet.offset && s.end.is_some_and(|limit| end <= limit))
            .copied()
    }
    pub(crate) fn can_reposition(&self, state: &Arc<StreamingState>) -> bool {
        !self.failed
            && !state.is_cancelled()
            && self
                .owner
                .as_ref()
                .is_none_or(|owner| owner.as_ptr() == Arc::as_ptr(state))
            && self
                .remaining
                .saturating_add(state.rendered_frames().saturating_sub(self.observed))
                .min(RESUME_BURST)
                != 0
    }
}
pub(super) fn check(
    file: &dyn AudioFile,
    state: &StreamingState,
    deadline: Instant,
) -> EngineResult<()> {
    if state.is_cancelled() {
        return Err(EngineError::new(ErrorCode::Cancelled));
    }
    if file.is_closed() {
        return Err(io_error());
    }
    if Instant::now() >= deadline {
        return Err(EngineError::new(ErrorCode::Timeout));
    }
    Ok(())
}
impl FileSource {
    pub(super) async fn loan(
        &mut self,
        offset: u64,
        length: usize,
        control: bool,
        state: &Arc<StreamingState>,
        deadline: Instant,
    ) -> EngineResult<AudioFileChunk> {
        let closed = self.file.closed_signal().notified();
        tokio::pin!(closed);
        closed.as_mut().enable();
        check(self.file.as_ref(), state, deadline)?;
        let read = if control {
            self.file
                .clone()
                .read_control_at(offset, length, state.clone(), deadline.into_std())?
        } else {
            self.file
                .clone()
                .read_at(offset, length, state.clone(), deadline.into_std())?
        };
        let chunk = tokio::select! { biased;
            _=state.cancelled()=>return Err(EngineError::new(ErrorCode::Cancelled)),
            _=&mut closed=>return Err(io_error()),
            result=tokio::time::timeout_at(deadline,read)=>result.map_err(|_|EngineError::new(ErrorCode::Timeout))?.map_err(|_|io_error())?.map_err(|_|io_error())??,
        };
        check(self.file.as_ref(), state, deadline)?;
        if chunk.as_ref().len() != length {
            return Err(io_error());
        }
        Ok(chunk)
    }
    async fn atom(
        &mut self,
        position: u64,
        state: &Arc<StreamingState>,
        deadline: Instant,
    ) -> EngineResult<AtomSpan> {
        if self.file.length().saturating_sub(position) < 8 {
            return Err(invalid("local M4A atom outside source"));
        }
        let header = self.loan(position, 8, false, state, deadline).await?;
        let span = AtomSpan::read(header.as_ref(), position, Some(self.file.length()))
            .map_err(|_| invalid("local M4A invalid atom"))?;
        drop(header);
        let span = if let Some(span) = span {
            span
        } else {
            let header = self.loan(position, 16, false, state, deadline).await?;
            AtomSpan::read(header.as_ref(), position, Some(self.file.length()))
                .map_err(|_| invalid("local M4A invalid extended atom"))?
                .ok_or_else(|| invalid("local M4A truncated atom"))?
        };
        if span.kind == AtomKind::Fragmented {
            return Err(invalid("fragmented MP4 is outside the local M4A profile"));
        }
        if span
            .end
            .is_none_or(|end| end <= position || end > self.file.length())
        {
            return Err(invalid("local M4A atom extent outside source"));
        }
        Ok(span)
    }
    async fn media(
        &mut self,
        packet: IndexedPacket,
        state: &Arc<StreamingState>,
        deadline: Instant,
    ) -> EngineResult<AtomSpan> {
        let end = packet
            .offset
            .checked_add(u64::from(packet.size))
            .filter(|end| *end > packet.offset && *end <= self.file.length())
            .ok_or_else(|| invalid("local M4A packet extent outside source"))?;
        if let Some(span) = self
            .index
            .media
            .iter()
            .flatten()
            .find(|s| s.body_offset <= packet.offset && s.end.is_some_and(|limit| end <= limit))
        {
            return Ok(*span);
        }
        let mut position = 0;
        let mut skipped = 0;
        for _ in 0..MAX_ATOMS {
            let span = self.atom(position, state, deadline).await?;
            let limit = span.end.unwrap();
            if span.kind == AtomKind::MediaData {
                self.index.remember(span)?;
            } else {
                skipped += limit - position;
                if skipped > MAX_SKIPPED {
                    return Err(invalid("local M4A metadata budget exceeded"));
                }
            }
            if span.offset <= packet.offset && packet.offset < limit {
                if span.kind != AtomKind::MediaData
                    || packet.offset < span.body_offset
                    || end > limit
                {
                    return Err(invalid("local M4A packet outside actual mdat"));
                }
                return Ok(span);
            }
            position = limit;
        }
        Err(invalid("local M4A media discovery atom budget exceeded"))
    }
    pub(super) async fn resolve(
        &mut self,
        request: SourceRequest,
        decoder: OffWorker<Option<FileDecoder>>,
        tx: &mpsc::Sender<StreamMsg>,
        state: &Arc<StreamingState>,
    ) -> EngineResult<Option<SourceResume<FileDecoder>>> {
        let deadline = Instant::now() + streaming::RESPONSE_HEADER_TIMEOUT;
        self.resolve_before(request, decoder, tx, state, deadline)
            .await
    }
    pub(super) async fn resolve_before(
        &mut self,
        request: SourceRequest,
        decoder: OffWorker<Option<FileDecoder>>,
        tx: &mpsc::Sender<StreamMsg>,
        state: &Arc<StreamingState>,
        deadline: Instant,
    ) -> EngineResult<Option<SourceResume<FileDecoder>>> {
        match request {
            SourceRequest::Index(request) => {
                if request.media.kind != AtomKind::MediaData
                    || request.next_atom_offset != request.media.end
                {
                    return Err(invalid("local M4A invalid index request"));
                }
                let actual = self.atom(request.media.offset, state, deadline).await?;
                if actual != request.media {
                    return Err(invalid("local M4A requested mdat changed"));
                }
                self.index.remember(actual)?;
                let mut position = request
                    .next_atom_offset
                    .ok_or_else(|| invalid("local M4A missing tail index extent"))?;
                let mut skipped = 0;
                for _ in 0..MAX_ATOMS {
                    let span = self.atom(position, state, deadline).await?;
                    let end = span.end.unwrap();
                    if span.kind == AtomKind::Movie {
                        let length = usize::try_from(end - position)
                            .map_err(|_| invalid("local M4A index size overflow"))?;
                        if length > IndexOptions::default().max_index_bytes {
                            return Err(invalid(
                                "local M4A index budget exceeded before body read",
                            ));
                        }
                        let bytes = self.loan(position, length, true, state, deadline).await?;
                        let object_len = self.file.length();
                        let active = state.clone();
                        let file = self.file.clone();
                        let operation = decoder.with(move |value| {
                            check(file.as_ref(), &active, deadline)?;
                            let Some(FileDecoder::M4a(decoder, _)) = value else {
                                return Err(invalid("local M4A decoder missing"));
                            };
                            if decoder.source_request() != Some(SourceRequest::Index(request)) {
                                return Err(invalid("local M4A index request changed"));
                            }
                            let index = AudioIndex::from_moov(
                                bytes.as_ref(),
                                IndexOptions {
                                    object_len: Some(object_len),
                                    ..Default::default()
                                },
                            )
                            .map_err(|_| invalid("local M4A invalid index"))?;
                            check(file.as_ref(), &active, deadline)?;
                            decoder.install_index(index, object_len)
                        });
                        let Some(result) = transport::while_active(
                            tx,
                            state,
                            tokio::time::timeout_at(deadline, operation),
                        )
                        .await
                        else {
                            return Ok(None);
                        };
                        let (decoder, result) =
                            result.map_err(|_| EngineError::new(ErrorCode::Timeout))??;
                        result?;
                        return Ok(Some(SourceResume {
                            decoder,
                            restart: false,
                        }));
                    }
                    if span.kind == AtomKind::MediaData {
                        self.index.remember(span)?;
                    } else {
                        skipped += end - position;
                        if skipped > MAX_SKIPPED {
                            return Err(invalid("local M4A tail metadata budget exceeded"));
                        }
                    }
                    position = end;
                }
                Err(invalid("local M4A index discovery atom budget exceeded"))
            }
            SourceRequest::ReadAt(packet) => {
                self.index.charge(state)?;
                let media = self.media(packet, state, deadline).await?;
                let count = (self.file.length() - packet.offset)
                    .min(migo_io::read_at_file::FILE_CHUNK_BYTES as u64)
                    as usize;
                let first = self
                    .loan(packet.offset, count, false, state, deadline)
                    .await?;
                let active = state.clone();
                let file = self.file.clone();
                let operation = decoder.with(move |value| {
                    let result = (|| {
                        check(file.as_ref(), &active, deadline)?;
                        let Some(FileDecoder::M4a(decoder, _)) = value else {
                            return Err(invalid("local M4A decoder missing"));
                        };
                        if decoder.source_request() != Some(SourceRequest::ReadAt(packet)) {
                            return Err(invalid("local M4A packet request changed"));
                        }
                        decoder.resume_at(media, packet.offset)
                    })();
                    (result, first)
                });
                let Some(result) = transport::while_active(
                    tx,
                    state,
                    tokio::time::timeout_at(deadline, operation),
                )
                .await
                else {
                    return Ok(None);
                };
                let (decoder, (result, first)) =
                    result.map_err(|_| EngineError::new(ErrorCode::Timeout))??;
                result?;
                self.position = packet.offset + count as u64;
                self.pending = Some(first);
                Ok(Some(SourceResume {
                    decoder,
                    restart: true,
                }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;
    #[test]
    fn only_consumed_frames_renew_the_fixed_source_bound_reposition_burst() {
        let source = StreamingState::new_reusable();
        let (commands, _requests) = crate::stream_control::channel();
        source.install_controls(commands.clone()).unwrap();
        let mut index = Index::new();
        for _ in 0..64 {
            index.charge(&source).unwrap();
        }
        let _reply = crate::stream_control::request(
            &commands,
            0,
            Instant::now() + streaming::RESPONSE_HEADER_TIMEOUT,
        )
        .unwrap();
        source
            .bytes_downloaded
            .fetch_add(1_000_000, Ordering::Relaxed);
        assert!(
            index.charge(&source).is_err(),
            "non-render work minted local source reposition credit"
        );
        source.record_rendered_frames(1024);
        assert!(
            index.charge(&source).is_err(),
            "exhausted local reposition budget recovered after retirement"
        );
        let source = StreamingState::new();
        let mut index = Index::new();
        index.charge(&source).unwrap();
        source.record_rendered_frames(10000);
        for _ in 0..64 {
            index.charge(&source).unwrap();
        }
        assert!(
            index.charge(&source).is_err(),
            "render credit exceeded the original reposition burst"
        );
    }
    #[test]
    fn original_consumer_progress_renews_without_banked_surplus_or_foreign_owners() {
        let source = StreamingState::new();
        let mut index = Index::new();
        for _ in 0..200 {
            index.charge(&source).unwrap();
            source.record_rendered_frames(1);
        }
        assert!(
            index.charge(&StreamingState::new()).is_err(),
            "reposition credit accepted a foreign source"
        );
        let source = StreamingState::new();
        let mut index = Index::new();
        source.cancel();
        assert!(
            index.charge(&source).is_err(),
            "cancelled source accepted an index reposition"
        );
    }
}

#[cfg(test)]
mod actual_extents {
    use super::*;
    #[tokio::test]
    async fn packet_offsets_are_checked_against_the_actual_media_atom_before_resume() {
        let f = super::super::tests::Fixture::new().await;
        let data = include_bytes!("decoder/aac-fixtures/1-front.m4a");
        let mut position = 0;
        let mut index = loop {
            let span = AtomSpan::read(&data[position..], position as u64, Some(data.len() as u64))
                .unwrap()
                .unwrap();
            if span.kind == AtomKind::Movie {
                break AudioIndex::from_moov(
                    &data[position..span.end.unwrap() as usize],
                    IndexOptions {
                        object_len: Some(data.len() as u64),
                        ..Default::default()
                    },
                )
                .unwrap();
            }
            position = span.end.unwrap() as usize;
        };
        let mut packet = index.next_packet().unwrap().unwrap();
        packet.offset = 8;
        packet.size = 1;
        let file = f.source(data).await;
        let mut source = FileSource {
            _owner: OffWorker::new(file.clone(), f.host.audio_cpu()),
            file,
            position: 0,
            index: Index::new(),
            pending: None,
        };
        assert!(
            source
                .media(
                    packet,
                    &StreamingState::new(),
                    Instant::now() + streaming::RESPONSE_HEADER_TIMEOUT
                )
                .await
                .is_err(),
            "source accepted packet bytes outside actual mdat"
        );
    }
}
