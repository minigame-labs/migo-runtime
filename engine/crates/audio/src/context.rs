#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;

use shared::audio_resources::AudioSnapshot;
use shared::error::{EngineError, EngineResult, ErrorCode};
use shared::protocol::audio_cmd::{AudioBufferId, AudioContextId, AudioContextState, AudioNodeId};

use crate::decoder::DecodedAudio;
use crate::limits::{PcmBudget, RetainedAudio};
use crate::nodes::{
    AudioNodeProcessor, BufferSourceNode, DestinationNode, GainNode, NodeConnection,
};
use crate::resampler::StreamResampler;

/// Special node ID for the destination node
pub const DESTINATION_NODE_ID: AudioNodeId = 0;

/// An AudioContext manages audio resources and the audio graph.
///
/// Nodes are stored in a generic HashMap and processed in topological order
/// determined by the connection graph. This supports arbitrary node types
/// and connection chains (e.g., source → filter → gain → destination).
pub struct AudioContext {
    id: AudioContextId,
    state: AudioContextState,
    sample_rate: u32,
    channels: u32,

    // Resources
    buffers: HashMap<AudioBufferId, Arc<RetainedAudio>>,
    next_buffer_id: AudioBufferId,
    pcm_budget: PcmBudget,

    // Generic node storage
    nodes: HashMap<AudioNodeId, Box<dyn AudioNodeProcessor>>,

    // Nodes whose JavaScript object has been collected. They are not dropped on
    // the spot: an effect node still carries audio from any source playing
    // through it, and the JS graph becomes unreachable as a whole, so its
    // finalizers can run while a sound is still in flight. See `prune`.
    released: std::collections::HashSet<AudioNodeId>,

    // Graph connections
    connections: Vec<NodeConnection>,

    // Processing order as dense indices into `dense_ids` (cached, invalidated
    // on any graph change).
    processing_order: Vec<u32>,
    graph_dirty: bool,

    // The graph in CSR form, rebuilt whenever it changes.
    //
    // Flat `Vec`s rather than `HashMap<AudioNodeId, Vec<AudioNodeId>>`: clearing a
    // map of vectors hands every inner allocation back, so a rebuild -- which
    // every fired sound effect causes, because an ended source leaves the graph --
    // bought a fresh `Vec` per connected node on the audio thread. Node ids churn,
    // so entry reuse could not have saved it either. These clear without
    // deallocating, and the render path walks a contiguous slice instead of
    // chasing a pointer per node.
    //
    // Nodes are addressed by a dense index assigned per rebuild; `dense_ids` maps
    // back to the id `nodes` and `node_buffers` are keyed by.
    dense_index: HashMap<AudioNodeId, u32>,
    dense_ids: Vec<AudioNodeId>,
    in_degree: Vec<u32>,
    input_start: Vec<u32>,
    /// Edge ids, grouped by destination. Ids rather than source indices, because a
    /// connection now carries port information that the mix step needs.
    input_edges: Vec<u32>,
    output_start: Vec<u32>,
    output_edges: Vec<u32>,
    ready_queue: Vec<u32>,
    /// Per-edge, indexed by edge id: the source, and the ports if they matter.
    /// `None` means "the whole bus", which is every connection except one into or
    /// out of a splitter or merger.
    edge_src: Vec<u32>,
    edge_src_port: Vec<Option<usize>>,
    edge_dst_port: Vec<Option<usize>>,

    // Processing buffers: per-node output buffers keyed by node ID
    node_buffers: HashMap<AudioNodeId, Vec<f32>>,
    // Render buffers of collected nodes, kept for the next node that needs one.
    //
    // A game fires sound effects continuously, and each one is a node added and
    // later collected. Buying its render buffer from the allocator on first
    // render put one allocation per sound effect on the audio thread; recycling
    // makes an add/collect cycle free. Bounded so a burst of simultaneous nodes
    // cannot leave a large pool behind.
    buffer_pool: Vec<Vec<f32>>,
    // Ids collected by the last `prune`, reported to the audio thread so it can
    // clear its node-to-context index. A member rather than a fresh `Vec`:
    // `prune` runs on the render path and collects a node on every quantum a
    // sound effect ends on, so returning an owned vector put one allocation per
    // sound effect on the audio thread.
    collected: Vec<AudioNodeId>,
    // Scratch buffer for mixing multiple inputs
    mix_buffer: Vec<f32>,

    // Context-rate output staging. These buffers and the resampler are retained
    // by the context so a device-rate conversion does not allocate per quantum.
    device_resampler: Option<StreamResampler>,
    resample_input: Vec<f32>,
    resample_output: Vec<f32>,
    resampler_device_rate: u32,

    // Frames processed for sample-accurate currentTime (W3C spec)
    frames_processed: u64,

    // Authoritative wall-clock epoch for idle periods.
    //
    // The native clock (`frames_processed / sr`) only advances when `process()`
    // is called, which only happens when there are active sources. JS's clock
    // advances continuously while the context is Running. This gap means a
    // source scheduled at `currentTime` after a long idle waits for the native
    // clock to catch up — the double-wait bug (AUD-01).
    //
    // Fix: `current_time()` returns `max(wall_elapsed, frames_processed/sr)`.
    // The wall clock starts at context creation and freezes on suspend, so the
    // native clock never lags further behind than one tick cycle (~50 ms).
    // Not present in tests: tests advance `frames_processed` directly so there
    // is no wall-clock dependency, and time is deterministic.
    #[cfg(not(test))]
    wall_origin: std::time::Instant,
    #[cfg(not(test))]
    wall_banked: f64,
    #[cfg(not(test))]
    clock_paused: bool,
}

impl AudioContext {
    /// Default capacity for audio resources.
    const DEFAULT_BUFFER_CAPACITY: usize = 8;
    const DEFAULT_NODE_CAPACITY: usize = 16;

    pub fn new(id: AudioContextId, sample_rate: u32, channels: u32) -> Self {
        Self::new_with_pcm_budget(id, sample_rate, channels, PcmBudget::for_context())
    }

    fn new_with_pcm_budget(
        id: AudioContextId,
        sample_rate: u32,
        channels: u32,
        pcm_budget: PcmBudget,
    ) -> Self {
        let nodes: HashMap<AudioNodeId, Box<dyn AudioNodeProcessor>> =
            HashMap::with_capacity(Self::DEFAULT_NODE_CAPACITY);

        let mut context = Self {
            id,
            state: AudioContextState::Running,
            sample_rate,
            channels,
            buffers: HashMap::with_capacity(Self::DEFAULT_BUFFER_CAPACITY),
            next_buffer_id: 1,
            pcm_budget,
            nodes,
            released: std::collections::HashSet::new(),
            connections: Vec::with_capacity(Self::DEFAULT_NODE_CAPACITY),
            processing_order: Vec::with_capacity(Self::DEFAULT_NODE_CAPACITY),
            graph_dirty: true,
            dense_index: HashMap::with_capacity(Self::DEFAULT_NODE_CAPACITY),
            dense_ids: Vec::with_capacity(Self::DEFAULT_NODE_CAPACITY),
            in_degree: Vec::with_capacity(Self::DEFAULT_NODE_CAPACITY),
            input_start: Vec::with_capacity(Self::DEFAULT_NODE_CAPACITY + 1),
            input_edges: Vec::with_capacity(Self::DEFAULT_NODE_CAPACITY),
            output_start: Vec::with_capacity(Self::DEFAULT_NODE_CAPACITY + 1),
            output_edges: Vec::with_capacity(Self::DEFAULT_NODE_CAPACITY),
            ready_queue: Vec::with_capacity(Self::DEFAULT_NODE_CAPACITY),
            edge_src: Vec::with_capacity(Self::DEFAULT_NODE_CAPACITY),
            edge_src_port: Vec::with_capacity(Self::DEFAULT_NODE_CAPACITY),
            edge_dst_port: Vec::with_capacity(Self::DEFAULT_NODE_CAPACITY),
            node_buffers: HashMap::with_capacity(Self::DEFAULT_NODE_CAPACITY),
            buffer_pool: Vec::with_capacity(Self::DEFAULT_NODE_CAPACITY),
            collected: Vec::with_capacity(Self::DEFAULT_NODE_CAPACITY),
            mix_buffer: Vec::new(),
            device_resampler: None,
            resample_input: Vec::with_capacity(4096 * channels.max(1) as usize),
            resample_output: Vec::with_capacity(4096 * channels.max(1) as usize),
            resampler_device_rate: 0,
            frames_processed: 0,
            #[cfg(not(test))]
            wall_origin: std::time::Instant::now(),
            #[cfg(not(test))]
            wall_banked: 0.0,
            #[cfg(not(test))]
            clock_paused: false,
        };

        // The destination goes through the same insertion path as every other
        // node, so it is provisioned a render buffer like the rest.
        context.add_node(Box::new(DestinationNode::new(
            DESTINATION_NODE_ID,
            channels,
        )));
        context
    }

    pub fn id(&self) -> AudioContextId {
        self.id
    }

