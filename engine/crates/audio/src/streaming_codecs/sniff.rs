//! Only signatures and bounded library tag extents; container parsing stays in the codecs.
use super::*;
#[derive(Default)]
pub(super) struct Detect {
    pub(super) prefix: [u8; 10],
    length: usize,
    skip: usize,
    metadata: usize,
    tags: usize,
}
pub(super) enum Kind {
    Mp3,
    Wave,
    Vorbis,
    Adts,
    M4a,
}
impl Detect {
    pub(super) fn step(&mut self, input: &[u8], eof: bool) -> EngineResult<(usize, Option<Kind>)> {
        if self.skip != 0 {
            let n = self.skip.min(input.len()).min(4096);
            if eof {
                return Err(invalid("truncated stream metadata"));
            }
            self.skip -= n;
            return Ok((n, None));
        }
        let want = if self.length >= 3 && self.prefix[..3] == *b"ID3" {
            10
        } else {
            8
        };
        let n = (want - self.length).min(input.len());
        self.prefix[self.length..self.length + n].copy_from_slice(&input[..n]);
        self.length += n;
        if self.length >= 3 && self.prefix[..3] == *b"ID3" {
            if self.length < 10 {
                if eof {
                    return Err(invalid("truncated stream ID3 header"));
                }
                return Ok((n, None));
            }
            let bytes = symphonia_metadata::id3v2::Id3v2Reader::total_tag_len(&self.prefix)
                .map_err(|e| invalid(e.to_string()))? as usize;
            self.tags += 1;
            self.metadata = self
                .metadata
                .checked_add(bytes)
                .filter(|n| *n <= 4 * 1024 * 1024)
                .ok_or_else(|| invalid("stream metadata byte budget exceeded"))?;
            if self.tags > 32 || bytes < 10 {
                return Err(invalid("stream metadata tag budget exceeded"));
            }
            self.skip = bytes - 10;
            self.length = 0;
            return Ok((n, None));
        }
        if self.length < 8 {
            if eof {
                return Err(invalid("stream contains no complete audio header"));
            }
            return Ok((n, None));
        }
        let p = &self.prefix;
        let kind = if p.starts_with(b"RIFF") {
            Kind::Wave
        } else if p.starts_with(b"OggS") {
            Kind::Vorbis
        } else if p[0] == 0xff && p[1] & 0xf6 == 0xf0 {
            Kind::Adts
        } else if matches!(
            &p[4..8],
            b"ftyp" | b"moov" | b"mdat" | b"free" | b"wide" | b"skip"
        ) {
            Kind::M4a
        } else {
            Kind::Mp3
        };
        Ok((n, Some(kind)))
    }
}
