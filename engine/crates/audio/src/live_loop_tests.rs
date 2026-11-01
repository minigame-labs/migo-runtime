//! Loop acceptance changes traversal metadata, never source identity or order.
use super::*;
use crate::{
    stream_control::{Command, Prepared, Reply},
    streaming::{PcmPool, StreamPresentation, StreamTimeline, StreamTraversal, stream_channel},
};
use tokio::sync::{mpsc, oneshot};

fn setup(
    cyclic: bool,
    tiny: bool,
) -> (
    InnerAudioPlayer,
    Arc<StreamingState>,
    mpsc::Sender<StreamMsg>,
    mpsc::Receiver<Command>,
) {
    let clock = StreamTimeline::new(if tiny { 17 } else { 48000 }, 48000, 44100, 1).unwrap();
    let source = StreamingState::new_reusable();
    source
        .bind_presentation(if cyclic {
            StreamPresentation::repeated(clock, None).unwrap()
        } else {
            StreamPresentation::single(clock)
        })
        .unwrap();
    let (sender, requests) = stream_control::channel();
    source.install_controls(sender).unwrap();
    let (tx, rx) = stream_channel();
    let mut p = InnerAudioPlayer::new(930, 1);
    p.shared.set_loop_enabled(cyclic);
    p.start_streaming("live loop".into(), rx, source.clone())
        .unwrap();
    send(&tx, StreamRevision::INITIAL, 24000, 0.25);
    p.poll_stream();
    assert!(p.shared.is_loaded());
    p.play();
    p.drain_events().for_each(drop);
    (p, source, tx, requests)
}
fn send(tx: &mpsc::Sender<StreamMsg>, revision: StreamRevision, frames: usize, value: f32) {
    assert!(
        tx.try_send(StreamMsg::Ready {
            revision,
            sample_rate: 44100,
            channels: 1,
        })
        .is_ok()
    );
    let mut pcm = PcmPool::new().take();
    pcm.buffer_mut().resize(frames, value);
    assert!(tx.try_send(StreamMsg::Samples { revision, pcm }).is_ok());
}
fn prepared(
    command: Command,
    source: &Arc<StreamingState>,
    revision: StreamRevision,
    change: impl FnOnce(&mut Option<StreamTraversal>),
) -> oneshot::Receiver<()> {
    let mut traversal = command
        .cutover
        .map(|r| r.traversal(source.timeline().unwrap()).unwrap());
    change(&mut traversal);
    let (accept, receipt) = oneshot::channel();
    command
        .reply
        .send(Reply::Prepared(Prepared {
            revision,
            target: command.target,
            traversal,
            source: source.clone(),
            deadline: command.deadline,
            accept,
        }))
        .unwrap_or_else(|_| panic!("reply closed"));
    receipt
}

