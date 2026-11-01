//! Shared Rubato stream adapter: only buffering, time and resource policy.
//! Filtering, SIMD, interpolation and FFT remain the library's implementation.
use rubato::{Indexing, Resampler, audioadapter_buffers::direct::InterleavedSlice};

pub const MAX_SCRATCH_SAMPLES: usize = 32 * 1024;
#[cfg(test)]
#[derive(Default)]
pub struct Trace {
    pub calls: usize,
    pub input_samples: usize,
    pub output_samples: usize,
}
#[cfg(test)]
thread_local! { static TRACE: std::cell::RefCell<Trace> = const { std::cell::RefCell::new(Trace { calls:0,input_samples:0,output_samples:0 }) }; }
#[cfg(test)]
pub fn take_trace() -> Trace {
    TRACE.with(|t| std::mem::take(&mut *t.borrow_mut()))
}

#[derive(Debug, PartialEq, Eq)]
pub enum Progress {
    NeedInput,
    Yield,
    Finished,
}
#[derive(Debug)]
pub struct Step {
    pub consumed: usize,
    pub progress: Progress,
}

pub struct Stream<R> {
    inner: R,
    channels: usize,
    input_rate: u64,
    output_rate: u64,
    input: Vec<f32>,
    output: Vec<f32>,
    delay: usize,
    input_frames: u64,
    output_frames: u64,
    discard_until: u64,
    eof: bool,
    finished: bool,
    failed: bool,
    output_end: Option<u64>,
}

pub fn validate_format(input: u32, output: u32, channels: u32) -> Result<(), String> {
    if input == 0 || output == 0 || !(1..=32).contains(&channels) {
        return Err("resampling format outside supported limits".into());
    }
    Ok(())
}

impl<R: Resampler<f32>> Stream<R> {
    pub fn new(inner: R, input_rate: u32, output_rate: u32, channels: u32) -> Result<Self, String> {
        validate_format(input_rate, output_rate, channels)?;
        Self::with_source_rates(inner, input_rate, output_rate, channels)
    }

    /// File metadata can contain any nonzero source rate. Public context policy
    /// remains separate from the sample and scratch limits of this owner.
    pub fn with_source_rates(
        inner: R,
        input_rate: u32,
        output_rate: u32,
        channels: u32,
    ) -> Result<Self, String> {
        if input_rate == 0 || output_rate == 0 || !(1..=32).contains(&channels) {
            return Err("invalid resampling format".into());
        }
        if inner.nbr_channels() != channels as usize {
            return Err("resampler channel count mismatch".into());
        }
        if inner.resample_ratio() != output_rate as f64 / input_rate as f64 {
            return Err("resampler ratio does not match declared sample rates".into());
        }
        let input_samples = inner
            .input_frames_max()
            .checked_mul(channels as usize)
            .filter(|n| *n > 0 && *n <= MAX_SCRATCH_SAMPLES)
            .ok_or("resampler input scratch budget")?;
        let output_samples = inner
            .output_frames_max()
            .checked_mul(channels as usize)
            .filter(|n| *n > 0 && *n <= MAX_SCRATCH_SAMPLES)
            .ok_or("resampler output scratch budget")?;
        let delay = inner.output_delay();
        let mut owner = Self {
            inner,
            channels: channels as usize,
            input_rate: input_rate as u64,
            output_rate: output_rate as u64,
            input: Vec::with_capacity(input_samples),
            output: vec![0.0; output_samples],
            delay,
            input_frames: 0,
            output_frames: 0,
            discard_until: 0,
            eof: false,
            finished: false,
            failed: false,
            output_end: None,
        };
        // The supplied mature state may have processed another clip. Establish
        // one common origin for both its filter history and our integer clock.
        owner.reset_from(0, 0)?;
        if owner.inner.resample_ratio() != output_rate as f64 / input_rate as f64 {
            return Err("reset kernel ratio differs from the declared rates".into());
        }
        Ok(owner)
    }

    /// Keep cascaded stages on the original clip duration, rather than rounding
    /// each intermediate duration into the next stage's source clock.
    pub fn with_output_end(mut self, frames: u64) -> Self {
        self.output_end = Some(frames);
        self
    }
    /// Bind the original source endpoint before the first EOF operation.
    /// Incremental cascades cannot know this clock until their source closes.
    pub(crate) fn set_output_end(&mut self, frames: u64) -> Result<(), String> {
        if self.eof || self.finished || frames < self.output_frames {
            return Err("resampler endpoint follows EOF or precedes published PCM".into());
        }
        self.output_end = Some(frames);
        Ok(())
    }

    pub fn has_buffered_quantum(&self) -> bool {
        self.input.len() == self.inner.input_frames_next() * self.channels
    }

    pub fn output_capacity(&self) -> usize {
        self.output.len()
    }

    /// Conservative zero-input span for the device-idle candidate. Rubato's
    /// async adapter retains two interpolation windows before each new block.
    pub(crate) fn quiet_input_frames(&self, sinc_points: usize) -> Result<usize, String> {
        sinc_points
            .checked_mul(2)
            .and_then(|history| history.checked_add(self.inner.input_frames_max()))
            .and_then(|frames| frames.checked_add(4))
            .ok_or_else(|| "SRC quiet history overflow".into())
    }

    pub fn reset(&mut self) {
        self.inner.reset();
        self.input.clear();
        self.delay = self.inner.output_delay();
        self.input_frames = 0;
        self.output_frames = 0;
        self.discard_until = 0;
        self.eof = false;
        self.finished = false;
        self.failed = false;
    }