    pub fn state(&self) -> AudioContextState {
        self.state
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn channels(&self) -> u32 {
        self.channels
    }

    /// Get the authoritative context time in seconds.
    ///
    /// Rendered frames remain the source of truth while the graph is active.
    /// During logical idle the render loop is asleep, so no frames are rendered
    /// to advance that counter. The idle epoch fills that gap and keeps native
    /// scheduling aligned with JS's running clock; suspension and background
    /// pause bank the epoch before stopping it.
    pub fn current_time(&self) -> f64 {
        let rendered = self.frames_processed as f64 / self.sample_rate.max(1) as f64;
        #[cfg(not(test))]
        {
            let idle = self.wall_banked
                + if self.state == AudioContextState::Running && !self.clock_paused {
                    self.wall_origin.elapsed().as_secs_f64()
                } else {
                    0.0
                };
            return rendered.max(idle);
        }
        #[cfg(test)]
        {
            rendered
        }
    }

    #[cfg(not(test))]
    fn bank_wall_clock(&mut self) {
        if self.state == AudioContextState::Running {
            self.wall_banked += self.wall_origin.elapsed().as_secs_f64();
            self.wall_origin = std::time::Instant::now();
        }
    }
    pub fn pause_clock_for_background(&mut self) {
        #[cfg(not(test))]
        {
            self.bank_wall_clock();
            self.clock_paused = true;
        }
    }

    pub fn resume_clock_after_background(&mut self) {
        #[cfg(not(test))]
        if self.clock_paused {
            self.wall_origin = std::time::Instant::now();
            self.clock_paused = false;
        }
    }

    pub fn suspend(&mut self) {
        #[cfg(not(test))]
        self.bank_wall_clock();
        self.state = AudioContextState::Suspended;
    }

    pub fn resume(&mut self) {
        if self.state == AudioContextState::Suspended {
            #[cfg(not(test))]
            {
                self.wall_origin = std::time::Instant::now();
            }
            self.state = AudioContextState::Running;
        }
    }

    fn advance_rendered_frames_for_test(&mut self, frames: u64) {
        if self.state == AudioContextState::Running {
            self.frames_processed = self.frames_processed.saturating_add(frames);
        }
    }

    pub fn close(&mut self) {
        #[cfg(not(test))]
        self.bank_wall_clock();
        self.state = AudioContextState::Closed;
        self.buffers.clear();
        self.nodes.clear();
        self.released.clear();
        self.connections.clear();
        self.processing_order.clear();
        self.in_degree.clear();
        self.ready_queue.clear();
        self.node_buffers.clear();
        self.collected.clear();
    }

    /// Check if this context has any active source nodes (playing or scheduled).
    ///
    /// Used by the power manager to avoid keeping the audio thread at high
    /// tick rate when a context is Running but has no audio to produce.
    pub fn has_active_sources(&self) -> bool {
        if self.state != AudioContextState::Running {
            return false;
        }
        self.nodes.values().any(|n| n.is_producing())
    }

    /// Mark a node as unreachable from JavaScript, and collect whatever that
    /// makes collectible right now.
    ///
    /// Idempotent and tolerant of unknown ids, because the caller is a GC
    /// finalizer. The destination is never releasable -- it is created with the
    /// context and belongs to it.
    ///
    /// The release is deliberately a *request*. Removing the node here would cut
    /// off audio: JavaScript drops a `source -> gain -> destination` chain as one
    /// unreachable object graph, so `gain`'s finalizer can run while the source
    /// is still playing through it. `prune` decides when it is actually safe.
    pub fn release_node(&mut self, node_id: AudioNodeId) -> &[AudioNodeId] {
        if node_id == DESTINATION_NODE_ID || !self.nodes.contains_key(&node_id) {
            self.collected.clear();
            return &self.collected;
        }
        self.released.insert(node_id);
        self.prune()
    }

    /// Drop every node that can no longer affect the output, and report the ids
    /// so the audio thread can clear its node-to-context index.
    ///
    /// Two rules, one pass. A **finished** source goes the moment it finishes. A
    /// **released** node goes once nothing upstream can still feed it, which is
    /// what keeps a `gain` alive for exactly as long as the source playing
    /// through it and no longer.
    ///
    /// The loop is what makes a chain work: releasing `source -> gain -> filter`
    /// can only collect the filter after the gain it reads from is gone, so each
    /// pass peels one layer. It terminates because every pass removes a node.
    fn prune(&mut self) -> &[AudioNodeId] {
        self.collected.clear();
        loop {
            let mut victim = None;
            for (&id, node) in self.nodes.iter() {
                if id == DESTINATION_NODE_ID {
                    continue;
                }
                let collectible = node.is_finished()
                    || (self.released.contains(&id)
                        && !node.is_producing()
                        && !self.has_live_active_input(id));
                if collectible {
                    victim = Some(id);
                    break;
                }
            }
            let Some(id) = victim else { break };
            self.nodes.remove(&id);
            self.recycle_render_buffer(id);
            self.released.remove(&id);
            self.connections.retain(|c| c.src != id && c.dst != id);
            self.collected.push(id);
        }
        if !self.collected.is_empty() {
            // The order and the CSR adjacency are rebuilt on the next quantum.
            self.graph_dirty = true;
        }
        &self.collected
    }

    /// Whether a node has an upstream node that can still emit audio. A
    /// released zero-state node is still a reachability barrier while it has
    /// an unreleased ancestor: this preserves the existing one-prune chain
    /// invariant. A fully released cycle has no such ancestor and is reclaimed.
    fn has_live_active_input(&self, node_id: AudioNodeId) -> bool {
        self.connections.iter().any(|c| {
            if c.dst != node_id {
                return false;
            }
            let Some(src) = self.nodes.get(&c.src) else {
                return false;
            };
            !self.released.contains(&c.src)
                || src.is_producing()
                || self.has_live_ancestor(c.src, self.nodes.len())
        })
    }

    /// Follow released, zero-state upstream nodes without allocating a
    /// temporary visited set. The node-count bound terminates legal cycles;
    /// an ancestor outside the released subgraph makes the whole chain live.
    fn has_live_ancestor(&self, node_id: AudioNodeId, remaining: usize) -> bool {
        if remaining == 0 {
            return false;
        }
        self.connections.iter().any(|c| {
            if c.dst != node_id {
                return false;
            }
            let Some(src) = self.nodes.get(&c.src) else {
                return false;
            };
            !self.released.contains(&c.src)
                || src.is_producing()
                || self.has_live_ancestor(src.id(), remaining - 1)
        })
    }

    // ==================== Buffer Management ====================

    pub fn add_buffer(&mut self, audio: DecodedAudio) -> EngineResult<AudioBufferId> {
        let (id, next_id) = self.buffer_id_candidate()?;
        let retained = RetainedAudio::try_new(audio, &self.pcm_budget)?;
        self.buffers.insert(id, Arc::new(retained));
        self.next_buffer_id = next_id;
        Ok(id)
    }

    pub fn get_buffer(&self, id: AudioBufferId) -> Option<Arc<RetainedAudio>> {
        self.buffers.get(&id).cloned()
    }

    pub fn remove_buffer(&mut self, id: AudioBufferId) -> bool {
        self.buffers.remove(&id).is_some()
    }

    fn buffer_id_candidate(&self) -> EngineResult<(AudioBufferId, AudioBufferId)> {
        let id = self.next_buffer_id;
        if id >= i32::MAX as AudioBufferId {
            return Err(shared::error::EngineError::from_detail(
                shared::error::ErrorCode::InputSaturated,
                "AudioBuffer id space exhausted at the V8 Smi boundary",
            ));
        }
        let next_id = id.checked_add(1).ok_or_else(|| {
            shared::error::EngineError::from_detail(
                shared::error::ErrorCode::InputSaturated,
                "AudioBuffer id space exhausted",
            )
        })?;
        if self.buffers.contains_key(&id) {
            return Err(shared::error::EngineError::from_detail(
                shared::error::ErrorCode::InputSaturated,
                format!("AudioBuffer id {id} is still in use"),
            ));
        }
        Ok((id, next_id))
    }

    /// Create an empty buffer with the given parameters.
    ///
    /// Validates channel count, sample rate and total PCM size (with checked
    /// arithmetic) before allocating, so an overflowing or budget-busting
    /// request is rejected instead of panicking / wrapping to a bogus size.
    pub fn create_empty_buffer(
        &mut self,
        channels: u32,
        length: u32,
        sample_rate: u32,
    ) -> EngineResult<AudioBufferId> {
        let bytes = crate::limits::validated_buffer_alloc_bytes(channels, length, sample_rate)?;
        let (id, next_id) = self.buffer_id_candidate()?;
        // Claim both aggregate budgets before asking the allocator for the Vec.
        let mut permit = self.pcm_budget.reserve(bytes)?;
        let sample_count = bytes / std::mem::size_of::<f32>();
        let samples = crate::limits::try_allocate_zeroed_pcm(sample_count)?;
        let capacity_bytes = crate::limits::pcm_bytes(samples.capacity())?;
        permit.try_grow_to(capacity_bytes)?;
        let audio = DecodedAudio {
            samples,
            sample_rate,
            channels,
        };
        self.buffers
            .insert(id, Arc::new(RetainedAudio::from_reserved(audio, permit)));
        self.next_buffer_id = next_id;
        Ok(id)
    }

    /// Number of channels in a buffer (0 if not found).
    pub fn buffer_channels(&self, buffer_id: AudioBufferId) -> Option<u32> {
        self.buffers.get(&buffer_id).map(|b| b.channels)
    }

    /// Get raw channel data from a buffer
    pub fn get_channel_data(&self, buffer_id: AudioBufferId, channel: u32) -> Option<Vec<f32>> {
        let buffer = self.buffers.get(&buffer_id)?;
        if channel >= buffer.channels {
            return None;
        }
        let frame_count = buffer.frame_count();
        let channels = buffer.channels as usize;
        let ch = channel as usize;
        let mut data = Vec::with_capacity(frame_count);
        for frame in 0..frame_count {
            data.push(buffer.samples[frame * channels + ch]);
        }
        Some(data)
    }

    /// Return all channels in one flat channel-major vector.
    ///
    /// The native buffer remains interleaved; this allocates exactly one
    /// result vector and fills it without constructing one `Vec` per channel.
    /// Reservation is fallible so malformed dimensions or allocator failure
    /// become structured errors before any output is built.
    pub fn take_decoded_buffer_data(
        &mut self,
        buffer_id: AudioBufferId,
    ) -> EngineResult<Option<Vec<f32>>> {
        let buffer = match self.buffers.remove(&buffer_id) {
            Some(buffer) => buffer,
            None => return Ok(None),
        };
        let channels = usize::try_from(buffer.channels).map_err(|_| {
            EngineError::from_detail(
                ErrorCode::InvalidArgument,
                "channel count does not fit usize",
            )
        })?;
        let frames = buffer.frame_count();
        let sample_count = channels.checked_mul(frames).ok_or_else(|| {
            EngineError::from_detail(
                ErrorCode::InvalidArgument,
                "audio channel data sample count overflow",
            )
        })?;

        let mut output = Vec::new();
        output.try_reserve_exact(sample_count).map_err(|_| {
            EngineError::from_detail(
                ErrorCode::OutOfMemory,
                "audio channel data allocation failed",
            )
        })?;
        for channel in 0..channels {
            for frame in 0..frames {
                let sample_index = frame
                    .checked_mul(channels)
                    .and_then(|base| base.checked_add(channel))
                    .ok_or_else(|| {
                        EngineError::from_detail(
                            ErrorCode::InvalidArgument,
                            "audio channel data index overflow",
                        )
                    })?;
                output.push(buffer.samples[sample_index]);
            }
        }
        Ok(Some(output))
    }

    /// Copy data into a specific channel of a buffer (copy-on-write via Arc::make_mut).
    pub fn copy_to_channel(
        &mut self,
        buffer_id: AudioBufferId,
        data: &[f32],
        channel: u32,
        start_frame: u32,
    ) -> EngineResult<bool> {
        let pcm_budget = self.pcm_budget.clone();
        let buffer = match self.buffers.get_mut(&buffer_id) {
            Some(b) => b,
            None => return Ok(false),
        };

        if channel >= buffer.channels {
            return Ok(false);
        }

        let channels = buffer.channels as usize;
        let frame_count = buffer.frame_count();
        let start = start_frame as usize;
        let ch = channel as usize;

        // Clamp copy length to buffer bounds
        let copy_len = data.len().min(frame_count.saturating_sub(start));
        if copy_len == 0 {
            return Ok(true); // Nothing to copy, but not an error
        }

        // A source may hold the old Arc. Reserve and allocate the COW copy
        // fallibly before replacing the map entry, keeping the old PCM intact
        // if either aggregate budget refuses the duplicate.
        if Arc::get_mut(buffer).is_none() {
            let cloned = buffer.try_clone_with_budget(&pcm_budget)?;
            *buffer = Arc::new(cloned);
        }
        let audio = Arc::get_mut(buffer)
            .ok_or_else(|| {
                shared::error::EngineError::from_detail(
                    shared::error::ErrorCode::Internal,
                    "retained PCM did not become uniquely owned after copy-on-write",
                )
            })?
            .audio_mut();

        for i in 0..copy_len {
            let sample_idx = (start + i) * channels + ch;
            if sample_idx < audio.samples.len() {
                audio.samples[sample_idx] = data[i];
            }
        }

        Ok(true)
    }

    // ==================== Node Management ====================

    /// Create a buffer source node with JS-provided node_id
    pub fn create_buffer_source(&mut self, node_id: AudioNodeId) {
        self.add_node(Box::new(BufferSourceNode::new(node_id, self.sample_rate)));
    }

    /// Create a gain node with JS-provided node_id
    pub fn create_gain(&mut self, node_id: AudioNodeId) {
        self.add_node(Box::new(GainNode::new(node_id)));
    }

    /// The single insertion point for every node type.
    ///
    /// Provisioning the render buffer here rather than on first render is what
    /// keeps the allocator off the audio thread: this runs on the command path,
    /// where an allocation is a throughput cost, not a missed deadline.
    pub fn add_node(&mut self, node: Box<dyn AudioNodeProcessor>) {
        let node_id = node.id();
        let buffer = self.take_render_buffer();
        self.nodes.insert(node_id, node);
        self.node_buffers.insert(node_id, buffer);
        self.graph_dirty = true;
    }

    /// A zeroed render buffer, recycled from a collected node when one is spare.
    fn take_render_buffer(&mut self) -> Vec<f32> {
        let samples = crate::audio_thread::RENDER_QUANTUM_FRAMES * self.channels.max(1) as usize;
        match self.buffer_pool.pop() {
            Some(mut buffer) => {
                buffer.clear();
                buffer.resize(samples, 0.0);
                buffer
            }
            None => vec![0.0f32; samples],
        }
    }

    /// Take a collected node's render buffer back for reuse.
    fn recycle_render_buffer(&mut self, node_id: AudioNodeId) {
        if let Some(buffer) = self.node_buffers.remove(&node_id) {
            if self.buffer_pool.len() < Self::DEFAULT_NODE_CAPACITY {
                self.buffer_pool.push(buffer);
            }
        }
    }

    /// Downcast and operate on a specific node type.
    /// Used for type-specific operations (e.g., set_buffer on BufferSourceNode).
    pub fn with_node_typed<T: AudioNodeProcessor + 'static, R>(
        &mut self,
        node_id: AudioNodeId,
        f: impl FnOnce(&mut T) -> R,
    ) -> Option<R> {
        let node = self.nodes.get_mut(&node_id)?;
        // Use Any for downcasting
        let any = node.as_any_mut();
        let typed = any.downcast_mut::<T>()?;
        Some(f(typed))
    }

