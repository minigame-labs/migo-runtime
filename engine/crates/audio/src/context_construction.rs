//! Construct managed graph data only after the complete initial batch is admitted.
use super::*;
use crate::graph_map::Initial as Table;
use crate::graph_vec::Initial as Vector;
use crate::limits::AudioAggregateLedger;
use crate::node_retirement::{Host, NodeAdmission, Reservation};
use migo_io::audio_cpu::AudioCpu;

struct InitialStorage {
    connections: Vector<NodeConnection>,
    processing_order: Vector<u32>,
    dense_ids: Vector<AudioNodeId>,
    in_degree: Vector<NodeState>,
    input_start: Vector<u32>,
    input_edges: Vector<u32>,
    output_start: Vector<u32>,
    output_edges: Vector<u32>,
    ready_queue: Vector<u32>,
    edge_src: Vector<u32>,
    edge_src_port: Vector<Option<usize>>,
    edge_dst_port: Vector<Option<usize>>,
    collected: Vector<AudioNodeId>,
    ended: Vector<AudioNodeId>,
    mix_buffer: Vector<f32>,
    native_input: Vector<f32>,
    native_collected: Vector<AudioNodeId>,
    buffer_pool: Vector<RenderBuffer>,
    nodes: Table<crate::node_retirement::Node>,
    buffers: Table<crate::node_retirement::Value<Arc<RetainedAudio>>>,
    node_buffers: Table<RenderBuffer>,
    dense_index: Table<u32>,
    released: Table<()>,
    ended_watch: Table<()>,
    destination: NodeAdmission<DestinationNode>,
    render: crate::render_buffer::Initial,
    close: Reservation<ContextStorage>,
}
impl InitialStorage {
    fn reserve(host: &Host, samples: usize) -> EngineResult<Self> {
        Ok(Self {
            connections: Vector::reserve(host, AudioContext::DEFAULT_NODE_CAPACITY)?,
            processing_order: Vector::reserve(host, AudioContext::DEFAULT_NODE_CAPACITY)?,
            dense_ids: Vector::reserve(host, AudioContext::DEFAULT_NODE_CAPACITY)?,
            in_degree: Vector::reserve(host, AudioContext::DEFAULT_NODE_CAPACITY)?,
            input_start: Vector::reserve(host, AudioContext::DEFAULT_NODE_CAPACITY + 1)?,
            input_edges: Vector::reserve(host, AudioContext::DEFAULT_NODE_CAPACITY)?,
            output_start: Vector::reserve(host, AudioContext::DEFAULT_NODE_CAPACITY + 1)?,
            output_edges: Vector::reserve(host, AudioContext::DEFAULT_NODE_CAPACITY)?,
            ready_queue: Vector::reserve(host, AudioContext::DEFAULT_NODE_CAPACITY)?,
            edge_src: Vector::reserve(host, AudioContext::DEFAULT_NODE_CAPACITY)?,
            edge_src_port: Vector::reserve(host, AudioContext::DEFAULT_NODE_CAPACITY)?,
            edge_dst_port: Vector::reserve(host, AudioContext::DEFAULT_NODE_CAPACITY)?,
            collected: Vector::reserve(host, AudioContext::DEFAULT_NODE_CAPACITY)?,
            ended: Vector::reserve(host, 0)?,
            mix_buffer: Vector::reserve(host, samples)?,
            native_input: Vector::reserve(host, samples)?,
            native_collected: Vector::reserve(host, AudioContext::DEFAULT_NODE_CAPACITY)?,
            buffer_pool: Vector::reserve(host, AudioContext::DEFAULT_NODE_CAPACITY)?,
            nodes: Table::reserve(host, AudioContext::DEFAULT_NODE_CAPACITY)?,
            buffers: Table::reserve(host, AudioContext::DEFAULT_BUFFER_CAPACITY)?,
            node_buffers: Table::reserve(host, AudioContext::DEFAULT_NODE_CAPACITY)?,
            dense_index: Table::reserve(host, AudioContext::DEFAULT_NODE_CAPACITY)?,
            released: Table::reserve(host, 1)?,
            ended_watch: Table::reserve(host, 0)?,
            destination: host.reserve_node::<DestinationNode>()?,
            render: crate::render_buffer::Initial::reserve(host, samples)?,
            close: host.reserve_value::<ContextStorage>()?,
        })
    }
}
impl AudioContext {
    pub(crate) fn new_managed(
        id: AudioContextId,
        sample_rate: u32,
        channels: u32,
        cpu: AudioCpu,
        aggregate: Arc<AudioAggregateLedger>,
    ) -> EngineResult<Self> {
        Self::new_managed_with_budget(
            id,
            sample_rate,
            channels,
            cpu,
            aggregate,
            PcmBudget::for_context,
        )
    }
    fn new_managed_with_budget(
        id: AudioContextId,
        sample_rate: u32,
        channels: u32,
        cpu: AudioCpu,
        aggregate: Arc<AudioAggregateLedger>,
        make_budget: impl FnOnce() -> PcmBudget,
    ) -> EngineResult<Self> {
        let samples = DEVICE_QUANTUM_FRAMES
            .checked_mul(channels.max(1) as usize)
            .ok_or_else(crate::node_storage::size_overflow)?;
        let host = Host::new(cpu, aggregate, channels)?;
        let initial = InitialStorage::reserve(&host, samples)?;
        // No fallible admission remains after this point. All Vec/table data,
        // destination Box and render allocation use their own pre-admitted owner.
        let mut context = Self {
            id,
            state: AudioContextState::Running,
            sample_rate,
            channels,
            buffers: initial.buffers.finish(),
            next_buffer_id: 1,
            pcm_budget: make_budget(),
            nodes: initial.nodes.finish(),
            node_retirement: Some(host),
            context_storage: Some(initial.close.finish(ContextStorage::default())),
            released: GraphSet {
                map: initial.released.finish(),
            },
            connections: initial.connections.finish(),
            processing_order: initial.processing_order.finish(),
            graph_dirty: true,
            device_controls_pending: true,
            dense_index: initial.dense_index.finish(),
            dense_ids: initial.dense_ids.finish(),
            in_degree: initial.in_degree.finish(),
            input_start: initial.input_start.finish(),
            input_edges: initial.input_edges.finish(),
            output_start: initial.output_start.finish(),
            output_edges: initial.output_edges.finish(),
            ready_queue: initial.ready_queue.finish(),
            edge_src: initial.edge_src.finish(),
            edge_src_port: initial.edge_src_port.finish(),
            edge_dst_port: initial.edge_dst_port.finish(),
            node_buffers: initial.node_buffers.finish(),
            buffer_pool: initial.buffer_pool.finish(),
            collected: initial.collected.finish(),
            ended_watch: GraphSet {
                map: initial.ended_watch.finish(),
            },
            ended: initial.ended.finish(),
            mix_buffer: initial.mix_buffer.finish(),
            device_epoch: DeviceEpoch::new(),
            native_input: initial.native_input.finish(),
            native_collected: initial.native_collected.finish(),
            frames_processed: 0,
            wall_origin: std::time::Instant::now(),
            wall_banked: 0.0,
            clock_paused: false,
            #[cfg(test)]
            test_wall_enabled: false,
            #[cfg(test)]
            test_dense_order: false,
        };
        context.mix_buffer.resize(samples, 0.0);
        context.nodes.insert(
            DESTINATION_NODE_ID,
            initial
                .destination
                .finish(DestinationNode::new(DESTINATION_NODE_ID, channels)),
        );
        context
            .node_buffers
            .insert(DESTINATION_NODE_ID, initial.render.finish());
        Ok(context)
    }
}
