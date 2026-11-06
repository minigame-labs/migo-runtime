//! Resolve the existing library's requests against the job's immutable input.
//! Atom framing and index parsing stay in Symphonia; no MP4 parser lives here.
use super::*;
use super::{
    incremental::Decoder,
    m4a::{M4a, SourceRequest as M4aSourceRequest},
};
use symphonia_format_isomp4::streaming::{
    AtomKind, AtomSpan, AudioIndex, IndexOptions, IndexRequest, IndexedPacket,
};

type M4aPort = Decoder<M4a>;
const MAX_ATOMS: u32 = 32;
const MAX_SKIPPED: u64 = 4 * 1024 * 1024;

struct Scan {
    position: u64,
    atoms: u32,
    skipped: u64,
}
impl Scan {
    fn new(position: u64) -> Self {
        Self {
            position,
            atoms: 0,
            skipped: 0,
        }
    }
    fn next(&mut self, data: &[u8]) -> EngineResult<AtomSpan> {
        self.atoms = self
            .atoms
            .checked_add(1)
            .filter(|n| *n <= MAX_ATOMS)
            .ok_or_else(|| invalid("whole-file M4A atom budget exceeded"))?;
        let start = usize::try_from(self.position)
            .ok()
            .filter(|p| *p < data.len())
            .ok_or_else(|| invalid("whole-file M4A source outside input"))?;
        let header = &data[start..(start + 16).min(data.len())];
        let span = AtomSpan::read(header, self.position, Some(data.len() as u64))
            .map_err(|_| invalid("whole-file M4A invalid atom"))?
            .ok_or_else(|| invalid("whole-file M4A truncated atom"))?;
        if span.kind == AtomKind::Fragmented {
            return Err(invalid("whole-file fragmented MP4 is outside the profile"));
        }
        let end = span
            .end
            .filter(|end| *end > self.position && *end <= data.len() as u64)
            .ok_or_else(|| invalid("whole-file M4A atom outside input"))?;
        if !matches!(span.kind, AtomKind::MediaData | AtomKind::Movie) {
            self.skipped = self
                .skipped
                .checked_add(end - self.position)
                .filter(|n| *n <= MAX_SKIPPED)
                .ok_or_else(|| invalid("whole-file M4A skipped metadata budget exceeded"))?;
        }
        self.position = end;
        Ok(span)
    }
}

enum Source {
    Index(Scan),
    Parse { start: usize, end: usize },
    Media { scan: Scan, packet: IndexedPacket },
}

pub(super) struct File {
    pub(super) decoder: M4aPort,
    source: Option<Source>,
}
impl File {
    #[cfg(test)]
    pub(super) fn source_stage(&self) -> usize {
        match self.source {
            None => 0,
            Some(Source::Index(_)) => 1,
            Some(Source::Parse { .. }) => 2,
            Some(Source::Media { .. }) => 3,
        }
    }

    pub(super) fn new(cap: usize, length: usize) -> Self {
        Self {
            decoder: M4aPort::with_object_len(Some(cap as u64), Some(length as u64)),
            source: None,
        }
    }

    pub(super) fn drain(
        &mut self,
        pcm: &mut Vec<f32>,
        cancel: &AtomicBool,
    ) -> EngineResult<Progress> {
        let progress = self.decoder.drain(pcm, cancel).map_err(codec_error)?;
        if progress == Progress::NeedSource {
            self.source = Some(match self.decoder.source_request() {
                Some(M4aSourceRequest::Index(IndexRequest {
                    media,
                    next_atom_offset,
                })) if media.kind == AtomKind::MediaData && next_atom_offset == media.end => {
                    Source::Index(Scan::new(next_atom_offset.ok_or_else(|| {
                        invalid("whole-file M4A has no following index extent")
                    })?))
                }
                Some(M4aSourceRequest::ReadAt(packet)) => Source::Media {
                    scan: Scan::new(0),
                    packet,
                },
                _ => return Err(invalid("whole-file M4A missing source request")),
            });
        }
        Ok(progress)
    }

    /// Exactly one library atom header, one bounded index parse, or one resume.
    /// A 2MiB library index parse is bounded but is not internally preemptible.
    pub(super) fn source_step(&mut self, data: &[u8], offset: &mut usize) -> EngineResult<bool> {
        let Some(source) = self.source.take() else {
            return Ok(false);
        };
        let next = match source {
            Source::Index(mut scan) => {
                let start = scan.position;
                let span = scan.next(data)?;
                if span.kind == AtomKind::Movie {
                    let end = scan.position;
                    if end - start > IndexOptions::default().max_index_bytes as u64 {
                        return Err(invalid("whole-file M4A index byte budget exceeded"));
                    }
                    Some(Source::Parse {
                        start: start as usize,
                        end: end as usize,
                    })
                } else {
                    Some(Source::Index(scan))
                }
            }
            Source::Parse { start, end } => {
                let index = AudioIndex::from_moov(
                    &data[start..end],
                    IndexOptions {
                        object_len: Some(data.len() as u64),
                        ..Default::default()
                    },
                )
                .map_err(|_| invalid("whole-file M4A invalid index"))?;
                self.decoder
                    .install_index_with_length(index, Some(data.len() as u64))
                    .map_err(|_| invalid("whole-file M4A index resume failed"))?;
                None
            }
            Source::Media { mut scan, packet } => {
                let span = scan.next(data)?;
                if span.kind == AtomKind::MediaData
                    && packet.offset >= span.body_offset
                    && packet
                        .offset
                        .checked_add(u64::from(packet.size))
                        .is_some_and(|end| end <= scan.position)
                {
                    self.decoder
                        .resume_at(span, packet.offset)
                        .map_err(|_| invalid("whole-file M4A media resume failed"))?;
                    *offset = usize::try_from(packet.offset)
                        .map_err(|_| invalid("whole-file M4A packet offset overflows"))?;
                    None
                } else {
                    Some(Source::Media { scan, packet })
                }
            }
        };
        self.source = next;
        Ok(true)
    }
}