    pub fn set_buffer(&mut self, node_id: AudioNodeId, buffer_id: Option<AudioBufferId>) -> bool {
        tracing::trace!("set_buffer: node_id={}, buffer_id={buffer_id:?}", node_id);
        let buffer = match buffer_id {
            Some(buffer_id) => match self.buffers.get(&buffer_id) {
                Some(buffer) => Some(Arc::clone(buffer)),
                None => {
                    tracing::warn!("set_buffer: buffer {} not found", buffer_id);
                    return false;
                }
            },
            None => None,
        };

        if let Some(node) = self.nodes.get_mut(&node_id) {
            let any = node.as_any_mut();
            if let Some(source) = any.downcast_mut::<BufferSourceNode>() {
                source.set_buffer(buffer);
                return true;
            }
        }
        tracing::warn!("set_buffer: source node {} not found", node_id);
        false
    }

    /// Replace or clear the JS-owned snapshot on a source node without
    /// copying its interleaved PCM.
    pub fn set_started_buffer(
        &mut self,
        node_id: AudioNodeId,
        buffer: Option<Arc<AudioSnapshot>>,
    ) -> bool {
        if let Some(node) = self.nodes.get_mut(&node_id) {
            let any = node.as_any_mut();
            if let Some(source) = any.downcast_mut::<BufferSourceNode>() {
                source.set_snapshot(buffer);
                return true;
            }
        }
        false
    }

    pub fn start_source(
        &mut self,
        node_id: AudioNodeId,
        when: f64,
        offset: f64,
        duration: Option<f64>,
    ) -> bool {
        tracing::trace!(
            "start_source: node_id={}, when={}, offset={}, duration={:?}",
            node_id,
            when,
            offset,
            duration
        );
        let current_time = self.current_time();
        if let Some(node) = self.nodes.get_mut(&node_id) {
            let any = node.as_any_mut();
            if let Some(source) = any.downcast_mut::<BufferSourceNode>() {
                source.start(when, offset, duration, current_time);
                tracing::trace!("start_source: node started");
                return true;
            }
        }
        tracing::warn!("start_source: node {} not found", node_id);
        false
    }

    pub fn stop_source(&mut self, node_id: AudioNodeId, when: f64) -> bool {
        if let Some(node) = self.nodes.get_mut(&node_id) {
            let any = node.as_any_mut();
            if let Some(source) = any.downcast_mut::<BufferSourceNode>() {
                source.stop(when);
                return true;
            }
        }
        false
    }

    /// If the node exists and has already finished (e.g. `stop(when <= 0)`
    /// finishes a buffer source immediately), drop it and everything that
    /// becomes collectible with it, and return `true`.
    ///
    /// Lets the audio thread fully clean up an immediately-finished node now,
    /// rather than waiting for the next `process()` sweep — which never runs
    /// while the context is suspended.
    pub fn remove_finished_node(&mut self, node_id: AudioNodeId) -> &[AudioNodeId] {
        let finished = self.nodes.get(&node_id).is_some_and(|n| n.is_finished());
        if !finished {
            self.collected.clear();
            return &self.collected;
        }
        self.prune()
    }

    pub fn set_loop(&mut self, node_id: AudioNodeId, enabled: bool, start: f64, end: f64) -> bool {
        if let Some(node) = self.nodes.get_mut(&node_id) {
            let any = node.as_any_mut();
            if let Some(source) = any.downcast_mut::<BufferSourceNode>() {
                source.set_loop(enabled, start, end);
                return true;
            }
        }
        false
    }

    pub fn set_playback_rate(&mut self, node_id: AudioNodeId, rate: f32) -> bool {
        if let Some(node) = self.nodes.get_mut(&node_id) {
            let any = node.as_any_mut();
            if let Some(source) = any.downcast_mut::<BufferSourceNode>() {
                source.set_playback_rate(rate);
                return true;
            }
        }
        false
    }

    pub fn set_gain(&mut self, node_id: AudioNodeId, value: f32) -> bool {
        if let Some(node) = self.nodes.get_mut(&node_id) {
            let any = node.as_any_mut();
            if let Some(gain) = any.downcast_mut::<GainNode>() {
                gain.set_gain(value);
                return true;
            }
        }
        false
    }

    /// Set an AudioParam value on a node by name
    pub fn set_node_param(&mut self, node_id: AudioNodeId, param_name: &str, value: f32) -> bool {
        let now = self.current_time();
        if let Some(node) = self.nodes.get_mut(&node_id) {
            if let Some(param) = node.get_param_mut(param_name) {
                param.set_value_now(value, now);
                return true;
            }
        }
        false
    }

    /// Schedule AudioParam automation on a node
    pub fn param_set_value_at_time(
        &mut self,
        node_id: AudioNodeId,
        param_name: &str,
        value: f32,
        time: f64,
    ) -> bool {
        let now = self.current_time();
        if let Some(node) = self.nodes.get_mut(&node_id) {
            if let Some(param) = node.get_param_mut(param_name) {
                param.set_value_at_time(value, time);
                param.gc_events(now);
                return true;
            }
        }
        false
    }

