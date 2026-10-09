//! The steady-state allocation requirement, applied to the frame
//! boundary.
//!
//! **This is an integration test because the property is a property of a
//! binary.** A frame takes its vectors from the process-wide command pools, so
//! measuring the cycle at zero requires that a pool hand back what the previous
//! frame returned. That holds in production — one game thread, one render
//! thread, a bounded number of packets in flight — and it does not hold inside a
//! lib-test binary, where several dozen other tests build packets concurrently
//! and a neighbour taking the pool's last vector leaves `take` with nothing to
//! give and it allocates a fresh one. Measured exactly that way while writing
//! this: green standalone, red under the full suite. An integration test is the
//! smallest unit that can hold the pools to itself, and it also has to be the
//! unit that installs the counting allocator, since a `#[global_allocator]` is
//! unique per binary.
//!
//! Everything below goes through public API, which is the point: the frame
//! boundary is reachable without the render thread's GL context, so the gate
//! does not need one.
//!
//! **Keep this file to one test.** A second test would run on a second thread
//! against the same pools and reintroduce exactly the interference this file
//! exists to exclude.

use migo_alloc_probe::{Burst, CountingAllocator, assert_no_steady_state_allocation};
use shared::command_vec_pool::{take_canvas_command_vec, take_gl_command_vec};
use shared::protocol::render_cmd::{Canvas2DCmd, CanvasId, GLCmd};
use shared::protocol::{CanvasBatchPayload, GlBatchPayload};
use shared::{FrameOp, FramePacketBuilder};

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator::system();

const WARMUP: usize = 4;
const MEASURED: usize = 64;

/// One iteration is a whole frame: the game thread's side builds the packet, the
/// render thread's side consumes it, and every vector involved goes back to its
/// pool.
///
/// Until the packet's op vector became a loan this could not be asserted at
/// zero. `FramePacketBuilder::new` started from `Vec::new()` and grew, so a
/// frame cost an allocation plus its doublings however little it drew, and the
/// command vectors inside it were returned by an explicit `recycle_*` call that
/// nothing could observe being forgotten.
///
/// The burst covers both halves deliberately. A gate over the consuming half
/// alone would leave the builder's allocation exactly where it was, and one over
/// the building half alone would not notice a loan that never came back.
#[test]
fn a_steady_state_frame_never_reaches_the_heap() {
    assert_no_steady_state_allocation(
        Burst {
            path: "frame boundary: build one packet and consume it",
            warmup: WARMUP,
            measured: MEASURED,
        },
        |_| {
            // The game thread's side: two segments' worth of commands, then the
            // packet that carries them. The op count and both command counts sit
            // inside the pools' minimum capacities, so a vector this burst is
            // handed is already wide enough and cannot be grown into a
            // reallocation — the emptiness of the pool is the only thing that
            // could allocate, and this binary is what rules that out.
            let mut canvas_commands = take_canvas_command_vec();
            canvas_commands.push(Canvas2DCmd::Save);
            canvas_commands.push(Canvas2DCmd::Restore);

            let mut gl_commands = take_gl_command_vec();
            gl_commands.push(GLCmd::Clear {
                canvas_id: CanvasId::from(1u32),
                bit_field: 0x4000,
            });

            let packet = FramePacketBuilder::new(1, 16.6)
                .push(FrameOp::BeginFrame)
                .push(FrameOp::CanvasBatch(CanvasBatchPayload {
                    canvas_id: CanvasId::from(2u32),
                    commands: canvas_commands,
                    present: true,
                    dirty_rect: None,
                }))
                .push(FrameOp::Materialize { canvas_id: 2 })
                .push(FrameOp::GlBatch(GlBatchPayload {
                    commands: gl_commands,
                }))
                .push(FrameOp::Present)
                .finish();

            // The render thread's side: walk the ops and drain each batch as it
            // executes, which is what leaves the loans empty on the way back.
            for op in packet.into_ops() {
                match op {
                    FrameOp::CanvasBatch(mut payload) => {
                        for command in payload.commands.drain(..) {
                            std::hint::black_box(command);
                        }
                    }
                    FrameOp::GlBatch(mut payload) => {
                        for command in payload.commands.drain(..) {
                            std::hint::black_box(command);
                        }
                    }
                    other => {
                        std::hint::black_box(&other);
                    }
                }
            }
        },
    );
}
