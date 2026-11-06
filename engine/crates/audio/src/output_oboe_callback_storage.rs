//! One admitted callback Box owns its physical layout until the final bridge release.
use super::OutputCallback;
use crate::{limits::AudioAggregatePermit, node_retirement::Host};
use shared::error::EngineResult;

pub(super) struct Data {
    #[cfg(test)]
    before_drop: Witness,
    pub(super) callback: OutputCallback,
    _permit: Option<AudioAggregatePermit>,
    #[cfg(test)]
    after_refund: Witness,
}

pub(super) struct Admission {
    permit: Option<AudioAggregatePermit>,
}
impl Admission {
    pub(super) fn bytes() -> usize {
        std::alloc::Layout::new::<Data>().size()
    }
    pub(super) fn new(host: &Host) -> EngineResult<Self> {
        let bytes = Self::bytes();
        assert!(
            bytes < 4096,
            "Oboe callback Box exceeds the inline boundary"
        );
        let permit = host.reserve_inline(bytes)?;
        Ok(Self {
            permit: Some(permit),
        })
    }
    pub(super) fn finish(self, callback: OutputCallback) -> Callback {
        Callback(Some(Box::new(Data {
            #[cfg(test)]
            before_drop: Witness::default(),
            callback,
            _permit: self.permit,
            #[cfg(test)]
            after_refund: Witness::default(),
        })))
    }
}

pub(super) struct Callback(Option<Box<Data>>);
impl Callback {
    pub(super) fn into_raw(mut self) -> *mut Data {
        Box::into_raw(self.0.take().unwrap())
    }
    #[cfg(test)]
    pub(super) fn observe(
        &mut self,
        before: std::sync::Arc<dyn Fn() + Send + Sync>,
        after: std::sync::Arc<dyn Fn() + Send + Sync>,
    ) {
        let data = self.0.as_mut().unwrap();
        data.before_drop.0 = Some(before);
        data.after_refund.0 = Some(after);
    }
}
impl Drop for Callback {
    fn drop(&mut self) {
        if let Some(data) = self.0.take() {
            // SAFETY: this unique Box has not been handed to the bridge.
            unsafe { release(Box::into_raw(data)) };
        }
    }
}

/// Release the unique callback allocation before its payload and physical permit.
///
/// # Safety
/// `pointer` came from Callback::into_raw, is released exactly once, and no
/// native callback may access it again. The bridge's final Context owns this call.
pub(super) unsafe fn release(pointer: *mut Data) {
    // SAFETY: the caller transfers back the unique live Box allocation.
    let data = unsafe { *Box::from_raw(pointer) };
    drop(data);
}

#[cfg(test)]
#[derive(Default)]
struct Witness(Option<std::sync::Arc<dyn Fn() + Send + Sync>>);
#[cfg(test)]
impl Drop for Witness {
    fn drop(&mut self) {
        if let Some(hook) = &self.0 {
            hook();
        }
    }
}
#[cfg(test)]
#[path = "output_oboe_callback_storage_tests.rs"]
mod tests;
