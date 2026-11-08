//! Shared playback-rate conversion. Construction/admission stay on the command
//! path; bounded resets do not allocate. Native storage retires through its Host.
use crate::node_retirement::{Host, Value};
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::{
    ffi::{c_int, c_long, c_void},
    ptr::NonNull,
};

pub(crate) const BLOCK: usize = 128;
/// `SRC_SINC_FASTEST`: band-limited (97 dB SNR, 80% bandwidth) at roughly a
/// ninth of the best-quality kernel's cost; the only converter compiled in.
pub(crate) const CONVERTER: c_int = 2;

pub(crate) fn history_frames() -> usize {
    unsafe { migo_src_history_frames() }
}
#[repr(C)]
struct Data {
    input: *const f32,
    output: *mut f32,
    input_frames: c_long,
    output_frames: c_long,
    used: c_long,
    produced: c_long,
    eof: c_int,
    ratio: f64,
}
unsafe extern "C" {
    fn migo_src_required_bytes(channels: c_int) -> usize;
    fn migo_src_history_frames() -> usize;
    fn src_new(kind: c_int, channels: c_int, error: *mut c_int) -> *mut c_void;
    fn src_delete(state: *mut c_void) -> *mut c_void;
    fn src_reset(state: *mut c_void) -> c_int;
    fn src_set_ratio(state: *mut c_void, ratio: f64) -> c_int;
    fn src_process(state: *mut c_void, data: *mut Data) -> c_int;
}
#[cfg(all(test, migo_native_alloc_probe))]
unsafe extern "C" {
    fn probe_begin();
    fn probe_end();
    fn probe_constructed();
}
#[inline]
fn native<T>(call: impl FnOnce() -> T) -> T {
    #[cfg(all(test, migo_native_alloc_probe))]
    unsafe {
        probe_begin()
    };
    let result = call();
    #[cfg(all(test, migo_native_alloc_probe))]
    unsafe {
        probe_end()
    };
    result
}
pub(crate) struct Converter {
    state: NonNull<c_void>,
    channels: usize,
    read_frame: c_long,
    loop_past: bool,
    stream_warm: (usize, u16),
}
// Each SRC_STATE exclusively owns its filter and PCM buffer. Only immutable
// coefficient/vtable tables are shared. No callback/TLS pointer is retained;
// moving this owner never permits concurrent native calls.
unsafe impl Send for Converter {}
fn failure() -> EngineError {
    EngineError::from_detail(
        ErrorCode::InvalidArgument,
        "native playback-rate conversion failed",
    )
}
impl Converter {
    pub(crate) fn channels(&self) -> usize {
        self.channels
    }

    /// WebAudio retains its f64 compound rate and sub-frame start position.
    /// The InnerAudio thousandth-rate entry points keep their original arithmetic.
    pub(crate) fn reset_fractional(
        &mut self,
        samples: &[f32],
        position: f64,
        looping: Option<(usize, usize)>,
    ) -> EngineResult<()> {
        let frames = samples.len() / self.channels;
        if !position.is_finite()
            || position < 0.0
            || position >= c_long::MAX as f64
            || (looping.is_none() && position > frames as f64)
            || looping.is_some_and(|(start, end)| start >= end || end > frames)
        {
            return Err(failure());
        }
        let frame = position.floor() as usize;
        let phase = position - frame as f64;
        let mut remaining = self.begin_reset(frame, u16::from(phase != 0.0), frame)?;
        let mut discard = [0.; BLOCK * 32];
        while remaining > 0 {
            let n = remaining.min(BLOCK);
            self.render_ratio_available(
                samples,
                1.0,
                looping,
                true,
                &mut discard[..n * self.channels],
            )?;
            remaining -= n;
        }
        if phase != 0.0 {
            self.render_ratio_available(
                samples,
                1.0 / (1.0 + phase),
                looping,
                true,
                &mut discard[..self.channels],
            )?;
        }
        Ok(())
    }

