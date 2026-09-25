//! Creating and resizing a canvas has the same effect on both lanes.
//!
//! The other parity tests beside this one compare records against ops for work
//! done *on* a canvas. This one is about the canvas: in process the facade
//! reaches three ops that travel the render thread's own FIFO -- a fire-and-
//! forget `CanvasCmd::RegisterOffscreen`, a `Canvas2DCmd::ResizeCanvas` pushed
//! into the frame collector, and a synchronous `CanvasCmd::DestroyCanvas` -- and
//! on the Performance+ lane there is no FIFO and no op, so all three are records
//! in the frame's own stream (`OP2D_REGISTER_CANVAS`, `OP2D_RESIZE_CANVAS`,
//! `OP2D_DESTROY_CANVAS`).
//!
//! # What is compared, and why it is not the commands themselves
//!
//! The two lanes do not produce the same command values, and cannot:
//!
//! * in process a registration is a `CanvasCmd` carrying its own id, while on
//!   the wire the id is the selection the records inherit and the command is a
//!   `Canvas2DCmd::RegisterCanvas`;
//! * the ids differ by construction. Both allocate from
//!   `PRODUCER_CANVAS_ID_BASE`, but the in-process counter is process-global and
//!   every other test in this binary that creates a canvas has already moved it.
//!
//! So each side is reduced to the sequence of *effects* -- which canvas, what
//! happened -- with ids replaced by the order they were first seen. Everything
//! that would make the lanes differ survives that reduction: a missing record, a
//! record on the wrong canvas, a resize that named the wrong dimension, a
//! destroy that arrived before the draws, a canvas created twice. What it drops
//! is only the numbering, and the numbering is checked separately: both sides'
//! first offscreen id must be at or above the base the renderer requires.
//!
//! # What is not here
//!
//! Destroying. There is no `canvas.destroy()` in either runtime: the engine
//! calls `op_destroy_canvas` from a `FinalizationRegistry`, and a fixture cannot
//! schedule a collection in both a V8 test runtime and WebKit. Reaching the op
//! around the facade would mean the two lanes calling it two different ways,
//! which is exactly what a parity fixture must not do. So the destroy record is
//! checked on each side where it can be checked exactly: what the producer
//! writes for it in `test/canvas-lifetime.test.mjs`, and what the host reads
//! back from it in `frame_decode::canvas2d`'s own cases.
//!
//! `#[ignore]` because the producer's packets come from node, driven by
//! `scripts/test-performance-plus-engine-contract.sh`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use shared::command_vec_pool::PooledVec;
use shared::protocol::render_cmd::{
    Canvas2DCmd, CanvasCmd, GLCmd, PRODUCER_CANVAS_ID_BASE, RenderCommand,
};
use shared::protocol::{CanvasBatchPayload, FrameOp};

use super::webgl::tests::{end_test_frame, new_webgl_runtime};

/// One thing that happened to one canvas, with the canvas named by the order it
/// was first seen rather than by its id.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Effect {
    canvas: String,
    what: String,
}

/// Names canvases in first-seen order: the onscreen canvas is `main` because
/// both lanes agree on that one number, and every other is `canvas-1`,
/// `canvas-2`, ... in the order it first appears.
#[derive(Default)]
struct Names {
    seen: HashMap<u32, String>,
    next: usize,
    first_offscreen: Option<u32>,
}

impl Names {
    fn of(&mut self, id: u32) -> String {
        if id == 1 {
            return "main".to_string();
        }
        if self.first_offscreen.is_none() {
            self.first_offscreen = Some(id);
        }
        if let Some(name) = self.seen.get(&id) {
            return name.clone();
        }
        self.next += 1;
        let name = format!("canvas-{}", self.next);
        self.seen.insert(id, name.clone());
        name
    }
}

fn resize(w: Option<u32>, h: Option<u32>) -> String {
    format!("resize w={w:?} h={h:?}")
}

