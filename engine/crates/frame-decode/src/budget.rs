//! Allocation-free admission for an entire external frame.

use frame_wire::canvas2d::{OP2D_BASE, OP2D_SELECT_CANVAS};
use frame_wire::stream::{RecordSpec, ValidatedStream, opcode_of, record_spec, word_count_of};
use shared::command_vec_pool::{
    CANVAS_COMMAND_VEC_INITIAL_CAPACITY, FRAME_OP_VEC_INITIAL_CAPACITY,
    GL_COMMAND_VEC_INITIAL_CAPACITY,
};
use shared::protocol::render_cmd::{Canvas2DCmd, GLCmd, UniformF32Values};
use shared::protocol::{FrameOp, FramePacket};

/// The most owned storage one external frame packet may decode into.
///
/// Together with the credit window this bounds queued command storage
/// independently of the 4 MiB wire ceiling. It is a safety limit, not a
/// measurement of process or GPU memory. A packet over it is refused -- which on
/// the Apple lane terminates the content -- so a producer has to be able to stay
/// under it without knowing this build's type sizes; see [`producer_bounds`].
pub const MAX_DECODED_FRAME_BYTES: usize = 4 * 1024 * 1024;

/// What a producer estimates a packet's decoded storage with.
///
/// [`estimate`] charges `size_of` each decoded type, and those sizes belong to
/// this build: a producer in another process cannot know them, and one that
/// guessed low would have a packet refused and its content ended. So the wire
/// contract (*Decoded storage* in `contracts/frame-wire/wire-v1.md`) publishes
/// the same formula over **upper bounds**, and this module is where the bounds
/// are held to the truth: every size is asserted at compile time to be within
/// its bound, and every capacity is the one [`estimate`] uses. The formula is
/// monotonic in each size, so a producer that splits at the bound's estimate is
/// never refused for a packet this reader would have estimated lower.
///
/// Mirrored in `platforms/apple/WebContent/PerformancePlus/src/decode-budget.mjs`
/// and checked there by `tests/decode_budget_js_agreement.rs`, which runs the
/// producer's estimate against this one on the same streams.
pub mod producer_bounds {
    /// Bounds `size_of::<GLCmd>()` (96 on 64-bit targets as of 2026-09-17).
    pub const GL_COMMAND_BYTES: usize = 144;
    /// Bounds `size_of::<Canvas2DCmd>()` (56).
    pub const CANVAS2D_COMMAND_BYTES: usize = 64;
    /// Bounds `size_of::<FrameOp>()` (56).
    pub const FRAME_OP_BYTES: usize = 64;
    /// Bounds `size_of::<FramePacket>()` (48).
    pub const FRAME_PACKET_BYTES: usize = 64;
    /// A uniform payload longer than this many words spills to the heap.
    pub const UNIFORM_INLINE_WORDS: usize = 16;
    pub const GL_BATCH_MIN_CAPACITY: usize =
        shared::command_vec_pool::GL_COMMAND_VEC_INITIAL_CAPACITY;
    pub const CANVAS_BATCH_MIN_CAPACITY: usize =
        shared::command_vec_pool::CANVAS_COMMAND_VEC_INITIAL_CAPACITY;
    pub const FRAME_OP_MIN_CAPACITY: usize =
        shared::command_vec_pool::FRAME_OP_VEC_INITIAL_CAPACITY;
    pub const PENDING_CANVAS_MIN_CAPACITY: usize = super::PENDING_CANVAS_MIN_CAPACITY;
}

const _: () = {
    use producer_bounds as bound;
    assert!(size_of::<GLCmd>() <= bound::GL_COMMAND_BYTES);
    assert!(size_of::<Canvas2DCmd>() <= bound::CANVAS2D_COMMAND_BYTES);
    assert!(size_of::<FrameOp>() <= bound::FRAME_OP_BYTES);
    assert!(size_of::<FramePacket>() <= bound::FRAME_PACKET_BYTES);
    // The producer's literal copies of the capacities, which are not bounds but
    // the same numbers: `decode-budget.mjs` restates them, and the agreement
    // test is what holds that file to these.
    assert!(bound::GL_BATCH_MIN_CAPACITY == 16);
    assert!(bound::CANVAS_BATCH_MIN_CAPACITY == 8);
    assert!(bound::FRAME_OP_MIN_CAPACITY == 8);
    assert!(bound::PENDING_CANVAS_MIN_CAPACITY == 4);
};

/// The smallest pending-canvas scratch [`estimate`] charges for.
const PENDING_CANVAS_MIN_CAPACITY: usize = 4;

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

/// The four sizes the estimate charges.
#[derive(Clone, Copy)]
struct Sizes {
    gl_command: usize,
    canvas_command: usize,
    frame_op: usize,
    frame_packet: usize,
}

/// This build's.
const ACTUAL: Sizes = Sizes {
    gl_command: size_of::<GLCmd>(),
    canvas_command: size_of::<Canvas2DCmd>(),
    frame_op: size_of::<FrameOp>(),
    frame_packet: size_of::<FramePacket>(),
};

