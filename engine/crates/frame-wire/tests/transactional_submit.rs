use frame_wire::builder::WireFrameBuilder;
use frame_wire::{FrameIngress, IngressDecision, SECTION_KIND_COMMAND_STREAM};

fn packet() -> Vec<u8> {
    let mut builder = WireFrameBuilder::new();
    builder.launch_nonce = 7;
    builder
        .section(SECTION_KIND_COMMAND_STREAM, 2, &[0; 8])
        .build()
}

#[test]
fn rejected_consumer_does_not_commit_sequence_or_credit() {
    let mut ingress = FrameIngress::new(7, 1);
    let bytes = packet();
    let outcome = ingress.submit_with(&bytes, |_frame| Err(2003));
    assert_eq!(outcome.decision, IngressDecision::Rejected);
    assert_eq!(outcome.wire_error_code, 2003);
    assert_eq!(outcome.accepted_sequence, 0);
    assert_eq!(ingress.last_accepted_sequence(), 0);
    assert_eq!(ingress.remaining_credits(), 2);
    let mut retained = None;
    let retry = ingress.submit_with(&bytes, |frame| {
        retained = Some(frame);
        Ok(())
    });
    assert_eq!(retry.decision, IngressDecision::Accepted);
    assert_eq!(ingress.last_accepted_sequence(), 1);
    assert_eq!(ingress.remaining_credits(), 1);
    drop(retained);
    assert_eq!(ingress.remaining_credits(), 2);
}

#[test]
fn decoded_bytes_return_before_the_consumers_credit() {
    let mut ingress = FrameIngress::new(7, 1);
    let (_, frame) = ingress.submit(&packet());
    let frame = frame.unwrap();
    assert_eq!(ingress.pool().idle_bytes(), 0);
    let credit = frame.into_credit();
    assert!(ingress.pool().idle_bytes() > 0);
    assert_eq!(ingress.remaining_credits(), 1);
    drop(credit);
    assert_eq!(ingress.remaining_credits(), 2);
}
