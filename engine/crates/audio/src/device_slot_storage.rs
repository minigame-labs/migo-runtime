//! Pending route slots reserve buckets before creating or scheduling an operation.
use super::*;
use crate::graph_map::{GraphMap, Plan};
use crate::node_retirement::Host as StorageHost;
use std::collections::HashMap;
use std::ops::Deref;
pub(super) struct Slots {
    data: GraphMap<Slot>,
    host: Option<StorageHost>,
}
impl Slots {
    #[cfg(test)]
    pub(super) fn new() -> Self {
        Self {
            data: GraphMap::new(),
            host: None,
        }
    }
    pub(super) fn managed(host: StorageHost) -> Self {
        Self {
            data: GraphMap::new(),
            host: Some(host),
        }
    }
    pub(super) fn create_with(
        &mut self,
        id: u32,
        factory: impl FnOnce() -> EngineResult<Slot>,
    ) -> EngineResult<()> {
        let required = self.data.len() + usize::from(!self.data.contains_key(&id));
        let plan = Plan::reserve(&self.data, self.host.as_ref(), required)?;
        let slot = factory()?;
        if let Some(plan) = plan {
            plan.install(&mut self.data);
        }
        self.data.insert(id, slot);
        Ok(())
    }
    pub(super) fn get_mut(&mut self, id: &u32) -> Option<&mut Slot> {
        self.data.get_mut(id)
    }
    pub(super) fn remove(&mut self, id: &u32) -> Option<Slot> {
        self.data.remove(id)
    }
    pub(super) fn retain(&mut self, keep: impl FnMut(&u32, &mut Slot) -> bool) {
        self.data.retain(keep);
    }
    pub(super) fn clear(&mut self) {
        self.data.clear();
    }
    #[cfg(test)]
    pub(super) fn table_bytes(&self) -> usize {
        self.data.allocation_bytes()
    }
    #[cfg(test)]
    pub(super) fn observe(&mut self, hook: Arc<dyn Fn() + Send + Sync>) {
        self.data.observe(hook);
    }
    #[cfg(test)]
    pub(super) fn identity(&self) -> (usize, usize, Vec<(u32, usize, usize)>) {
        let mut rows: Vec<_> = self
            .data
            .iter()
            .map(|(id, slot)| {
                (
                    *id,
                    slot as *const Slot as usize,
                    Arc::as_ptr(&slot.operation) as usize,
                )
            })
            .collect();
        rows.sort_unstable();
        (self.data.len(), self.data.capacity(), rows)
    }
}
impl Drop for Slots {
    fn drop(&mut self) {
        // Slot Drop revokes operations and aborts pending tasks immediately.
        // Empty bulk buckets keep their charge through original CPU destruction.
        for (_, slot) in self.data.drain() {
            drop(slot);
        }
    }
}
impl Deref for Slots {
    type Target = HashMap<u32, Slot>;
    fn deref(&self) -> &Self::Target {
        &self.data
    }
}
