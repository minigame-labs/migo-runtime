//! The actor's output block admits capacity before a new output is published.
use shared::error::EngineResult;
use std::ops::{Deref, DerefMut};

pub(super) struct MixBuffer {
    data: Vec<f32>,
}
impl MixBuffer {
    pub(super) fn new(samples: usize) -> Self {
        Self {
            data: vec![0.0; samples],
        }
    }
    /// Resize for a replacement output's quantum, handing the output back.
    pub(super) fn admit<T>(&mut self, candidate: T, samples: usize) -> EngineResult<T> {
        self.data.resize(samples, 0.0);
        Ok(candidate)
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
