//! Reusable canvas IDs without holding a TLS borrow across host callbacks.

use std::cell::RefCell;

thread_local! {
    static IDS: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
}

pub(crate) struct MaterializeScratch(Vec<u32>);

impl MaterializeScratch {
    pub(crate) fn take(capacity_limit: usize) -> Self {
        let mut ids = IDS.with(|slot| std::mem::take(&mut *slot.borrow_mut()));
        if ids.capacity() > capacity_limit {
            // A previous large scene must not defeat this frame's admission.
            ids = Vec::new();
        }
        ids.reserve_exact(capacity_limit);
        Self(ids)
    }
}

impl std::ops::Deref for MaterializeScratch {
    type Target = Vec<u32>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for MaterializeScratch {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for MaterializeScratch {
    fn drop(&mut self) {
        self.0.clear();
        // Retain at most 32 KiB per decoding thread. Bigger unadmitted callers
        // can still decode, but cannot permanently enlarge this scratch cache.
        if self.0.capacity() <= 8192 {
            let _ = IDS.try_with(|slot| {
                let mut cached = slot.borrow_mut();
                if self.0.capacity() > cached.capacity() {
                    *cached = std::mem::take(&mut self.0);
                }
            });
        }
    }
}
