use frame_wire::builder::WireFrameBuilder;
use frame_wire::{FrameIngress, SECTION_KIND_COMMAND_STREAM};
use shared::{FrameOp, FramePacket, FramePacketBuilder};

fn credited(ingress: &mut FrameIngress, sequence: u64) -> FramePacket {
    let mut wire = WireFrameBuilder::new();
    wire.launch_nonce = 7;
    wire.sequence = sequence;
    let bytes = wire
        .section(SECTION_KIND_COMMAND_STREAM, 2, &[0; 8])
        .build();
    let (_, frame) = ingress.submit(&bytes);
    FramePacketBuilder::new(sequence, 0.0)
        .push(FrameOp::BeginFrame)
        .push(FrameOp::Present)
        .finish()
        .with_credit(frame.unwrap().into_credit())
}

#[test]
fn queued_and_partially_consumed_operations_hold_credit() {
    let mut ingress = FrameIngress::new(7, 1);
    let first = credited(&mut ingress, 1);
    let second = credited(&mut ingress, 2);
    assert_eq!(ingress.remaining_credits(), 0);
    let mut ops = first.into_ops().into_iter();
    assert!(matches!(ops.next(), Some(FrameOp::BeginFrame)));
    assert_eq!(ingress.remaining_credits(), 0);
    drop(ops);
    assert_eq!(ingress.remaining_credits(), 1);
    drop(second);
    assert_eq!(ingress.remaining_credits(), 2);
}

#[test]
fn consuming_scope_keeps_credit_while_deferred_operations_execute() {
    let mut ingress = FrameIngress::new(7, 1);
    let packet = credited(&mut ingress, 1);
    packet.into_ops().consume(|ops| {
        let deferred: Vec<_> = ops.into_iter().collect();
        assert_eq!(ingress.remaining_credits(), 1);
        for op in deferred {
            std::hint::black_box(op);
            assert_eq!(ingress.remaining_credits(), 1);
        }
    });
    assert_eq!(ingress.remaining_credits(), 2);
}
