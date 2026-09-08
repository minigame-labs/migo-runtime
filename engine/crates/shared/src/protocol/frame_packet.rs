use crate::command_vec_pool::{PooledIntoIter, PooledVec};
use crate::protocol::render_cmd::{Canvas2DCmd, CanvasBatchPayload, DirtyRect, GlBatchPayload};

#[derive(Debug)]
pub enum FrameOp {
    BeginFrame,
    CanvasBatch(CanvasBatchPayload),
    GlBatch(GlBatchPayload),
    /// Flush/materialize pending Canvas2D work for the given canvas
    /// so subsequent GL ops can observe it in the framebuffer.
    Materialize {
        canvas_id: u32,
    },
    Present,
}

#[derive(Debug)]
pub struct FramePacket {
    frame_id: u64,
    raf_time_ms: f64,
    ops: PooledVec<FrameOp>,
    credit: Option<frame_wire::FrameCredit>,
}

/// Owned frame operations and their admission credit. The credit is returned
/// after the operations are consumed or discarded, including early iterator exit.
#[derive(Debug)]
pub struct FrameOps {
    ops: PooledVec<FrameOp>,
    credit: Option<frame_wire::FrameCredit>,
}

impl FrameOps {
    /// Execute all phases inside this scope, including any deferred operations.
    /// The callback must not move operations into work that outlives this scope.
    pub fn consume<R>(self, consume: impl FnOnce(PooledVec<FrameOp>) -> R) -> R {
        let Self { ops, credit } = self;
        let result = consume(ops);
        drop(credit);
        result
    }
}

impl std::ops::Deref for FrameOps {
    type Target = [FrameOp];

    fn deref(&self) -> &Self::Target {
        &self.ops
    }
}

pub struct FrameOpsIter {
    ops: PooledIntoIter<FrameOp>,
    // Field order keeps the credit alive while unconsumed operations are dropped.
    _credit: Option<frame_wire::FrameCredit>,
}

impl Iterator for FrameOpsIter {
    type Item = FrameOp;

    fn next(&mut self) -> Option<Self::Item> {
        self.ops.next()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.ops.size_hint()
    }
}

impl ExactSizeIterator for FrameOpsIter {}

impl IntoIterator for FrameOps {
    type Item = FrameOp;
    type IntoIter = FrameOpsIter;

    fn into_iter(self) -> Self::IntoIter {
        FrameOpsIter {
            ops: self.ops.into_iter(),
            _credit: self.credit,
        }
    }
}

impl<'a> IntoIterator for &'a FrameOps {
    type Item = &'a FrameOp;
    type IntoIter = std::slice::Iter<'a, FrameOp>;

    fn into_iter(self) -> Self::IntoIter {
        self.ops.iter()
    }
}

pub struct FramePacketBuilder {
    frame_id: u64,
    raf_time_ms: f64,
    ops: PooledVec<FrameOp>,
}

impl FramePacketBuilder {
    /// The op vector comes from the pool rather than from `Vec::new`.
    ///
    /// It used to start empty and grow: one allocation plus its doublings on
    /// every frame the engine renders, for the life of the process, on the
    /// thread running the game. It is the same cross-thread hand-off the command
    /// vectors inside it already make — built here, consumed on the render
    /// thread — so it uses the same recycler, and the loan returns itself when
    /// the render thread finishes with the packet.
    pub fn new(frame_id: u64, raf_time_ms: f64) -> Self {
        Self {
            frame_id,
            raf_time_ms,
            ops: PooledVec::take(),
        }
    }

    /// Use the capacity computed by frame admission, avoiding an oversized
    /// cached allocation left behind by a previous frame.
    pub fn with_op_capacity(frame_id: u64, raf_time_ms: f64, capacity: usize) -> Self {
        Self {
            frame_id,
            raf_time_ms,
            ops: PooledVec::take_with_capacity_limit(capacity),
        }
    }

    pub fn push(mut self, op: FrameOp) -> Self {
        self.push_op(op);
        self
    }

    pub fn push_op(&mut self, op: FrameOp) {
        self.ops.push(op);
    }

    pub fn finish(self) -> FramePacket {
        FramePacket {
            frame_id: self.frame_id,
            raf_time_ms: self.raf_time_ms,
            ops: self.ops,
            credit: None,
        }
    }
}

