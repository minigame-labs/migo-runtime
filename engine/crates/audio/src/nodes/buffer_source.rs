use std::any::Any;
use std::sync::Arc;

use shared::audio_resources::{AudioBufferFormat, AudioSnapshot};
use shared::protocol::audio_cmd::AudioNodeId;

use crate::limits::RetainedAudio;
use crate::param::AudioParamTimeline;

use super::AudioNodeProcessor;

/// State of an AudioBufferSourceNode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferSourceState {
    /// Not yet started (start() not called)
    Pending,
    /// start() called but `when` is in the future
    Scheduled,
    /// Currently playing
    Playing,
    /// Finished playback
    Finished,
}

/// A source may still be backed by the legacy decoded-buffer registry, or by
/// the JS-owned snapshot path. Both variants retain the original Arc and
/// expose the same interleaved sample view; no PCM copy is made on binding.
enum SourceBuffer {
    Legacy(Arc<RetainedAudio>),
    Snapshot(Arc<AudioSnapshot>),
}

impl Clone for SourceBuffer {
    fn clone(&self) -> Self {
        match self {
            Self::Legacy(buffer) => Self::Legacy(Arc::clone(buffer)),
            Self::Snapshot(buffer) => Self::Snapshot(Arc::clone(buffer)),
        }
    }
}

impl SourceBuffer {
    #[inline]
    fn format(&self) -> AudioBufferFormat {
        match self {
            Self::Legacy(buffer) => AudioBufferFormat {
                channels: buffer.channels,
                frames: buffer.frame_count() as u32,
                sample_rate: buffer.sample_rate,
            },
            Self::Snapshot(buffer) => buffer.format(),
        }
    }

    #[inline]
    fn samples(&self) -> &[f32] {
        match self {
            Self::Legacy(buffer) => &buffer.samples,
            Self::Snapshot(buffer) => buffer.samples(),
        }
    }
}

pub struct BufferSourceNode {
    id: AudioNodeId,
    buffer: Option<SourceBuffer>,
    context_sample_rate: f64,
    state: BufferSourceState,

    // Playback position, in **buffer sample frames**. Fractional: the read is
    // interpolated, so a non-integral playbackRate does not quantise.
    position: f64,

    // Buffer frames consumed since `start()`, never wrapped by looping. `duration`
    // is measured against this, per the spec's `bufferTimeElapsed`.
    elapsed_frames: f64,

    // Whether `position` has been seeded from `start_offset`. Deferred because
    // `start_offset` is in seconds and the conversion needs the buffer's own
    // sample rate, which is not known until a buffer is bound.
    offset_applied: bool,

    // Start parameters
    start_when: f64,
    start_offset: f64,
    duration: Option<f64>,

    // Loop parameters
    loop_enabled: bool,
    loop_start: f64,
    loop_end: f64,

    // AudioParam for playback rate
    playback_rate: AudioParamTimeline,

    // Detune AudioParam (in cents)
    detune: AudioParamTimeline,

    // Context time when started (set when start() is called)
    start_time: Option<f64>,

    // Scheduled stop time (None = no stop scheduled)
    stop_when: Option<f64>,
}

impl BufferSourceNode {
    pub fn new(id: AudioNodeId, context_sample_rate: u32) -> Self {
        Self {
            id,
            buffer: None,
            context_sample_rate: context_sample_rate.max(1) as f64,
            state: BufferSourceState::Pending,
            position: 0.0,
            elapsed_frames: 0.0,
            offset_applied: false,
            start_when: 0.0,
            start_offset: 0.0,
            duration: None,
            loop_enabled: false,
            loop_start: 0.0,
            loop_end: 0.0,
            playback_rate: AudioParamTimeline::new(1.0, -3.4028235e38, 3.4028235e38),
            detune: AudioParamTimeline::new(0.0, -3.4028235e38, 3.4028235e38),
            start_time: None,
            stop_when: None,
        }
    }

    pub fn set_buffer(&mut self, buffer: Option<Arc<RetainedAudio>>) {
        self.buffer = buffer.map(SourceBuffer::Legacy);
    }

    pub fn set_snapshot(&mut self, buffer: Option<Arc<AudioSnapshot>>) {
        self.buffer = buffer.map(SourceBuffer::Snapshot);
    }

