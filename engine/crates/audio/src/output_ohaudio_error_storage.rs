//! OHAudio error flag: one admitted Arc cell through its actual final free.
use super::AtomicBool;
use crate::limits::AudioAggregatePermit;
use crate::node_retirement::Host;
use shared::error::EngineResult;
use std::{
    alloc::Layout,
    ops::Deref,
    sync::{Arc, atomic::AtomicUsize},
};

struct Data {
    state: AtomicBool,
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
        state: impl FnOnce() -> AtomicBool,
    ) -> EngineResult<Self> {
        let bytes = Self::bytes();
        assert!(
            bytes < 4096,
            "OHAudio error flag exceeds the inline boundary"
        );
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
    type Target = AtomicBool;
    fn deref(&self) -> &AtomicBool {
        &self.0.as_ref().unwrap().state
    }
}
impl Drop for Reference {
    fn drop(&mut self) {
        if let Some(cell) = self.0.take() {
            // Every clone uses into_inner, including simultaneous final drops.
            // The cell is freed before the error flag, then its physical permit.
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
#[path = "output_ohaudio_error_storage_tests.rs"]
mod tests;
