use super::*;
use crate::runtime::service_ops;
use shared::audio_channel::AudioCommandReceiver;
use shared::audio_resources::AudioResourceRegistry;
use shared::channel::ThreadWakeup;
use shared::op_state::AudioHostStartSignal;
use shared::protocol::audio_cmd::{AudioCmd, DecodedPcm};

fn binding() -> (AudioBinding, AudioCommandReceiver) {
    let (tx, rx) = shared::audio_channel::channel();
    (
        AudioBinding {
            sender: AudioSender::hosted(
                tx,
                ThreadWakeup::new(),
                AudioHostStartSignal::new(),
                AudioResourceRegistry::new(),
            ),
            runtime_generation: 1,
            // An allow list with one host, so a refusal a test asserts is the
            // policy's and not "this lane has no network".
            network_policy: shared::network_policy::NetworkPolicy::from_json(
                r#"{"download":["media.example"]}"#,
            )
            .unwrap(),
        },
        rx,
    )
}

struct SourceState(tokio::sync::Notify);
impl shared::audio_load::AudioLoadState for SourceState {
    fn generation(&self) -> u64 {
        1
    }
    fn generation_changed(&self) -> &tokio::sync::Notify {
        &self.0
    }
}
fn accept_source_begin(rx: &AudioCommandReceiver) {
    let AudioCmd::InnerAudioBeginLoad {
        source_id, resp, ..
    } = rx.try_recv().unwrap()
    else {
        panic!("source begin");
    };
    let state = std::sync::Arc::new(SourceState(tokio::sync::Notify::new()));
    resp.send(Ok(shared::audio_load::InnerAudioLoadToken::new(
        &state, 1, source_id,
    )))
    .unwrap();
}

fn no_sources() -> LocalSources {
    LocalSources {
        code_dir: None,
        vfs: None,
        local_file: None,
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn bits(value: f32) -> OwnedValue {
    OwnedValue::U32(value.to_bits())
}

/// Every audio op the contract sends to the host is answered here -- or, for
/// the platform's audio settings, by the service context -- in the shape its
/// lane says: a synchronous op is not accepted as a command, and a
/// command is not answered as a request. An audio op with a number and no
/// handler would reach the host and be refused as unknown -- a sound that
/// never plays with nothing thrown.
#[test]
fn every_numbered_audio_op_is_handled_in_its_contract_lane() {
    let contract: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../contracts/runtime/op-boundary.json"
    ))
    .expect("the contract is JSON");
    let audio = contract["extensions"]["host_v8_audio"]["ops"]
        .as_object()
        .expect("the audio extension's ops");
    let mut handled = 0;
    for (name, number) in service_ops::ALL {
        let Some(lane) = audio.get(*name) else {
            continue;
        };
        let lane = lane.as_str().expect("an audio lane is a name");
        let answered = [
            ("sync", is_sync(*number) || is_platform(*number)),
            ("async", is_async(*number)),
            ("command", is_command(*number)),
        ];
        let lanes: Vec<&str> = answered
            .iter()
            .filter(|(_, yes)| *yes)
            .map(|(lane, _)| *lane)
            .collect();
        assert_eq!(lanes, [lane], "{name} is {lane} in the contract");
        handled += 1;
    }
    let sent = audio
        .values()
        .filter(|lane| lane.as_str() != Some("unsupported"))
        .count();
    assert_eq!(
        handled, sent,
        "every audio op the host answers has a number"
    );
}

#[test]
fn a_source_end_watch_is_a_command_with_a_node_and_a_flag() {
    let (audio, rx) = binding();
    command(
        &audio,
        id::op_audio_watch_source_ended,
        vec![OwnedValue::U32(7), OwnedValue::Bool(true)],
    )
    .unwrap();
    assert!(matches!(
        rx.try_recv().unwrap(),
        AudioCmd::WatchSourceEnded {
            node_id: 7,
            enabled: true
        }
    ));
    let error = command(
        &audio,
        id::op_audio_watch_source_ended,
        vec![OwnedValue::U32(7)],
    )
    .unwrap_err();
    assert_eq!(error.class, "TypeError");
}

