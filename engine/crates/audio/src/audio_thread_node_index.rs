//! The actor's reverse index reserves capacity before entering a node factory.
use super::*;

pub(super) struct NodeContextIndex {
    node_to_ctx: HashMap<AudioNodeId, AudioContextId>,
}
impl NodeContextIndex {
    pub(super) fn new() -> Self {
        Self {
            node_to_ctx: HashMap::with_capacity(64),
        }
    }
    pub(super) fn create(
        &mut self,
        node_id: AudioNodeId,
        ctx_id: AudioContextId,
        factory: impl FnOnce() -> bool,
    ) -> bool {
        if !factory() {
            return false;
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
}