/// The decoder's host for the producer's packets: this fixture pushes no WebGL
/// errors and keeps no transform-feedback state, so the context half answers
/// what a canvas-only decode needs and the sink is what collects.
#[derive(Default)]
struct Host {
    names: Names,
    effects: Vec<Effect>,
}

impl frame_decode::GlDecodeContext for Host {
    fn image_upload(&mut self, _upload: frame_decode::ImageUpload) -> Option<GLCmd> {
        None
    }
    fn push_error(&mut self, _canvas_id: u32, _code: u32) {}
    fn transform_feedback_captures(&self, _canvas_id: u32) -> bool {
        false
    }
    fn set_transform_feedback(
        &mut self,
        _canvas_id: u32,
        _phase: frame_decode::TransformFeedbackPhase,
    ) {
    }
}

impl frame_decode::RenderSink for Host {
    fn canvas_batch(&mut self, canvas_id: u32, commands: PooledVec<Canvas2DCmd>) {
        for command in commands.iter() {
            let what = match command {
                Canvas2DCmd::RegisterCanvas { width, height } => {
                    format!("register {width}x{height}")
                }
                Canvas2DCmd::ResizeCanvas { w, h } => resize(*w, *h),
                Canvas2DCmd::DestroyCanvas => "destroy".to_string(),
                other => panic!("the canvas-lifetime fixture produced a draw command: {other:?}"),
            };
            let canvas = self.names.of(canvas_id);
            self.effects.push(Effect { canvas, what });
        }
    }
    fn gl_batch(&mut self, commands: PooledVec<GLCmd>, _approx_bytes: usize) {
        if let Some(command) = commands.first() {
            panic!("the canvas-lifetime fixture produced a GL command: {command:?}");
        }
    }
    fn materialize(&mut self, _canvas_id: u32) {}
}

fn repository() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn batch_effects(names: &mut Names, batch: &CanvasBatchPayload, into: &mut Vec<Effect>) {
    for command in batch.commands.iter() {
        let what = match command {
            Canvas2DCmd::ResizeCanvas { w, h } => resize(*w, *h),
            other => panic!("the in-process fixture batched an unexpected command: {other:?}"),
        };
        let canvas = names.of(batch.canvas_id);
        into.push(Effect { canvas, what });
    }
}

