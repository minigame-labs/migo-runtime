/// Represents a scheduled automation event on an AudioParam timeline.
#[derive(Debug, Clone)]
enum AutomationEvent {
    /// Set the value at a specific time
    SetValue { value: f32, time: f64 },
    /// Linear ramp to a value ending at a specific time
    LinearRamp { value: f32, end_time: f64 },
    /// Exponential ramp to a value ending at a specific time
    ExponentialRamp { value: f32, end_time: f64 },
    /// Asymptotically approach a target value starting at a specific time
    SetTarget {
        target: f32,
        start_time: f64,
        time_constant: f64,
    },
    /// Cancel all scheduled events after a specific time
    #[allow(dead_code)]
    CancelScheduled { cancel_time: f64 },
}

impl AutomationEvent {
    /// The value the timeline holds once this event is behind us.
    ///
    /// `SetTarget` is asymptotic, so its nominal endpoint is its target: that is
    /// what a following ramp has to interpolate from.
    fn end_value(&self) -> Option<f32> {
        match self {
            Self::SetValue { value, .. }
            | Self::LinearRamp { value, .. }
            | Self::ExponentialRamp { value, .. } => Some(*value),
            Self::SetTarget { target, .. } => Some(*target),
            Self::CancelScheduled { .. } => None,
        }
    }

    fn time(&self) -> f64 {
        match self {
            Self::SetValue { time, .. } => *time,
            Self::LinearRamp { end_time, .. } => *end_time,
            Self::ExponentialRamp { end_time, .. } => *end_time,
            Self::SetTarget { start_time, .. } => *start_time,
            Self::CancelScheduled { cancel_time } => *cancel_time,
        }
    }
}

/// An audio parameter with support for scheduled value changes (automation).
///
/// Implements the W3C AudioParam specification for sample-accurate automation.
/// Events are stored in a sorted timeline and evaluated during audio processing.
#[derive(Debug, Clone)]
pub struct AudioParamTimeline {
    current_value: f32,
    default_value: f32,
    min_value: f32,
    max_value: f32,
    events: Vec<AutomationEvent>,
}

impl AudioParamTimeline {
    pub fn new(default_value: f32, min_value: f32, max_value: f32) -> Self {
        Self {
            current_value: default_value,
            default_value,
            min_value,
            max_value,
            events: Vec::new(),
        }
    }

    /// Get the current static value (without automation)
    #[inline]
    pub fn value(&self) -> f32 {
        self.current_value
    }

    /// Set the current value directly (equivalent to setting .value property)
    #[inline]
    pub fn set_value(&mut self, value: f32) {
        self.current_value = value.clamp(self.min_value, self.max_value);
    }

    /// Set the current value now, dropping automation anchors at or before
    /// `current_time` so a direct `.value = x` takes effect immediately. Future
    /// scheduled events are preserved. Per Web Audio, `.value = x` is equivalent
    /// to `setValueAtTime(x, now)`: if an in-progress ramp survives, we insert an
    /// anchor at `current_time` holding the NEW value, so the value jumps now and
    /// the ramp continues from it toward its target (ramp events store only their
    /// end, so without the anchor they would interpolate from t=0).
    pub fn set_value_now(&mut self, value: f32, current_time: f64) {
        let clamped = value.clamp(self.min_value, self.max_value);
        self.events.retain(|e| e.time() > current_time);
        let has_active_ramp = self.events.iter().any(|e| {
            matches!(
                e,
                AutomationEvent::LinearRamp { .. } | AutomationEvent::ExponentialRamp { .. }
            )
        });
        if has_active_ramp {
            self.insert_event(AutomationEvent::SetValue {
                value: clamped,
                time: current_time,
            });
        }
        self.current_value = clamped;
    }

    pub fn default_value(&self) -> f32 {
        self.default_value
    }

    pub fn min_value(&self) -> f32 {
        self.min_value
    }

    pub fn max_value(&self) -> f32 {
        self.max_value
    }

    /// Schedule a value change at a specific time
    pub fn set_value_at_time(&mut self, value: f32, time: f64) {
        let value = value.clamp(self.min_value, self.max_value);
        self.insert_event(AutomationEvent::SetValue { value, time });
    }

    /// Schedule a linear ramp to a value ending at end_time
    pub fn linear_ramp_to_value_at_time(&mut self, value: f32, end_time: f64) {
        let value = value.clamp(self.min_value, self.max_value);
        self.insert_event(AutomationEvent::LinearRamp { value, end_time });
    }

