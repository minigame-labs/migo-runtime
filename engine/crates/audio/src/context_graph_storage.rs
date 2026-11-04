//! A complete growth batch reserves every affected vector before installation.
use super::AudioContext;
use crate::graph_vec::Plan;
use crate::nodes::NodeConnection;
use shared::error::EngineResult;

pub(super) struct NodeGrowth {
    dense_ids: Option<Plan<u32>>,
    in_degree: Option<Plan<u32>>,
    processing_order: Option<Plan<u32>>,
    ready_queue: Option<Plan<u32>>,
    input_start: Option<Plan<u32>>,
    output_start: Option<Plan<u32>>,
    collected: Option<Plan<u32>>,
    native_collected: Option<Plan<u32>>,
}
impl NodeGrowth {
    pub(super) fn reserve(ctx: &AudioContext, nodes: usize) -> EngineResult<Self> {
        let host = ctx.node_retirement.as_ref();
        let starts = nodes
            .checked_add(1)
            .ok_or_else(crate::node_storage::size_overflow)?;
        Ok(Self {
            dense_ids: Plan::reserve(&ctx.dense_ids, host, nodes)?,
            in_degree: Plan::reserve(&ctx.in_degree, host, nodes)?,
            processing_order: Plan::reserve(&ctx.processing_order, host, nodes)?,
            ready_queue: Plan::reserve(&ctx.ready_queue, host, nodes)?,
            input_start: Plan::reserve(&ctx.input_start, host, starts)?,
            output_start: Plan::reserve(&ctx.output_start, host, starts)?,
            collected: Plan::reserve(&ctx.collected, host, nodes)?,
            native_collected: Plan::reserve(&ctx.native_collected, host, nodes)?,
        })
    }
    pub(super) fn install(self, ctx: &mut AudioContext) {
        if let Some(plan) = self.dense_ids {
            plan.install(&mut ctx.dense_ids);
        }
        if let Some(plan) = self.in_degree {
            plan.install(&mut ctx.in_degree);
        }
        if let Some(plan) = self.processing_order {
            plan.install(&mut ctx.processing_order);
        }
        if let Some(plan) = self.ready_queue {
            plan.install(&mut ctx.ready_queue);
        }
        if let Some(plan) = self.input_start {
            plan.install(&mut ctx.input_start);
        }
        if let Some(plan) = self.output_start {
            plan.install(&mut ctx.output_start);
        }
        if let Some(plan) = self.collected {
            plan.install(&mut ctx.collected);
        }
        if let Some(plan) = self.native_collected {
            plan.install(&mut ctx.native_collected);
        }
    }
}
pub(super) struct EdgeGrowth {
    connections: Option<Plan<NodeConnection>>,
    input_edges: Option<Plan<u32>>,
    output_edges: Option<Plan<u32>>,
    edge_src: Option<Plan<u32>>,
    edge_src_port: Option<Plan<Option<usize>>>,
    edge_dst_port: Option<Plan<Option<usize>>>,
}
impl EdgeGrowth {
    pub(super) fn reserve(ctx: &AudioContext, edges: usize) -> EngineResult<Self> {
        let host = ctx.node_retirement.as_ref();
        Ok(Self {
            connections: Plan::reserve(&ctx.connections, host, edges)?,
            input_edges: Plan::reserve(&ctx.input_edges, host, edges)?,
            output_edges: Plan::reserve(&ctx.output_edges, host, edges)?,
            edge_src: Plan::reserve(&ctx.edge_src, host, edges)?,
            edge_src_port: Plan::reserve(&ctx.edge_src_port, host, edges)?,
            edge_dst_port: Plan::reserve(&ctx.edge_dst_port, host, edges)?,
        })
    }
    pub(super) fn install(self, ctx: &mut AudioContext) {
        if let Some(plan) = self.connections {
            plan.install(&mut ctx.connections);
        }
        if let Some(plan) = self.input_edges {
            plan.install(&mut ctx.input_edges);
        }
        if let Some(plan) = self.output_edges {
            plan.install(&mut ctx.output_edges);
        }
        if let Some(plan) = self.edge_src {
            plan.install(&mut ctx.edge_src);
        }
        if let Some(plan) = self.edge_src_port {
            plan.install(&mut ctx.edge_src_port);
        }
        if let Some(plan) = self.edge_dst_port {
            plan.install(&mut ctx.edge_dst_port);
        }
    }
}
