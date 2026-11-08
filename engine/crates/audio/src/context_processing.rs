//! Cycle classification and Delay reader/writer ordering on the existing CSR.
use super::{AudioContext, DESTINATION_NODE_ID};
use crate::nodes::DelayNode;

/// The existing per-node degree allocation also holds bounded DFS scratch.
/// Its complete layout is admitted by the same GraphVec growth/retirement owner.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct NodeState {
    pub(super) degree: u32,
    index: u32,
    low: u32,
    next_edge: u32,
    stacked: bool,
    pub(super) feedback_delay: bool,
    muted: bool,
}

impl AudioContext {
    /// Iterative Tarjan: ready_queue is the DFS stack, processing_order the SCC
    /// stack. Both already have admitted node capacity; no recursion or heap work.
    fn classify_cycles(&mut self, remove_delays: bool) {
        for state in self.in_degree.iter_mut() {
            state.index = u32::MAX;
            state.stacked = false;
        }
        self.processing_order.clear();
        self.ready_queue.clear();
        let mut index = 0;
        for root in 0..self.dense_ids.len() as u32 {
            if self.in_degree[root as usize].index != u32::MAX {
                continue;
            }
            self.enter_cycle_search(root, index);
            index += 1;
            while let Some(&node) = self.ready_queue.last() {
                let n = node as usize;
                let edge = self.in_degree[n].next_edge;
                if edge < self.output_start[n + 1] {
                    self.in_degree[n].next_edge += 1;
                    let next = self.output_edges[edge as usize] as usize;
                    if remove_delays
                        && (self.in_degree[n].feedback_delay || self.in_degree[next].feedback_delay)
                    {
                        continue;
                    }
                    if self.in_degree[next].index == u32::MAX {
                        self.enter_cycle_search(next as u32, index);
                        index += 1;
                    } else if self.in_degree[next].stacked {
                        self.in_degree[n].low =
                            self.in_degree[n].low.min(self.in_degree[next].index);
                    }
                    continue;
                }
                if self.in_degree[n].low == self.in_degree[n].index {
                    let start = self
                        .processing_order
                        .iter()
                        .rposition(|&v| v == node)
                        .unwrap();
                    let self_loop = !(remove_delays && self.in_degree[n].feedback_delay)
                        && self.output_edges
                            [self.output_start[n] as usize..self.output_start[n + 1] as usize]
                            .contains(&node);
                    let cyclic = self.processing_order.len() - start > 1 || self_loop;
                    while self.processing_order.len() > start {
                        let member = self.processing_order.pop().unwrap() as usize;
                        self.in_degree[member].stacked = false;
                        if remove_delays {
                            self.in_degree[member].muted = cyclic;
                        } else {
                            self.in_degree[member].feedback_delay = cyclic
                                && self
                                    .nodes
                                    .get_mut(&self.dense_ids[member])
                                    .unwrap()
                                    .as_any_mut()
                                    .is::<DelayNode>();
                        }
                    }
                }
                self.ready_queue.pop();
                if let Some(&parent) = self.ready_queue.last() {
                    let low = self.in_degree[n].low;
                    self.in_degree[parent as usize].low =
                        self.in_degree[parent as usize].low.min(low);
                }
            }
        }
    }

    fn enter_cycle_search(&mut self, node: u32, index: u32) {
        let state = &mut self.in_degree[node as usize];
        state.index = index;
        state.low = index;
        state.next_edge = self.output_start[node as usize];
        state.stacked = true;
        self.processing_order.push(node);
        self.ready_queue.push(node);
    }

