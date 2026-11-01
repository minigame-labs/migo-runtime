//! Persistent graph session: drain a finite zero tail, then stop calling the
//! source and SRC. Waking keeps the same owner, pending zeros and rational phase.
use super::*;

trait Graph: Source {
    /// True for playing, scheduled, effect-tail or control work that needs a
    /// graph render. Inactive renders must produce silence.
    fn active(&self) -> bool;
}
#[derive(Debug, PartialEq, Eq)]
enum SessionTurn {
    Work(Turn),
    Quiescent,
}
struct Session {
    pull: Box<Pull>,
    quiet_span: u64,
    quiet_at: Option<u64>,
    sleeping: bool,
}
struct Live<'a, G>(&'a mut G);
impl<G: Graph> Source for Live<'_, G> {
    fn render(&mut self, output: &mut [f32]) -> EngineResult<Block> {
        let block = self.0.render(output)?;
        if block.eof {
            return Err(invalid("continuous graph submitted EOF"));
        }
        Ok(block)
    }
    fn drain_ended(&mut self) -> impl Iterator<Item = u32> {
        self.0.drain_ended()
    }
}
impl Session {
    #[cfg(test)]
    async fn new(ready: Box<Ready>, limits: Limits) -> EngineResult<Box<Self>> {
        let cpu = ready.request.cpu.clone();
        cpu.run(move || Self::allocate(ready, limits)).await?
    }
    fn allocate(ready: Box<Ready>, limits: Limits) -> EngineResult<Box<Self>> {
        let pull = Pull::allocate(ready, limits)?;
        let quiet_span = pull.ready.stream.as_ref().unwrap().quiet_source_frames()?;
        Ok(Box::new(Self {
            pull,
            quiet_span,
            quiet_at: None,
            sleeping: true,
        }))
    }
    fn suspend(&mut self) {
        self.pull.suspend();
    }
    fn resume(&mut self) {
        self.pull.resume();
    }
    fn pop_ended(&mut self) -> EngineResult<Option<u32>> {
        self.pull.pop_ended()
    }
    fn step(&mut self, graph: &mut impl Graph, output: &mut [f32]) -> EngineResult<SessionTurn> {
        self.pull.check_live()?;
        let channels = self.pull.ready.request.format.channels as usize;
        if self.pull.suspended || output.len() < channels {
            return Ok(SessionTurn::Work(Turn::Idle));
        }
        if graph.active() {
            self.sleeping = false;
            self.quiet_at = None;
        } else if self.sleeping {
            return Ok(SessionTurn::Quiescent);
        }
        let turn = self.pull.step(&mut Live(graph), output)?;
        if graph.active() {
            self.quiet_at = None;
        } else if self.quiet_at.is_none() {
            let Some(end) = self.pull.rendered.checked_add(self.quiet_span) else {
                self.pull.retire(ErrorCode::InvalidArgument);
                return Err(invalid("device quiet source clock overflow"));
            };
            self.quiet_at = Some(end);
        }
        if self
            .quiet_at
            .is_some_and(|end| self.pull.clocks().published_source >= end)
            && self.pull.buffers.as_ref().unwrap().events.is_empty()
            && self.pull.ready.output[self.pull.output_head..]
                .iter()
                .all(|&sample| sample == 0.0)
        {
            self.sleeping = true;
        }
        Ok(SessionTurn::Work(turn))
    }
}

#[path = "device_session_tests.rs"]
#[cfg(test)]
mod tests;

#[path = "device_resources.rs"]
mod resources;

pub(crate) use resources::DeviceMixer;
