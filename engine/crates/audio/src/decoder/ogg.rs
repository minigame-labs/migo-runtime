//! Whole-file Vorbis drains the same owned steps used by shared scheduling.
#[cfg(test)]
use super::DecodedAudio;
#[cfg(test)]
use shared::error::EngineResult;
#[cfg(test)]
use std::sync::atomic::AtomicBool;
#[path = "ogg_job.rs"]
mod job;
pub(crate) use job::OggDecodeJob;

#[cfg(test)]
pub fn decode(data: &[u8]) -> EngineResult<DecodedAudio> {
    decode_with_limit(data, crate::limits::MAX_AUDIO_PCM_SAMPLES as usize)
}
#[cfg(test)]
fn decode_with_limit(data: &[u8], limit: usize) -> EngineResult<DecodedAudio> {
    let mut job = OggDecodeJob::with_limit(data, None, limit)?;
    while !job.step(&AtomicBool::new(false))? {}
    job.finish()
}
#[cfg(test)]
#[path = "ogg_reference.rs"]
mod reference;
#[cfg(test)]
#[path = "ogg_tests.rs"]
mod tests;