    pub fn param_linear_ramp(
        &mut self,
        node_id: AudioNodeId,
        param_name: &str,
        value: f32,
        end_time: f64,
    ) -> bool {
        let now = self.current_time();
        if let Some(node) = self.nodes.get_mut(&node_id) {
            if let Some(param) = node.get_param_mut(param_name) {
                param.linear_ramp_to_value_at_time(value, end_time);
                param.gc_events(now);
                return true;
            }
        }
        false
    }

    pub fn param_exponential_ramp(
        &mut self,
        node_id: AudioNodeId,
        param_name: &str,
        value: f32,
        end_time: f64,
    ) -> bool {
        let now = self.current_time();
        if let Some(node) = self.nodes.get_mut(&node_id) {
            if let Some(param) = node.get_param_mut(param_name) {
                param.exponential_ramp_to_value_at_time(value, end_time);
                param.gc_events(now);
                return true;
            }
        }
        false
    }

    pub fn param_set_target(
        &mut self,
        node_id: AudioNodeId,
        param_name: &str,
        target: f32,
        start_time: f64,
        time_constant: f64,
    ) -> bool {
        let now = self.current_time();
        if let Some(node) = self.nodes.get_mut(&node_id) {
            if let Some(param) = node.get_param_mut(param_name) {
                param.set_target_at_time(target, start_time, time_constant);
                param.gc_events(now);
                return true;
            }
        }
        false
    }

    pub fn param_cancel_scheduled(
        &mut self,
        node_id: AudioNodeId,
        param_name: &str,
        cancel_time: f64,
    ) -> bool {
        if let Some(node) = self.nodes.get_mut(&node_id) {
            if let Some(param) = node.get_param_mut(param_name) {
                param.cancel_scheduled_values(cancel_time);
                return true;
            }
        }
        false
    }

    // ==================== Graph ====================

    pub fn connect(&mut self, src: AudioNodeId, dst: AudioNodeId) {
        self.connect_ports(src, 0, dst, 0);
    }

    /// Connect one output port of `src` to one input port of `dst`.
    ///
    /// The ports are what make a splitter and a merger mean anything: a splitter
    /// has one port per channel, and a merger accepts one channel per port. A
    /// duplicate is keyed on the whole quadruple, so the same pair of nodes can be
    /// wired through several different ports.
    pub fn connect_ports(
        &mut self,
        src: AudioNodeId,
        src_output: u32,
        dst: AudioNodeId,
        dst_input: u32,
    ) {
        let duplicate = self.connections.iter().any(|c| {
            c.src == src && c.dst == dst && c.src_output == src_output && c.dst_input == dst_input
        });
        if !duplicate {
            self.connections.push(NodeConnection {
                src,
                src_output,
                dst,
                dst_input,
            });
            self.graph_dirty = true;
        }
    }

    pub fn disconnect(&mut self, node_id: AudioNodeId, dst: Option<AudioNodeId>) {
        let old_len = self.connections.len();
        self.connections.retain(|c| {
            if let Some(dst_id) = dst {
                !(c.src == node_id && c.dst == dst_id)
            } else {
                c.src != node_id
            }
        });
        if self.connections.len() != old_len {
            self.graph_dirty = true;
        }
    }

    /// Rebuild the processing order (Kahn's algorithm) and the CSR adjacency the
    /// render loop reads its inputs from.
    ///
    /// **Runs on the audio thread and must not allocate.** It is triggered by any
    /// graph change, and one fired sound effect is a graph change -- the source is
    /// dropped when it ends -- so this runs constantly during ordinary play. It
    /// used to build two fresh `HashMap<AudioNodeId, Vec<_>>` and a `Vec` every
    /// time; the steady-state allocation gate could not see it, because the graph
    /// it renders is an oscillator that never finishes and so never has to be
    /// re-sorted. Every collection here is a member buffer cleared and refilled,
    /// and none of them owns a nested allocation that clearing would hand back.
    fn rebuild_processing_order(&mut self) {
        let node_count = self.nodes.len();

        // Address nodes by a dense index, so the graph is an array and not a map.
        self.dense_index.clear();
        self.dense_ids.clear();
        for &node_id in self.nodes.keys() {
            self.dense_index
                .insert(node_id, self.dense_ids.len() as u32);
            self.dense_ids.push(node_id);
        }

        // Count edges per endpoint, then prefix-sum into CSR starts. Counting
        // first is what lets the edge arrays be filled in place afterwards.
        self.input_start.clear();
        self.input_start.resize(node_count + 1, 0);
        self.output_start.clear();
        self.output_start.resize(node_count + 1, 0);
        self.edge_src.clear();
        self.edge_src_port.clear();
        self.edge_dst_port.clear();
        for conn in &self.connections {
            let (Some(&src), Some(&dst)) = (
                self.dense_index.get(&conn.src),
                self.dense_index.get(&conn.dst),
            ) else {
                continue;
            };
            self.input_start[dst as usize + 1] += 1;
            self.output_start[src as usize + 1] += 1;
            // A port index only means anything when the node actually has more
            // than one, so single-port nodes keep the whole-bus behaviour and pay
            // nothing for the feature.
            let src_port = self
                .nodes
                .get(&conn.src)
                .filter(|node| node.output_ports() > 1)
                .map(|_| conn.src_output as usize);
            let dst_port = self
                .nodes
                .get(&conn.dst)
                .filter(|node| node.input_ports() > 1)
                .map(|_| conn.dst_input as usize);
            self.edge_src.push(src);
            self.edge_src_port.push(src_port);
            self.edge_dst_port.push(dst_port);
        }
        let edge_count = self.edge_src.len();
        for i in 0..node_count {
            self.input_start[i + 1] += self.input_start[i];
            self.output_start[i + 1] += self.output_start[i];
        }

        // Fill both edge arrays in one pass. `in_degree` is the in-edge fill
        // cursor and ends up holding each node's real in-degree, which is exactly
        // what the traversal below needs. `processing_order` is borrowed as the
        // out-edge cursor rather than adding a seventh member for it, and is
        // cleared before it takes its own meaning.
        self.input_edges.clear();
        self.input_edges.resize(edge_count, 0);
        self.output_edges.clear();
        self.output_edges.resize(edge_count, 0);
        self.in_degree.clear();
        self.in_degree.resize(node_count, 0);
        self.processing_order.clear();
        self.processing_order.resize(node_count, 0);
        let mut edge_id = 0u32;
        for conn in &self.connections {
            let (Some(&src), Some(&dst)) = (
                self.dense_index.get(&conn.src),
                self.dense_index.get(&conn.dst),
            ) else {
                continue;
            };
            let in_slot =
                self.input_start[dst as usize] as usize + self.in_degree[dst as usize] as usize;
            self.input_edges[in_slot] = edge_id;
            self.in_degree[dst as usize] += 1;

            let out_slot = self.output_start[src as usize] as usize
                + self.processing_order[src as usize] as usize;
            self.output_edges[out_slot] = dst;
            self.processing_order[src as usize] += 1;
            edge_id += 1;
        }

        // Kahn's algorithm. The destination is held back unconditionally so it is
        // always rendered last, after every contributor has written its buffer.
        let destination = self.dense_index.get(&DESTINATION_NODE_ID).copied();
        self.processing_order.clear();
        self.ready_queue.clear();
        for node in 0..node_count as u32 {
            if self.in_degree[node as usize] == 0 && Some(node) != destination {
                self.ready_queue.push(node);
            }
        }
        // Deterministic order for a deterministic mix.
        self.ready_queue.sort_unstable();

        while let Some(node) = self.ready_queue.pop() {
            self.processing_order.push(node);
            let start = self.output_start[node as usize] as usize;
            let end = self.output_start[node as usize + 1] as usize;
            for slot in start..end {
                let next = self.output_edges[slot];
                let degree = &mut self.in_degree[next as usize];
                *degree = degree.saturating_sub(1);
                if *degree == 0 && Some(next) != destination {
                    self.ready_queue.push(next);
                }
            }
        }

        // Anything with in-degree left over sits on a cycle.
        //
        // A cycle is legal Web Audio -- a feedback delay is the canonical effect --
        // and dropping those nodes from the order made them render nothing at all:
        // the graph went silent instead of echoing, and because they never ran they
        // never finished, so they were never collected either. There is no
        // topological order inside a cycle by definition, so they are appended in
        // index order and read their inputs from the previous quantum's buffers --
        // exactly the one-quantum delay the spec requires a cycle to contain.
        let placed = self.processing_order.len() + usize::from(destination.is_some());
        if placed < node_count {
            for node in 0..node_count as u32 {
                if self.in_degree[node as usize] > 0 && Some(node) != destination {
                    self.processing_order.push(node);
                }
            }
        }

        if let Some(destination) = destination {
            self.processing_order.push(destination);
        }
        self.graph_dirty = false;
    }

    /// Drop route-specific resampler state after a device recovery.
    ///
    /// A new output may use a different device rate; retaining the old
    /// resampler would carry its phase and layout into the new route.
    pub fn renegotiate_device(&mut self, _device_rate: u32, _device_channels: u32) {
        self.device_resampler = None;
        self.resampler_device_rate = 0;
        self.resample_output.clear();
    }