/// The contract's, which a producer estimates with.
const BOUNDS: Sizes = Sizes {
    gl_command: producer_bounds::GL_COMMAND_BYTES,
    canvas_command: producer_bounds::CANVAS2D_COMMAND_BYTES,
    frame_op: producer_bounds::FRAME_OP_BYTES,
    frame_packet: producer_bounds::FRAME_PACKET_BYTES,
};

/// The estimate a producer computes for this stream: [`estimate`]'s formula
/// over [`producer_bounds`]. Never below this build's own estimate.
pub fn producer_estimated_bytes(stream: &ValidatedStream<'_>) -> usize {
    estimate_with(stream, BOUNDS).estimated_bytes
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
    fn canvas_batch(&mut self, sizes: Sizes) {
        if self.canvas_commands == 0 {
            return;
        }
        let capacity = storage_capacity(self.canvas_commands, CANVAS_COMMAND_VEC_INITIAL_CAPACITY);
        self.bytes = self
            .bytes
            .saturating_add(capacity.saturating_mul(sizes.canvas_command));
        self.canvas_commands = 0;
        self.ops += 1;
        // Duplicate IDs overestimate scratch and materialize ops deliberately:
        // deduplication needs storage, admission must allocate nothing.
        self.pending_canvases += 1;
        self.peak_pending_canvases = self.peak_pending_canvases.max(self.pending_canvases);
    }
    fn gl_batch(&mut self, sizes: Sizes) {
        if self.gl_commands == 0 {
            return;
        }
        let capacity = storage_capacity(self.gl_commands, GL_COMMAND_VEC_INITIAL_CAPACITY);
        self.bytes = self
            .bytes
            .saturating_add(capacity.saturating_mul(sizes.gl_command));
        self.gl_commands = 0;
        self.ops += 1;
    }
    fn materialize(&mut self) {
        self.ops += self.pending_canvases;
        self.pending_canvases = 0;
    }
}

#[cfg(test)]
mod bound_tests {
    use super::*;

    /// Not a compile-time assertion only because `SmallVec`'s inline size is not
    /// a `const` expression.
    #[test]
    fn the_uniform_inline_bound_is_the_inline_size() {
        assert_eq!(
            UniformF32Values::new().inline_size(),
            producer_bounds::UNIFORM_INLINE_WORDS
        );
        assert_eq!(
            shared::protocol::render_cmd::UniformI32Values::new().inline_size(),
            producer_bounds::UNIFORM_INLINE_WORDS
        );
    }
}

#[cfg(test)]
thread_local! {
    static ESTIMATE_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_estimate_calls() {
    ESTIMATE_CALLS.with(|calls| calls.set(0));
}

#[cfg(test)]
pub(crate) fn estimate_calls() -> usize {
    ESTIMATE_CALLS.with(std::cell::Cell::get)
}

#[cfg(test)]
#[inline]
fn note_estimate_call() {
    ESTIMATE_CALLS.with(|calls| calls.set(calls.get() + 1));
}

#[cfg(not(test))]
#[inline]
fn note_estimate_call() {}

pub(crate) fn estimate(stream: &ValidatedStream<'_>) -> FrameDecodeBudget {
    note_estimate_call();
    estimate_with(stream, ACTUAL)
}

fn estimate_with(stream: &ValidatedStream<'_>, sizes: Sizes) -> FrameDecodeBudget {
    let mut count = Counter::default();
    let mut canvas_selected = false;
    let mut cursor = 2;
    let words = stream.words();
    while cursor < words.len() {
        let opcode = opcode_of(words[cursor]);
        let wc = word_count_of(words[cursor]) as usize;
        cursor += wc;
        if opcode == OP2D_SELECT_CANVAS {
            count.canvas_batch(sizes);
            canvas_selected = true;
        } else if opcode >= OP2D_BASE {
            if canvas_selected {
                count.gl_batch(sizes);
                count.canvas_commands += 1;
            }
        } else {
            count.canvas_batch(sizes);
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
            if payload_words > producer_bounds::UNIFORM_INLINE_WORDS {
                count.bytes = count.bytes.saturating_add(
                    storage_capacity(payload_words, 0).saturating_mul(size_of::<u32>()),
                );
            }
        }
    }
    count.canvas_batch(sizes);
    count.gl_batch(sizes);
    count.materialize();
    let max_frame_ops = count.ops + 2;
    let frame_op_capacity = storage_capacity(max_frame_ops, FRAME_OP_VEC_INITIAL_CAPACITY);
    let pending_canvas_capacity = if count.peak_pending_canvases == 0 {
        0
    } else {
        storage_capacity(count.peak_pending_canvases, PENDING_CANVAS_MIN_CAPACITY)
    };
    let estimated_bytes = count
        .bytes
        .saturating_add(frame_op_capacity.saturating_mul(sizes.frame_op))
        .saturating_add(pending_canvas_capacity.saturating_mul(size_of::<u32>()))
        .saturating_add(sizes.frame_packet);
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
