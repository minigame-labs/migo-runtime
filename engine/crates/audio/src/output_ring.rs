//! Both SPSC endpoints own the admitted backing through its real CPU disposal.
use crate::node_retirement::{Host, Reservation, Value};
use ringbuf::wrap::Wrap;
use ringbuf::{
    HeapRb,
    rb::RbRef,
    traits::Observer,
    wrap::{CachingCons, CachingProd},
};
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::{
    alloc::Layout,
    sync::{Arc, atomic::AtomicUsize},
};

struct Data {
    ring: Value<HeapRb<f32>>,
    host: Option<Host>,
}

/// Every clone refers to the same boxed SharedRb; its address never changes.
pub(crate) struct Reference(Option<Arc<Data>>);
pub(crate) type Producer = CachingProd<Reference>;
pub(crate) type Consumer = CachingCons<Reference>;

fn cell_bytes() -> usize {
    Layout::new::<[AtomicUsize; 2]>()
        .extend(Layout::new::<Data>())
        .unwrap()
        .0
        .pad_to_align()
        .size()
}

pub(super) fn new(samples: usize, host: Option<&Host>) -> EngineResult<(Producer, Consumer)> {
    if samples == 0 {
        return Err(EngineError::from_detail(
            ErrorCode::InvalidArgument,
            "empty output ring",
        ));
    }
    let bytes = samples
        .checked_mul(size_of::<f32>())
        .and_then(|bytes| bytes.checked_add(cell_bytes()))
        .ok_or_else(crate::node_storage::size_overflow)?;
    let reservation = match host {
        Some(host) => host.reserve_value_with_bytes::<HeapRb<f32>>(bytes)?,
        None => Reservation::unmanaged(),
    };
    // ringbuf 0.5.2 converts its Vec into an exactly sized boxed slice.
    // No allocation or shared endpoint exists before the reservation succeeds.
    let ring = HeapRb::<f32>::new(samples);
    debug_assert_eq!(ring.capacity().get(), samples);
    let value = reservation.finish(ring);
    #[cfg(test)]
    let value = {
        let mut value = value;
        OBSERVE.with(|hook| {
            if let Some(hook) = hook.borrow().as_ref() {
                value.observe(hook.clone());
            }
        });
        value
    };
    let reference = Reference(Some(Arc::new(Data {
        ring: value,
        host: host.cloned(),
    })));
    Ok((Producer::new(reference.clone()), Consumer::new(reference)))
}

impl Reference {
    pub(super) fn host(&self) -> Option<Host> {
        self.0.as_ref().unwrap().host.clone()
    }
}
pub(super) fn host(producer: &Producer) -> Option<Host> {
    producer.rb_ref().host()
}
impl Clone for Reference {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl AsRef<HeapRb<f32>> for Reference {
    fn as_ref(&self) -> &HeapRb<f32> {
        &self.0.as_ref().unwrap().ring
    }
}
// SAFETY: references and all their clones expose the very same HeapRb in its
// original box. The final reference alone retires that box, after both caching
// wrappers have committed their indices and released their held flags.
unsafe impl RbRef for Reference {
    type Rb = HeapRb<f32>;
}
impl Drop for Reference {
    fn drop(&mut self) {
        if let Some(cell) = self.0.take() {
            // Free this bounded Arc cell before handing its original ring and
            // physical credit to the already registered CPU cleanup slot.
            drop(Arc::into_inner(cell));
        }
    }
}

/// One inline deadline bounds failed output allocation retries.
#[derive(Default)]
pub(crate) struct Retry {
    next: Option<std::time::Instant>,
}
impl Retry {
    pub(crate) fn attempt<T>(
        &mut self,
        now: std::time::Instant,
        create: impl FnOnce() -> EngineResult<Option<T>>,
    ) -> EngineResult<Option<T>> {
        if self.next.is_some_and(|next| now < next) {
            return Ok(None);
        }
        let result = create();
        self.next = result
            .as_ref()
            .err()
            .map(|_| now + std::time::Duration::from_millis(100));
        result
    }
}

#[cfg(test)]
#[path = "output_ring_tests.rs"]
mod tests;

#[cfg(test)]
thread_local! {
    static OBSERVE: std::cell::RefCell<Option<Arc<dyn Fn()+Send+Sync>>> = const {std::cell::RefCell::new(None)};
}
#[cfg(test)]
pub(super) fn observe<R>(hook: Arc<dyn Fn() + Send + Sync>, create: impl FnOnce() -> R) -> R {
    struct Reset(Option<Arc<dyn Fn() + Send + Sync>>);
    impl Drop for Reset {
        fn drop(&mut self) {
            OBSERVE.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let _reset = Reset(OBSERVE.with(|slot| slot.replace(Some(hook))));
    create()
}