    /// Render a context-rate block and convert it into the device-rate mix bus.
    ///
    /// A context's clock advances by the number of frames rendered at its own
    /// sample rate. The resampler then maps those frames to the device quantum;
    /// passing the device quantum directly to `process` is what made 44.1 kHz
    /// contexts run 48/44.1 fast on a 48 kHz route.
    pub fn process_for_device(
        &mut self,
        output: &mut [f32],
        device_rate: u32,
        device_channels: u32,
    ) -> &[AudioNodeId] {
        let device_rate = device_rate.max(1);
        let device_channels = device_channels.max(1) as usize;
        let context_channels = self.channels.max(1) as usize;
        let device_frames = output.len() / device_channels;
        if self.sample_rate == device_rate {
            return self.process(output);
        }

        let context_frames = ((device_frames as u64)
            .saturating_mul(self.sample_rate.max(1) as u64)
            .saturating_add(device_rate as u64 - 1)
            / device_rate as u64) as usize
            + 2;
        let context_samples = context_frames.saturating_mul(context_channels);
        let mut context_input = std::mem::take(&mut self.resample_input);
        if context_input.len() < context_samples {
            context_input.resize(context_samples, 0.0);
        }
        context_input[..context_samples].fill(0.0);
        let _ = self.process(&mut context_input[..context_samples]);

        if self.resampler_device_rate != device_rate {
            self.device_resampler = Some(StreamResampler::new(
                self.sample_rate,
                device_rate,
                self.channels,
            ));
            self.resampler_device_rate = device_rate;
        }
        let resampler = self
            .device_resampler
            .as_mut()
            .expect("resampler initialized");
        self.resample_output.clear();
        resampler.process_into(&context_input[..context_samples], &mut self.resample_output);

        // Resampling preserves context channels; map those channels to the
        // recovered device layout without duplicating stale device state.
        let source_frames = self.resample_output.len() / context_channels;
        let frames = source_frames.min(device_frames);
        for frame in 0..frames {
            let src = frame * context_channels;
            let dst = frame * device_channels;
            if context_channels == 1 {
                for ch in 0..device_channels {
                    output[dst + ch] += self.resample_output[src];
                }
            } else if device_channels == 1 {
                let sum: f32 = self.resample_output[src..src + context_channels]
                    .iter()
                    .copied()
                    .sum();
                output[dst] += sum / context_channels as f32;
            } else {
                for ch in 0..context_channels.min(device_channels) {
                    output[dst + ch] += self.resample_output[src + ch];
                }
            }
        }
        self.resample_input = context_input;
        &self.collected
    }

    // ==================== Processing ====================

