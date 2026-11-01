//! Physical PCM credit shared by native audio and Host-owned registry copies.
//! Domain-specific item and byte admission remains separate.
use crate::error::{EngineError, EngineResult, ErrorCode};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock};
/// Original aggregate ceiling, derived from the unchanged retained/cache/PCM domains.
pub const MAX_AUDIO_AGGREGATE_PHYSICAL_BYTES: usize =
    256 * 1024 * 1024 + 128 * 1024 * 1024 + 64 * 1024 * 1024 * 2;
/// Process-wide physical PCM ledger shared by every audio allocation domain.
#[derive(Debug)]
pub struct AudioAggregateLedger {
    max_bytes: usize,
    bytes: AtomicUsize,
}

impl AudioAggregateLedger {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            bytes: AtomicUsize::new(0),
        }
    }

    pub fn process_global() -> Arc<Self> {
        static PROCESS_LEDGER: LazyLock<Arc<AudioAggregateLedger>> = LazyLock::new(|| {
            Arc::new(AudioAggregateLedger::new(
                MAX_AUDIO_AGGREGATE_PHYSICAL_BYTES,
            ))
        });
        Arc::clone(&PROCESS_LEDGER)
    }

    pub fn used_bytes(&self) -> usize {
        self.bytes.load(Ordering::Acquire)
    }

    pub fn try_reserve(
        self: &Arc<Self>,
        bytes: usize,
        scope: &'static str,
    ) -> EngineResult<AudioAggregatePermit> {
        self.bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes)
                    .filter(|next| *next <= self.max_bytes)
            })
            .map_err(|_| {
                EngineError::from_detail(
                    ErrorCode::InputSaturated,
                    format!("{scope} aggregate physical PCM limit exceeded"),
                )
            })?;
        Ok(AudioAggregatePermit {
            ledger: Arc::clone(self),
            bytes,
        })
    }
}

#[derive(Debug)]
pub struct AudioAggregatePermit {
    ledger: Arc<AudioAggregateLedger>,
    bytes: usize,
}

impl Drop for AudioAggregatePermit {
    fn drop(&mut self) {
        self.ledger.bytes.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

impl AudioAggregatePermit {
    pub fn belongs_to(&self, ledger: &Arc<AudioAggregateLedger>) -> bool {
        Arc::ptr_eq(&self.ledger, ledger)
    }

    pub fn try_grow_to(&mut self, new_bytes: usize, scope: &'static str) -> EngineResult<()> {
        if new_bytes <= self.bytes {
            return Ok(());
        }
        let additional = new_bytes - self.bytes;
        self.ledger
            .bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(additional)
                    .filter(|next| *next <= self.ledger.max_bytes)
            })
            .map_err(|_| {
                EngineError::from_detail(
                    ErrorCode::InputSaturated,
                    format!("{scope} aggregate physical PCM limit exceeded"),
                )
            })?;
        self.bytes = new_bytes;
        Ok(())
    }

    pub fn shrink_to(&mut self, new_bytes: usize) {
        if new_bytes < self.bytes {
            self.ledger
                .bytes
                .fetch_sub(self.bytes - new_bytes, Ordering::AcqRel);
            self.bytes = new_bytes;
        }
    }
}