    /// Schedule an exponential ramp to a value ending at end_time
    pub fn exponential_ramp_to_value_at_time(&mut self, value: f32, end_time: f64) {
        // Exponential ramp requires non-zero positive values
        let value = if value <= 0.0 {
            f32::EPSILON
        } else {
            value.clamp(f32::EPSILON, self.max_value)
        };
        self.insert_event(AutomationEvent::ExponentialRamp { value, end_time });
    }

    /// Asymptotically approach target starting at start_time with time_constant
    pub fn set_target_at_time(&mut self, target: f32, start_time: f64, time_constant: f64) {
        let target = target.clamp(self.min_value, self.max_value);
        self.insert_event(AutomationEvent::SetTarget {
            target,
            start_time,
            time_constant: time_constant.max(0.0001), // Prevent division by zero
        });
    }

    /// Cancel all scheduled events at or after cancel_time
    pub fn cancel_scheduled_values(&mut self, cancel_time: f64) {
        self.events.retain(|e| e.time() < cancel_time);
    }

    /// Check if there are any scheduled automation events
    #[inline]
    pub fn has_automation(&self) -> bool {
        !self.events.is_empty()
    }

    /// Compute the parameter value at a given context time (k-rate).
    pub fn compute_value(&self, current_time: f64) -> f32 {
        if self.events.is_empty() {
            return self.current_value;
        }
        let mut cursor = self.cursor();
        self.value_at(current_time, &mut cursor)
    }

    /// Fill a buffer with per-sample automation values (a-rate).
    ///
    /// `start_time`: context time at the start of the buffer
    /// `buffer`: one value per **frame**
    /// `sample_rate`: audio sample rate for time calculation
    ///
    /// One forward walk of the timeline for the whole buffer. This used to call
    /// `compute_value` per sample, and each of those re-walked every event from the
    /// start, so a-rate automation cost `O(frames * events)` per quantum.
    pub fn compute_values(&self, start_time: f64, buffer: &mut [f32], sample_rate: u32) {
        self.compute_values_offset(start_time, 0, buffer, sample_rate);
    }

    /// A bounded slice of the same original block, preserving its sample grid.
    /// Rebuilding a clock from a chunk's rounded start would change frame times.
    pub(crate) fn compute_values_offset(
        &self,
        start_time: f64,
        frame_offset: usize,
        buffer: &mut [f32],
        sample_rate: u32,
    ) {
        let mut block = self.values_block(start_time, sample_rate);
        self.compute_values_from(&mut block, frame_offset, buffer);
    }

    pub(crate) fn values_block(&self, start_time: f64, sample_rate: u32) -> AutomationBlock {
        AutomationBlock {
            clock: crate::render_time::RenderTime::new(start_time, sample_rate),
            cursor: self.cursor(),
        }
    }

    /// Fill increasing offsets with one cursor for the entire original block.
    pub(crate) fn compute_values_from(
        &self,
        block: &mut AutomationBlock,
        frame_offset: usize,
        buffer: &mut [f32],
    ) {
        if self.events.is_empty() {
            // No automation -- fill with constant value
            buffer.fill(self.current_value);
            return;
        }

        let clock = &block.clock;
        let end_time = clock.at(frame_offset + buffer.len());

        // Fast path: nothing is active yet. Only valid when the first event is a
        // plain SetValue/SetTarget whose start is after this block. Ramps are
        // excluded: their interpolation begins at the preceding event (or t=0), so
        // a ramp whose *end* is after the block may still be active within it.
        if let Some(first) = self.events.first() {
            let inactive = first.time() > end_time
                && !matches!(
                    first,
                    AutomationEvent::LinearRamp { .. } | AutomationEvent::ExponentialRamp { .. }
                );
            if inactive {
                buffer.fill(self.current_value);
                return;
            }
        }

        let cursor = &mut block.cursor;
        for (index, sample) in buffer.iter_mut().enumerate() {
            let time = clock.at(frame_offset + index);
            *sample = self.value_at(time, cursor);
        }
    }

    /// Clean up past events that are no longer needed (optimization).
    /// Call periodically to prevent unbounded event list growth.
    pub fn gc_events(&mut self, current_time: f64) {
        // Keep the most recent event before current_time and all future events.
        // We need the most recent past event to compute interpolation start values.
        if self.events.len() <= 1 {
            return;
        }

        let mut last_past_idx = None;
        for (i, event) in self.events.iter().enumerate() {
            if event.time() <= current_time {
                last_past_idx = Some(i);
            } else {
                break;
            }
        }

        if let Some(idx) = last_past_idx {
            if idx > 0 {
                // Update current_value to the evaluated value at this point
                self.current_value = self.compute_value(current_time);
                // Remove all past events except the most recent one
                self.events.drain(..idx);
            }
        }
    }

