//! Bounded, persistent sinc cascade for known or incrementally supplied input.
//! Codec owners retain unconsumed suffixes; one call runs at most one kernel.
use crate::resample_job::bandlimited;
use rubato::Resampler;
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) const STEP_SAMPLES: usize = 8192;
const QUANTUM: usize = 128;

struct Layer {
    filter: bandlimited::Stream<rubato::Async<f32>>,
    pcm: Vec<f32>,
    head: usize,
    done: bool,
}

// Cascading the same library filter keeps each downsampling ratio >= 1/4.
// A fixed 256-tap single stage cannot reject aliases at ratios such as 1/256.
pub(crate) fn rates(input: u32, output: u32) -> Vec<u32> {
    let mut result = Vec::new();
    let mut current = input;
    while u64::from(current) > 4 * u64::from(output) {
        current = current.div_ceil(4);
        result.push(current);
    }
    result.push(output);
    result
}
pub(crate) fn kernel(input: u32, output: u32, channels: u32) -> EngineResult<rubato::Async<f32>> {
    let ratio = f64::from(output) / f64::from(input);
    let max_input =
        (((STEP_SAMPLES / channels as usize).saturating_sub(10)) as f64 / ratio).floor() as usize;
    let (fixed, quantum) = if max_input > 0 {
        (rubato::FixedAsync::Input, max_input.min(QUANTUM))
    } else {
        (rubato::FixedAsync::Output, STEP_SAMPLES / channels as usize)
    };
    let core = rubato::Async::new_sinc(
        ratio,
        1.0,
        &rubato::SincInterpolationParameters::default(),
        quantum,
        channels as usize,
        fixed,
    )
    .map_err(|e| invalid(e.to_string()))?;
    if core.input_frames_max() * channels as usize > STEP_SAMPLES
        || core.output_frames_max() * channels as usize > STEP_SAMPLES
    {
        return Err(invalid("SRC kernel exceeds its working-set budget"));
    }
    Ok(core)
}

