//! One small shared state cell owns its actual layout through the final clone.
use super::AudioSyncState;
use crate::limits::AudioAggregatePermit;
use crate::node_retirement::Host;
use shared::error::EngineResult;
use std::{
    alloc::Layout,
    ops::Deref,
    sync::{Arc, atomic::AtomicUsize},
};

struct Data {
    state: AudioSyncState,
    #[cfg(test)]
    before_refund: Witness,
    _permit: Option<AudioAggregatePermit>,
    #[cfg(test)]
    after_refund: Witness,
}

pub(super) struct Reference(Option<Arc<Data>>);
impl Reference {
    pub(super) fn bytes() -> usize {
        Layout::new::<[AtomicUsize; 2]>()
            .extend(Layout::new::<Data>())
            .unwrap()
            .0
            .pad_to_align()
            .size()
    }
    pub(super) fn new(
        host: Option<&Host>,
        state: impl FnOnce() -> AudioSyncState,
    ) -> EngineResult<Self> {
        let bytes = Self::bytes();
        assert!(bytes < 4096, "AudioSync state exceeds the inline boundary");
        let permit = host.map(|host| host.reserve_inline(bytes)).transpose()?;
        Ok(Self(Some(Arc::new(Data {
            state: state(),
            #[cfg(test)]
            before_refund: Witness::default(),
            _permit: permit,
            #[cfg(test)]
            after_refund: Witness::default(),
        }))))
    }
    #[cfg(test)]
    pub(super) fn charged_bytes(&self) -> usize {
        if self.0.as_ref().unwrap()._permit.is_some() {
            Self::bytes()
        } else {
            0
        }
    }
    #[cfg(test)]
    pub(super) fn observe(
        &mut self,
        before: Arc<dyn Fn() + Send + Sync>,
        after: Arc<dyn Fn() + Send + Sync>,
    ) {
        let data = Arc::get_mut(self.0.as_mut().unwrap()).unwrap();
        data.before_refund.0 = Some(before);
        data.after_refund.0 = Some(after);
    }
}
impl Clone for Reference {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl Deref for Reference {
    type Target = AudioSyncState;
    fn deref(&self) -> &AudioSyncState {
        &self.0.as_ref().unwrap().state
    }
}
impl Drop for Reference {
    fn drop(&mut self) {
        if let Some(cell) = self.0.take() {
            // Every clone uses into_inner, including simultaneous final drops.
            // The cell is freed before State/notifier, then its physical permit.
            drop(Arc::into_inner(cell));
        }
    }
}
#[cfg(test)]
#[derive(Default)]
struct Witness(Option<Arc<dyn Fn() + Send + Sync>>);
#[cfg(test)]
impl Drop for Witness {
    fn drop(&mut self) {
        if let Some(hook) = &self.0 {
            hook();
        }
    }
}

#[cfg(test)]
#[path = "output_sync_storage_tests.rs"]
mod tests;
