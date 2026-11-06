//! Product format dispatch owns the same codec/PCM/SRC jobs as direct decoding.
use super::{
    DecodedAudio, aac::AacDecodeJob, mp3::Mp3DecodeJob, ogg::OggDecodeJob, wav::WavDecodeJob,
};
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::sync::atomic::AtomicBool;

pub(crate) enum WholeDecodeJob<B> {
    Mp3(Mp3DecodeJob<B>),
    Wave(WavDecodeJob<B>),
    Vorbis(OggDecodeJob<B>),
    Aac(AacDecodeJob<B>),
}
impl<B: AsRef<[u8]>> WholeDecodeJob<B> {
    pub(crate) fn new(input: B, target: Option<u32>) -> EngineResult<Self> {
        if target == Some(0) || input.as_ref().len() > 16 * 1024 * 1024 {
            return Err(EngineError::from_detail(
                ErrorCode::InvalidArgument,
                "audio decode input or target outside limits",
            ));
        }
        let data = input.as_ref();
        if data.len() < 4 {
            return Err(EngineError::from_detail(
                ErrorCode::InvalidArgument,
                "Audio data too short to detect format",
            ));
        }
        if data.starts_with(b"RIFF") {
            Ok(Self::Wave(WavDecodeJob::new(input, target)?))
        } else if data.starts_with(b"OggS") {
            Ok(Self::Vorbis(OggDecodeJob::new(input, target)?))
        } else if super::is_adts(data) || super::is_m4a(data) {
            let m4a = super::is_m4a(data);
            Ok(Self::Aac(AacDecodeJob::new(input, target, m4a)?))
        } else if super::is_mp3(data) {
            Ok(Self::Mp3(Mp3DecodeJob::new(input, target)))
        } else {
            Err(EngineError::from_detail(
                ErrorCode::InvalidArgument,
                "Unknown audio format",
            ))
        }
    }
    pub(crate) fn step(&mut self, cancel: &AtomicBool) -> EngineResult<bool> {
        match self {
            Self::Mp3(job) => job.step(cancel),
            Self::Wave(job) => job.step(cancel),
            Self::Vorbis(job) => job.step(cancel),
            Self::Aac(job) => job.step(cancel),
        }
    }
    pub(crate) fn finish(self) -> EngineResult<DecodedAudio> {
        match self {
            Self::Mp3(job) => job.finish(),
            Self::Wave(job) => job.finish(),
            Self::Vorbis(job) => job.finish(),
            Self::Aac(job) => job.finish(),
        }
    }
}

#[cfg(test)]
#[path = "whole_tests.rs"]
mod tests;
