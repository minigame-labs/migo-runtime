//! Allocation-free admission for an entire external frame.

use frame_wire::canvas2d::{OP2D_BASE, OP2D_SELECT_CANVAS};
use frame_wire::stream::{RecordSpec, ValidatedStream, opcode_of, record_spec, word_count_of};
use shared::command_vec_pool::{
    CANVAS_COMMAND_VEC_INITIAL_CAPACITY, FRAME_OP_VEC_INITIAL_CAPACITY,
    GL_COMMAND_VEC_INITIAL_CAPACITY,
};
use shared::protocol::render_cmd::{Canvas2DCmd, GLCmd, UniformF32Values};
use shared::protocol::{FrameOp, FramePacket};

/// Conservative owned storage for decoding one structurally validated frame.
/// Includes command capacities, spilled uniform capacities, materialization
/// scratch, and the op vector with BeginFrame and Present. Pool caches and the
/// separately bounded wire credit are not owned by the decoded frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameDecodeBudget {
    estimated_bytes: usize,
    max_frame_ops: usize,
    frame_op_capacity: usize,
    pub(crate) pending_canvas_capacity: usize,
}

impl FrameDecodeBudget {
    pub fn estimated_bytes(&self) -> usize {
        self.estimated_bytes
    }
    pub fn max_frame_ops(&self) -> usize {
        self.max_frame_ops
    }
    /// Reserve the receiving FramePacketBuilder with this capacity and use a
    /// bounded pool loan, so a previous large frame cannot inflate this one.
    pub fn frame_op_capacity(&self) -> usize {
        self.frame_op_capacity
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameDecodeBudgetError {
    pub required_bytes: usize,
    pub max_decoded_bytes: usize,
}

impl std::fmt::Display for FrameDecodeBudgetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "decoded frame requires {} bytes, limit is {}",
            self.required_bytes, self.max_decoded_bytes
        )
    }
}
impl std::error::Error for FrameDecodeBudgetError {}

/// Check the decoded frame's storage BEFORE decoding or allocating its packet.
/// Semantic GL failures can only reduce this estimate; even calls ultimately
/// skipped are charged here. The scan neither allocates nor calls the host.
pub fn validate_frame_budget(
    stream: &ValidatedStream<'_>,
    max_decoded_bytes: usize,
) -> Result<FrameDecodeBudget, FrameDecodeBudgetError> {
    let budget = estimate(stream);
    if budget.estimated_bytes > max_decoded_bytes {
        Err(FrameDecodeBudgetError {
            required_bytes: budget.estimated_bytes,
            max_decoded_bytes,
        })
    } else {
        Ok(budget)
    }
}

pub(crate) fn storage_capacity(count: usize, minimum: usize) -> usize {
    count
        .max(minimum)
        .checked_next_power_of_two()
        .unwrap_or(usize::MAX)
}

#[derive(Default)]
struct Counter {
    bytes: usize,
    ops: usize,
    canvas_commands: usize,
    gl_commands: usize,
    pending_canvases: usize,
    peak_pending_canvases: usize,
}

impl Counter {
    fn canvas_batch(&mut self) {
        if self.canvas_commands == 0 {
            return;
        }
        let capacity = storage_capacity(self.canvas_commands, CANVAS_COMMAND_VEC_INITIAL_CAPACITY);
        self.bytes = self
            .bytes
            .saturating_add(capacity.saturating_mul(size_of::<Canvas2DCmd>()));
        self.canvas_commands = 0;
        self.ops += 1;
        // Duplicate IDs overestimate scratch and materialize ops deliberately:
        // deduplication needs storage, admission must allocate nothing.
        self.pending_canvases += 1;
        self.peak_pending_canvases = self.peak_pending_canvases.max(self.pending_canvases);
    }
    fn gl_batch(&mut self) {
        if self.gl_commands == 0 {
            return;
        }
        let capacity = storage_capacity(self.gl_commands, GL_COMMAND_VEC_INITIAL_CAPACITY);
        self.bytes = self
            .bytes
            .saturating_add(capacity.saturating_mul(size_of::<GLCmd>()));
        self.gl_commands = 0;
        self.ops += 1;
    }
    fn materialize(&mut self) {
        self.ops += self.pending_canvases;
        self.pending_canvases = 0;
    }
}

pub(crate) fn estimate(stream: &ValidatedStream<'_>) -> FrameDecodeBudget {
    let mut count = Counter::default();
    let mut canvas_selected = false;
    let mut cursor = 2;
    let words = stream.words();
    while cursor < words.len() {
        let opcode = opcode_of(words[cursor]);
        let wc = word_count_of(words[cursor]) as usize;
        cursor += wc;
        if opcode == OP2D_SELECT_CANVAS {
            count.canvas_batch();
            canvas_selected = true;
        } else if opcode >= OP2D_BASE {
            if canvas_selected {
                count.gl_batch();
                count.canvas_commands += 1;
            }
        } else {
            count.canvas_batch();
            count.materialize();
            count.gl_commands += 1;
            let payload_words = match record_spec(opcode) {
                Some(RecordSpec::VectorUniform { .. }) => wc - 3,
                Some(RecordSpec::MatrixUniform { .. }) => wc - 4,
                _ => 0,
            };
            // Both uniform element types have the same inline capacity. Their
            // exact-size iterator is collected with SmallVec's power-of-two
            // reserve, including the 17 -> 32 element spill.
            if payload_words > UniformF32Values::new().inline_size() {
                count.bytes = count.bytes.saturating_add(
                    storage_capacity(payload_words, 0).saturating_mul(size_of::<u32>()),
                );
            }
        }
    }
    count.canvas_batch();
    count.gl_batch();
    count.materialize();
    let max_frame_ops = count.ops + 2;
    let frame_op_capacity = storage_capacity(max_frame_ops, FRAME_OP_VEC_INITIAL_CAPACITY);
    let pending_canvas_capacity = if count.peak_pending_canvases == 0 {
        0
    } else {
        storage_capacity(count.peak_pending_canvases, 4)
    };
    let estimated_bytes = count
        .bytes
        .saturating_add(frame_op_capacity.saturating_mul(size_of::<FrameOp>()))
        .saturating_add(pending_canvas_capacity.saturating_mul(size_of::<u32>()))
        .saturating_add(size_of::<FramePacket>());
    FrameDecodeBudget {
        estimated_bytes,
        max_frame_ops,
        frame_op_capacity,
        pending_canvas_capacity,
    }
}

/// Count the rest of a batch before acquiring its bounded command vector.
/// Each record is scanned at most once here in addition to admission/decode.
pub(crate) fn batch_capacity(
    words: &[u32],
    mut cursor: usize,
    canvas: bool,
    mut selected: bool,
) -> usize {
    let mut count = 0;
    while cursor < words.len() {
        let opcode = opcode_of(words[cursor]);
        if opcode == OP2D_SELECT_CANVAS {
            if canvas {
                break;
            }
            selected = true;
        } else if opcode >= OP2D_BASE {
            if canvas {
                count += 1;
            } else if selected {
                break;
            }
        } else if canvas {
            break;
        } else {
            count += 1;
        }
        cursor += word_count_of(words[cursor]) as usize;
    }
    storage_capacity(
        count,
        if canvas {
            CANVAS_COMMAND_VEC_INITIAL_CAPACITY
        } else {
            GL_COMMAND_VEC_INITIAL_CAPACITY
        },
    )
}
