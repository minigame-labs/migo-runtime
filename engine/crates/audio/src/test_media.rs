//! Media fixtures shared by the stream tests.

/// A 16-bit PCM WAVE of a non-constant signal (hound, a dev-dependency).
pub(crate) fn wav(rate: u32, channels: u16, frames: usize) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    let mut writer = hound::WavWriter::new(
        &mut out,
        hound::WavSpec {
            channels,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .unwrap();
    for frame in 0..frames {
        for ch in 0..channels {
            writer
                .write_sample(((frame * 137 + usize::from(ch) * 31) % 12001) as i16 - 6000)
                .unwrap();
        }
    }
    writer.finalize().unwrap();
    out.into_inner()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AtomKind {
    MediaData,
    Movie,
    Other,
}

/// One top-level ISO-BMFF box: `[start, end)` and its kind.
pub(crate) struct AtomSpan {
    pub header_len: u64,
    pub end: Option<u64>,
    pub kind: AtomKind,
}

impl AtomSpan {
    /// The box header at the start of `data` (which begins at `offset` of an
    /// object of `object_len` bytes).
    pub(crate) fn read(data: &[u8], offset: u64, object_len: Option<u64>) -> Option<Option<Self>> {
        if data.len() < 8 {
            return None;
        }
        let size32 = u32::from_be_bytes(data[..4].try_into().unwrap()) as u64;
        let kind = match &data[4..8] {
            b"mdat" => AtomKind::MediaData,
            b"moov" => AtomKind::Movie,
            _ => AtomKind::Other,
        };
        let (size, header_len) = match size32 {
            1 => (u64::from_be_bytes(data.get(8..16)?.try_into().unwrap()), 16),
            0 => (object_len? - offset, 8),
            n => (n, 8),
        };
        Some(Some(Self {
            header_len,
            end: Some(offset + size),
            kind,
        }))
    }
}