    pub(crate) fn render_precise(
        &mut self,
        samples: &[f32],
        rate: f64,
        looping: Option<(usize, usize)>,
        output: &mut [f32],
    ) -> EngineResult<()> {
        if !(0.5..=2.0).contains(&rate) {
            return Err(failure());
        }
        let frames = self.render_ratio_available(samples, 1.0 / rate, looping, true, output)?;
        if frames * self.channels != output.len() {
            return Err(failure());
        }
        Ok(())
    }
    pub(crate) fn prepare(host: &Host, channels: usize) -> EngineResult<Value<Self>> {
        if !(1..=32).contains(&channels) {
            return Err(failure());
        }
        let bytes = unsafe { migo_src_required_bytes(channels as c_int) };
        let admission = host.reserve_value_with_bytes::<Self>(bytes)?;
        let mut error = 0;
        let state = native(|| unsafe { src_new(CONVERTER, channels as c_int, &mut error) });
        let state = NonNull::new(state).ok_or_else(|| {
            EngineError::from_detail(
                ErrorCode::InputSaturated,
                "native playback-rate allocation failed",
            )
        })?;
        debug_assert_eq!(error, 0);
        #[cfg(all(test, migo_native_alloc_probe))]
        unsafe {
            probe_constructed()
        };
        Ok(admission.finish(Self {
            state,
            channels,
            read_frame: 0,
            loop_past: false,
            stream_warm: (0, 0),
        }))
    }
    /// Rebuild bounded left history after a seek or activation. The final dummy
    /// step establishes the existing thousandth phase, including frame 0 + phase.
    pub(crate) fn reset(
        &mut self,
        samples: &[f32],
        frame: usize,
        phase: u16,
        looping: bool,
        past_frames: usize,
    ) -> EngineResult<()> {
        let warm = self.begin_reset(frame, phase, past_frames)?;
        let mut discard = [0.; BLOCK * 32];
        let mut remaining = warm;
        while remaining > 0 {
            let n = remaining.min(BLOCK);
            self.render(samples, 1000, looping, &mut discard[..n * self.channels])?;
            remaining -= n;
        }
        if phase != 0 {
            self.render(
                samples,
                1000 + u32::from(phase),
                looping,
                &mut discard[..self.channels],
            )?;
        }
        Ok(())
    }
    fn begin_reset(&mut self, frame: usize, phase: u16, past_frames: usize) -> EngineResult<usize> {
        if native(|| unsafe { src_reset(self.state.as_ptr()) }) != 0 {
            return Err(failure());
        }
        let history = history_frames();
        let warm = if phase == 0 {
            past_frames.min(history)
        } else {
            past_frames.saturating_sub(1).min(history)
        };
        self.read_frame = frame as c_long - warm as c_long - c_long::from(phase != 0);
        self.loop_past = past_frames > frame;
        self.stream_warm = (0, 0);
        Ok(warm)
    }
    pub(crate) fn reset_stream(&mut self, frame: usize, phase: u16) -> EngineResult<()> {
        let warm = self.begin_reset(frame, phase, frame)?;
        self.stream_warm = (warm, phase);
        Ok(())
    }
    pub(crate) fn trim_stream_prefix(&mut self, frames: usize) {
        self.read_frame -= frames as c_long;
    }
    /// A live source may stop before the filter has enough right context.
    /// Retain consumed input and partial reset history, without synthesizing EOF.
    pub(crate) fn render_stream(
        &mut self,
        samples: &[f32],
        units: u32,
        complete: bool,
        output: &mut [f32],
    ) -> EngineResult<usize> {
        if self.stream_warm != (0, 0) {
            let mut discard = [0.; BLOCK * 32];
            while self.stream_warm.0 > 0 {
                let n = self.stream_warm.0.min(BLOCK);
                let produced = self.render_available(
                    samples,
                    1000,
                    false,
                    complete,
                    &mut discard[..n * self.channels],
                )?;
                self.stream_warm.0 -= produced;
                if produced < n {
                    return Ok(0);
                }
            }
            let phase = self.stream_warm.1;
            if phase != 0 {
                if self.render_available(
                    samples,
                    1000 + u32::from(phase),
                    false,
                    complete,
                    &mut discard[..self.channels],
                )? == 0
                {
                    return Ok(0);
                }
                self.stream_warm.1 = 0;
            }
        }
        self.render_available(samples, units, false, complete, output)
    }
    /// Native EOF does not define media duration. The player supplies the exact
    /// integer-clock output extent; bounded zeros provide filter lookahead only.
    pub(crate) fn render(
        &mut self,
        samples: &[f32],
        units: u32,
        looping: bool,
        output: &mut [f32],
    ) -> EngineResult<()> {
        let frames = self.render_available(samples, units, looping, true, output)?;
        if frames * self.channels != output.len() {
            return Err(failure());
        }
        Ok(())
    }
    fn render_available(
        &mut self,
        samples: &[f32],
        units: u32,
        looping: bool,
        complete: bool,
        output: &mut [f32],
    ) -> EngineResult<usize> {
        debug_assert!((500..=2000).contains(&units));
        self.render_ratio_available(
            samples,
            1000.0 / f64::from(units),
            looping.then_some((0, samples.len() / self.channels)),
            complete,
            output,
        )
    }

