//! The actor's output block admits capacity before a new output is published.
use crate::graph_vec::{GraphVec, Initial, Plan};
use crate::node_retirement::Host;
use shared::error::EngineResult;
use std::ops::{Deref, DerefMut};

pub(super) struct MixBuffer {
    data: GraphVec<f32>,
    host: Host,
}
impl MixBuffer {
    pub(super) fn new(host: Host, samples: usize) -> EngineResult<Self> {
        let initial = Initial::reserve(&host, samples)?;
        let mut data = initial.finish();
        data.resize(samples, 0.0);
        Ok(Self { data, host })
    }
    fn resize(&mut self, samples: usize) -> EngineResult<()> {
        if let Some(plan) = Plan::reserve(&self.data, Some(&self.host), samples)? {
            plan.install(&mut self.data);
        }
        self.data.resize(samples, 0.0);
        Ok(())
    }
    /// Failure disposes only the proposed output, keeping the old route intact.
    pub(super) fn admit<T>(&mut self, candidate: T, samples: usize) -> EngineResult<T> {
        self.resize(samples)?;
        Ok(candidate)
    }
    #[cfg(test)]
    fn capacity(&self) -> usize {
        self.data.capacity()
    }
    #[cfg(test)]
    fn observe(&mut self, hook: std::sync::Arc<dyn Fn() + Send + Sync>) {
        self.data.observe(hook);
    }
}
impl Deref for MixBuffer {
    type Target = [f32];
    fn deref(&self) -> &[f32] {
        &self.data
    }
}
impl DerefMut for MixBuffer {
    fn deref_mut(&mut self) -> &mut [f32] {
        &mut self.data
    }
}

#[cfg(test)]
#[path = "audio_thread_mix_buffer_tests.rs"]
mod tests;
