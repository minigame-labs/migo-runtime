//! Pull-side adapter for a prepared device SRC. One turn renders one source
//! block, advances one mature SRC step, or publishes at most 128 output frames.
//! Clocks describe graph rendering and PCM publication, never the DAC position.
use super::{Format, Ready, cancelled};
use crate::resample_job::bandlimited;
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::{collections::VecDeque, sync::atomic::Ordering};

const QUANTUM: usize = 128;
const MAX_AHEAD: u64 = 65536;
const MAX_EVENTS: usize = 4096;

#[derive(Clone, Copy)]
struct Limits {
    source_ahead: u64,
    pending_events: usize,
}
struct Block {
    frames: usize,
    eof: bool,
}
trait Source {
    /// The zeroed destination is at most 128 interleaved source frames.
    /// A zero-frame block must be EOF; partial final blocks are allowed.
    fn render(&mut self, output: &mut [f32]) -> EngineResult<Block>;
    /// Source-block end notifications; their publication fence is that block's
    /// endpoint. Collection of graph node IDs remains the source's immediate job.
    fn drain_ended(&mut self) -> impl Iterator<Item = u32>;
}
#[derive(Debug, PartialEq, Eq)]
enum Turn {
    Rendered(usize),
    Resampled,
    Published(usize),
    EventsReady,
    Finished,
    Idle,
}
#[derive(Debug, PartialEq, Eq)]
struct Clocks {
    rendered_source: u64,
    published_output: u64,
    published_source: u64,
}
struct Event {
    id: u32,
    fence: u64,
}
struct Buffers {
    input: Vec<f32>,
    head: usize,
    len: usize,
    events: VecDeque<Event>,
}
struct Pull {
    ready: Box<Ready>,
    buffers: Option<Buffers>,
    limits: Limits,
    output_head: usize,
    rendered: u64,
    published: u64,
    source_eof: bool,
    filter_done: bool,
    need_input: bool,
    suspended: bool,
}
impl Pull {
    /// Allocate the pull scratch and bounded event queue on the same CPU lane
    /// that prepared the filter. The ready output buffer itself is the PCM FIFO.
    #[cfg(test)]
    async fn new(ready: Box<Ready>, limits: Limits) -> EngineResult<Box<Self>> {
        let cpu = ready.request.cpu.clone();
        cpu.run(move || Self::allocate(ready, limits)).await?
    }
    fn allocate(ready: Box<Ready>, limits: Limits) -> EngineResult<Box<Self>> {
        if ready.request.cancel.load(Ordering::Acquire) {
            return Err(cancelled());
        }
        if !ready.is_ready()
            || ready.is_bypass()
            || limits.source_ahead == 0
            || limits.source_ahead > MAX_AHEAD
            || limits.pending_events > MAX_EVENTS
        {
            return Err(invalid("invalid prepared device pull configuration"));
        }
        let channels = ready.request.format.channels as usize;
        Ok(Box::new(Self {
            ready,
            buffers: Some(Buffers {
                input: vec![0.0; QUANTUM * channels],
                head: 0,
                len: 0,
                events: VecDeque::with_capacity(limits.pending_events),
            }),
            limits,
            output_head: 0,
            rendered: 0,
            published: 0,
            source_eof: false,
            filter_done: false,
            need_input: true,
            suspended: false,
        }))
    }
    fn clocks(&self) -> Clocks {
        let f = self.ready.request.format;
        let source =
            u64::try_from(u128::from(self.published) * u128::from(f.source) / u128::from(f.target))
                .unwrap_or(u64::MAX);
        Clocks {
            rendered_source: self.rendered,
            published_output: self.published,
            published_source: if self.source_eof {
                source.min(self.rendered)
            } else {
                source
            },
        }
    }
    fn suspend(&mut self) {
        self.suspended = true;
    }
    fn resume(&mut self) {
        self.suspended = false;
    }
    fn retire(&mut self, code: ErrorCode) {
        self.ready.retire(code);
        self.buffers = None;
        self.output_head = 0;
    }
    fn check_live(&mut self) -> EngineResult<()> {
        if self.ready.error.is_none() {
            if self.ready.request.cancel.load(Ordering::Acquire) {
                self.retire(ErrorCode::Cancelled);
            } else if self.ready.request.cpu.is_closed() {
                self.retire(EngineError::from(migo_io::pools::PoolError::Closed).code);
            }
        }
        if let Some(code) = self.ready.error {
            return Err(EngineError::from_detail(code, "device pull owner retired"));
        }
        Ok(())
    }
    fn pop_ended(&mut self) -> EngineResult<Option<u32>> {
        self.check_live()?;
        if self.suspended {
            return Ok(None);
        }
        let events = &mut self.buffers.as_mut().unwrap().events;
        Ok(
            if events
                .front()
                .is_some_and(|event| event.fence <= self.published)
            {
                events.pop_front().map(|event| event.id)
            } else {
                None
            },
        )
    }
    fn step(&mut self, source: &mut impl Source, output: &mut [f32]) -> EngineResult<Turn> {
        self.check_live()?;
        let result = self.advance(source, output);
        if let Err(error) = &result {
            self.retire(error.code);
        }
        result
    }
    fn queue_ended(
        buffers: &mut Buffers,
        limits: Limits,
        f: Format,
        source_frame: u64,
        ids: impl Iterator<Item = u32>,
    ) -> EngineResult<()> {
        let fence = u64::try_from(
            (u128::from(source_frame) * u128::from(f.target)).div_ceil(u128::from(f.source)),
        )
        .map_err(|_| invalid("device event clock overflow"))?;
        for id in ids {
            if buffers.events.len() == limits.pending_events {
                return Err(EngineError::from_detail(
                    ErrorCode::InputSaturated,
                    "device pending event budget exhausted",
                ));
            }
            buffers.events.push_back(Event { id, fence });
        }
        Ok(())
    }
    fn control_ended(&mut self, ids: impl Iterator<Item = u32>) -> EngineResult<()> {
        self.check_live()?;
        let result = Self::queue_ended(
            self.buffers.as_mut().unwrap(),
            self.limits,
            self.ready.request.format,
            self.rendered,
            ids,
        );
        if let Err(error) = &result {
            self.retire(error.code);
        }
        result
    }
    fn advance(&mut self, source: &mut impl Source, output: &mut [f32]) -> EngineResult<Turn> {
        let f = self.ready.request.format;
        let channels = f.channels as usize;
        let quota = (output.len() / channels).min(QUANTUM);
        if self.suspended || quota == 0 {
            return Ok(Turn::Idle);
        }
        let buffers = self.buffers.as_mut().unwrap();
        if buffers
            .events
            .front()
            .is_some_and(|event| event.fence <= self.published)
        {
            return Ok(Turn::EventsReady);
        }
        if self.output_head < self.ready.output.len() {
            let mut frames = quota.min((self.ready.output.len() - self.output_head) / channels);
            if let Some(event) = buffers.events.front() {
                frames = frames.min((event.fence - self.published).min(QUANTUM as u64) as usize);
            }
            let next_published = self
                .published
                .checked_add(frames as u64)
                .ok_or_else(|| invalid("device publication clock overflow"))?;
            let count = frames * channels;
            for (to, from) in output[..count]
                .iter_mut()
                .zip(&self.ready.output[self.output_head..self.output_head + count])
            {
                *to += *from;
            }
            self.published = next_published;
            self.output_head += count;
            return Ok(Turn::Published(frames));
        }
        if self.filter_done {
            return Ok(Turn::Finished);
        }
        if self.need_input && buffers.head == buffers.len && !self.source_eof {
            let source_clock =
                u128::from(self.published) * u128::from(f.source) / u128::from(f.target);
            let room = (source_clock + u128::from(self.limits.source_ahead))
                .saturating_sub(u128::from(self.rendered));
            let frames = room.min(QUANTUM as u128) as usize;
            if frames == 0 {
                return Err(EngineError::from_detail(
                    ErrorCode::InputSaturated,
                    "device source lookahead budget exhausted",
                ));
            }
            let input = &mut buffers.input[..frames * channels];
            input.fill(0.0);
            let block = source.render(input)?;
            if block.frames > frames || (block.frames == 0 && !block.eof) {
                return Err(invalid("device source block violates its frame quota"));
            }
            self.rendered = self
                .rendered
                .checked_add(block.frames as u64)
                .ok_or_else(|| invalid("device source clock overflow"))?;
            Self::queue_ended(buffers, self.limits, f, self.rendered, source.drain_ended())?;
            buffers.head = 0;
            buffers.len = block.frames * channels;
            self.source_eof = block.eof;
            self.need_input = false;
            return Ok(Turn::Rendered(block.frames));
        }
        self.output_head = 0;
        let offered = &buffers.input[buffers.head..buffers.len];
        let step = self
            .ready
            .step(offered, self.source_eof && offered.is_empty())?;
        if step.consumed > offered.len()
            || !step.consumed.is_multiple_of(channels)
            || self.ready.output.len() > crate::streaming_sinc::STEP_SAMPLES
        {
            return Err(invalid("device SRC violated its sample quota"));
        }
        buffers.head += step.consumed;
        self.need_input = step.progress == bandlimited::Progress::NeedInput;
        self.filter_done = step.progress == bandlimited::Progress::Finished;
        Ok(Turn::Resampled)
    }
}
fn invalid(detail: &str) -> EngineError {
    EngineError::from_detail(ErrorCode::InvalidArgument, detail)
}

#[path = "device_pull_tests.rs"]
#[cfg(test)]
mod tests;

#[path = "device_session.rs"]
mod session;

pub(crate) use session::DeviceMixer;