    pub(crate) fn retire_buffer(
        &mut self,
        host: Option<&crate::node_retirement::Host>,
    ) -> shared::error::EngineResult<()> {
        crate::node_retirement::retire_replacement(&mut self.buffer, host, 0)
    }

    pub fn start(&mut self, when: f64, offset: f64, duration: Option<f64>, current_time: f64) {
        if self.state != BufferSourceState::Pending {
            return; // Can only start once per W3C spec
        }

        self.start_when = when;
        self.start_offset = offset;
        self.duration = duration;
        // `offset` is in **seconds** (Web Audio). Converting it needs the buffer's
        // sample rate, and a buffer may be bound after `start()`, so the seek is
        // deferred to the first render instead of being written into `position` --
        // which is a frame index, and used to receive the raw seconds value.
        self.position = 0.0;
        self.elapsed_frames = 0.0;
        self.offset_applied = false;

        // If scheduled time is now or in the past, start immediately
        if current_time >= when {
            self.start_time = Some(current_time);
            self.state = BufferSourceState::Playing;
        } else {
            // Schedule for future — will transition to Playing in process()
            self.start_time = Some(when);
            self.state = BufferSourceState::Scheduled;
        }
    }

    pub fn stop(&mut self, when: f64) {
        if self.state == BufferSourceState::Finished {
            return;
        }
        // Per W3C spec: stop at the given time, or immediately if when <= 0
        if when <= 0.0 {
            self.state = BufferSourceState::Finished;
        } else {
            self.stop_when = Some(when);
        }
    }

    pub fn set_loop(&mut self, enabled: bool, start: f64, end: f64) {
        self.loop_enabled = enabled;
        self.loop_start = start;
        self.loop_end = end;
    }

    /// Frame index at which playback wraps: the loop end (clamped to the buffer)
    /// when looping with a valid `loopEnd`, otherwise the end of the buffer. A
    /// `loopEnd` of 0 or <= `loopStart` means "use the whole buffer" (Web Audio).
    #[inline]
    fn loop_wrap_frame(&self, buffer_frames: usize, sample_rate: f64) -> usize {
        if self.loop_enabled && self.loop_end > self.loop_start && self.loop_end > 0.0 {
            ((self.loop_end * sample_rate) as usize).min(buffer_frames)
        } else {
            buffer_frames
        }
    }

    /// Frame index to jump back to when looping. Clamps `loopStart` to a valid
    /// frame; an out-of-range `loopStart` restarts at 0 (full-buffer loop) rather
    /// than jumping past the buffer and looping silence.
    #[inline]
    fn loop_restart_pos(&self, buffer_frames: usize, sample_rate: f64) -> f64 {
        let start_frame = (self.loop_start.max(0.0) * sample_rate).floor();
        if start_frame >= buffer_frames as f64 {
            0.0
        } else {
            start_frame
        }
    }

    pub fn set_playback_rate(&mut self, rate: f32) {
        self.playback_rate.set_value(rate.max(0.0));
    }

    /// Compute the source-frame increment once per render block. Web Audio's
    /// intrinsic rate is playbackRate × detune pitch ratio, then converted
    /// from buffer frames to context frames for buffers at another rate.
    #[inline]
    fn block_playback_rate(&self, current_time: f64, buffer_sample_rate: f64) -> f64 {
        let playback_rate = self.playback_rate.compute_value(current_time).max(0.0) as f64;
        let detune = self.detune.compute_value(current_time) as f64;
        playback_rate
            * (2.0f64).powf(detune / 1200.0)
            * (buffer_sample_rate / self.context_sample_rate)
    }

    #[allow(dead_code)]
    pub fn state(&self) -> BufferSourceState {
        self.state
    }

