//! Candidate codec -> bounded Rubato steps -> production PCM publication.
use crate::{
    bandlimited, push_decoder,
    streaming_transport::{Decoder, Progress, Step},
};
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::sync::atomic::{AtomicBool, Ordering};

pub fn core(input: u32, output: u32, channels: u32) -> EngineResult<rubato::Async<f32>> {
    bandlimited::validate_format(input, output, channels).map_err(error)?;
    rubato::Async::new_sinc(
        output as f64 / input as f64,
        1.0,
        &rubato::SincInterpolationParameters::default(),
        128,
        channels as usize,
        rubato::FixedAsync::Input,
    )
    .map_err(|cause| error(cause.to_string()))
}

fn error(message: impl Into<String>) -> EngineError {
    EngineError::from_detail(ErrorCode::IoError, message.into())
}

// Selected from the measured 1/4/8/16 quantum candidates. Retain the explicit
// const-generic arms for comparisons; all share the same input/output bounds.
pub type Resampled = Batched<16>;
#[cfg(feature = "wave-streaming-probe")]
pub type WaveResampled = Batched<16, push_decoder::Wave>;
#[cfg(feature = "adts-streaming-probe")]
pub type AdtsResampled = Batched<16, push_decoder::Adts>;
#[cfg(feature = "vorbis-streaming-probe")]
pub type VorbisResampled = Batched<16, push_decoder::Vorbis>;

pub struct Batched<
    const QUANTA: usize,
    P: push_decoder::incremental::Packetizer = push_decoder::Mp3,
> {
    decoder: push_decoder::incremental::Decoder<P>,
    resampler: Option<bandlimited::Stream<rubato::Async<f32>>>,
    decoded: Vec<f32>,
    position: usize,
    target_rate: u32,
    input_ended: bool,
    decoder_done: bool,
    stopped: bool,
    failed: bool,
}

impl<const QUANTA: usize, P: push_decoder::incremental::Packetizer> Batched<QUANTA, P> {
    pub fn new(target_rate: u32) -> Self {
        assert!((1..=32).contains(&QUANTA));
        Self {
            decoder: push_decoder::incremental::Decoder::<P>::new(None),
            resampler: None,
            decoded: Vec::with_capacity(P::OUTPUT_SAMPLES_PER_STEP),
            position: 0,
            target_rate,
            input_ended: false,
            decoder_done: false,
            stopped: false,
            failed: false,
        }
    }

    fn outcome(&self, consumed: usize, progress: Progress) -> Step {
        Step {
            consumed,
            progress,
            sample_rate: if self.decoder.rate == 0 {
                0
            } else {
                self.target_rate
            },
            channels: self.decoder.channels,
        }
    }

    fn step_inner(
        &mut self,
        input: &[u8],
        eof: bool,
        pcm: &mut Vec<f32>,
        cancel: &AtomicBool,
    ) -> EngineResult<Step> {
        if self.stopped || cancel.load(Ordering::Acquire) {
            self.stopped = true;
            self.decoded.clear();
            return Ok(self.outcome(0, Progress::Cancelled));
        }
        if !pcm.is_empty() {
            return Err(error("PCM destination must be empty"));
        }
        if self.position < self.decoded.len() {
            let sampler = self.resampler.as_mut().expect("pending PCM has resampler");
            for _ in 0..QUANTA {
                if cancel.load(Ordering::Acquire) {
                    self.stopped = true;
                    pcm.clear();
                    self.decoded.clear();
                    return Ok(self.outcome(0, Progress::Cancelled));
                }
                if pcm.capacity() - pcm.len() < sampler.output_capacity() {
                    break;
                }
                let step = sampler
                    .append_step(&self.decoded[self.position..], false, pcm)
                    .map_err(error)?;
                self.position += step.consumed;
                if self.position == self.decoded.len() {
                    break;
                }
            }
            // Drain before accepting another byte, including when the last
            // resampling quantum filled no PCM because of the startup delay.
            return Ok(self.outcome(0, Progress::Yield));
        }
        self.decoded.clear();
        self.position = 0;
        if self.decoder_done {
            let sampler = self
                .resampler
                .as_mut()
                .expect("bypass finishes with the decoder");
            let mut progress = Progress::Yield;
            for _ in 0..QUANTA {
                if cancel.load(Ordering::Acquire) {
                    self.stopped = true;
                    pcm.clear();
                    return Ok(self.outcome(0, Progress::Cancelled));
                }
                if pcm.capacity() - pcm.len() < sampler.output_capacity() {
                    break;
                }
                let step = sampler.append_step(&[], true, pcm).map_err(error)?;
                if step.progress == bandlimited::Progress::Finished {
                    progress = Progress::Finished;
                    break;
                }
            }
            return Ok(self.outcome(0, progress));
        }
        let consumed = if eof {
            if !self.input_ended {
                self.decoder.finish_input();
                self.input_ended = true;
            }
            0
        } else {
            self.decoder.push(input).map_err(error)?
        };
        let progress = self
            .decoder
            .drain(&mut self.decoded, cancel)
            .map_err(error)?;
        if self.decoder.rate != 0
            && self.target_rate != self.decoder.rate
            && self.resampler.is_none()
        {
            self.resampler = Some(
                bandlimited::Stream::new(
                    core(self.decoder.rate, self.target_rate, self.decoder.channels)?,
                    self.decoder.rate,
                    self.target_rate,
                    self.decoder.channels,
                )
                .map_err(error)?,
            );
        }
        if progress == push_decoder::Progress::Cancelled {
            self.stopped = true;
            return Ok(self.outcome(consumed, Progress::Cancelled));
        }
        if self.target_rate == self.decoder.rate {
            // Same-rate playback goes directly to the publication buffer; no
            // filter, PCM copy, resampler construction or extra drain cycle.
            std::mem::swap(pcm, &mut self.decoded);
            return Ok(self.outcome(
                consumed,
                match progress {
                    push_decoder::Progress::Finished => Progress::Finished,
                    push_decoder::Progress::NeedInput => Progress::NeedInput,
                    push_decoder::Progress::Yield => Progress::Yield,
                    push_decoder::Progress::Cancelled => unreachable!(),
                },
            ));
        }
        self.decoder_done = progress == push_decoder::Progress::Finished;
        let progress = if self.decoder_done
            || !self.decoded.is_empty()
            || progress == push_decoder::Progress::Yield
        {
            Progress::Yield
        } else {
            Progress::NeedInput
        };
        Ok(self.outcome(consumed, progress))
    }
}

impl<const QUANTA: usize, P: push_decoder::incremental::Packetizer> Decoder for Batched<QUANTA, P> {
    fn step(
        &mut self,
        input: &[u8],
        eof: bool,
        pcm: &mut Vec<f32>,
        cancel: &AtomicBool,
    ) -> EngineResult<Step> {
        if self.failed {
            return Err(error("composed decoder previously failed"));
        }
        let result = self.step_inner(input, eof, pcm, cancel);
        if result.is_err() {
            self.failed = true;
            self.decoded.clear();
            pcm.clear();
        }
        result
    }
}
