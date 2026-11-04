//! Route buckets are admitted before requesting a filter; live routes drop synchronously.
use super::*;
use crate::graph_map::{GraphMap, Plan};
use crate::node_retirement::Host as StorageHost;
use std::collections::HashMap;
use std::ops::Deref;
pub(super) struct Routes {
    data: GraphMap<Route>,
    host: Option<StorageHost>,
}
impl Routes {
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
        factory: impl FnOnce() -> EngineResult<Route>,
    ) -> EngineResult<()> {
        let required = self.data.len() + usize::from(!self.data.contains_key(&id));
        let plan = Plan::reserve(&self.data, self.host.as_ref(), required)?;
        let route = factory()?;
        if let Some(plan) = plan {
            plan.install(&mut self.data);
        }
        self.data.insert(id, route);
        Ok(())
    }
    pub(super) fn get_mut(&mut self, id: &u32) -> Option<&mut Route> {
        self.data.get_mut(id)
    }
    pub(super) fn remove(&mut self, id: &u32) -> Option<Route> {
        self.data.remove(id)
    }
    pub(super) fn retain(&mut self, keep: impl FnMut(&u32, &mut Route) -> bool) {
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
    pub(super) fn identity(&self) -> (usize, usize, Vec<(u32, usize)>) {
        let mut rows: Vec<_> = self
            .data
            .iter()
            .map(|(id, route)| (*id, route as *const Route as usize))
            .collect();
        rows.sort_unstable();
        (self.data.len(), self.data.capacity(), rows)
    }
    #[cfg(test)]
    pub(super) fn insert(&mut self, id: u32, route: Route) {
        self.create_with(id, || Ok(route)).unwrap();
    }
}
impl Drop for Routes {
    fn drop(&mut self) {
        // Device leases begin their reserved handoff on this caller.
        // Empty bulk buckets keep their charge through original CPU destruction.
        for (_, route) in self.data.drain() {
            drop(route);
        }
    }
}
impl Deref for Routes {
    type Target = HashMap<u32, Route>;
    fn deref(&self) -> &Self::Target {
        &self.data
    }
}
