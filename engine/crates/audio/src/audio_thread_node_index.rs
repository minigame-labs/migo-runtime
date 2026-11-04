//! The actor's reverse index reserves capacity before entering a node factory.
use super::*;
use crate::graph_map::{GraphMap, Initial, Plan};
use crate::node_retirement::Host;

pub(super) struct NodeContextIndex {
    node_to_ctx: GraphMap<AudioContextId>,
    host: Option<Host>,
}
impl NodeContextIndex {
    #[cfg(test)]
    pub(super) fn new() -> Self {
        Self {
            node_to_ctx: GraphMap::with_capacity(64),
            host: None,
        }
    }
    pub(super) fn new_managed(
        cpu: migo_io::audio_cpu::AudioCpu,
        aggregate: Arc<AudioAggregateLedger>,
        channels: u32,
    ) -> EngineResult<Self> {
        Self::with_host(Host::new(cpu, aggregate, channels)?)
    }
    pub(super) fn with_host(host: Host) -> EngineResult<Self> {
        let initial = Initial::reserve(&host, 64)?;
        Ok(Self {
            node_to_ctx: initial.finish(),
            host: Some(host),
        })
    }
    pub(super) fn storage_host(&self) -> Host {
        self.host.as_ref().expect("actor index is managed").clone()
    }
    pub(super) fn create(
        &mut self,
        node_id: AudioNodeId,
        ctx_id: AudioContextId,
        factory: impl FnOnce() -> bool,
    ) -> bool {
        let required =
            self.node_to_ctx.len() + usize::from(!self.node_to_ctx.contains_key(&node_id));
        let plan = match Plan::reserve(&self.node_to_ctx, self.host.as_ref(), required) {
            Ok(plan) => plan,
            Err(error) => {
                tracing::warn!("Node index admission rejected: {error}");
                return false;
            }
        };
        if !factory() {
            return false;
        }
        if let Some(plan) = plan {
            plan.install(&mut self.node_to_ctx);
        }
        self.node_to_ctx.insert(node_id, ctx_id);
        true
    }
    #[cfg(test)]
    pub(super) fn register(&mut self, node_id: AudioNodeId, ctx_id: AudioContextId) {
        assert!(self.create(node_id, ctx_id, || true));
    }
    pub(super) fn unregister(&mut self, node_id: AudioNodeId) {
        self.node_to_ctx.remove(&node_id);
    }
    pub(super) fn get_context(&self, node_id: AudioNodeId) -> Option<AudioContextId> {
        self.node_to_ctx.get(&node_id).copied()
    }
    pub(super) fn clear_context(&mut self, ctx_id: AudioContextId) {
        self.node_to_ctx.retain(|_, value| *value != ctx_id);
    }
    pub(super) fn clear_all(&mut self) {
        self.node_to_ctx.clear();
    }
    #[cfg(test)]
    pub(super) fn identity(&self) -> (usize, usize, Vec<(u32, u32, usize)>) {
        let mut rows: Vec<_> = self
            .node_to_ctx
            .iter()
            .map(|(node, ctx)| (*node, *ctx, ctx as *const u32 as usize))
            .collect();
        rows.sort_unstable();
        (self.node_to_ctx.len(), self.node_to_ctx.capacity(), rows)
    }
    #[cfg(test)]
    pub(super) fn allocation_bytes(&self) -> usize {
        self.node_to_ctx.allocation_bytes()
    }
    #[cfg(test)]
    pub(super) fn observe(&mut self, hook: Arc<dyn Fn() + Send + Sync>) {
        self.node_to_ctx.observe(hook);
    }
}