    fn render_ratio_available(
        &mut self,
        samples: &[f32],
        ratio: f64,
        looping: Option<(usize, usize)>,
        complete: bool,
        output: &mut [f32],
    ) -> EngineResult<usize> {
        debug_assert!(output.len() <= BLOCK * self.channels);
        let mut input_scratch = [0.; BLOCK * 32];
        let frames = samples.len() / self.channels;
        if frames == 0 && complete {
            return Err(failure());
        }
        let (loop_start, loop_end) = looping.unwrap_or((0, frames));
        if looping.is_some() && (loop_start >= loop_end || loop_end > frames) {
            return Err(failure());
        }
        let mut written = 0;
        if native(|| unsafe { src_set_ratio(self.state.as_ptr(), ratio) }) != 0 {
            return Err(failure());
        }
        while written < output.len() {
            let input = if self.read_frame < 0 && !self.loop_past {
                let n = (-self.read_frame as usize).min(BLOCK) * self.channels;
                input_scratch[..n].fill(0.);
                &input_scratch[..n]
            } else if looping.is_some() || self.read_frame < 0 {
                let start = if self.read_frame < 0 || self.read_frame >= loop_end as c_long {
                    loop_start
                        + (self.read_frame - loop_start as c_long)
                            .rem_euclid((loop_end - loop_start) as c_long)
                            as usize
                } else {
                    self.read_frame as usize
                };
                let count = if self.read_frame < 0 {
                    (-self.read_frame as usize).min(BLOCK)
                } else {
                    BLOCK
                };
                if start + count <= loop_end {
                    &samples[start * self.channels..(start + count) * self.channels]
                } else {
                    // Coalesce short periods in the existing bounded scratch;
                    // the native call still receives at most 128 source frames.
                    let mut copied = 0;
                    let mut offset = start;
                    while copied < count {
                        let n = (loop_end - offset).min(count - copied);
                        input_scratch[copied * self.channels..(copied + n) * self.channels]
                            .copy_from_slice(
                                &samples[offset * self.channels..(offset + n) * self.channels],
                            );
                        copied += n;
                        offset = loop_start;
                    }
                    &input_scratch[..count * self.channels]
                }
            } else if (self.read_frame as usize) < frames {
                let start = self.read_frame as usize;
                &samples[start * self.channels..(start + BLOCK).min(frames) * self.channels]
            } else if complete {
                input_scratch[..BLOCK * self.channels].fill(0.);
                &input_scratch[..BLOCK * self.channels]
            } else {
                &samples[..0]
            };
            let mut data = Data {
                input: input.as_ptr(),
                output: output[written..].as_mut_ptr(),
                input_frames: (input.len() / self.channels) as c_long,
                output_frames: ((output.len() - written) / self.channels) as c_long,
                used: 0,
                produced: 0,
                eof: 0,
                ratio,
            };
            let code = native(|| unsafe { src_process(self.state.as_ptr(), &mut data) });
            if code != 0
                || data.used < 0
                || data.used > data.input_frames
                || data.produced < 0
                || data.produced > data.output_frames
            {
                return Err(failure());
            }
            if data.used + data.produced == 0 {
                if complete {
                    return Err(failure());
                }
                break;
            }
            self.read_frame += data.used;
            if looping.is_some() && self.read_frame >= loop_end as c_long {
                self.read_frame = loop_start as c_long
                    + (self.read_frame - loop_start as c_long) % (loop_end - loop_start) as c_long;
            }
            written += data.produced as usize * self.channels;
        }
        Ok(written / self.channels)
    }
}
impl Drop for Converter {
    fn drop(&mut self) {
        unsafe {
            src_delete(self.state.as_ptr());
        }
    }
}