impl FramePacket {
    pub fn with_credit(mut self, credit: frame_wire::FrameCredit) -> Self {
        assert!(
            self.credit.is_none(),
            "frame already owns an admission credit"
        );
        self.credit = Some(credit);
        self
    }

    pub fn for_canvas_batch(
        frame_id: u64,
        raf_time_ms: f64,
        canvas_id: u32,
        commands: PooledVec<Canvas2DCmd>,
        present: bool,
        dirty_rect: Option<DirtyRect>,
    ) -> Self {
        let builder = FramePacketBuilder::new(frame_id, raf_time_ms)
            .push(FrameOp::BeginFrame)
            .push(FrameOp::CanvasBatch(CanvasBatchPayload {
                canvas_id,
                commands,
                present,
                dirty_rect,
            }));
        if present {
            builder.push(FrameOp::Present).finish()
        } else {
            builder.finish()
        }
    }

    pub fn frame_id(&self) -> u64 {
        self.frame_id
    }

    pub fn raf_time_ms(&self) -> f64 {
        self.raf_time_ms
    }

    pub fn ops(&self) -> &[FrameOp] {
        &self.ops
    }

    /// Hands over the ops *and the loan that holds them*, so a consumer that
    /// simply lets the result go out of scope returns the allocation.
    pub fn into_ops(self) -> FrameOps {
        FrameOps {
            ops: self.ops,
            credit: self.credit,
        }
    }

    /// Convenience constructor for a WebGL-only frame packet.
    ///
    /// Used by `op_gl_flush` at the WebGL frame boundary.  The packet
    /// carries a single `GlBatch` op and gets stamped by RenderServer
    /// with real frame_id / raf_time_ms on the render thread.
    pub fn for_gl_batch(
        frame_id: u64,
        raf_time_ms: f64,
        commands: PooledVec<super::render_cmd::GLCmd>,
    ) -> Self {
        FramePacketBuilder::new(frame_id, raf_time_ms)
            .push(FrameOp::BeginFrame)
            .push(FrameOp::GlBatch(GlBatchPayload { commands }))
            .finish()
    }

    /// Overwrite frame metadata.  Called by RenderServer on the render thread
    /// to stamp the authoritative frame_id and raf_time_ms before execution.
    pub fn set_frame_metadata(&mut self, frame_id: u64, raf_time_ms: f64) {
        self.frame_id = frame_id;
        self.raf_time_ms = raf_time_ms;
    }

