//! Cache buckets keep their physical allocation through original CPU disposal.
use super::{CacheEntry, Key};
use crate::graph_table_layout::Layout;
use crate::limits::AudioAggregatePermit;
use crate::node_retirement::{Host, Reservation, Value};
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::{collections::HashMap, ops::Deref};

type Data = HashMap<Key, CacheEntry>;
pub(super) struct RetiredBuckets {
    _data: Data,
    #[cfg(test)]
    freed: Option<std::sync::Arc<std::sync::atomic::AtomicUsize>>,
}
#[cfg(test)]
impl Drop for RetiredBuckets {
    fn drop(&mut self) {
        let before = migo_alloc_probe::thread_counts();
        drop(std::mem::take(&mut self._data));
        if let Some(freed) = &self.freed {
            freed.store(
                (migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed) as usize,
                std::sync::atomic::Ordering::Release,
            );
        }
    }
}
enum Owner {
    Native,
    Inline(AudioAggregatePermit),
    Cpu(Value<Option<RetiredBuckets>>),
}
enum Admission {
    Inline(AudioAggregatePermit),
    Cpu(Reservation<Option<RetiredBuckets>>),
}
impl Admission {
    fn reserve(host: &Host, bytes: usize) -> EngineResult<Self> {
        if bytes < 4096 {
            Ok(Self::Inline(host.reserve_inline(bytes)?))
        } else {
            Ok(Self::Cpu(
                host.reserve_value_with_bytes::<Option<RetiredBuckets>>(bytes)?,
            ))
        }
    }
    fn finish(self) -> Owner {
        match self {
            Self::Inline(permit) => Owner::Inline(permit),
            Self::Cpu(reservation) => Owner::Cpu(reservation.finish(None)),
        }
    }
}
pub(super) struct Entries {
    // Inline credit drops after the data. CPU owners take these same buckets.
    data: Data,
    layout: Layout,
    owner: Owner,
    #[cfg(test)]
    freed: Option<std::sync::Arc<std::sync::atomic::AtomicUsize>>,
}
pub(super) struct Plan {
    layout: Layout,
    admission: Option<Admission>,
}
impl Entries {
    pub(super) fn new() -> Self {
        Self::with_capacity(0)
    }
    pub(super) fn with_capacity(capacity: usize) -> Self {
        let layout = Layout::for_entries::<(Key, CacheEntry)>(capacity)
            .expect("native cache table layout overflow");
        let result = Self {
            data: HashMap::with_capacity(capacity),
            layout,
            owner: Owner::Native,
            #[cfg(test)]
            freed: None,
        };
        assert_eq!(
            result.capacity(),
            layout.capacity,
            "native cache bucket allocation model changed"
        );
        result
    }
    pub(super) fn reserve(&self, host: Option<&Host>) -> EngineResult<Option<Plan>> {
        if let Some(host) = host {
            let accepts = match &self.owner {
                Owner::Native => self.layout.bytes == 0,
                Owner::Inline(permit) => host.accepts_inline(permit),
                Owner::Cpu(value) => value.accepts_physical_ledger(host),
            };
            if !accepts {
                return Err(EngineError::from_detail(
                    ErrorCode::InvalidArgument,
                    "cache table belongs to another physical ledger",
                ));
            }
        }
        // Eviction/replacement removes old keys before insertion. A removed
        // bucket can become a tombstone and lower std's reported capacity.
        // One slot beyond the current len covers all those later removals.
        let required = self
            .len()
            .checked_add(1)
            .ok_or_else(crate::node_storage::size_overflow)?;
        if required <= self.capacity() {
            return Ok(None);
        }
        let layout = Layout::for_entries::<(Key, CacheEntry)>(
            required
                .max(self.layout.capacity)
                .max(super::AudioCache::DEFAULT_CACHE_CAPACITY),
        )?;
        let admission = host
            .map(|host| Admission::reserve(host, layout.bytes))
            .transpose()?;
        Ok(Some(Plan { layout, admission }))
    }
    pub(super) fn get_mut(&mut self, key: &str) -> Option<&mut CacheEntry> {
        self.data.get_mut(key)
    }
    pub(super) fn remove(&mut self, key: &str) -> Option<CacheEntry> {
        self.data.remove(key)
    }
    pub(super) fn insert(&mut self, key: Key, entry: CacheEntry) {
        assert!(
            self.data.len() < self.data.capacity(),
            "cache buckets exceeded admitted capacity"
        );
        self.data.insert(key, entry);
    }
    pub(super) fn clear(&mut self) {
        self.data.clear();
    }
    #[cfg(test)]
    pub(super) fn allocation_bytes(&self) -> usize {
        self.layout.bytes
    }
    #[cfg(test)]
    pub(super) fn observe(&mut self, hook: std::sync::Arc<dyn Fn() + Send + Sync>) {
        if let Owner::Cpu(value) = &mut self.owner {
            value.observe(hook);
        }
    }
    #[cfg(test)]
    pub(super) fn watch_deallocation(
        &mut self,
        freed: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) {
        self.freed = Some(freed);
    }
}
impl Plan {
    pub(super) fn install(self, target: &mut Entries) {
        let mut replacement = Entries {
            data: HashMap::with_capacity_and_hasher(
                self.layout.capacity,
                target.data.hasher().clone(),
            ),
            layout: self.layout,
            owner: self.admission.map_or(Owner::Native, Admission::finish),
            #[cfg(test)]
            freed: None,
        };
        assert_eq!(
            replacement.capacity(),
            self.layout.capacity,
            "standard cache bucket allocation model changed"
        );
        replacement.data.extend(target.data.drain());
        *target = replacement;
    }
}
impl Deref for Entries {
    type Target = Data;
    fn deref(&self) -> &Data {
        &self.data
    }
}
impl Drop for Entries {
    fn drop(&mut self) {
        // Release live PCM/key handles synchronously through their own owners;
        // the empty allocation alone follows its bucket retirement ticket.
        self.data.clear();
        if let Owner::Cpu(value) = &mut self.owner {
            **value = Some(RetiredBuckets {
                _data: std::mem::take(&mut self.data),
                #[cfg(test)]
                freed: self.freed.take(),
            });
        }
    }
}

#[cfg(test)]
#[path = "cache_entries_storage_tests.rs"]
mod tests;
