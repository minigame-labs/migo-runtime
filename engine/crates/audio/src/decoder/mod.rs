pub(crate) mod pcm;
pub(crate) mod whole;

#[cfg(test)]
mod aac_fixtures;
/// Every M4A layout fixture with its format and presentation length.
#[cfg(test)]
pub(crate) fn aac_fixture_cases() -> &'static [(&'static str, &'static [u8], u32, u32, usize)] {
    aac_fixtures::CASES
}

#[cfg(test)]
use shared::error::EngineResult;

/// Decoded audio data in f32 samples, interleaved for multi-channel
#[derive(Debug, Clone)]
pub struct DecodedAudio {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u32,
}

impl DecodedAudio {
    /// Number of sample frames (samples / channels)
    pub fn frame_count(&self) -> usize {
        if self.channels == 0 {
            0
        } else {
            self.samples.len() / self.channels as usize
        }
    }

    /// Duration in seconds
    pub fn duration(&self) -> f64 {
        if self.sample_rate == 0 {
            0.0
        } else {
            self.frame_count() as f64 / self.sample_rate as f64
        }
    }
}

/// Decode a whole file at its own rate (the product path, as tests use it).
#[cfg(test)]
pub fn decode(data: &[u8]) -> EngineResult<DecodedAudio> {
    let mut job = whole::WholeDecodeJob::new(data.to_vec(), None)?;
    while !job.step(&std::sync::atomic::AtomicBool::new(false))? {}
    job.finish()
}

pub(crate) fn is_mp3(data: &[u8]) -> bool {
    data.len() >= 4 && (data.starts_with(b"ID3") || (data[0] == 0xFF && (data[1] & 0xE0) == 0xE0))
}

/// Preserve the submitted target through each product codec, collection and SRC phase.
#[cfg(test)]
pub(crate) fn decode_for_rate(data: &[u8], target_rate: u32) -> EngineResult<DecodedAudio> {
    let mut job = whole::WholeDecodeJob::new(data.to_vec(), Some(target_rate))?;
    while !job.step(&std::sync::atomic::AtomicBool::new(false))? {}
    job.finish()
}

// ADTS has MPEG layer bits 00 and must precede the older broad MP3 sync check.
pub(crate) fn is_adts(data: &[u8]) -> bool {
    let mut data = data;
    let mut skipped = 0_usize;
    // Skip only bounded, library-validated tag extents. A sync word inside an
    // ID3 body is metadata, and ID3 alone does not distinguish MP3 from ADTS.
    for tag in 0..=32 {
        if !data.starts_with(b"ID3") {
            return data.len() >= 4 && data[0] == 0xff && data[1] & 0xf6 == 0xf0;
        }
        if tag == 32 {
            return false;
        }
        let Some(header) = data.get(..10).and_then(|h| h.try_into().ok()) else {
            return false;
        };
        let Ok(length) = symphonia_metadata::id3v2::Id3v2Reader::total_tag_len(header) else {
            return false;
        };
        let length = length as usize;
        skipped += length;
        if length == 0 || skipped > 4 * 1024 * 1024 {
            return false;
        }
        let Some(rest) = data.get(length..) else {
            return false;
        };
        data = rest;
    }
    false
}
pub(crate) fn is_m4a(data: &[u8]) -> bool {
    data.len() >= 8
        && matches!(
            &data[4..8],
            b"ftyp" | b"moov" | b"mdat" | b"free" | b"wide" | b"skip"
        )
}