    /// Walk every Canvas2D draw-image command in the packet and
    /// feed its referenced image id into `sink` (F-1).  Used by
    /// the render thread to pin `ImageStore` entries while the
    /// packet is queued — a concurrent `DestroyImage` on one of
    /// those ids then defers the actual `glDeleteTextures` call
    /// to the post-frame Present barrier instead of freeing the
    /// GL texture underneath Skia's still-pending draw.
    ///
    /// Pass-through rather than `Vec<u32>` so callers can
    /// allocate their collection shape once (typically a
    /// `SmallVec<[u32; 16]>` on the render thread) without this
    /// helper forcing a `Vec` allocation first.
    pub fn for_each_referenced_image<F: FnMut(crate::protocol::render_cmd::ImageId)>(
        &self,
        mut sink: F,
    ) {
        for op in &self.ops {
            if let FrameOp::CanvasBatch(payload) = op {
                for cmd in &payload.commands {
                    cmd.for_each_referenced_image(&mut sink);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::render_cmd::{Canvas2DCmd, DirtyRect, GlBatchPayload};

    #[test]
    fn canvas_batch_helper_builds_non_presenting_packet() {
        let packet =
            FramePacket::for_canvas_batch(7, 12.5, 1, vec![Canvas2DCmd::Save].into(), false, None);

        assert_eq!(packet.frame_id(), 7);
        assert!(matches!(packet.ops()[0], FrameOp::BeginFrame));
        assert!(matches!(
            packet.ops()[1],
            FrameOp::CanvasBatch(CanvasBatchPayload { present: false, .. })
        ));
        assert!(!matches!(packet.ops().last(), Some(FrameOp::Present)));
    }

    #[test]
    fn canvas_batch_helper_appends_present_for_presenting_packets() {
        let packet = FramePacket::for_canvas_batch(
            3,
            16.6,
            1,
            vec![Canvas2DCmd::Save].into(),
            true,
            Some(DirtyRect {
                x: 0.0,
                y: 0.0,
                width: 32.0,
                height: 32.0,
            }),
        );

        assert!(matches!(packet.ops().last(), Some(FrameOp::Present)));
    }

    #[test]
    fn packet_builder_keeps_ops_in_submission_order() {
        let packet = FramePacketBuilder::new(7, 16.666)
            .push(FrameOp::BeginFrame)
            .push(FrameOp::CanvasBatch(CanvasBatchPayload {
                canvas_id: 3,
                present: false,
                dirty_rect: None,
                commands: vec![Canvas2DCmd::Save].into(),
            }))
            .push(FrameOp::Present)
            .finish();

        assert_eq!(packet.frame_id(), 7);
        assert_eq!(packet.raf_time_ms(), 16.666);
        assert_eq!(packet.ops().len(), 3);

        assert!(matches!(packet.ops()[0], FrameOp::BeginFrame));
        assert!(matches!(
            packet.ops()[1],
            FrameOp::CanvasBatch(CanvasBatchPayload {
                canvas_id: 3,
                present: false,
                dirty_rect: None,
                ..
            })
        ));
        assert!(matches!(packet.ops()[2], FrameOp::Present));
    }

    #[test]
    fn packet_can_carry_presenting_canvas_batch() {
        let packet = FramePacketBuilder::new(3, 16.6)
            .push(FrameOp::BeginFrame)
            .push(FrameOp::CanvasBatch(CanvasBatchPayload {
                canvas_id: 1,
                present: true,
                dirty_rect: Some(DirtyRect {
                    x: 0.0,
                    y: 0.0,
                    width: 32.0,
                    height: 32.0,
                }),
                commands: vec![Canvas2DCmd::Save].into(),
            }))
            .push(FrameOp::Present)
            .finish();

        assert_eq!(packet.frame_id(), 3);
        let FrameOp::CanvasBatch(payload) = &packet.ops()[1] else {
            panic!("expected canvas batch");
        };
        assert!(payload.present);
        assert_eq!(
            payload.dirty_rect.as_ref().map(|rect| rect.width),
            Some(32.0)
        );
    }

    #[test]
    fn packet_can_carry_gl_batch_payload() {
        let packet = FramePacketBuilder::new(9, 20.0)
            .push(FrameOp::BeginFrame)
            .push(FrameOp::GlBatch(GlBatchPayload {
                commands: Vec::new().into(),
            }))
            .push(FrameOp::Present)
            .finish();

        assert!(matches!(
            packet.ops()[1],
            FrameOp::GlBatch(GlBatchPayload { .. })
        ));
    }

    // ---- WebGL packet path tests ----

    #[test]
    fn for_gl_batch_builds_packet_with_begin_frame_and_gl_ops() {
        use crate::protocol::render_cmd::GLCmd;

        let packet = FramePacket::for_gl_batch(
            0,
            0.0,
            vec![GLCmd::Clear {
                canvas_id: 1,
                bit_field: 0x4000,
            }]
            .into(),
        );

        assert_eq!(packet.ops().len(), 2);
        assert!(matches!(packet.ops()[0], FrameOp::BeginFrame));
        assert!(matches!(
            packet.ops()[1],
            FrameOp::GlBatch(GlBatchPayload { .. })
        ));
        // No FrameOp::Present — dirty is driven by execute_gl_batch return value.
        assert!(!matches!(packet.ops().last(), Some(FrameOp::Present)));
    }

    #[test]
    fn for_gl_batch_uses_sentinel_metadata() {
        let packet = FramePacket::for_gl_batch(0, 0.0, Vec::new().into());
        assert_eq!(packet.frame_id(), 0);
        assert_eq!(packet.raf_time_ms(), 0.0);
    }

    #[test]
    fn gl_packet_gets_stamped_by_set_frame_metadata() {
        let mut packet = FramePacket::for_gl_batch(0, 0.0, Vec::new().into());
        packet.set_frame_metadata(42, 16.666);
        assert_eq!(packet.frame_id(), 42);
        assert!((packet.raf_time_ms() - 16.666).abs() < f64::EPSILON);
    }
}