    pub(super) fn order_cycle_aware_graph(&mut self) {
        // The common acyclic graph keeps the original single Kahn pass.
        self.order_graph();
        if self.processing_order.len() == self.dense_ids.len() {
            for id in &self.dense_ids {
                if let Some(delay) = self
                    .nodes
                    .get_mut(id)
                    .unwrap()
                    .as_any_mut()
                    .downcast_mut::<DelayNode>()
                {
                    delay.set_feedback(false);
                }
            }
            return;
        }
        self.classify_cycles(false);
        // Only residual cycles are muted. Descendants of a cycle remain normal
        // nodes and may have other audible inputs; a leftover Kahn degree is not
        // enough to distinguish them from the actual cyclic component.
        self.classify_cycles(true);
        for (index, id) in self.dense_ids.iter().enumerate() {
            if let Some(delay) = self
                .nodes
                .get_mut(id)
                .unwrap()
                .as_any_mut()
                .downcast_mut::<DelayNode>()
            {
                delay.set_feedback(self.in_degree[index].feedback_delay);
            }
        }
        for state in self.in_degree.iter_mut() {
            state.degree = 0;
        }
        for node in 0..self.dense_ids.len() {
            if self.in_degree[node].muted || self.in_degree[node].feedback_delay {
                continue;
            }
            for &next in &self.output_edges
                [self.output_start[node] as usize..self.output_start[node + 1] as usize]
            {
                if !self.in_degree[next as usize].muted {
                    self.in_degree[next as usize].degree += 1;
                }
            }
        }
        self.order_graph();
        debug_assert_eq!(
            self.processing_order.len(),
            self.in_degree.iter().filter(|s| !s.muted).count()
        );
    }

    fn order_graph(&mut self) {
        self.processing_order.clear();
        self.ready_queue.clear();
        let destination = self.dense_index.get(&DESTINATION_NODE_ID).copied();
        for node in 0..self.dense_ids.len() as u32 {
            if !self.in_degree[node as usize].muted
                && self.in_degree[node as usize].degree == 0
                && Some(node) != destination
            {
                self.ready_queue.push(node);
            }
        }
        self.ready_queue.sort_unstable();
        while let Some(node) = self.ready_queue.pop() {
            self.processing_order.push(node);
            if self.in_degree[node as usize].feedback_delay {
                continue;
            }
            for slot in self.output_start[node as usize]..self.output_start[node as usize + 1] {
                let next = self.output_edges[slot as usize];
                let state = &mut self.in_degree[next as usize];
                if state.muted {
                    continue;
                }
                state.degree -= 1;
                if state.degree == 0 && Some(next) != destination {
                    self.ready_queue.push(next);
                }
            }
        }
        if let Some(destination) = destination.filter(|&n| !self.in_degree[n as usize].muted) {
            self.processing_order.push(destination);
        }
    }
}

