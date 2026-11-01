//! Public player commands must use source receipts without a mixer mailbox.
use super::*;
use crate::{
    stream_control::{self, Command, Prepared, Reply},
    streaming::{PcmPool, StreamTimeline, stream_channel},
};
use tokio::{
    sync::{mpsc, oneshot},
    time::Instant,
};

fn setup(
    state: PlaybackState,
) -> (
    InnerAudioPlayer,
    Arc<StreamingState>,
    mpsc::Sender<StreamMsg>,
    mpsc::Receiver<Command>,
) {
    let source = StreamingState::new_reusable();
    source
        .bind_timeline(StreamTimeline::new(44100, 44100, 48000, 1).unwrap())
        .unwrap();
    let (commands, requests) = stream_control::channel();
    source.install_controls(commands).unwrap();
    let (tx, rx) = stream_channel();
    let mut player = InnerAudioPlayer::new(911, 1);
    player
        .start_streaming("controlled source".into(), rx, source.clone())
        .unwrap();
    send_pcm(&tx, StreamRevision::INITIAL, 24000, 0.25);
    player.poll_stream();
    assert!(player.shared.is_loaded());
    player.shared.set_state(state);
    player.drain_events().for_each(drop);
    (player, source, tx, requests)
}
fn send_pcm(tx: &mpsc::Sender<StreamMsg>, revision: StreamRevision, frames: usize, value: f32) {
    assert!(
        tx.try_send(StreamMsg::Ready {
            revision,
            sample_rate: 48000,
            channels: 1
        })
        .is_ok()
    );
    let mut pool = PcmPool::new();
    let mut pcm = pool.take();
    pcm.buffer_mut().resize(frames, value);
    assert!(tx.try_send(StreamMsg::Samples { revision, pcm }).is_ok());
}

#[test]
fn configured_start_refusal_never_plays_unpositioned_pcm() {
    for refusal in 0..4 {
        let (mut player, source, _tx, mut commands) = setup(PlaybackState::Stopped);
        player.set_start_time(0.25);
        if refusal == 0 {
            commands.close();
        } else if refusal == 1 {
            let sender = source.control_sender().unwrap();
            for _ in 0..crate::streaming::STREAM_CHANNEL_CAPACITY {
                let (reply, _response) = oneshot::channel();
                sender
                    .try_send(Command {
                        target: 0,
                        cutover: None,
                        deadline: Instant::now() + std::time::Duration::from_secs(1),
                        reply,
                    })
                    .unwrap();
            }
        }
        player.play();
        let mut bus = [0.75; 3];
        player.process(&mut bus);
        assert_eq!(bus, [0.75; 3], "no prefix before the start receipt");
        if refusal >= 2 {
            let command = commands.try_recv().unwrap();
            assert_eq!(command.target, 11025);
            assert!(
                command
                    .reply
                    .send(if refusal == 2 {
                        Reply::Rejected(ErrorCode::InputSaturated)
                    } else {
                        Reply::NotStarted(ErrorCode::Cancelled)
                    })
                    .is_ok()
            );
            player.poll_stream();
        }
        player.process(&mut bus);
        assert_eq!(
            bus, [0.75; 3],
            "a refused start must not mix the old prefix"
        );
        assert!(!player.shared.is_loaded());
        assert_eq!(player.shared.state(), PlaybackState::Stopped);
        assert!(source.is_cancelled());
        let observed = events(&mut player);
        assert_eq!(
            observed
                .iter()
                .filter(|e| **e == InnerAudioEventType::Error)
                .count(),
            1
        );
        assert!(!observed.contains(&InnerAudioEventType::Ended));
        assert!(!observed.contains(&InnerAudioEventType::Seeked));
    }
}
fn prepare(
    command: Command,
    source: &Arc<StreamingState>,
    revision: StreamRevision,
) -> oneshot::Receiver<()> {
    let (accept, receipt) = oneshot::channel();
    assert!(
        command
            .reply
            .send(Reply::Prepared(Prepared {
                traversal: None,
                revision,
                target: command.target,
                source: source.clone(),
                deadline: command.deadline,
                accept
            }))
            .is_ok()
    );
    receipt
}
fn events(player: &mut InnerAudioPlayer) -> Vec<InnerAudioEventType> {
    player.drain_events().map(|e| e.event_type).collect()
}