#[test]
#[ignore = "needs the producer's packets from node; run through scripts/test-performance-plus-engine-contract.sh"]
fn a_canvas_created_and_resized_has_the_same_effect_on_both_lanes() {
    let directory = PathBuf::from(
        std::env::var("MIGO_CANVAS_LIFETIME_PARITY_DIR")
            .expect("MIGO_CANVAS_LIFETIME_PARITY_DIR names engine-resource-parity.mjs's output"),
    );
    let script =
        std::fs::read_to_string(repository().join(
            "platforms/apple/WebContent/PerformancePlus/test/fixtures/canvas-lifetime-calls.js",
        ))
        .expect("the fixture script");

    // In process. The render thread is this helper: it answers the two
    // synchronous commands the facade makes -- `GetInfo`, which every `Canvas`
    // constructor calls, and `DestroyCanvas`, which blocks the caller -- and
    // records what arrives, in the order it arrives, which is the order the
    // ops were called because both kinds travel one channel.
    let (mut runtime, render_rx) = new_webgl_runtime();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<Vec<Effect>>();
    let collector = std::thread::spawn(move || {
        let mut names = Names::default();
        let mut effects = Vec::new();
        // The fixture is done when the channel goes quiet: the frame end that
        // follows it submits the last packet, and nothing else is coming.
        while let Ok(command) = render_rx.recv_timeout(Duration::from_secs(5)) {
            match command {
                RenderCommand::Canvas(CanvasCmd::GetInfo { id: _, resp }) => resp.send(Ok((1, 1))),
                RenderCommand::Canvas(CanvasCmd::RegisterOffscreen { id, width, height }) => {
                    let canvas = names.of(id);
                    effects.push(Effect {
                        canvas,
                        what: format!("register {width}x{height}"),
                    });
                }
                RenderCommand::Canvas(CanvasCmd::DestroyCanvas { id, resp }) => {
                    let canvas = names.of(id);
                    effects.push(Effect {
                        canvas,
                        what: "destroy".to_string(),
                    });
                    resp.send(Ok(()));
                }
                RenderCommand::FramePacket(packet) => {
                    for op in packet.into_ops() {
                        if let FrameOp::CanvasBatch(batch) = op {
                            batch_effects(&mut names, &batch, &mut effects);
                        }
                    }
                }
                _ => {}
            }
        }
        let first = names.first_offscreen;
        let _ = done_tx.send(effects);
        first
    });

    runtime
        .exec_script_owned("canvas-lifetime-calls.js".to_string(), &script)
        .expect("the fixture runs in the embedded runtime");
    end_test_frame(&mut runtime);
    drop(runtime);
    let embedded = done_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the collector thread reports what arrived");
    let embedded_first_offscreen = collector.join().expect("the collector thread finishes");

    // On the producer.
    let mut packets: Vec<PathBuf> = std::fs::read_dir(&directory)
        .expect("the parity directory")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "bin"))
        .collect();
    packets.sort();
    assert!(!packets.is_empty(), "the producer wrote no packets");
    let mut host = Host::default();
    for path in &packets {
        let bytes = std::fs::read(path).expect("a packet");
        let frame = frame_wire::validate(&bytes)
            .unwrap_or_else(|error| panic!("{}: {error:?}", path.display()));
        let Some(stream) = frame.command_stream() else {
            continue;
        };
        let words: Vec<u32> = stream
            .bytes
            .chunks_exact(4)
            .map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
            .collect();
        let stream = frame_wire::stream::validate_frame_stream(&words, words.len() as u32)
            .unwrap_or_else(|error| panic!("{}: the stream is refused: {error:?}", path.display()));
        frame_decode::decode_render_stream_into(&mut host, stream);
    }

    let decoded = host.effects;
    if let Some(index) = (0..embedded.len().max(decoded.len()))
        .find(|&index| embedded.get(index) != decoded.get(index))
    {
        let show = |effect: Option<&Effect>| {
            effect.map_or("<none>".to_string(), |e| format!("{} {}", e.canvas, e.what))
        };
        panic!(
            "effect {index} differs (in process {} effects, producer {}):\n  in process: {}\n  producer:   {}",
            embedded.len(),
            decoded.len(),
            show(embedded.get(index)),
            show(decoded.get(index)),
        );
    }

    // The numbering the comparison above deliberately drops: both lanes allocate
    // out of the base the renderer requires, and a lane that allocated below it
    // would have every canvas refused on a device while this test still passed.
    for (lane, first) in [
        ("in process", embedded_first_offscreen),
        ("the producer", host.names.first_offscreen),
    ] {
        let first = first.unwrap_or_else(|| panic!("{lane} created no offscreen canvas"));
        assert!(
            first >= PRODUCER_CANVAS_ID_BASE,
            "{lane} allocated canvas id {first}, below the {PRODUCER_CANVAS_ID_BASE} the renderer requires"
        );
    }

    // The fixture's shape, so a harness that ran nothing cannot pass: three
    // canvases created and the resizes of all four.
    let registers = embedded
        .iter()
        .filter(|effect| effect.what.starts_with("register"))
        .count();
    let resizes = embedded
        .iter()
        .filter(|effect| effect.what.starts_with("resize"))
        .count();
    assert_eq!(registers, 3, "the fixture creates three offscreen canvases");
    assert_eq!(
        resizes, 7,
        "the fixture resizes seven times across four canvases"
    );
    println!(
        "{} canvas lifetime effects agree between the in-process ops and the producer's records",
        embedded.len()
    );
}