impl AudioContext {
    pub(super) fn render_graph_block(&mut self, output: &mut [f32]) {
        let buffer_size = output.len();
        let current_time = self.rendered_time();
        let sample_rate = self.sample_rate;

        // Ensure mix buffer is large enough
        if self.mix_buffer.len() < buffer_size {
            self.mix_buffer.resize(buffer_size, 0.0);
        }

        for index in 0..self.dense_ids.len() {
            let state = self.in_degree[index];
            if !state.muted && !state.feedback_delay {
                continue;
            }
            let id = self.dense_ids[index];
            let buffer = self.node_buffers.get_mut(&id).unwrap();
            if buffer.len() < buffer_size {
                buffer.resize(buffer_size, 0.0);
            }
            buffer.as_mut_slice()[..buffer_size].fill(0.0);
            if state.feedback_delay {
                self.nodes
                    .get_mut(&id)
                    .unwrap()
                    .as_any_mut()
                    .downcast_mut::<DelayNode>()
                    .unwrap()
                    .read_feedback(
                        &mut buffer.as_mut_slice()[..buffer_size],
                        sample_rate,
                        current_time,
                    );
            }
        }

        let mix_buffer = self.mix_buffer.as_mut_slice();

        // Process each node in topological order. Indexed rather than iterated so
        // the loop body can borrow the other members mutably.
        let order_len = self.processing_order.len();
        for order_idx in 0..order_len {
            let node = self.processing_order[order_idx];
            let node_id = self.dense_ids[node as usize];

            // Gather mixed input from upstream: a contiguous slice of the CSR
            // edge array, not a map lookup and a pointer chase per node.
            mix_buffer[..buffer_size].fill(0.0);
            let mut has_input = false;
            let input_start = self.input_start[node as usize] as usize;
            let input_end = self.input_start[node as usize + 1] as usize;
            let channels = self.channels.max(1) as usize;
            for slot in input_start..input_end {
                let edge = self.input_edges[slot] as usize;
                let src_id = self.dense_ids[self.edge_src[edge] as usize];
                let Some(src_buf) = self.node_buffers.get(&src_id) else {
                    continue;
                };
                let src_buf = src_buf.as_slice();
                let len = src_buf.len().min(buffer_size);
                match (self.edge_src_port[edge], self.edge_dst_port[edge]) {
                    // The ordinary case: a whole bus mixed into a whole bus.
                    (None, None) => {
                        for i in 0..len {
                            mix_buffer[i] += src_buf[i];
                        }
                    }
                    // From a splitter port: one source channel, as mono, spread
                    // across the destination bus.
                    (Some(from), None) => {
                        let from = from.min(channels - 1);
                        for frame in 0..len / channels {
                            let sample = src_buf[frame * channels + from];
                            for ch in 0..channels {
                                mix_buffer[frame * channels + ch] += sample;
                            }
                        }
                    }
                    // Into a merger port: the source, downmixed to mono, lands in
                    // exactly one destination channel.
                    (from, Some(into)) => {
                        let into = into.min(channels - 1);
                        for frame in 0..len / channels {
                            let base = frame * channels;
                            let sample = match from {
                                Some(from) => src_buf[base + from.min(channels - 1)],
                                None => {
                                    let sum: f32 = src_buf[base..base + channels].iter().sum();
                                    sum / channels as f32
                                }
                            };
                            mix_buffer[base + into] += sample;
                        }
                    }
                }
                has_input = true;
            }

            if self.in_degree[node as usize].feedback_delay {
                self.nodes
                    .get_mut(&node_id)
                    .unwrap()
                    .as_any_mut()
                    .downcast_mut::<DelayNode>()
                    .unwrap()
                    .write_feedback(
                        &mix_buffer[..buffer_size],
                        buffer_size / self.channels.max(1) as usize,
                    );
                continue;
            }

            // Every node is provisioned a render buffer by `add_node`, on the
            // command path, so this is a lookup and never an allocation.
            let Some(node_buf) = self.node_buffers.get_mut(&node_id) else {
                continue;
            };
            if node_buf.len() < buffer_size {
                node_buf.resize(buffer_size, 0.0);
            }
            let node_buf = node_buf.as_mut_slice();
            node_buf[..buffer_size].fill(0.0);

            if let Some(node) = self.nodes.get_mut(&node_id) {
                let input = if has_input {
                    &mix_buffer[..buffer_size]
                } else {
                    &[] as &[f32]
                };

                node.process(
                    input,
                    &mut node_buf[..buffer_size],
                    sample_rate,
                    self.channels,
                    current_time,
                );
            }
        }

        // Add destination node's output to final output (additive for multi-context mixing)
        if let Some(dest_buf) = self.node_buffers.get(&DESTINATION_NODE_ID) {
            let dest_buf = dest_buf.as_slice();
            let len = dest_buf.len().min(buffer_size);
            for i in 0..len {
                output[i] += dest_buf[i];
            }
        }

        // No limiting here on purpose. `output` is the shared mix bus and this
        // method is additive, so a limiter at this point would be applied once per
        // context, in `HashMap` iteration order, over a partial sum that already
        // contains other contexts' audio -- and would still miss the InnerAudio
        // players that mix in afterwards. The audio thread applies one pass over
        // the finished mix instead; see `soft_limit`.
    }
}
