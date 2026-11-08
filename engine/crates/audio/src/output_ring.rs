//! The SPSC ring between the audio thread and the device callback.
use ringbuf::{HeapCons, HeapProd, HeapRb, traits::Split};
use shared::error::{EngineError, EngineResult, ErrorCode};

pub(crate) type Producer = HeapProd<f32>;
pub(crate) type Consumer = HeapCons<f32>;

pub(super) fn new(samples: usize) -> EngineResult<(Producer, Consumer)> {
    if samples == 0 {
        return Err(EngineError::from_detail(
            ErrorCode::InvalidArgument,
            "empty output ring",
        ));
    }
    Ok(HeapRb::<f32>::new(samples).split())
}

/// Opening a device that just failed is retried at most every 100 ms.
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
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn a_failed_open_is_not_retried_for_100_ms() {
        let mut retry = Retry::default();
        let start = Instant::now();
        let failed: EngineResult<Option<()>> = retry.attempt(start, || {
            Err(EngineError::from_detail(ErrorCode::Internal, "no device"))
        });
        assert!(failed.is_err());
        let mut called = false;
        let skipped = retry.attempt(start + Duration::from_millis(50), || {
            called = true;
            Ok(Some(()))
        });
        assert!(matches!(skipped, Ok(None)) && !called);
        let opened = retry.attempt(start + Duration::from_millis(100), || Ok(Some(())));
        assert!(matches!(opened, Ok(Some(()))));
    }

    #[test]
    fn an_empty_ring_is_refused() {
        assert!(new(0).is_err());
        let (producer, _consumer) = new(8).unwrap();
        assert_eq!(ringbuf::traits::Observer::capacity(&producer).get(), 8);
    }
}