#[test]
fn public_seek_waits_for_source_then_for_fresh_pcm_in_every_play_state() {
    for state in [
        PlaybackState::Playing,
        PlaybackState::Paused,
        PlaybackState::Stopped,
    ] {
        let (mut p, source, tx, mut commands) = setup(state);
        let before = p.source.samples().to_vec();
        p.seek(0.125);
        assert_eq!(p.source.samples(), before);
        let command = commands.try_recv().unwrap();
        assert_eq!(command.target, 5512); // source frames; not 6000 output frames
        let mut output = [0.75; 3];
        p.process(&mut output);
        assert_eq!(output, [0.75; 3]);
        assert!(events(&mut p).is_empty());
        let revision = StreamRevision::INITIAL.checked_next().unwrap();
        let mut receipt = prepare(command, &source, revision);
        assert!(receipt.try_recv().is_err());
        p.poll_stream();
        assert!(receipt.try_recv().is_ok());
        assert!(p.source.is_empty());
        assert_eq!(p.shared.position_frames(), 6000);
        assert_eq!(p.shared.state(), state);
        assert_eq!(events(&mut p), [InnerAudioEventType::Seeking]);
        send_pcm(&tx, revision, 8, 0.5);
        p.poll_stream();
        assert_eq!(events(&mut p), [InnerAudioEventType::Seeked]);
        assert_eq!(p.shared.duration(), 1.0);
        assert_eq!(p.shared.state(), state);
    }
}

#[test]
fn a_later_stop_cannot_be_reversed_by_an_older_seek_receipt() {
    {
        let (mut probe, source, _tx, _commands) = setup(PlaybackState::Playing);
        probe.seek(0.25);
        probe.stop();
        probe
            .accept_stream_revision_as(
                &source,
                StreamRevision::INITIAL.checked_next().unwrap(),
                12000,
                false,
                None,
                false,
            )
            .unwrap();
        assert_eq!(
            probe.shared.position_frames(),
            0,
            "a concurrent reader can observe the receipt before the outer poll continues"
        );
    }

    let (mut p, source, tx, mut commands) = setup(PlaybackState::Playing);
    p.seek(0.25);
    p.stop();
    p.play();
    assert_eq!(p.shared.position_frames(), 0);
    let first = commands.try_recv().unwrap();
    let restart = commands.try_recv().unwrap();
    assert_eq!((first.target, restart.target), (11025, 0));
    let revision = StreamRevision::INITIAL.checked_next().unwrap();
    let mut first_receipt = prepare(first, &source, revision);
    p.poll_stream();
    assert!(first_receipt.try_recv().is_ok());
    assert_eq!(p.shared.position_frames(), 0);
    let mut output = [0.0; 4];
    p.process(&mut output);
    assert_eq!(output, [0.0; 4]);
    assert!(p.needs_stream_poll());
    let next = revision.checked_next().unwrap();
    let mut restarted = prepare(restart, &source, next);
    p.poll_stream();
    assert!(restarted.try_recv().is_ok());
    send_pcm(&tx, revision, 3, 0.9);
    send_pcm(&tx, next, 8, 0.5);
    p.poll_stream();
    p.process(&mut output);
    assert_eq!(output, [0.5; 4]);
    let events = events(&mut p);
    assert!(!events.contains(&InnerAudioEventType::Seeking));
    assert!(!events.contains(&InnerAudioEventType::Seeked));
    assert!(!events.contains(&InnerAudioEventType::Ended));
}

