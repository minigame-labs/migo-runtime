//! Real AudioContext port for the continuous managed SRC. Graph collection is
//! immediate; ended notifications cross the existing local publication fence.
use super::*;
use shared::protocol::audio_cmd::AudioContextState;

struct Port<'a, F> {
    context: &'a mut AudioContext,
    collected: &'a mut F,
}
impl<F: FnMut(u32)> Graph for Port<'_, F> {
    fn active(&self) -> bool {
        self.context.has_active_sources()
    }
}
impl<F: FnMut(u32)> Source for Port<'_, F> {
    fn render(&mut self, output: &mut [f32]) -> EngineResult<Block> {
        let channels = self.context.channels().max(1) as usize;
        let frames = output.len() / channels;
        if frames == 0 || frames > QUANTUM || !output.len().is_multiple_of(channels) {
            return Err(invalid("invalid context source quantum"));
        }
        for &id in self.context.process_device_source(output) {
            (self.collected)(id);
        }
        Ok(Block { frames, eof: false })
    }
    fn drain_ended(&mut self) -> impl Iterator<Item = u32> {
        self.context.drain_ended()
    }
}
#[cfg(test)]
#[derive(Debug, PartialEq)]
struct GraphClocks {
    absolute_graph_frame: u64,
    public_seconds: f64,
    stream_source: u64,
    published_source: u64,
    published_output: u64,
}
impl Device {
    pub(super) fn graph_session(&mut self, context: &AudioContext) -> EngineResult<&mut Session> {
        self.session()?;
        let bound = self
            .binding
            .as_ref()
            .is_some_and(|binding| binding.epoch.same(&context.device_epoch()));
        let session = self.session()?;
        let request = &session.pull.ready.request;
        if !bound
            || request.context != context.id()
            || request.format.source != context.sample_rate()
            || request.format.channels != context.channels()
        {
            self.retire(ErrorCode::Cancelled);
            return Err(cancelled());
        }
        let session = self.session()?;
        if context.state() == AudioContextState::Suspended {
            session.suspend();
        } else {
            session.resume();
        }
        Ok(session)
    }
    pub(super) fn service_context_controls(
        &mut self,
        context: &mut AudioContext,
        mut collected: impl FnMut(u32),
    ) -> EngineResult<()> {
        let session = self.graph_session(context)?;
        if !context.has_device_controls() {
            return Ok(());
        }
        for &id in context.collect_device_controls() {
            collected(id);
        }
        let result = session.pull.control_ended(context.drain_ended());
        if let Err(error) = &result {
            self.retire(error.code);
        }
        result
    }
    pub(super) fn step_context(
        &mut self,
        context: &mut AudioContext,
        output: &mut [f32],
        mut collected: impl FnMut(u32),
    ) -> EngineResult<SessionTurn> {
        self.graph_session(context)?;
        if context.state() != AudioContextState::Running
            || output.len() < context.channels().max(1) as usize
        {
            return Ok(SessionTurn::Work(Turn::Idle));
        }
        self.service_context_controls(context, &mut collected)?;
        let session = self.graph_session(context)?;
        let events = &session.pull.buffers.as_ref().unwrap().events;
        if events
            .front()
            .is_some_and(|event| event.fence <= session.pull.published)
        {
            return Ok(SessionTurn::Work(Turn::EventsReady));
        }
        if !events.is_empty() {
            session.sleeping = false;
        }
        self.step(
            &mut Port {
                context,
                collected: &mut collected,
            },
            output,
        )
    }
    pub(super) fn pop_ended_context(
        &mut self,
        context: &AudioContext,
    ) -> EngineResult<Option<u32>> {
        self.graph_session(context)?;
        self.pop_ended()
    }
    #[cfg(test)]
    fn graph_clocks(&mut self, context: &AudioContext) -> EngineResult<GraphClocks> {
        let session = self.graph_session(context)?;
        let clocks = session.pull.clocks();
        Ok(GraphClocks {
            absolute_graph_frame: context.device_graph_frame(),
            public_seconds: context.current_time(),
            stream_source: clocks.rendered_source,
            published_source: clocks.published_source,
            published_output: clocks.published_output,
        })
    }
}

#[path = "device_graph_tests.rs"]
#[cfg(test)]
mod tests;