#[path = "cyclic_seek_tests.rs"]
mod cyclic_seek;
#[test]
fn loop_waits_for_exact_source_revision_then_keeps_position_without_seek_events() {
    let (mut p, source, tx, mut requests) = setup(false, false);
    p.process(&mut vec![0.0; 6000]);
    let at = p.shared.position_frames();
    p.drain_events().for_each(drop);
    p.set_loop_enabled(true);
    assert!(!p.shared.loop_enabled());
    assert!(!p.source.is_empty());
    let command = requests.try_recv().unwrap();
    assert_eq!(command.cutover.unwrap().output_start, at);
    let mut bus = [0.5; 16];
    p.process(&mut bus);
    assert_eq!(bus, [0.5; 16]);
    assert_eq!(p.shared.position_frames(), at);
    let revision = StreamRevision::INITIAL.checked_next().unwrap();
    let mut ack = prepared(command, &source, revision, |_| {});
    p.poll_stream();
    assert!(ack.try_recv().is_ok());
    assert!(p.source.is_empty());
    assert!(p.shared.loop_enabled());
    assert_eq!(p.shared.position_frames(), at);
    send(&tx, StreamRevision::INITIAL, 16, -0.75);
    send(&tx, revision, 24000, 0.75);
    p.poll_stream();
    let mut out = [0.0; 16];
    assert!(p.process(&mut out));
    assert_eq!(out, [0.75; 16]);
    assert_eq!(p.shared.duration(), 1.0);
    assert_eq!(
        source.presentation(),
        Some(StreamPresentation::single(source.timeline().unwrap()))
    );
    assert!(!p.drain_events().any(|e| matches!(
        e.event_type,
        InnerAudioEventType::Seeking
            | InnerAudioEventType::Seeked
            | InnerAudioEventType::Ended
            | InnerAudioEventType::Error
    )));
}
#[test]
fn disabling_after_many_wraps_uses_new_extent_and_one_exact_ended() {
    let (mut p, source, tx, mut requests) = setup(true, true);
    p.process(&mut vec![0.0; 6000]);
    p.drain_events().for_each(drop);
    let at = p.shared.position_frames();
    p.set_loop_enabled(false);
    let command = requests.try_recv().unwrap();
    let traversal = StreamTraversal::cutover(source.timeline().unwrap(), at, false).unwrap();
    let end = traversal.presentation().output_end().unwrap();
    assert!(end > at);
    let revision = StreamRevision::INITIAL.checked_next().unwrap();
    let mut ack = prepared(command, &source, revision, |_| {});
    p.poll_stream();
    assert!(ack.try_recv().is_ok());
    assert!(!p.shared.loop_enabled());
    assert_eq!(p.shared.position_frames(), at);
    send(&tx, revision, (end - at) as usize, 0.75);
    assert!(tx.try_send(StreamMsg::Done { revision }).is_ok());
    p.poll_stream();
    let mut out = [0.0; 64];
    assert!(!p.process(&mut out));
    assert_eq!(&out[..(end - at) as usize], vec![0.75; (end - at) as usize]);
    let events: Vec<_> = p.drain_events().collect();
    let ended: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == InnerAudioEventType::Ended)
        .collect();
    assert_eq!(ended.len(), 1);
    assert_eq!(ended[0].current_time, 17.0 / 48000.0);
    assert_eq!(ended[0].duration, 17.0 / 48000.0);
    assert_eq!(p.shared.current_time(), 0.0);
    assert!(
        events
            .iter()
            .all(|e| e.event_type != InnerAudioEventType::Error)
    );
    p.process(&mut out);
    assert!(
        !p.drain_events()
            .any(|e| e.event_type == InnerAudioEventType::Ended)
    );
}
#[test]
fn loop_receipt_must_match_the_requested_output_mode_and_clock() {
    for bad in 0..4 {
        let (mut p, source, _tx, mut requests) = setup(false, false);
        p.process(&mut vec![0.0; 6000]);
        p.set_loop_enabled(true);
        let command = requests.try_recv().unwrap();
        let request = command.cutover.unwrap();
        let mut ack = prepared(
            command,
            &source,
            StreamRevision::INITIAL.checked_next().unwrap(),
            |traversal| {
                *traversal = match bad {
                    0 => None,
                    1 => Some(
                        StreamTraversal::cutover(
                            source.timeline().unwrap(),
                            request.output_start + 1,
                            true,
                        )
                        .unwrap(),
                    ),
                    2 => Some(
                        StreamTraversal::cutover(
                            source.timeline().unwrap(),
                            request.output_start,
                            false,
                        )
                        .unwrap(),
                    ),
                    _ => Some(
                        StreamTraversal::cutover(
                            StreamTimeline::new(48001, 48000, 44100, 1).unwrap(),
                            request.output_start,
                            true,
                        )
                        .unwrap(),
                    ),
                };
            },
        );
        p.poll_stream();
        assert!(ack.try_recv().is_err());
        assert!(source.is_cancelled());
        assert!(!p.shared.is_loaded());
    }
}
#[test]
fn queued_on_off_and_later_stop_never_restore_an_older_position() {
    let (mut p, source, _tx, mut requests) = setup(false, false);
    p.process(&mut vec![0.0; 6000]);
    p.set_loop_enabled(true);
    p.set_loop_enabled(false);
    p.stop();
    assert_eq!(p.shared.position_frames(), 0);
    let mut revision = StreamRevision::INITIAL;
    for expected in [true, false, false] {
        let command = requests.try_recv().unwrap();
        assert_eq!(command.cutover.unwrap().enabled, expected);
        revision = revision.checked_next().unwrap();
        let mut ack = prepared(command, &source, revision, |_| {});
        p.poll_stream();
        assert!(ack.try_recv().is_ok());
        assert_eq!(p.shared.position_frames(), 0);
        assert_eq!(p.shared.state(), PlaybackState::Stopped);
    }
    assert!(!p.shared.loop_enabled());
    assert!(!source.is_cancelled());
    assert!(!p.drain_events().any(|e| matches!(
        e.event_type,
        InnerAudioEventType::Seeked
            | InnerAudioEventType::Seeking
            | InnerAudioEventType::Play
            | InnerAudioEventType::Error
    )));
}
#[test]
fn rejected_loop_retains_pcm_position_mode_and_live_source() {
    let (mut p, source, _tx, mut requests) = setup(false, false);
    p.process(&mut [0.0; 137]);
    let at = p.shared.position_frames();
    let samples = p.source.samples().to_vec();
    p.set_loop_enabled(true);
    requests
        .try_recv()
        .unwrap()
        .reply
        .send(Reply::Rejected(shared::error::ErrorCode::InvalidArgument))
        .unwrap_or_else(|_| panic!("closed"));
    p.poll_stream();
    assert_eq!(p.source.samples(), samples);
    assert_eq!(p.shared.position_frames(), at);
    assert!(!p.shared.loop_enabled());
    assert!(!source.is_cancelled());
    assert!(p.shared.is_loaded());
    assert!(requests.try_recv().is_err());
}
#[test]
fn polling_and_mixing_a_pending_loop_have_no_steady_heap_activity() {
    use migo_alloc_probe::{Burst, assert_no_steady_state_allocation};
    let (mut p, _source, _tx, mut requests) = setup(false, false);
    p.set_loop_enabled(true);
    let _command = requests.try_recv().unwrap();
    assert_no_steady_state_allocation(
        Burst {
            path: "pending loop receipt/mix",
            warmup: 64,
            measured: 256,
        },
        |_| {
            p.poll_stream();
            let mut bus = [0.25; 16];
            p.process(&mut bus);
            assert_eq!(bus, [0.25; 16]);
            p.drain_events().for_each(drop);
        },
    );
}
#[test]
fn loading_loop_intent_is_submitted_once_after_source_can_play() {
    let source = StreamingState::new_reusable();
    source
        .bind_presentation(StreamPresentation::single(
            StreamTimeline::new(48000, 48000, 44100, 1).unwrap(),
        ))
        .unwrap();
    let (sender, mut requests) = stream_control::channel();
    source.install_controls(sender).unwrap();
    let (tx, rx) = stream_channel();
    let mut p = InnerAudioPlayer::new(931, 1);
    p.start_streaming("loading loop".into(), rx, source.clone())
        .unwrap();
    p.set_loop_enabled(true);
    assert!(requests.try_recv().is_err());
    send(&tx, StreamRevision::INITIAL, 24000, 0.25);
    p.poll_stream();
    let command = requests.try_recv().unwrap();
    assert!(command.cutover.unwrap().enabled);
    p.poll_stream();
    assert!(p.shared.is_loaded());
    assert!(requests.try_recv().is_err());
    let revision = StreamRevision::INITIAL.checked_next().unwrap();
    let mut ack = prepared(command, &source, revision, |_| {});
    p.poll_stream();
    assert!(ack.try_recv().is_ok());
    assert!(p.shared.loop_enabled());
    p.set_loop_enabled(false);
    let command = requests.try_recv().unwrap();
    assert!(!command.cutover.unwrap().enabled);
    let mut ack = prepared(command, &source, revision.checked_next().unwrap(), |_| {});
    p.poll_stream();
    assert!(ack.try_recv().is_ok());
    assert!(!p.shared.loop_enabled());
    assert!(requests.try_recv().is_err());
}
#[test]
fn loop_acceptance_does_not_resume_a_paused_or_stopped_player() {
    for state in [PlaybackState::Paused, PlaybackState::Stopped] {
        let (mut p, source, _tx, mut requests) = setup(false, false);
        p.shared.set_state(state);
        p.set_loop_enabled(true);
        let command = requests.try_recv().unwrap();
        let mut ack = prepared(
            command,
            &source,
            StreamRevision::INITIAL.checked_next().unwrap(),
            |_| {},
        );
        p.poll_stream();
        assert!(ack.try_recv().is_ok());
        assert_eq!(p.shared.state(), state);
        assert!(!p.drain_events().any(|e| matches!(
            e.event_type,
            InnerAudioEventType::Play | InnerAudioEventType::Seeking | InnerAudioEventType::Seeked
        )));
    }
}