#[test]
fn public_play_restarts_a_reclaimed_completed_source_without_replaying_its_tail() {
    let (mut p, source, tx, mut commands) = setup(PlaybackState::Stopped);
    let revision = StreamRevision::INITIAL.checked_next().unwrap();
    p.accept_stream_revision(&source, revision, 47990).unwrap();
    send_pcm(&tx, revision, 10, 0.8);
    assert!(tx.try_send(StreamMsg::Done { revision }).is_ok());
    p.poll_stream();
    events(&mut p);
    p.play();
    p.process(&mut [0.0; 20]);
    assert_eq!(
        events(&mut p)
            .iter()
            .filter(|e| **e == InnerAudioEventType::Ended)
            .count(),
        1
    );
    assert!(p.replay_needed);
    assert!(!p.needs_stream_poll());
    p.play();
    let command = commands.try_recv().unwrap();
    assert_eq!(command.target, 0);
    assert!(p.needs_stream_poll());
    let mut output = [0.0; 4];
    p.process(&mut output);
    assert_eq!(output, [0.0; 4]);
    let next = revision.checked_next().unwrap();
    let mut receipt = prepare(command, &source, next);
    p.poll_stream();
    assert!(receipt.try_recv().is_ok());
    send_pcm(&tx, next, 8, 0.5);
    p.poll_stream();
    p.process(&mut output);
    assert_eq!(output, [0.5; 4]);
    assert!(!events(&mut p).contains(&InnerAudioEventType::Ended));
}

#[test]
fn rejected_public_targets_preserve_the_previous_pcm_clock_and_play_state() {
    for not_started in [false, true] {
        let (mut p, _, _tx, mut commands) = setup(PlaybackState::Paused);
        let before = p.source.samples().to_vec();
        p.seek(5.0);
        let command = commands.try_recv().unwrap();
        assert_eq!(command.target, 44100);
        let reply = if not_started {
            Reply::NotStarted(ErrorCode::Timeout)
        } else {
            Reply::Rejected(ErrorCode::InvalidArgument)
        };
        assert!(command.reply.send(reply).is_ok());
        p.poll_stream();
        assert_eq!(p.source.samples(), before);
        assert_eq!(p.shared.position_frames(), 0);
        assert_eq!(p.shared.duration(), 1.0);
        assert_eq!(p.shared.state(), PlaybackState::Paused);
        assert_eq!(events(&mut p), [InnerAudioEventType::Error]);
    }
}

#[test]
fn replacement_discards_pending_receipts_before_the_new_source_is_installed() {
    let (mut p, source, _tx, mut commands) = setup(PlaybackState::Playing);
    p.seek(0.25);
    let mut receipt = prepare(
        commands.try_recv().unwrap(),
        &source,
        StreamRevision::INITIAL.checked_next().unwrap(),
    );
    p.load_audio(DecodedAudio {
        samples: vec![0.5; 8],
        sample_rate: 48000,
        channels: 1,
    })
    .unwrap();
    p.poll_stream();
    assert!(receipt.try_recv().is_err());
    assert!(source.is_cancelled());
    assert_eq!(p.source.samples(), [0.5; 8]);
    assert!(p.stream_controls.is_empty());
    assert_eq!(p.stream_revision, StreamRevision::INITIAL);
    assert!(!events(&mut p).contains(&InnerAudioEventType::Seeking));
}

#[test]
fn public_control_backlog_and_waiting_deadline_are_bounded() {
    let (mut p, source, _tx, mut commands) = setup(PlaybackState::Paused);
    for _ in 0..STREAM_CHANNEL_CAPACITY {
        p.seek(0.25);
    }
    let queued: Vec<_> = (0..STREAM_CHANNEL_CAPACITY)
        .map(|_| commands.try_recv().unwrap())
        .collect();
    p.seek(0.5);
    assert!(commands.try_recv().is_err());
    assert!(p.stream_controls.is_full());
    assert_eq!(events(&mut p), [InnerAudioEventType::Error]);
    assert!(!source.is_cancelled());
    p.stop(); // Cannot claim a rewind which did not fit the bounded channel.
    assert!(source.is_cancelled());
    assert!(p.stream_controls.is_empty());
    assert!(queued.iter().all(|command| command.reply.is_closed()));
    let (mut p, source, _tx, _commands) = setup(PlaybackState::Playing);
    p.seek(0.25);
    // Exercise the elapsed absolute-deadline edge without requiring Tokio test-util.
    p.stream_controls.front().unwrap().deadline =
        Instant::now() - std::time::Duration::from_secs(1);
    p.poll_stream();
    assert!(source.is_cancelled());
    assert!(!p.shared.is_loaded());
    assert_eq!(events(&mut p), [InnerAudioEventType::Error]);
}