    /// Process with explicit output channel count
    pub fn process_with_channels(
        &mut self,
        output: &mut [f32],
        output_channels: u32,
        current_time: f64,
    ) -> usize {
        let channels = output_channels.max(1) as usize;
        let frames = output.len() / channels;
        let clock =
            crate::render_time::RenderTime::new(current_time, self.context_sample_rate as u32);
        let stop = self.stop_when.unwrap_or(f64::INFINITY);
        let stopped = clock.stopped(stop, frames);
        let active = clock.active(self.start_when, stop, frames);
        output[..frames * channels].fill(0.0);
        if !matches!(
            self.state,
            BufferSourceState::Playing | BufferSourceState::Scheduled
        ) || active.is_empty()
        {
            if stopped && self.state != BufferSourceState::Pending {
                self.state = BufferSourceState::Finished;
            }
            return 0;
        }
        self.state = BufferSourceState::Playing;
        let delay = if self.offset_applied {
            0.0
        } else {
            (clock.at(active.start) - self.start_time.unwrap_or(current_time)).max(0.0)
        };

        let buffer = match &self.buffer {
            Some(b) => b.clone(), // Clone Arc to release borrow
            None => {
                self.state = BufferSourceState::Finished;
                return 0;
            }
        };

        let format = buffer.format();
        let src_channels = format.channels as usize;
        let dst_channels = output_channels as usize;
        let buffer_frames = format.frames as usize;
        let output_frames = active.len();
        let samples = buffer.samples();
        let sample_rate = format.sample_rate as f64;

        let rendered = self.render(
            &mut output[active.start * channels..active.end * channels],
            samples,
            buffer_frames,
            output_frames,
            src_channels,
            dst_channels,
            sample_rate,
            current_time,
            delay,
        );
        if stopped {
            self.state = BufferSourceState::Finished;
        }
        rendered
    }

    /// Render one block from the bound buffer.
    ///
    /// One loop rather than a mono/stereo/generic trio. Each of the three copies
    /// carried its own transcription of the loop-wrap, `duration` and
    /// end-of-buffer rules -- one rule written three times, and three places for
    /// it to drift. The channel mapping was the only part that actually differed,
    /// and it is two compares per frame against interpolation and two multiplies.
    #[allow(clippy::too_many_arguments)]
    fn render(
        &mut self,
        output: &mut [f32],
        samples: &[f32],
        buffer_frames: usize,
        output_frames: usize,
        src_channels: usize,
        dst_channels: usize,
        sample_rate: f64,
        current_time: f64,
        start_delay: f64,
    ) -> usize {
        if src_channels == 0 || dst_channels == 0 {
            self.state = BufferSourceState::Finished;
            return 0;
        }
        // Trust the declared frame count no further than the PCM actually backing
        // it: a snapshot's format crosses from JS. Clamping once here is what lets
        // the render loop below index without a per-sample bounds check.
        let buffer_frames = buffer_frames.min(samples.len() / src_channels);
        if buffer_frames == 0 {
            self.state = BufferSourceState::Finished;
            return 0;
        }

        // `start(when, offset)` takes `offset` in **seconds**; `position` is a
        // frame index. Seeding it here rather than in `start()` is what makes the
        // conversion possible at all -- the buffer, and so its sample rate, may be
        // bound after `start()` was called.
        let rate = self.block_playback_rate(current_time, sample_rate);
        if !self.offset_applied {
            self.offset_applied = true;
            self.position = self.start_offset.max(0.0) * sample_rate;
            let advance = start_delay * rate * self.context_sample_rate;
            self.position += advance;
            self.elapsed_frames += advance;
            if self.position >= buffer_frames as f64 && !self.loop_enabled {
                self.state = BufferSourceState::Finished;
                return 0;
            }
        }

        let wrap_frame = self.loop_wrap_frame(buffer_frames, sample_rate);
        // Clamped below the wrap so the loop length is always at least one frame;
        // a `loopStart` past `loopEnd` would otherwise wrap on every frame.
        let restart = self
            .loop_restart_pos(buffer_frames, sample_rate)
            .min((wrap_frame as f64 - 1.0).max(0.0));
        let loop_restart_frame = self.loop_enabled.then_some(restart as usize);
        // `duration` is buffer time, so it is measured against frames consumed --
        // not against the wrapped read position, which looping resets.
        let duration_frames = self.duration.map(|d| d.max(0.0) * sample_rate);
        let mut frames_written = 0;

        for frame_idx in 0..output_frames {
            if self.position >= wrap_frame as f64 {
                if !self.loop_enabled {
                    self.state = BufferSourceState::Finished;
                    break;
                }
                // Carry the overshoot so a fractional or high rate keeps loop
                // timing, and render the restart frame in this same iteration --
                // skipping it would drop one output frame per wrap.
                let loop_len = (wrap_frame as f64 - restart).max(1.0);
                let overshoot = self.position - wrap_frame as f64;
                self.position = restart + overshoot.rem_euclid(loop_len);
            }

            if duration_frames.is_some_and(|limit| self.elapsed_frames >= limit) {
                self.state = BufferSourceState::Finished;
                break;
            }

            self.write_interpolated_frame(
                output,
                samples,
                buffer_frames,
                loop_restart_frame,
                frame_idx,
                src_channels,
                dst_channels,
            );

            self.position += rate;
            self.elapsed_frames += rate;
            frames_written += 1;
        }

        // The next read position is already known. Finish in the block that
        // consumed the last sample, including exact block-boundary endings.
        // Looping keeps the read position alive, but not a consumed duration.
        if (!self.loop_enabled && self.position >= buffer_frames as f64)
            || duration_frames.is_some_and(|limit| self.elapsed_frames >= limit)
        {
            self.state = BufferSourceState::Finished;
        }

        frames_written
    }