    /// Insert event in sorted order by time
    fn insert_event(&mut self, event: AutomationEvent) {
        let time = event.time();
        // Equal-time events retain call order; a later set/target must not be
        // evaluated before the anchor that was scheduled immediately before it.
        let pos = self.events.partition_point(|e| e.time() <= time);
        self.events.insert(pos, event);
    }
}

/// Where a timeline walk has got to.
///
/// Automation is evaluated at strictly increasing times within a render quantum,
/// so a walk only ever moves forward. Carrying that position across the quantum is
/// what turns a-rate evaluation from `O(frames * events)` into `O(frames +
/// events)`: `compute_values` used to call `compute_value` per sample, and each of
/// those calls re-walked the whole event list from the beginning.
pub(crate) struct AutomationBlock {
    clock: crate::render_time::RenderTime,
    cursor: TimelineCursor,
}

#[derive(Debug, Clone, Copy)]
struct TimelineCursor {
    /// Index of the first event not yet fully behind us.
    index: usize,
    /// Time and value of the last completed event, which bound any ramp in
    /// progress. Zero and the static value before the first event, matching what
    /// the timeline holds at context time zero.
    prev_time: f64,
    prev_value: f32,
    /// Value currently held, for times that fall in no segment.
    held: f32,
}

impl AudioParamTimeline {
    fn cursor(&self) -> TimelineCursor {
        TimelineCursor {
            index: 0,
            prev_time: 0.0,
            prev_value: self.current_value,
            held: self.current_value,
        }
    }