#[test]
fn source_ports_and_public_time_conversion_reject_invalid_identity_or_clock() {
    let loading = StreamingState::new_reusable();
    let (sender, mut commands) = stream_control::channel();
    loading.install_controls(sender).unwrap();
    let (_tx, rx) = stream_channel();
    let mut player = InnerAudioPlayer::new(915, 1);
    player
        .start_streaming("still preparing".into(), rx, loading.clone())
        .unwrap();
    player.stop();
    assert!(!loading.is_cancelled());
    assert!(commands.try_recv().is_err());
    assert!(events(&mut player).is_empty());
    let (tx, _rx) = stream_control::channel();
    assert!(StreamingState::new().install_controls(tx).is_err());
    let state = StreamingState::new_reusable();
    let (tx, _rx) = stream_control::channel();
    state.install_controls(tx).unwrap();
    let (tx, _rx) = stream_control::channel();
    assert!(state.install_controls(tx).is_err());
    let retired = StreamingState::new_reusable();
    retired.cancel();
    let (tx, _rx) = stream_control::channel();
    assert!(retired.install_controls(tx).is_err());
    let t = StreamTimeline::new(44100, 44100, 48000, 1).unwrap();
    assert_eq!(t.seek_frame(-5.0).unwrap(), 0);
    assert_eq!(t.seek_frame(0.125).unwrap(), 5512);
    assert_eq!(t.seek_frame(50.0).unwrap(), 44100);
    for time in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(t.seek_frame(time).is_err());
    }
    assert!(
        StreamTimeline::new(u64::MAX, 48000, 16000, 1)
            .unwrap()
            .seek_frame(f64::MAX)
            .is_err()
    );
    // A port cannot report buffered success without a source clock.
    let source = StreamingState::new_reusable();
    let (commands_tx, mut commands) = stream_control::channel();
    source.install_controls(commands_tx).unwrap();
    let (tx, rx) = stream_channel();
    let mut p = InnerAudioPlayer::new(912, 1);
    p.start_streaming("missing source clock".into(), rx, source)
        .unwrap();
    send_pcm(&tx, StreamRevision::INITIAL, 24000, 0.25);
    p.poll_stream();
    events(&mut p);
    p.seek(0.25);
    assert!(commands.try_recv().is_err());
    assert_eq!(events(&mut p), [InnerAudioEventType::Error]);
}

#[test]
fn polling_a_pending_public_control_never_allocates_or_renders_old_pcm() {
    use migo_alloc_probe::{Burst, assert_no_steady_state_allocation};
    let (mut p, _source, _tx, mut commands) = setup(PlaybackState::Playing);
    p.seek(0.25);
    let _command = commands.try_recv().unwrap();
    let mut output = [0.75; 4];
    assert_no_steady_state_allocation(
        Burst {
            path: "public source receipt wait",
            warmup: 64,
            measured: 256,
        },
        |_| {
            p.poll_stream();
            p.process(&mut output);
            assert!(p.needs_stream_poll());
            assert_eq!(output, [0.75; 4]);
        },
    );
}

#[test]
fn public_receipts_must_match_the_requested_target_deadline_and_source() {
    for fault in 0..3 {
        let (mut p, source, _tx, mut commands) = setup(PlaybackState::Paused);
        p.seek(0.25);
        let command = commands.try_recv().unwrap();
        let (accept, mut receipt) = oneshot::channel();
        let foreign = StreamingState::new_reusable();
        foreign.bind_timeline(source.timeline().unwrap()).unwrap();
        let prepared = Prepared {
            traversal: None,
            revision: StreamRevision::INITIAL.checked_next().unwrap(),
            target: command.target + i64::from(fault == 0),
            deadline: command.deadline + std::time::Duration::from_secs(u64::from(fault == 1)),
            source: if fault == 2 {
                foreign.clone()
            } else {
                source.clone()
            },
            accept,
        };
        assert!(command.reply.send(Reply::Prepared(prepared)).is_ok());
        p.poll_stream();
        assert!(receipt.try_recv().is_err());
        assert!(!p.shared.is_loaded());
        assert_eq!(p.stream_revision, StreamRevision::INITIAL);
        assert!(!events(&mut p).contains(&InnerAudioEventType::Seeked));
    }
}