    /// Read the source at the current fractional frame and write it to output
    /// frame `frame_idx`, up- or down-mixing to `dst_channels`.
    ///
    /// **Interpolating rather than truncating is the point.** `position as usize`
    /// turns any `playbackRate` or `detune` other than unity into a zero-order
    /// hold, whose error is a sawtooth at the resampling rate -- broadband
    /// aliasing, heard as grit on exactly the pitch-varied one-shots games use
    /// most.
    #[allow(clippy::too_many_arguments)]
    #[inline]
    fn write_interpolated_frame(
        &self,
        output: &mut [f32],
        samples: &[f32],
        buffer_frames: usize,
        loop_restart_frame: Option<usize>,
        frame_idx: usize,
        src_channels: usize,
        dst_channels: usize,
    ) {
        let frame0 = self.position as usize;
        let frac = (self.position - frame0 as f64) as f32;
        // Past the last frame the successor is the loop restart, so the seam
        // interpolates across the wrap instead of fading into the final sample.
        // A one-shot holds, which contributes nothing: it is only ever reached on
        // the last frame, where `frac` is what the caller already advanced past.
        let frame1 = match loop_restart_frame {
            _ if frame0 + 1 < buffer_frames => frame0 + 1,
            Some(restart) => restart,
            None => frame0,
        };

        let base0 = frame0 * src_channels;
        let base1 = frame1 * src_channels;
        let dst = frame_idx * dst_channels;
        let at = |ch: usize| -> f32 {
            let a = samples[base0 + ch];
            let b = samples[base1 + ch];
            a + (b - a) * frac
        };

        if src_channels == 1 {
            // Mono up-mixes by duplication, per the spec's channel-up-mixing rules.
            let sample = at(0);
            output[dst..dst + dst_channels].fill(sample);
        } else {
            let shared = dst_channels.min(src_channels);
            for ch in 0..shared {
                output[dst + ch] = at(ch);
            }
            // Discrete up-mix: extra output channels are silent. Repeating the
            // last source channel (what the old generic path did) decorrelates
            // nothing and only raises the level.
            output[dst + shared..dst + dst_channels].fill(0.0);
        }
    }
}

