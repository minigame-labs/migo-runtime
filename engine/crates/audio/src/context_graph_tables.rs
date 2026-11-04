//! Node commands reserve all table tickets before graph/buffer mutation.
use super::AudioContext;
use crate::graph_map::Plan;
use crate::node_retirement::Node;
use crate::render_buffer::RenderBuffer;
use shared::error::EngineResult;
pub(super) struct TableGrowth {
    nodes: Option<Plan<Node>>,
    buffers: Option<Plan<RenderBuffer>>,
    index: Option<Plan<u32>>,
    released: Option<Plan<()>>,
}
impl TableGrowth {
    pub(super) fn reserve(ctx: &AudioContext, nodes: usize) -> EngineResult<Self> {
        let host = ctx.node_retirement.as_ref();
        Ok(Self {
            nodes: Plan::reserve(&ctx.nodes, host, nodes)?,
            buffers: Plan::reserve(&ctx.node_buffers, host, nodes)?,
            index: Plan::reserve(&ctx.dense_index, host, nodes)?,
            released: Plan::reserve(&ctx.released.map, host, nodes)?,
        })
    }
    pub(super) fn install(self, ctx: &mut AudioContext) {
        if let Some(plan) = self.nodes {
            plan.install(&mut ctx.nodes);
        }
        if let Some(plan) = self.buffers {
            plan.install(&mut ctx.node_buffers);
        }
        if let Some(plan) = self.index {
            plan.install(&mut ctx.dense_index);
        }
        if let Some(plan) = self.released {
            plan.install(&mut ctx.released.map);
        }
    }
}