    /// Restart on the original output grid. The caller supplies real decoded
    /// filter history from an input origin whose output position is integral.
    /// Warmup advances both absolute clocks but never reaches the consumer.
    pub fn reset_from(&mut self, input_origin: u64, output_start: u64) -> Result<(), String> {
        let scaled = u128::from(input_origin) * u128::from(self.output_rate);
        if !scaled.is_multiple_of(u128::from(self.input_rate)) {
            return Err("resampler origin is not on the output grid".into());
        }
        let origin = u64::try_from(scaled / u128::from(self.input_rate))
            .map_err(|_| "resampler origin overflow")?;
        if output_start < origin {
            return Err("resampler publication precedes its origin".into());
        }
        self.reset();
        self.input_frames = input_origin;
        self.output_frames = origin;
        self.discard_until = output_start;
        Ok(())
    }

    pub fn step(&mut self, input: &[f32], eof: bool, out: &mut Vec<f32>) -> Result<Step, String> {
        if !out.is_empty() {
            self.failed = true;
            out.clear();
            return Err("resampler output must be empty".into());
        }
        self.append_step(input, eof, out)
    }

    /// Append one library quantum into the caller's bounded destination. This
    /// lets the CPU worker group quanta without an additional PCM scratch copy.
    pub fn append_step(
        &mut self,
        input: &[f32],
        eof: bool,
        out: &mut Vec<f32>,
    ) -> Result<Step, String> {
        if self.failed {
            return Err("resampler previously failed".into());
        }
        let result = self.step_inner(input, eof, out);
        if result.is_err() {
            self.failed = true;
            out.clear();
        }
        result
    }

    fn step_inner(&mut self, input: &[f32], eof: bool, out: &mut Vec<f32>) -> Result<Step, String> {
        if out.capacity().saturating_sub(out.len()) < self.output.len() {
            return Err("resampler output has insufficient preallocated space".into());
        }
        if !input.len().is_multiple_of(self.channels)
            || ((eof || self.eof || self.finished) && !input.is_empty())
        {
            return Err("resampler input has partial frames or follows EOF".into());
        }
        if self.finished {
            return Ok(Step {
                consumed: 0,
                progress: Progress::Finished,
            });
        }
        self.eof |= eof;
        let need = self
            .inner
            .input_frames_next()
            .checked_mul(self.channels)
            .ok_or("resampler input size overflow")?;
        if need > self.input.capacity() || self.input.len() > need {
            return Err("resampler input quantum exceeds scratch".into());
        }
        let consumed = input.len().min(need - self.input.len());
        self.input.extend_from_slice(&input[..consumed]);
        self.input_frames = self
            .input_frames
            .checked_add((consumed / self.channels) as u64)
            .ok_or("input duration overflow")?;
        if !self.eof && self.input.len() < need {
            return Ok(Step {
                consumed,
                progress: Progress::NeedInput,
            });
        }
        let target = (u128::from(self.input_frames) * u128::from(self.output_rate))
            .div_ceil(u128::from(self.input_rate));
        let local_target = u64::try_from(target).map_err(|_| "output duration overflow")?;
        let target = if self.eof {
            self.output_end.unwrap_or(local_target)
        } else {
            self.output_end
                .map_or(local_target, |end| end.min(local_target))
        };
        if self.eof && self.output_frames == target {
            self.finished = true;
            return Ok(Step {
                consumed,
                progress: Progress::Finished,
            });
        }
        let input_frames = self.input.len() / self.channels;
        let source = InterleavedSlice::new(&self.input, self.channels, input_frames)
            .map_err(|e| e.to_string())?;
        let output_frames = self.output.len() / self.channels;
        let mut destination =
            InterleavedSlice::new_mut(&mut self.output, self.channels, output_frames)
                .map_err(|e| e.to_string())?;
        let indexing = self.eof.then(|| Indexing::new().partial_len(input_frames));
        #[cfg(test)]
        TRACE.with(|t| {
            let mut t = t.borrow_mut();
            t.calls += 1;
            t.input_samples += input_frames * self.channels;
        });
        let (used, produced) = self
            .inner
            .process_into_buffer(&source, &mut destination, indexing.as_ref())
            .map_err(|e| e.to_string())?;
        // Rubato reports the logical block consumption, including implicit
        // zero padding at EOF; only accepted input samples count as audio.
        if used != need / self.channels || produced > output_frames {
            return Err("resampler returned inconsistent frame counts".into());
        }
        self.input.clear();
        #[cfg(test)]
        TRACE.with(|t| t.borrow_mut().output_samples += produced * self.channels);
        let skip = self.delay.min(produced);
        self.delay -= skip;
        let available = produced - skip;
        let keep = if self.eof {
            available.min((target - self.output_frames) as usize)
        } else {
            self.output_end.map_or(available, |end| {
                available.min(end.saturating_sub(self.output_frames) as usize)
            })
        };
        let discard = self
            .discard_until
            .saturating_sub(self.output_frames)
            .min(keep as u64) as usize;
        out.extend_from_slice(
            &self.output[(skip + discard) * self.channels..(skip + keep) * self.channels],
        );
        self.output_frames = self
            .output_frames
            .checked_add(keep as u64)
            .ok_or("output duration overflow")?;
        if self.output_frames > target {
            return Err("resampler published beyond supplied input duration".into());
        }
        if self.eof && self.output_frames == target {
            self.finished = true;
            Ok(Step {
                consumed,
                progress: Progress::Finished,
            })
        } else {
            Ok(Step {
                consumed,
                progress: if self.eof {
                    Progress::Yield
                } else {
                    Progress::NeedInput
                },
            })
        }
    }
}