/// Input and output clocks count frames, while `Step::consumed` counts samples.
/// EOF is a separate empty-input call after all offered suffixes were accepted.
/// The output appends into caller-owned capacity; it never grows that capacity.
pub(crate) struct Stream {
    source: u32,
    target: u32,
    channels: usize,
    pub(crate) rates: Vec<u32>,
    layers: Vec<Layer>,
    input_origin: u64,
    output_start: u64,
    input_frames: u64,
    output_frames: u64,
    source_end: Option<u64>,
    eof: bool,
    finished: bool,
    error: Option<ErrorCode>,
}
impl Stream {
    pub(crate) fn new(
        source: u32,
        target: u32,
        channels: u32,
        source_end: Option<u64>,
    ) -> EngineResult<Self> {
        Self::new_at(source, target, channels, source_end, 0, 0)
    }
    /// Cold stream on the original track's integer clock. The caller supplies
    /// real decoded history from input_origin; only the final layer discards
    /// preroll before output_start. No kernel or interpolation math changes.
    pub(crate) fn new_at(
        source: u32,
        target: u32,
        channels: u32,
        source_end: Option<u64>,
        input_origin: u64,
        output_start: u64,
    ) -> EngineResult<Self> {
        bandlimited::validate_format(source, target, channels).map_err(invalid)?;
        let rates = if source == target {
            Vec::new()
        } else {
            rates(source, target)
        };
        let mut origin = input_origin;
        for rate in &rates {
            origin = map_origin(input_origin, source, *rate)?;
        }
        if output_start < origin || source_end.is_some_and(|end| input_origin > end) {
            return Err(invalid("SRC publication precedes its origin or source end"));
        }
        if (input_origin != 0 || output_start != 0)
            && source_end.is_some_and(|end| {
                u128::from(output_start)
                    > (u128::from(end) * u128::from(target)).div_ceil(u128::from(source))
            })
        {
            return Err(invalid("SRC publication exceeds its source endpoint"));
        }
        let layers = Vec::with_capacity(rates.len());
        Ok(Self {
            source,
            target,
            channels: channels as usize,
            rates,
            layers,
            input_origin,
            output_start,
            input_frames: input_origin,
            output_frames: output_start,
            source_end,
            eof: false,
            finished: false,
            error: None,
        })
    }
    pub(crate) fn is_preparing(&self) -> bool {
        self.layers.len() < self.rates.len()
    }
    pub(crate) fn grid_period(&self) -> u64 {
        let mut common = self.source;
        for rate in &self.rates {
            let (mut a, mut b) = (common, *rate);
            while b != 0 {
                (a, b) = (b, a % b);
            }
            common = a;
        }
        u64::from(self.source / common)
    }
    #[cfg(test)]
    pub(crate) fn prepared_layers(&self) -> usize {
        self.layers.len()
    }
    /// Device-idle candidate only. Map the prepared stages' finite history and
    /// block bounds back to the original source clock; no extra filter is built.
    pub(crate) fn quiet_source_frames(&self) -> EngineResult<u64> {
        if self.is_preparing() || self.eof || self.finished || self.error.is_some() {
            return Err(invalid("quiet bound requires a prepared live SRC"));
        }
        // Match upstream make_interpolator's rounding, using the same default
        // parameters as kernel(). This is a resource bound, not filter math.
        let points = rubato::SincInterpolationParameters::default()
            .sinc_len
            .next_multiple_of(8);
        let mut source_rate = self.source;
        let mut bound = 0u128;
        for (index, layer) in self.layers.iter().enumerate() {
            let frames = layer.filter.quiet_input_frames(points).map_err(invalid)?;
            bound += ((frames as u128) * u128::from(self.source)).div_ceil(u128::from(source_rate));
            source_rate = self.rates[index];
        }
        u64::try_from(bound).map_err(|_| invalid("SRC quiet source clock overflow"))
    }
    fn endpoint(&self, rate: u32) -> EngineResult<Option<u64>> {
        self.source_end
            .map(|frames| {
                u64::try_from(
                    (u128::from(frames) * u128::from(rate)).div_ceil(u128::from(self.source)),
                )
                .map_err(|_| invalid("SRC output duration overflow"))
            })
            .transpose()
    }
    fn retire(&mut self, code: ErrorCode) {
        self.layers = Vec::new();
        self.rates = Vec::new();
        self.error = Some(code);
    }
    pub(crate) fn step(
        &mut self,
        input: &[f32],
        eof: bool,
        out: &mut Vec<f32>,
        cancel: &AtomicBool,
    ) -> EngineResult<bandlimited::Step> {
        if cancel.load(Ordering::Acquire) {
            self.retire(ErrorCode::Cancelled);
        }
        let result = if let Some(code) = self.error {
            Err(EngineError::from_detail(code, "streaming SRC retired"))
        } else {
            self.advance(input, eof, out)
        };
        if let Err(error) = &result {
            self.retire(error.code);
            out.clear();
        }
        result
    }
    fn advance(
        &mut self,
        input: &[f32],
        eof: bool,
        out: &mut Vec<f32>,
    ) -> EngineResult<bandlimited::Step> {
        use bandlimited::{Progress, Step};
        if !input.len().is_multiple_of(self.channels)
            || (eof && !input.is_empty())
            || ((self.eof || self.finished) && !input.is_empty())
        {
            return Err(invalid("SRC requires complete frames and a separate EOF"));
        }
        if self.finished {
            return Ok(Step {
                consumed: 0,
                progress: Progress::Finished,
            });
        }
        if eof && !self.eof {
            if self.source_end.is_some_and(|end| end != self.input_frames) {
                return Err(invalid("SRC source ended before its declared duration"));
            }
            self.source_end = Some(self.input_frames);
            self.eof = true;
            for index in 0..self.layers.len() {
                let end = self.endpoint(self.rates[index])?.unwrap();
                self.layers[index]
                    .filter
                    .set_output_end(end)
                    .map_err(invalid)?;
            }
        }
        if self.is_preparing() {
            let index = self.layers.len();
            let source = if index == 0 {
                self.source
            } else {
                self.rates[index - 1]
            };
            let target = self.rates[index];
            let mut filter = bandlimited::Stream::new(
                kernel(source, target, self.channels as u32)?,
                source,
                target,
                self.channels as u32,
            )
            .map_err(invalid)?;
            if let Some(end) = self.endpoint(target)? {
                filter = filter.with_output_end(end);
            }
            if self.input_origin != 0 || self.output_start != 0 {
                let input_origin = map_origin(self.input_origin, self.source, source)?;
                let output_start = if index + 1 == self.rates.len() {
                    self.output_start
                } else {
                    map_origin(self.input_origin, self.source, target)?
                };
                filter
                    .reset_from(input_origin, output_start)
                    .map_err(invalid)?;
            }
            let pcm = Vec::with_capacity(filter.output_capacity());
            self.layers.push(Layer {
                filter,
                pcm,
                head: 0,
                done: false,
            });
            return Ok(Step {
                consumed: 0,
                progress: Progress::Yield,
            });
        }
        if self.rates.is_empty() {
            let consumed = input
                .len()
                .min((STEP_SAMPLES / self.channels) * self.channels);
            if out.capacity().saturating_sub(out.len()) < consumed {
                return Err(invalid("SRC output has insufficient reserved capacity"));
            }
            let frames = self
                .input_frames
                .checked_add((consumed / self.channels) as u64)
                .ok_or_else(|| invalid("SRC input duration overflow"))?;
            if self.source_end.is_some_and(|end| frames > end) {
                return Err(invalid("SRC input exceeds its declared duration"));
            }
            let discard = self
                .output_start
                .saturating_sub(self.input_frames)
                .min((consumed / self.channels) as u64) as usize
                * self.channels;
            out.extend_from_slice(&input[discard..consumed]);
            self.input_frames = frames;
            self.output_frames = self
                .output_frames
                .checked_add(((consumed - discard) / self.channels) as u64)
                .ok_or_else(|| invalid("SRC output duration overflow"))?;
            self.finished = self.eof;
            if self.finished && self.output_frames != frames {
                return Err(invalid(
                    "SRC publication begins after the actual source endpoint",
                ));
            }
            return Ok(Step {
                consumed,
                progress: if self.finished {
                    Progress::Finished
                } else if consumed < input.len() {
                    Progress::Yield
                } else {
                    Progress::NeedInput
                },
            });
        }
        let index = (0..self.layers.len()).rev().find(|&index| {
            let layer = &self.layers[index];
            if layer.done || !layer.pcm.is_empty() {
                return false;
            }
            let (available, eof) = if index == 0 {
                (!input.is_empty(), self.eof)
            } else {
                let previous = &self.layers[index - 1];
                (previous.head < previous.pcm.len(), previous.done)
            };
            available || eof || layer.filter.has_buffered_quantum()
        });
        let Some(index) = index else {
            if self.eof {
                return Err(invalid("SRC pipeline made no progress at EOF"));
            }
            return Ok(Step {
                consumed: 0,
                progress: Progress::NeedInput,
            });
        };
        let last = self.layers.len() - 1;
        let (previous, current) = self.layers.split_at_mut(index);
        let layer = &mut current[0];
        let (slice, end) = if let Some(prev) = previous.last() {
            (
                &prev.pcm[prev.head..],
                prev.done && prev.head == prev.pcm.len(),
            )
        } else {
            let count = input
                .len()
                .min((STEP_SAMPLES / self.channels) * self.channels);
            (&input[..count], self.eof)
        };
        let step = layer
            .filter
            .step(slice, end, &mut layer.pcm)
            .map_err(invalid)?;
        if step.consumed > slice.len() || layer.pcm.len() > STEP_SAMPLES {
            return Err(invalid("SRC operation exceeded its sample budget"));
        }
        let consumed = if let Some(prev) = previous.last_mut() {
            prev.head += step.consumed;
            if prev.head == prev.pcm.len() {
                prev.head = 0;
                prev.pcm.clear();
            }
            0
        } else {
            step.consumed
        };
        self.input_frames = self
            .input_frames
            .checked_add((consumed / self.channels) as u64)
            .ok_or_else(|| invalid("SRC input duration overflow"))?;
        if self.source_end.is_some_and(|end| self.input_frames > end) {
            return Err(invalid("SRC input exceeds its declared duration"));
        }
        layer.done = matches!(step.progress, Progress::Finished);
        let mut done = false;
        if index == last {
            if out.capacity().saturating_sub(out.len()) < layer.pcm.len() {
                return Err(invalid("SRC output has insufficient reserved capacity"));
            }
            self.output_frames = self
                .output_frames
                .checked_add((layer.pcm.len() / self.channels) as u64)
                .ok_or_else(|| invalid("SRC output duration overflow"))?;
            out.extend_from_slice(&layer.pcm);
            layer.pcm.clear();
            done = layer.done;
        }
        if done {
            let expected = self
                .endpoint(self.target)?
                .ok_or_else(|| invalid("SRC finished before source EOF"))?;
            if !self.eof || self.output_frames != expected {
                return Err(invalid("SRC did not finish the original source duration"));
            }
            self.finished = true;
            self.layers = Vec::new();
            self.rates = Vec::new();
        }
        Ok(Step {
            consumed,
            progress: if done {
                Progress::Finished
            } else {
                Progress::Yield
            },
        })
    }
}
fn map_origin(origin: u64, source: u32, target: u32) -> EngineResult<u64> {
    let scaled = u128::from(origin) * u128::from(target);
    if !scaled.is_multiple_of(u128::from(source)) {
        return Err(invalid("SRC origin is not on every cascade output grid"));
    }
    u64::try_from(scaled / u128::from(source)).map_err(|_| invalid("SRC absolute origin overflow"))
}
fn invalid(detail: impl Into<String>) -> EngineError {
    EngineError::from_detail(ErrorCode::InvalidArgument, detail.into())
}

#[cfg(test)]
#[path = "streaming_sinc_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "streaming_sinc_origin_tests.rs"]
mod origin_tests;