    /// Advance `cursor` to `time` and return the value there.
    ///
    /// The single definition of what the timeline means; both the k-rate
    /// (`compute_value`) and a-rate (`compute_values`) paths go through it, so they
    /// cannot disagree about a ramp's shape.
    fn value_at(&self, time: f64, cursor: &mut TimelineCursor) -> f32 {
        // Retire every segment that is wholly in the past.
        while let Some(event) = self.events.get(cursor.index) {
            let finished = match event {
                AutomationEvent::SetValue { time: at, .. } => time >= *at,
                AutomationEvent::LinearRamp { end_time, .. }
                | AutomationEvent::ExponentialRamp { end_time, .. } => time >= *end_time,
                // Asymptotic: only a later event ends it.
                AutomationEvent::SetTarget { .. } => self
                    .events
                    .get(cursor.index + 1)
                    .is_some_and(|next| time >= next.time()),
                AutomationEvent::CancelScheduled { .. } => true,
            };
            if !finished {
                break;
            }
            cursor.prev_time = event.time();
            if let Some(value) = event.end_value() {
                cursor.prev_value = value;
                cursor.held = value;
            }
            cursor.index += 1;
        }

        let mut value = cursor.held;
        if let Some(event) = self.events.get(cursor.index) {
            match event {
                AutomationEvent::SetValue { .. } | AutomationEvent::CancelScheduled { .. } => {}
                AutomationEvent::LinearRamp {
                    value: end,
                    end_time,
                } => {
                    if time >= cursor.prev_time {
                        let duration = *end_time - cursor.prev_time;
                        value = if duration > 0.0 {
                            let progress = ((time - cursor.prev_time) / duration) as f32;
                            cursor.prev_value + (*end - cursor.prev_value) * progress
                        } else {
                            *end
                        };
                    }
                }
                AutomationEvent::ExponentialRamp {
                    value: end,
                    end_time,
                } => {
                    if time >= cursor.prev_time && cursor.prev_value.abs() > f32::EPSILON {
                        let duration = *end_time - cursor.prev_time;
                        if duration > 0.0 {
                            let progress = ((time - cursor.prev_time) / duration) as f32;
                            value = cursor.prev_value * (*end / cursor.prev_value).powf(progress);
                        }
                    }
                }
                AutomationEvent::SetTarget {
                    target,
                    start_time,
                    time_constant,
                } => {
                    if time >= *start_time {
                        let elapsed = time - *start_time;
                        let decay = (-elapsed / *time_constant).exp() as f32;
                        value = *target + (cursor.prev_value - *target) * decay;
                    }
                }
            }
        }
        value.clamp(self.min_value, self.max_value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_set_value_at_time() {
        let mut param = AudioParamTimeline::new(0.5, 0.0, 1.0);
        param.set_value_at_time(0.8, 1.0);

        assert!((param.compute_value(0.5) - 0.5).abs() < f32::EPSILON);
        assert!((param.compute_value(1.0) - 0.8).abs() < f32::EPSILON);
        assert!((param.compute_value(2.0) - 0.8).abs() < f32::EPSILON);
    }

    #[test]
    fn test_linear_ramp() {
        let mut param = AudioParamTimeline::new(0.0, 0.0, 1.0);
        param.set_value_at_time(0.0, 0.0);
        param.linear_ramp_to_value_at_time(1.0, 1.0);

        assert!((param.compute_value(0.0) - 0.0).abs() < 0.01);
        assert!((param.compute_value(0.5) - 0.5).abs() < 0.01);
        assert!((param.compute_value(1.0) - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_cancel_scheduled() {
        let mut param = AudioParamTimeline::new(0.0, 0.0, 1.0);
        param.set_value_at_time(0.5, 1.0);
        param.set_value_at_time(1.0, 2.0);
        param.cancel_scheduled_values(1.5);

        assert!((param.compute_value(1.0) - 0.5).abs() < f32::EPSILON);
        // Event at 2.0 should be cancelled
        assert!((param.compute_value(2.0) - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn test_compute_values_buffer() {
        let param = AudioParamTimeline::new(1.0, 0.0, 10.0);
        let mut buffer = vec![0.0f32; 4];
        param.compute_values(0.0, &mut buffer, 44100);
        assert!(buffer.iter().all(|&v| (v - 1.0).abs() < f32::EPSILON));
    }

    #[test]
    fn gc_events_collapses_past_but_keeps_future() {
        // Repeated scheduling would grow `events` without bound; gc collapses
        // consumed past events to a single anchor.
        let mut param = AudioParamTimeline::new(0.0, -1.0e30, 1.0e30);
        for i in 0..100 {
            param.set_value_at_time(i as f32, i as f64);
        }
        assert_eq!(param.events.len(), 100);
        param.gc_events(100.0); // now is past every scheduled event
        assert!(
            param.events.len() <= 1,
            "gc must collapse consumed past events, got {}",
            param.events.len()
        );

        // Future events must survive a gc whose time precedes them.
        let mut future = AudioParamTimeline::new(0.0, -1.0e30, 1.0e30);
        future.set_value_at_time(1.0, 10.0);
        future.set_value_at_time(2.0, 20.0);
        future.gc_events(5.0);
        assert_eq!(future.events.len(), 2, "future events must be kept");
    }
}

#[cfg(test)]
mod cursor_tests {
    use super::*;

    /// The a-rate walk and the k-rate evaluation must agree at every point. They
    /// share `value_at`, and this is what says so: a cursor carried across a buffer
    /// must produce exactly what a fresh evaluation at each of those times does.
    #[test]
    fn the_forward_walk_matches_pointwise_evaluation() {
        let mut param = AudioParamTimeline::new(0.25, 0.0, 4.0);
        param.set_value_at_time(1.0, 0.001);
        param.linear_ramp_to_value_at_time(2.0, 0.003);
        param.exponential_ramp_to_value_at_time(0.5, 0.005);
        param.set_target_at_time(3.0, 0.006, 0.001);
        param.set_value_at_time(0.75, 0.009);

        let sample_rate = 8_000;
        let mut buffer = vec![0.0f32; 128];
        param.compute_values(0.0, &mut buffer, sample_rate);

        for (index, &walked) in buffer.iter().enumerate() {
            let time = index as f64 / sample_rate as f64;
            let pointwise = param.compute_value(time);
            assert!(
                (walked - pointwise).abs() < 1e-6,
                "sample {index} at t={time}: walk gave {walked}, pointwise {pointwise}"
            );
        }
    }

    /// A cursor only moves forward, so a long event list must not change what a
    /// short buffer sees -- and the walk must still land on the right segment when
    /// most of the timeline is already behind it.
    #[test]
    fn a_buffer_starting_mid_timeline_lands_on_the_right_segment() {
        let mut param = AudioParamTimeline::new(0.0, 0.0, 100.0);
        for i in 0..50 {
            param.set_value_at_time(i as f32, i as f64 * 0.001);
        }

        let sample_rate = 8_000;
        let mut buffer = vec![0.0f32; 8];
        param.compute_values(0.030, &mut buffer, sample_rate);

        // t = 0.030 is event 30's time, so the value held is 30.
        assert!(
            (buffer[0] - 30.0).abs() < 1e-6,
            "expected the value at t=0.030 to be 30, got {}",
            buffer[0]
        );
    }
}