#[test]
fn a_command_is_applied_in_the_shape_the_embedded_op_sends() {
    let (audio, rx) = binding();
    command(
        &audio,
        id::op_audio_set_loop,
        vec![
            OwnedValue::U32(4),
            OwnedValue::Bool(true),
            OwnedValue::F64(0.5),
            OwnedValue::F64(1.5),
        ],
    )
    .unwrap();
    assert!(matches!(
        rx.try_recv().unwrap(),
        AudioCmd::SetLoop {
            node_id: 4,
            loop_enabled: true,
            loop_start: 0.5,
            loop_end: 1.5
        }
    ));

    command(
        &audio,
        id::op_audio_param_set_target,
        vec![
            OwnedValue::U32(4),
            OwnedValue::Str("gain".into()),
            bits(0.25),
            OwnedValue::F64(2.0),
            OwnedValue::F64(0.1),
        ],
    )
    .unwrap();
    assert!(matches!(
        rx.try_recv().unwrap(),
        AudioCmd::AudioParamSetTarget { node_id: 4, ref param_name, target, start_time, time_constant }
            if param_name == "gain" && target == 0.25 && start_time == 2.0 && time_constant == 0.1
    ));

    command(
        &audio,
        id::op_inner_audio_set_volume,
        vec![OwnedValue::U32(9), bits(0.5)],
    )
    .unwrap();
    assert!(matches!(
        rx.try_recv().unwrap(),
        AudioCmd::InnerAudioSetVolume { id: 9, volume } if volume == 0.5
    ));
}

#[test]
fn a_check_a_command_fails_is_refused_before_it_is_queued() {
    let (audio, rx) = binding();
    let error = command(
        &audio,
        id::op_audio_start_oscillator,
        vec![OwnedValue::U32(1), OwnedValue::F64(-1.0)],
    )
    .unwrap_err();
    assert_eq!(error.message, "when must be a finite, non-negative number");
    let error = command(&audio, id::op_audio_set_loop, vec![OwnedValue::U32(1)]).unwrap_err();
    assert_eq!(error.class, "TypeError");
    assert!(rx.try_recv().is_err());
}

/// A decode answers the buffer's shape in `AudioBufferInfo`'s field order and
/// the id it was adopted as; starting that id then needs no backing.
#[test]
fn a_decode_is_answered_as_its_info_and_its_pcm_stays_here() {
    let (audio, rx) = binding();
    let rt = runtime();
    let future = call_async(
        &audio,
        no_sources(),
        id::op_audio_decode_audio_data,
        vec![OwnedValue::U32(2), OwnedValue::Bytes(vec![1, 2, 3, 4])],
    )
    .unwrap();
    let AudioCmd::DecodeAudioData {
        ctx_id: 2,
        data,
        resp,
    } = rx.try_recv().unwrap()
    else {
        panic!("a decode");
    };
    assert_eq!(data.as_slice(), &[1, 2, 3, 4]);
    resp.send(Ok(DecodedPcm::new(44_100, 1, 3, vec![0.0, 0.5, 1.0])))
        .unwrap();
    let OwnedValue::Array(info) = rt.block_on(future).unwrap() else {
        panic!("an array");
    };
    let [
        OwnedValue::U32(buffer_id),
        OwnedValue::F64(duration),
        OwnedValue::U32(44_100),
        OwnedValue::U32(1),
        OwnedValue::U32(3),
    ] = info.as_slice()
    else {
        panic!("id, duration, sample_rate, channels, length: {info:?}");
    };
    assert_eq!(*duration, 3.0 / 44_100.0);

    command(
        &audio,
        id::op_audio_start_buffer,
        vec![
            OwnedValue::U32(2),
            OwnedValue::U32(5),
            OwnedValue::U32(*buffer_id),
            OwnedValue::Null,
            OwnedValue::F64(0.0),
            OwnedValue::F64(0.0),
            OwnedValue::F64(-1.0),
        ],
    )
    .unwrap();
    let AudioCmd::StartBuffer {
        buffer: Some(snapshot),
        ..
    } = rx.try_recv().unwrap()
    else {
        panic!("a start with the adopted snapshot");
    };
    assert_eq!(snapshot.samples(), &[0.0, 0.5, 1.0]);

    // And its channels come back only when asked for, as f32 bytes.
    let OwnedValue::Bytes(planar) = call_sync(
        &audio,
        id::op_audio_materialize_buffer,
        vec![OwnedValue::U32(*buffer_id)],
    )
    .unwrap() else {
        panic!("bytes");
    };
    let samples: Vec<f32> = planar
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    assert_eq!(samples, [0.0, 0.5, 1.0]);
}