    /// Process audio and **add** to the output buffer.
    ///
    /// Uses topological sort to process nodes in dependency order.
    /// Each node receives its upstream mixed inputs and writes to its own buffer.
    /// The destination node's output is **added** (not copied) to the final output,
    /// allowing multiple AudioContexts to be mixed together by the caller.
    ///
    /// The caller must zero the output buffer before the first context's process() call.
    ///
    /// Returns the ids of source nodes that finished during this block, so the
    /// audio thread can drop its `node → context` index entries for them.
    pub fn process(&mut self, output: &mut [f32]) -> &[AudioNodeId] {
        if self.state != AudioContextState::Running {
            // Don't touch output — other contexts may have already written to it
            self.collected.clear();
            return &self.collected;
        }

        // Rebuild processing order if graph changed
        if self.graph_dirty {
            self.rebuild_processing_order();
        }

        let buffer_size = output.len();
        let current_time = self.current_time();
        let sample_rate = self.sample_rate;

        // Ensure mix buffer is large enough
        if self.mix_buffer.len() < buffer_size {
            self.mix_buffer.resize(buffer_size, 0.0);
        }

        // Process each node in topological order. Indexed rather than iterated so
        // the loop body can borrow the other members mutably.
        let order_len = self.processing_order.len();
        for order_idx in 0..order_len {
            let node = self.processing_order[order_idx];
            let node_id = self.dense_ids[node as usize];

            // Gather mixed input from upstream: a contiguous slice of the CSR
            // edge array, not a map lookup and a pointer chase per node.
            self.mix_buffer[..buffer_size].fill(0.0);
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
                let len = src_buf.len().min(buffer_size);
                match (self.edge_src_port[edge], self.edge_dst_port[edge]) {
                    // The ordinary case: a whole bus mixed into a whole bus.
                    (None, None) => {
                        for i in 0..len {
                            self.mix_buffer[i] += src_buf[i];
                        }
                    }
                    // From a splitter port: one source channel, as mono, spread
                    // across the destination bus.
                    (Some(from), None) => {
                        let from = from.min(channels - 1);
                        for frame in 0..len / channels {
                            let sample = src_buf[frame * channels + from];
                            for ch in 0..channels {
                                self.mix_buffer[frame * channels + ch] += sample;
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
                            self.mix_buffer[base + into] += sample;
                        }
                    }
                }
                has_input = true;
            }

            // Every node is provisioned a render buffer by `add_node`, on the
            // command path, so this is a lookup and never an allocation.
            let Some(node_buf) = self.node_buffers.get_mut(&node_id) else {
                continue;
            };
            if node_buf.len() < buffer_size {
                node_buf.resize(buffer_size, 0.0);
            }
            node_buf[..buffer_size].fill(0.0);

            if let Some(node) = self.nodes.get_mut(&node_id) {
                let input = if has_input {
                    &self.mix_buffer[..buffer_size]
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

        // Track processed frames for sample-accurate currentTime
        let frames = buffer_size / self.channels.max(1) as usize;
        self.frames_processed += frames as u64;

        // Drop whatever can no longer affect the output: sources that finished
        // this block, and released nodes their disappearance just orphaned.
        // Missing any of this leaked one node, one output buffer and one
        // node-index entry per fired sound effect.
        self.prune()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::{PcmBudget, PcmUsage, PcmUsageSnapshot};
    use crate::nodes::OscillatorNode;

    fn budgeted_context(
        context_bytes: usize,
        context_buffers: usize,
        process: Arc<PcmUsage>,
    ) -> (AudioContext, Arc<PcmUsage>) {
        let context = Arc::new(PcmUsage::new(context_bytes, context_buffers));
        let budget = PcmBudget::new(Arc::clone(&context), process);
        (
            AudioContext::new_with_pcm_budget(1, 48_000, 2, budget),
            context,
        )
    }

    #[test]
    fn all_channel_data_is_one_channel_major_flat_buffer() {
        let mut ctx = AudioContext::new(1, 48_000, 2);
        let id = ctx
            .add_buffer(DecodedAudio {
                samples: vec![1.0, 10.0, 2.0, 20.0, 3.0, 30.0],
                sample_rate: 48_000,
                channels: 2,
            })
            .unwrap();

        assert_eq!(
            ctx.take_decoded_buffer_data(id).unwrap(),
            Some(vec![1.0, 2.0, 3.0, 10.0, 20.0, 30.0])
        );
        assert!(ctx.get_buffer(id).is_none());
        assert_eq!(ctx.take_decoded_buffer_data(id).unwrap(), None);
    }

    #[test]
    fn retained_pcm_enforces_per_context_bytes_and_count() {
        let process = Arc::new(PcmUsage::new(1024, 16));
        let (mut ctx, usage) = budgeted_context(16, 1, process);
        let id = ctx
            .create_empty_buffer(1, 4, 48_000)
            .expect("exact context byte and count budget");
        assert_eq!(
            usage.snapshot(),
            PcmUsageSnapshot {
                bytes: 16,
                buffers: 1
            }
        );

        let error = ctx
            .create_empty_buffer(1, 1, 48_000)
            .expect_err("second retained buffer exceeds count and bytes");
        assert_eq!(error.code, shared::error::ErrorCode::InputSaturated);
        assert_eq!(
            usage.snapshot(),
            PcmUsageSnapshot {
                bytes: 16,
                buffers: 1
            }
        );
        assert!(ctx.get_buffer(id).is_some());
    }

    #[test]
    fn retained_pcm_enforces_process_budget_across_contexts() {
        let process = Arc::new(PcmUsage::new(16, 1));
        let (mut first, first_usage) = budgeted_context(32, 2, Arc::clone(&process));
        let (mut second, second_usage) = budgeted_context(32, 2, Arc::clone(&process));
        first
            .create_empty_buffer(1, 4, 48_000)
            .expect("first context fills process budget");

        let error = second
            .create_empty_buffer(1, 1, 48_000)
            .expect_err("second context exceeds process budget");
        assert_eq!(error.code, shared::error::ErrorCode::InputSaturated);
        assert_eq!(first_usage.snapshot().bytes, 16);
        assert_eq!(second_usage.snapshot(), PcmUsageSnapshot::default());
        assert_eq!(
            process.snapshot(),
            PcmUsageSnapshot {
                bytes: 16,
                buffers: 1
            }
        );
    }

    #[test]
    fn decoded_pcm_is_charged_by_vector_capacity_not_logical_length() {
        let mut samples = Vec::with_capacity(8);
        samples.push(0.0);
        let retained_bytes = samples.capacity() * std::mem::size_of::<f32>();
        assert!(retained_bytes > samples.len() * std::mem::size_of::<f32>());

        let process = Arc::new(PcmUsage::new(1024, 8));
        let (mut rejected, rejected_usage) =
            budgeted_context(retained_bytes - 1, 8, Arc::clone(&process));
        let error = rejected
            .add_buffer(DecodedAudio {
                samples,
                sample_rate: 48_000,
                channels: 1,
            })
            .expect_err("spare Vec capacity is retained heap and must not bypass the budget");
        assert_eq!(error.code, shared::error::ErrorCode::InputSaturated);
        assert_eq!(rejected_usage.snapshot(), PcmUsageSnapshot::default());

        let mut exact_samples = Vec::with_capacity(8);
        exact_samples.push(0.0);
        let exact_bytes = exact_samples.capacity() * std::mem::size_of::<f32>();
        let (mut accepted, accepted_usage) =
            budgeted_context(exact_bytes, 8, Arc::new(PcmUsage::new(1024, 8)));
        accepted
            .add_buffer(DecodedAudio {
                samples: exact_samples,
                sample_rate: 48_000,
                channels: 1,
            })
            .expect("exact retained capacity must fit");
        assert_eq!(accepted_usage.snapshot().bytes, exact_bytes);
    }

    #[test]
    fn releasing_map_entry_keeps_budget_until_source_clears_last_arc() {
        let process = Arc::new(PcmUsage::new(128, 8));
        let (mut ctx, usage) = budgeted_context(128, 8, process);
        let id = ctx.create_empty_buffer(1, 4, 48_000).unwrap();
        ctx.create_buffer_source(10);
        assert!(ctx.set_buffer(10, Some(id)));

        assert!(ctx.remove_buffer(id));
        assert_eq!(
            usage.snapshot(),
            PcmUsageSnapshot {
                bytes: 16,
                buffers: 1
            }
        );

        assert!(ctx.set_buffer(10, None));
        assert_eq!(usage.snapshot(), PcmUsageSnapshot::default());
    }

    #[test]
    fn replacing_source_buffer_releases_the_replaced_last_arc() {
        let process = Arc::new(PcmUsage::new(128, 8));
        let (mut ctx, usage) = budgeted_context(128, 8, process);
        let first = ctx.create_empty_buffer(1, 4, 48_000).unwrap();
        let second = ctx.create_empty_buffer(1, 2, 48_000).unwrap();
        ctx.create_buffer_source(10);
        assert!(ctx.set_buffer(10, Some(first)));
        assert!(ctx.remove_buffer(first));

        assert!(ctx.set_buffer(10, Some(second)));
        assert_eq!(
            usage.snapshot(),
            PcmUsageSnapshot {
                bytes: 8,
                buffers: 1
            }
        );
    }

    #[test]
    fn copy_on_write_reserves_before_cloning_shared_pcm() {
        let process = Arc::new(PcmUsage::new(16, 1));
        let (mut ctx, usage) = budgeted_context(16, 1, process);
        let id = ctx.create_empty_buffer(1, 4, 48_000).unwrap();
        ctx.create_buffer_source(10);
        assert!(ctx.set_buffer(10, Some(id)));

        let error = ctx
            .copy_to_channel(id, &[1.0], 0, 0)
            .expect_err("COW clone must reserve another retained allocation first");
        assert_eq!(error.code, shared::error::ErrorCode::InputSaturated);
        assert_eq!(
            usage.snapshot(),
            PcmUsageSnapshot {
                bytes: 16,
                buffers: 1
            }
        );
        assert_eq!(ctx.get_channel_data(id, 0).unwrap()[0], 0.0);

        assert!(ctx.set_buffer(10, None));
        assert!(ctx.copy_to_channel(id, &[1.0], 0, 0).unwrap());
        assert_eq!(ctx.get_channel_data(id, 0).unwrap()[0], 1.0);
    }

    #[test]
    fn copy_on_write_reserves_at_least_the_original_vector_capacity() {
        let mut samples = Vec::with_capacity(8);
        samples.push(0.0);
        let retained_bytes = samples.capacity() * std::mem::size_of::<f32>();
        let process = Arc::new(PcmUsage::new(retained_bytes * 2 - 1, 8));
        let (mut ctx, usage) = budgeted_context(retained_bytes * 2 - 1, 8, Arc::clone(&process));
        let id = ctx
            .add_buffer(DecodedAudio {
                samples,
                sample_rate: 48_000,
                channels: 1,
            })
            .unwrap();
        ctx.create_buffer_source(10);
        assert!(ctx.set_buffer(10, Some(id)));

        let error = ctx
            .copy_to_channel(id, &[1.0], 0, 0)
            .expect_err("COW must conservatively reserve the original allocation capacity");
        assert_eq!(error.code, shared::error::ErrorCode::InputSaturated);
        assert_eq!(usage.snapshot().bytes, retained_bytes);
        assert_eq!(process.snapshot().bytes, retained_bytes);
        assert_eq!(ctx.get_channel_data(id, 0).unwrap()[0], 0.0);
    }

    #[test]
    fn copy_on_write_shrinks_pre_reservation_to_the_clone_actual_capacity() {
        let mut samples = Vec::with_capacity(8);
        samples.push(0.0);
        let original_bytes = samples.capacity() * std::mem::size_of::<f32>();
        let process = Arc::new(PcmUsage::new(original_bytes * 2, 8));
        let (mut ctx, usage) = budgeted_context(original_bytes * 2, 8, Arc::clone(&process));
        let id = ctx
            .add_buffer(DecodedAudio {
                samples,
                sample_rate: 48_000,
                channels: 1,
            })
            .unwrap();
        ctx.create_buffer_source(10);
        assert!(ctx.set_buffer(10, Some(id)));

        assert!(ctx.copy_to_channel(id, &[1.0], 0, 0).unwrap());

        let clone_bytes =
            ctx.get_buffer(id).unwrap().samples.capacity() * std::mem::size_of::<f32>();
        assert!(
            clone_bytes < original_bytes,
            "fixture requires a smaller clone"
        );
        assert_eq!(usage.snapshot().bytes, original_bytes + clone_bytes);
        assert_eq!(process.snapshot().bytes, original_bytes + clone_bytes);
    }

    #[test]
    fn buffer_id_overflow_is_structured_and_does_not_collide() {
        let mut ctx = AudioContext::new(1, 48_000, 2);
        ctx.next_buffer_id = i32::MAX as AudioBufferId;

        let error = ctx
            .create_empty_buffer(1, 1, 48_000)
            .expect_err("buffer ids must not silently wrap");
        assert_eq!(error.code, shared::error::ErrorCode::InputSaturated);
        assert!(ctx.buffers.is_empty());
        assert_eq!(ctx.next_buffer_id, i32::MAX as AudioBufferId);
    }

    #[test]
    fn process_fully_cleans_up_finished_nodes() {
        let mut ctx = AudioContext::new(1, 48_000, 2);

        // A buffer source wired to the destination.
        ctx.create_buffer_source(10);
        ctx.connect(10, DESTINATION_NODE_ID);

        // Per W3C, stop(when <= 0) finishes the source immediately.
        assert!(ctx.stop_source(10, 0.0));

        let mut out = vec![0.0f32; 2 * 128];
        let finished = ctx.process(&mut out);

        assert!(
            finished.contains(&10),
            "finished node id must be reported so the audio thread can unregister it"
        );
        assert!(!ctx.nodes.contains_key(&10), "removed from the node map");
        assert!(
            !ctx.node_buffers.contains_key(&10),
            "per-node output buffer must be freed"
        );
        assert!(
            !ctx.connections.iter().any(|c| c.src == 10 || c.dst == 10),
            "graph connections referencing the finished node must be dropped"
        );
    }

    /// A gain node per sound effect is the ordinary Web Audio shape, and nothing
    /// used to remove one: `is_finished()` is false for every effect node, so the
    /// node, its render buffer and its node-index entry stayed for the context's
    /// whole life and were processed every quantum forever.
    #[test]
    fn releasing_an_idle_effect_node_drops_it_and_its_render_buffer() {
        let mut ctx = AudioContext::new(1, 48_000, 2);
        ctx.create_gain(10);
        ctx.connect(10, DESTINATION_NODE_ID);
        let mut out = vec![0.0f32; 2 * 128];
        ctx.process(&mut out);
        assert!(ctx.node_buffers.contains_key(&10));

        assert_eq!(ctx.release_node(10), [10]);
        assert!(!ctx.nodes.contains_key(&10));
        assert!(
            !ctx.node_buffers.contains_key(&10),
            "the per-node render buffer must be freed with the node"
        );
        assert!(!ctx.connections.iter().any(|c| c.src == 10 || c.dst == 10));
    }

    /// JavaScript drops `source -> gain -> destination` as one unreachable object
    /// graph, so the gain's finalizer can run while the source is still playing
    /// through it. Honouring that release immediately would cut the sound off.
    #[test]
    fn a_released_effect_node_survives_until_its_live_source_finishes() {
        let mut ctx = AudioContext::new(1, 48_000, 2);
        let mut oscillator = OscillatorNode::new(10, 48_000);
        oscillator.start(0.0);
        ctx.add_node(Box::new(oscillator));
        ctx.create_gain(11);
        ctx.connect(10, 11);
        ctx.connect(11, DESTINATION_NODE_ID);

        assert!(
            ctx.release_node(11).is_empty(),
            "a gain carrying a playing source must not be dropped"
        );
        assert!(ctx.nodes.contains_key(&11));

        // The source stops; both it and the now-orphaned gain go together.
        ctx.with_node_typed::<OscillatorNode, _>(10, |osc| osc.stop(0.0));
        let mut out = vec![0.0f32; 2 * 128];
        let mut removed = ctx.process(&mut out).to_vec();
        removed.sort_unstable();
        assert_eq!(removed, vec![10, 11]);
        assert!(ctx.nodes.is_empty() || ctx.nodes.contains_key(&DESTINATION_NODE_ID));
    }

    /// Collecting a released chain has to peel one layer at a time: the filter is
    /// only orphaned once the gain feeding it is gone.
    #[test]
    fn releasing_a_whole_chain_collects_every_layer_in_one_prune() {
        let mut ctx = AudioContext::new(1, 48_000, 2);
        ctx.create_gain(10);
        ctx.create_gain(11);
        ctx.create_gain(12);
        ctx.connect(10, 11);
        ctx.connect(11, 12);
        ctx.connect(12, DESTINATION_NODE_ID);

        // Release the downstream ones first, so only the upstream release can
        // start the cascade.
        assert!(ctx.release_node(12).is_empty());
        assert!(ctx.release_node(11).is_empty());
        let mut removed = ctx.release_node(10).to_vec();
        removed.sort_unstable();
        assert_eq!(removed, vec![10, 11, 12]);
        assert!(ctx.node_buffers.is_empty() || !ctx.node_buffers.contains_key(&12));
    }

    /// A source that was never started can never make a sound, and once JS has
    /// dropped it nobody can start it. Treating it as active pinned the audio
    /// thread to its 5 ms tick and held the output device open.
    #[test]
    fn an_unstarted_source_is_neither_active_nor_undroppable() {
        let mut ctx = AudioContext::new(1, 48_000, 2);
        ctx.create_buffer_source(10);
        ctx.connect(10, DESTINATION_NODE_ID);

        assert!(
            !ctx.has_active_sources(),
            "an unstarted source must not keep the audio thread awake"
        );
        assert_eq!(ctx.release_node(10), [10]);
    }

    #[test]
    fn releasing_the_destination_or_an_unknown_node_is_a_no_op() {
        let mut ctx = AudioContext::new(1, 48_000, 2);
        assert!(ctx.release_node(DESTINATION_NODE_ID).is_empty());
        assert!(ctx.nodes.contains_key(&DESTINATION_NODE_ID));
        assert!(ctx.release_node(9_999).is_empty());
    }

    /// `createChannelSplitter()` used to return something that did not split:
    /// every output port carried the whole bus, because a connection had no port
    /// index to read a single channel through.
    #[test]
    fn a_splitter_output_port_carries_only_its_own_channel() {
        use crate::nodes::ChannelSplitterNode;

        // A stereo buffer whose channels differ, so which one arrives is visible.
        let stereo = DecodedAudio {
            samples: vec![1.0, -1.0, 1.0, -1.0, 1.0, -1.0, 1.0, -1.0],
            sample_rate: 48_000,
            channels: 2,
        };

        for (port, expected) in [(0u32, 1.0f32), (1, -1.0)] {
            let mut ctx = AudioContext::new(1, 48_000, 2);
            let buffer = ctx.add_buffer(stereo.clone()).unwrap();
            ctx.create_buffer_source(10);
            assert!(ctx.set_buffer(10, Some(buffer)));
            ctx.start_source(10, 0.0, 0.0, None);
            ctx.add_node(Box::new(ChannelSplitterNode::new(11, 2)));
            ctx.connect(10, 11);
            ctx.connect_ports(11, port, DESTINATION_NODE_ID, 0);

            let mut out = vec![0.0f32; 2 * 4];
            ctx.process(&mut out);

            assert!(
                out.iter().all(|&s| (s - expected).abs() < 1e-6),
                "output port {port} must carry only channel {port} ({expected}): {out:?}"
            );
        }
    }

    /// `createChannelMerger()` used to sum every input into every channel. Input
    /// port `j` must land in channel `j` and nowhere else.
    #[test]
    fn a_merger_input_port_lands_in_only_its_own_channel() {
        use crate::nodes::{ChannelMergerNode, ConstantSourceNode};

        let mut ctx = AudioContext::new(1, 48_000, 2);
        ctx.add_node(Box::new(ChannelMergerNode::new(20, 2)));

        // Two constant sources, one per merger input port.
        let mut left = ConstantSourceNode::new(21);
        left.start(0.0);
        left.get_param_mut("offset").unwrap().set_value(1.0);
        ctx.add_node(Box::new(left));
        let mut right = ConstantSourceNode::new(22);
        right.start(0.0);
        right.get_param_mut("offset").unwrap().set_value(0.25);
        ctx.add_node(Box::new(right));

        ctx.connect_ports(21, 0, 20, 0);
        ctx.connect_ports(22, 0, 20, 1);
        ctx.connect(20, DESTINATION_NODE_ID);

        let mut out = vec![0.0f32; 2 * 4];
        ctx.process(&mut out);

        for frame in 0..4 {
            assert!(
                (out[frame * 2] - 1.0).abs() < 1e-6,
                "input port 0 must reach only the left channel: {out:?}"
            );
            assert!(
                (out[frame * 2 + 1] - 0.25).abs() < 1e-6,
                "input port 1 must reach only the right channel: {out:?}"
            );
        }
    }

    /// An ordinary connection must keep mixing the whole bus. The port machinery
    /// only engages for a node that actually has more than one port, so nothing
    /// else pays for it or changes behaviour.
    #[test]
    fn a_single_port_connection_still_mixes_the_whole_bus() {
        use crate::nodes::ConstantSourceNode;

        let mut ctx = AudioContext::new(1, 48_000, 2);
        let mut source = ConstantSourceNode::new(30);
        source.start(0.0);
        source.get_param_mut("offset").unwrap().set_value(0.5);
        ctx.add_node(Box::new(source));
        ctx.create_gain(31);
        ctx.connect(30, 31);
        ctx.connect(31, DESTINATION_NODE_ID);

        let mut out = vec![0.0f32; 2 * 4];
        ctx.process(&mut out);
        assert!(
            out.iter().all(|&s| (s - 0.5).abs() < 1e-6),
            "both channels must carry the signal: {out:?}"
        );
    }

    /// A cycle is legal Web Audio -- a feedback delay is the canonical effect.
    /// Nodes on one used to be dropped from the processing order entirely, so the
    /// graph went silent instead of echoing, and because they never ran they never
    /// finished and were never collected either.
    #[test]
    fn nodes_on_a_feedback_cycle_are_still_rendered() {
        use crate::nodes::ConstantSourceNode;

        let mut ctx = AudioContext::new(1, 48_000, 2);
        ctx.add_node(Box::new(crate::nodes::DelayNode::new(10, 0.05, 48_000, 2)));
        ctx.create_gain(11);
        let mut source = ConstantSourceNode::new(12);
        source.start(0.0);
        ctx.add_node(Box::new(source));
        ctx.connect(12, 10); // external signal enters the legal feedback cycle
        ctx.connect(10, 11);
        ctx.connect(11, 10); // the feedback edge closes the cycle
        ctx.connect(10, DESTINATION_NODE_ID);

        let mut out = vec![0.0f32; 2 * 128];
        ctx.process(&mut out);
        assert!(
            out.iter().any(|sample| sample.abs() > 1e-6),
            "a legal delay cycle must render its external input, got {out:?}"
        );

        for id in [10, 11, DESTINATION_NODE_ID] {
            let dense = ctx.dense_index[&id];
            assert!(
                ctx.processing_order.contains(&dense),
                "node {id} was dropped from the render order"
            );
        }
        assert_eq!(
            ctx.processing_order.last().copied(),
            Some(ctx.dense_index[&DESTINATION_NODE_ID]),
            "the destination must still be rendered last"
        );
    }

    #[test]
    fn remove_finished_node_purges_immediate_stop_only() {
        let mut ctx = AudioContext::new(1, 48_000, 2);

        // stop(when <= 0) finishes a buffer source immediately -> fully removed.
        ctx.create_buffer_source(20);
        ctx.connect(20, DESTINATION_NODE_ID);
        assert!(ctx.stop_source(20, 0.0));
        assert_eq!(
            ctx.remove_finished_node(20),
            [20],
            "immediate-finished node removed"
        );
        assert!(!ctx.nodes.contains_key(&20));
        assert!(!ctx.node_buffers.contains_key(&20));
        assert!(!ctx.connections.iter().any(|c| c.src == 20 || c.dst == 20));

        // A future-dated stop has NOT finished yet -> must stay reachable.
        ctx.create_buffer_source(21);
        ctx.connect(21, DESTINATION_NODE_ID);
        assert!(ctx.stop_source(21, 1000.0));
        assert!(
            ctx.remove_finished_node(21).is_empty(),
            "future-dated stop must remain until it actually finishes"
        );
        assert!(ctx.nodes.contains_key(&21));
    }

    /// AUD-06 RED: a unit impulse fed into a DelayNode must still appear in the
    /// output *after* the source is stopped, because the delay buffer holds the
    /// sample and must drain it as zero-input frames advance through.
    ///
    /// Before the fix, `DelayNode::process()` returned 0 immediately when given
    /// an empty input slice -- the source was pruned, so the mix step passed `&[]`
    /// -- and the buffered pulse was silently discarded.
    #[test]
    fn delay_tail_samples_arrive_after_source_stops() {
        use crate::nodes::DelayNode;

        const SAMPLE_RATE: u32 = 48_000;
        const CHANNELS: u32 = 1;
        const QUANTUM: usize = 128;
        const DELAY_SECS: f32 = 10.0 / 1000.0; // 480 frames

        let mut ctx = AudioContext::new(1, SAMPLE_RATE, CHANNELS);

        let buf_id = ctx
            .create_empty_buffer(CHANNELS, QUANTUM as u32, SAMPLE_RATE)
            .expect("buffer allocation");
        ctx.copy_to_channel(buf_id, &[1.0], 0, 0)
            .expect("write impulse");
        ctx.create_buffer_source(10);
        assert!(ctx.set_buffer(10, Some(buf_id)));
        ctx.start_source(10, 0.0, 0.0, Some(1.0 / SAMPLE_RATE as f64));

        ctx.add_node(Box::new(DelayNode::new(
            20,
            DELAY_SECS,
            SAMPLE_RATE,
            CHANNELS,
        )));
        ctx.connect(10, 20);
        ctx.connect(20, DESTINATION_NODE_ID);
        let mut tail_energy = 0.0;
        let mut source_removed = false;
        for _ in 0..8 {
            let mut block = vec![0.0f32; QUANTUM];
            ctx.process(&mut block);
            if !ctx.nodes.contains_key(&10) {
                source_removed = true;
                tail_energy += block.iter().map(|s| s * s).sum::<f32>();
            }
        }
        assert!(
            source_removed,
            "source must be pruned during the zero-input run"
        );
        assert!(
            tail_energy > 1e-6,
            "the delayed impulse must appear after source pruning; got energy={tail_energy}"
        );
    }

    /// AUD-06 RED: two Gain nodes wired in a feedback cycle, with both released
    /// by JavaScript and neither producing audio, must be collected.
    ///
    /// Before the fix, `has_live_input()` counted every existing node, so each
    /// Gain saw the other as a live input and the cycle was never reclaimed.
    #[test]
    fn released_silent_cycle_is_reclaimed() {
        let mut ctx = AudioContext::new(1, 48_000, 2);
        ctx.create_gain(10);
        ctx.create_gain(11);
        ctx.connect(10, 11);
        ctx.connect(11, 10); // cycle

        // Release both; neither is producing, neither has input from outside the cycle.
        ctx.release_node(11);
        let collected = ctx.release_node(10).to_vec();

        assert!(
            collected.contains(&10) && collected.contains(&11),
            "both nodes in the silent cycle must be collected; got {collected:?}"
        );
        assert!(!ctx.nodes.contains_key(&10));
        assert!(!ctx.nodes.contains_key(&11));
    }

    /// AUD-06 RED: a BiquadFilter that received an impulse must continue to
    /// produce non-zero output for at least one more block after the source
    /// is gone, because its internal state still holds the filter's tail.
    ///
    /// Before the fix, `BiquadFilterNode::process()` returned 0 on an empty
    /// input slice without advancing the filter state, silently discarding the
    /// resonant tail.
    #[test]
    fn biquad_tail_drains_after_source_stops() {
        use crate::nodes::{BiquadFilterNode, BiquadFilterType};

        const SAMPLE_RATE: u32 = 48_000;
        const CHANNELS: u32 = 1;
        const BLOCK: usize = 128;

        let mut ctx = AudioContext::new(1, SAMPLE_RATE, CHANNELS);

        // Source: single-sample impulse.
        let buf_id = ctx
            .create_empty_buffer(CHANNELS, BLOCK as u32, SAMPLE_RATE)
            .expect("buffer allocation");
        ctx.copy_to_channel(buf_id, &[1.0], 0, 0)
            .expect("write impulse");
        ctx.create_buffer_source(10);
        assert!(ctx.set_buffer(10, Some(buf_id)));
        ctx.start_source(10, 0.0, 0.0, None);
        ctx.stop_source(10, 1.0 / SAMPLE_RATE as f64);

        // Resonant lowpass (high Q) will ring for many frames after the impulse.
        let mut filter = BiquadFilterNode::new(20, CHANNELS, SAMPLE_RATE);
        filter.set_type(BiquadFilterType::Lowpass);
        filter.get_param_mut("frequency").unwrap().set_value(440.0);
        filter.get_param_mut("Q").unwrap().set_value(10.0);
        ctx.add_node(Box::new(filter));
        ctx.connect(10, 20);
        ctx.connect(20, DESTINATION_NODE_ID);

        // Block 1: impulse plays through, source is pruned.
        let mut out1 = vec![0.0f32; BLOCK * CHANNELS as usize];
        ctx.process(&mut out1);

        // Block 2: source is gone; the filter must still output its decaying tail.
        let mut out2 = vec![0.0f32; BLOCK * CHANNELS as usize];
        ctx.process(&mut out2);

        let tail_energy: f32 = out2.iter().map(|s| s * s).sum();
        assert!(
            tail_energy > 1e-8,
            "the resonant lowpass tail must still be present in block 2; \
             energy={tail_energy} (first few samples: {:?})",
            &out2[..8.min(out2.len())]
        );
    }
    #[test]
    fn idle_clock_does_not_double_wait_for_scheduled_start() {
        let mut ctx = AudioContext::new(1, 48_000, 2);
        ctx.advance_rendered_frames_for_test(480_000);
        let idle_time = ctx.current_time();
        assert!((idle_time - 10.0).abs() < f64::EPSILON);

        let buffer = ctx
            .add_buffer(DecodedAudio {
                samples: vec![1.0, 1.0],
                sample_rate: 48_000,
                channels: 2,
            })
            .unwrap();
        ctx.create_buffer_source(10);
        assert!(ctx.set_buffer(10, Some(buffer)));
        ctx.connect(10, DESTINATION_NODE_ID);
        assert!(ctx.start_source(10, idle_time, 0.0, None));

        let mut output = vec![0.0; 128 * 2];
        ctx.process(&mut output);
        assert!(
            output[0] > 0.0,
            "a source scheduled at the idle native time must render immediately"
        );
    }
    #[test]
    fn clock_freezes_on_suspend_and_resumes_from_the_same_position() {
        let mut ctx = AudioContext::new(1, 48_000, 2);
        ctx.advance_rendered_frames_for_test(48_000);
        assert!((ctx.current_time() - 1.0).abs() < f64::EPSILON);
        ctx.suspend();
        ctx.advance_rendered_frames_for_test(48_000);
        assert!(
            (ctx.current_time() - 1.0).abs() < f64::EPSILON,
            "suspension must freeze the render clock"
        );
        ctx.resume();
        ctx.advance_rendered_frames_for_test(24_000);
        assert!((ctx.current_time() - 1.5).abs() < f64::EPSILON);
    }
    #[test]
    fn dual_rate_context_keeps_one_second_of_context_time_per_device_second() {
        let mut ctx = AudioContext::new(1, 44_100, 1);
        let mut oscillator = OscillatorNode::new(10, 44_100);
        oscillator.start(0.0);
        ctx.add_node(Box::new(oscillator));
        ctx.connect(10, DESTINATION_NODE_ID);

        let mut output = vec![0.0; 48_000];
        ctx.process_for_device(&mut output, 48_000, 1);

        assert!(
            (ctx.current_time() - 1.0).abs() < 0.001,
            "context time must remain in context-rate seconds: {}",
            ctx.current_time()
        );
        assert!(output.iter().any(|sample| sample.abs() > 1e-6));
    }
    #[test]
    fn device_recovery_restarts_rate_and_channel_resampler_state() {
        let mut ctx = AudioContext::new(1, 44_100, 1);
        let mut oscillator = OscillatorNode::new(10, 44_100);
        oscillator.start(0.0);
        ctx.add_node(Box::new(oscillator));
        ctx.connect(10, DESTINATION_NODE_ID);

        let mut first_route = vec![0.0; 48_000 * 2];
        ctx.process_for_device(&mut first_route, 48_000, 2);
        ctx.renegotiate_device(44_100, 1);

        let mut recovered_route = vec![0.0; 44_100];
        ctx.process_for_device(&mut recovered_route, 44_100, 1);
        assert!(
            recovered_route.iter().any(|sample| sample.abs() > 1e-6),
            "recovered route must render with its new mono layout"
        );
        assert!(
            (ctx.current_time() - 2.0).abs() < 0.002,
            "recovery must not reuse the old route's rate phase"
        );
    }
}

// ── Section 7.3: zero steady-state allocation ───────────────────────────────

#[cfg(test)]
mod steady_state_allocation {
    use super::*;
    use crate::nodes::{GainNode, OscillatorNode};
    use migo_alloc_probe::{Burst, assert_no_steady_state_allocation};

    /// Section 7.3, on the audio graph's real-time path.
    ///
    /// `process` is the audio callback's work: it runs on the output thread —
    /// SCHED_FIFO on Android, per `audio_thread.rs` — once per quantum, for the
    /// life of every sound the game plays. A heap allocation here is not a
    /// throughput cost like the ones on the frame path; it is a deadline miss,
    /// heard as a dropout, because the allocator can block behind a thread that
    /// is not real-time scheduled at all.
    ///
    /// The graph is a source into a gain into the destination — the shape every
    /// `AudioBufferSourceNode`-plus-volume playback has — so the burst covers the
    /// upstream mix, a node with input, and the additive write into the output.
    #[test]
    fn steady_state_audio_quantum_never_reaches_the_heap() {
        const CHANNELS: u32 = 2;
        const QUANTUM_FRAMES: usize = 128;

        let mut ctx = AudioContext::new(1, 48_000, CHANNELS);
        let mut oscillator = OscillatorNode::new(10, 48_000);
        oscillator.start(0.0);
        ctx.add_node(Box::new(oscillator));
        ctx.add_node(Box::new(GainNode::new(11)));
        ctx.connect(10, 11);
        ctx.connect(11, DESTINATION_NODE_ID);

        let mut output = vec![0.0f32; QUANTUM_FRAMES * CHANNELS as usize];

        assert_no_steady_state_allocation(
            Burst {
                path: "audio: one graph quantum on the output thread",
                // Covers the topological rebuild, the mix buffer's first resize
                // and every node's output buffer being created on first use.
                warmup: 8,
                measured: 64,
            },
            |_| {
                output.fill(0.0);
                // The borrow must not escape the burst body; the length is the
                // only part the gate needs and reading it keeps the call live.
                ctx.process(&mut output).len()
            },
        );
    }

    /// The same property, on the path the burst above cannot reach.
    ///
    /// **Saying why is the point.** The gate above renders an oscillator that
    /// never finishes, so the graph never changes and `rebuild_processing_order`
    /// runs exactly once, during the warm-up. That is not what a game does: every
    /// fired sound effect ends, an ended source is dropped from the graph, and
    /// that marks the order dirty and rebuilds it on the very next quantum. The
    /// rebuild used to build two fresh `HashMap`s and a `Vec` every time -- on the
    /// thread that must never be late -- and the measured window never saw it.
    ///
    /// So every iteration here adds, plays out and collects a one-shot source:
    /// a graph change per quantum, strictly more churn than real playback, and it
    /// must still never reach the heap.
    #[test]
    fn a_graph_change_on_the_render_path_never_reaches_the_heap() {
        const CHANNELS: u32 = 2;
        const QUANTUM_FRAMES: usize = 128;

        let mut ctx = AudioContext::new(1, 48_000, CHANNELS);
        ctx.create_gain(11);
        ctx.connect(11, DESTINATION_NODE_ID);
        let mut output = vec![0.0f32; QUANTUM_FRAMES * CHANNELS as usize];

        // The boxed nodes are built before the measured window: `Box::new` is the
        // burst body's own allocation, not the render path's, and Section 7.3
        // forbids a body that takes from a pool it does not control.
        const ITERATIONS: usize = 8 + 64;
        let mut ready: Vec<Box<dyn AudioNodeProcessor>> = (0..ITERATIONS)
            .map(|i| {
                let mut oscillator = OscillatorNode::new(100 + i as AudioNodeId, 48_000);
                oscillator.start(0.0);
                oscillator.stop(0.0);
                Box::new(oscillator) as Box<dyn AudioNodeProcessor>
            })
            .collect();

        assert_no_steady_state_allocation(
            Burst {
                path: "audio: one graph quantum that adds and collects a source",
                warmup: 8,
                measured: 64,
            },
            |_| {
                let node = ready.pop().expect("a prepared node per iteration");
                let id = node.id();
                ctx.add_node(node);
                ctx.connect(id, 11);
                output.fill(0.0);
                ctx.process(&mut output).len()
            },
        );
    }
}