impl AudioNodeProcessor for BufferSourceNode {
    fn id(&self) -> AudioNodeId {
        self.id
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn process(
        &mut self,
        _inputs: &[f32],
        output: &mut [f32],
        _sample_rate: u32,
        channels: u32,
        current_time: f64,
    ) -> usize {
        // Source node: ignore inputs, generate from buffer
        self.process_with_channels(output, channels, current_time)
    }

    fn is_finished(&self) -> bool {
        self.state == BufferSourceState::Finished
    }

    fn is_producing(&self) -> bool {
        matches!(
            self.state,
            BufferSourceState::Scheduled | BufferSourceState::Playing
        )
    }

    fn output_channels(&self) -> u32 {
        self.buffer
            .as_ref()
            .map(|buffer| buffer.format().channels)
            .unwrap_or(2)
    }

    fn prepare_params(
        &mut self,
        host: &crate::node_retirement::Host,
    ) -> shared::error::EngineResult<()> {
        for param in [&mut self.playback_rate, &mut self.detune] {
            param.prepare_storage(host)?;
        }
        Ok(())
    }

    fn get_param_mut(&mut self, name: &str) -> Option<&mut AudioParamTimeline> {
        match name {
            "playbackRate" => Some(&mut self.playback_rate),
            "detune" => Some(&mut self.detune),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decoder::DecodedAudio;
    use crate::limits::PcmBudget;

    /// A mono ramp, so an output sample reads back as the source frame index it
    /// came from and any seek or rate error is the numeric difference.
    fn ramp_node(frames: usize, sample_rate: u32) -> BufferSourceNode {
        let audio = DecodedAudio {
            samples: (0..frames).map(|i| i as f32).collect(),
            sample_rate,
            channels: 1,
        };
        let retained = RetainedAudio::try_new(audio, &PcmBudget::for_context()).unwrap();
        let mut node = BufferSourceNode::new(1, sample_rate);
        node.set_buffer(Some(Arc::new(retained)));
        node
    }

    /// `start(when, offset)` takes `offset` in seconds. It used to be assigned
    /// straight into the frame-indexed read position, so `start(0, 0.5)` on a
    /// 48 kHz buffer began at frame 0 instead of frame 24000 -- a request to skip
    /// half a second played the file from the top.
    #[test]
    fn start_offset_is_seconds_not_frames() {
        let mut node = ramp_node(48_000, 48_000);
        node.start(0.0, 0.5, None, 0.0);

        let mut out = [0.0f32; 4];
        assert_eq!(node.process(&[], &mut out, 48_000, 1, 0.0), 4);
        assert_eq!(out, [24_000.0, 24_001.0, 24_002.0, 24_003.0]);
    }

    #[test]
    fn an_offset_past_the_end_of_a_one_shot_finishes_instead_of_reading_frame_zero() {
        let mut node = ramp_node(1_000, 48_000);
        node.start(0.0, 5.0, None, 0.0);

        let mut out = [0.0f32; 4];
        assert_eq!(node.process(&[], &mut out, 48_000, 1, 0.0), 0);
        assert!(node.is_finished());
        assert_eq!(out, [0.0; 4], "nothing may be written past the buffer");
    }

    /// A fractional read position must interpolate. Truncating it is a
    /// zero-order hold, and on a ramp that shows up as repeated samples where
    /// the true signal is a straight line.
    #[test]
    fn a_fractional_playback_rate_interpolates_between_source_frames() {
        let mut node = ramp_node(64, 48_000);
        node.set_playback_rate(0.5);
        node.start(0.0, 0.0, None, 0.0);

        let mut out = [0.0f32; 6];
        assert_eq!(node.process(&[], &mut out, 48_000, 1, 0.0), 6);
        assert_eq!(out, [0.0, 0.5, 1.0, 1.5, 2.0, 2.5]);
    }

    /// `duration` is buffer time, so a looping source must still stop after
    /// `duration` seconds of source consumed -- it cannot be derived from the
    /// read position, which looping resets.
    #[test]
    fn duration_is_measured_in_consumed_buffer_time_across_a_loop() {
        let sample_rate = 48_000;
        let mut node = ramp_node(4, sample_rate);
        node.set_loop(true, 0.0, 0.0);
        // 10 frames of buffer time at unity rate, over a 4-frame buffer.
        node.start(0.0, 0.0, Some(10.0 / sample_rate as f64), 0.0);

        let mut out = [-1.0f32; 16];
        let written = node.process(&[], &mut out, sample_rate, 1, 0.0);

        assert_eq!(written, 10, "must stop after 10 consumed buffer frames");
        assert!(node.is_finished());
        assert_eq!(
            &out[..8],
            &[0.0, 1.0, 2.0, 3.0, 0.0, 1.0, 2.0, 3.0],
            "the loop must wrap without dropping or repeating a frame"
        );
    }

    #[test]
    fn block_rate_includes_detune_and_sample_rate_conversion() {
        let mut node = BufferSourceNode::new(1, 48_000);
        node.set_playback_rate(2.0);
        node.detune.set_value(1_200.0);

        let actual = node.block_playback_rate(0.0, 44_100.0);
        let expected = 2.0 * 2.0 * 44_100.0 / 48_000.0;
        assert!((actual - expected).abs() < f64::EPSILON * 32.0);
    }
}