#[test]
fn a_written_buffer_starts_from_the_bytes_content_sent() {
    let (audio, rx) = binding();
    let OwnedValue::U32(buffer_id) = call_sync(
        &audio,
        id::op_audio_reserve_buffer,
        vec![
            OwnedValue::U32(1),
            OwnedValue::U32(2),
            OwnedValue::U32(48_000),
        ],
    )
    .unwrap() else {
        panic!("a buffer id");
    };
    let initialized = call_sync(
        &audio,
        id::op_audio_materialize_buffer,
        vec![OwnedValue::U32(buffer_id)],
    )
    .unwrap();
    assert!(
        matches!(initialized,OwnedValue::Bytes(ref bytes) if bytes==&vec![0;8]),
        "external initial backing changed wire shape or zero PCM"
    );
    assert!(
        call_sync(
            &audio,
            id::op_audio_materialize_buffer,
            vec![OwnedValue::U32(buffer_id)]
        )
        .is_err(),
        "external initial backing initialized twice"
    );
    let backing: Vec<u8> = [0.25f32, -0.25]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    command(
        &audio,
        id::op_audio_set_started_buffer,
        vec![
            OwnedValue::U32(1),
            OwnedValue::U32(6),
            OwnedValue::U32(buffer_id),
            OwnedValue::Bytes(backing),
        ],
    )
    .unwrap();
    let AudioCmd::SetStartedBuffer {
        node_id: 6,
        buffer: Some(snapshot),
        ..
    } = rx.try_recv().unwrap()
    else {
        panic!("the started buffer");
    };
    assert_eq!(snapshot.samples(), &[0.25, -0.25]);
}

#[test]
fn inner_audio_state_is_answered_in_its_field_order() {
    let value = inner_audio_state(InnerAudioState {
        current_time: 1.5,
        duration: 3.0,
        paused: false,
        volume: 0.5,
        loop_enabled: true,
        playback_rate: 1.25,
        buffered: true,
    });
    assert_eq!(
        value,
        OwnedValue::Array(vec![
            OwnedValue::F64(1.5),
            OwnedValue::F64(3.0),
            OwnedValue::Bool(false),
            OwnedValue::F64(0.5),
            OwnedValue::Bool(true),
            OwnedValue::F64(1.25),
            OwnedValue::Bool(true),
        ])
    );
}

/// A streamed source is held to this session's network policy -- the same gate
/// `fetch` is held to -- and one it admits reaches the audio thread.
#[test]
fn a_streamed_source_goes_through_the_session_s_network_policy() {
    let (audio, rx) = binding();
    let refused = call_async(
        &audio,
        no_sources(),
        id::op_inner_audio_load_url,
        vec![
            OwnedValue::U32(3),
            OwnedValue::U32(3001),
            OwnedValue::Str("https://blocked.example/a.mp3".into()),
        ],
    )
    .unwrap();
    accept_source_begin(&rx);
    let error = runtime().block_on(refused).unwrap_err();
    assert_eq!(error.class, "AudioNetworkError");
    assert!(
        error.message.contains("is not in the download allowlist"),
        "{}",
        error.message
    );
    assert!(rx.try_recv().is_err(), "a refused source is never queued");

    let admitted = call_async(
        &audio,
        no_sources(),
        id::op_inner_audio_load_url,
        vec![
            OwnedValue::U32(3),
            OwnedValue::U32(3001),
            OwnedValue::Str("https://media.example/a.mp3".into()),
        ],
    )
    .unwrap();
    accept_source_begin(&rx);
    let rt = runtime();
    let answer = rt.spawn(admitted);
    let command = rt.block_on(async {
        loop {
            if let Ok(command) = rx.try_recv() {
                return command;
            }
            tokio::task::yield_now().await;
        }
    });
    let AudioCmd::InnerAudioLoadUrl {
        id: 3, url, resp, ..
    } = command
    else {
        panic!("a streamed load");
    };
    assert_eq!(url, "https://media.example/a.mp3");
    resp.send(Ok(())).unwrap();
    rt.block_on(answer).unwrap().unwrap();
}

#[test]
fn start_time_configuration_is_an_ordered_f64_command_without_an_implicit_seek() {
    let (audio, rx) = binding();
    command(
        &audio,
        id::op_inner_audio_set_start_time,
        vec![OwnedValue::U32(7), OwnedValue::F64(0.25)],
    )
    .unwrap();
    command(&audio, id::op_inner_audio_play, vec![OwnedValue::U32(7)]).unwrap();
    assert!(matches!(
        rx.try_recv().unwrap(),
        AudioCmd::InnerAudioSetStartTime { id: 7, time: 0.25 }
    ));
    assert!(matches!(
        rx.try_recv().unwrap(),
        AudioCmd::InnerAudioPlay { id: 7 }
    ));
    assert!(rx.try_recv().is_err());
    let error = command(
        &audio,
        id::op_inner_audio_set_start_time,
        vec![OwnedValue::U32(7), OwnedValue::Bool(true)],
    )
    .unwrap_err();
    assert_eq!(error.class, "TypeError");
    assert!(rx.try_recv().is_err());
}

#[test]
fn frequency_response_serializes_owned_arrays_to_the_original_little_endian_shape() {
    use shared::audio_frequency_response::{FrequencyResponse, FrequencyResponseStorage};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    struct Storage {
        data: (Vec<f32>, Vec<f32>),
        dropped: Arc<AtomicBool>,
    }
    impl FrequencyResponseStorage for Storage {
        fn values(&self) -> (&[f32], &[f32]) {
            assert!(!self.dropped.load(Ordering::Acquire));
            (&self.data.0, &self.data.1)
        }
    }
    impl Drop for Storage {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::Release);
        }
    }
    let (audio, rx) = binding();
    let values = vec![0.0_f32, -0.0, 1.5, f32::INFINITY, f32::NAN];
    let bytes = values
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect::<Vec<_>>();
    let future = call_async(
        &audio,
        no_sources(),
        id::op_audio_get_frequency_response,
        vec![OwnedValue::U32(20), OwnedValue::Bytes(bytes.clone())],
    )
    .unwrap();
    let AudioCmd::GetFrequencyResponse {
        node_id,
        frequencies,
        resp,
    } = rx.try_recv().unwrap()
    else {
        panic!("frequency request")
    };
    assert_eq!(node_id, 20);
    assert_eq!(
        frequencies.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
        values.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
    );
    let dropped = Arc::new(AtomicBool::new(false));
    resp.send(Ok(FrequencyResponse::new(Storage {
        data: (values.clone(), values),
        dropped: dropped.clone(),
    })))
    .unwrap();
    assert!(!dropped.load(Ordering::Acquire));
    let result = runtime().block_on(future).unwrap();
    assert_eq!(
        result,
        OwnedValue::Array(vec![
            OwnedValue::Bytes(bytes.clone()),
            OwnedValue::Bytes(bytes)
        ]),
        "external response bytes changed"
    );
    assert!(
        dropped.load(Ordering::Acquire),
        "external conversion retained native arrays"
    );
}
