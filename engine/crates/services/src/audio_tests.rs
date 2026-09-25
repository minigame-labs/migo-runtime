use super::*;
use shared::audio_channel::AudioCommandReceiver;
use shared::channel::ThreadWakeup;
use shared::op_state::AudioHostStartSignal;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

/// A hosted sender with a registry, and the receiving end the audio thread
/// would hold -- here, the test.
fn hosted() -> (AudioSender, AudioCommandReceiver) {
    let (tx, rx) = shared::audio_channel::channel();
    let sender = AudioSender::hosted(
        tx,
        ThreadWakeup::new(),
        AudioHostStartSignal::new(),
        AudioResourceRegistry::new(),
    );
    (sender, rx)
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn section<'a>(source: &'a str, name: &str, end: &str) -> &'a str {
    let start = source.find(name).unwrap_or_else(|| panic!("{name}"));
    let stop = source[start..]
        .find(end)
        .map(|offset| start + offset)
        .unwrap_or_else(|| panic!("end of {name}"));
    &source[start..stop]
}

fn make_temp_dir(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("{}_{}", prefix, nanos))
}

fn sandbox(prefix: &str) -> (PathBuf, PathBuf, Arc<VirtualFS>) {
    let base = make_temp_dir(prefix);
    let code = base.join("code");
    let user = base.join("user");
    let cache = base.join("cache");
    let tmp = base.join("tmp");
    for directory in [&code, &user, &cache, &tmp] {
        fs::create_dir_all(directory).unwrap();
    }
    let vfs = Arc::new(VirtualFS::new(code.clone(), user, cache, tmp));
    (base, code, vfs)
}

// ---------------------------------------------------------------------------
// Checks
// ---------------------------------------------------------------------------

#[test]
fn every_failure_is_an_audio_error() {
    assert_eq!(audio_error("x").class, "AudioError");
    let error = engine_error(EngineError::from_detail(ErrorCode::InputSaturated, "why"));
    assert_eq!(error.class, "AudioError");
    assert!(
        error.message.starts_with("[InputSaturated] "),
        "{}",
        error.message
    );
    assert!(error.message.ends_with(" (why)"), "{}", error.message);
}

#[test]
fn scheduled_times_are_finite_and_not_negative() {
    assert!(validate_scheduled_time("when", 0.0).is_ok());
    assert!(validate_scheduled_time("when", 1.25).is_ok());
    for invalid in [-1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(
            validate_scheduled_time("when", invalid)
                .unwrap_err()
                .message,
            "when must be a finite, non-negative number"
        );
    }
    assert_eq!(validate_optional_duration(-1.0).unwrap(), None);
    assert_eq!(validate_optional_duration(0.0).unwrap(), Some(0.0));
    assert!(validate_optional_duration(-2.0).is_err());
    assert!(validate_optional_duration(f64::NAN).is_err());
}

#[test]
fn encoded_audio_size_boundary_is_inclusive() {
    assert!(validate_encoded_audio_size(MAX_ENCODED_AUDIO_BYTES).is_ok());
    let error = validate_encoded_audio_size(MAX_ENCODED_AUDIO_BYTES + 1)
        .expect_err("limit + 1 must be rejected before copying");
    assert!(error.message.contains("InputSaturated"));
}

#[test]
fn wave_shaper_curve_requires_f32_alignment_and_inclusive_byte_cap() {
    assert!(validate_wave_shaper_curve_size(0).is_ok());
    assert!(validate_wave_shaper_curve_size(MAX_WAVE_SHAPER_CURVE_BYTES).is_ok());
    let unaligned = validate_wave_shaper_curve_size(3)
        .expect_err("a partial f32 must not be silently truncated");
    assert!(unaligned.message.contains("InvalidArgument"));
    let over_limit = validate_wave_shaper_curve_size(MAX_WAVE_SHAPER_CURVE_BYTES + 4)
        .expect_err("limit + one f32 must be rejected");
    assert!(over_limit.message.contains("InputSaturated"));
}

#[test]
fn every_content_controlled_audio_payload_has_a_single_item_boundary() {
    assert!(
        validate_audio_control_string("field", &"x".repeat(MAX_AUDIO_CONTROL_STRING_BYTES)).is_ok()
    );
    assert!(
        validate_audio_control_string("field", &"x".repeat(MAX_AUDIO_CONTROL_STRING_BYTES + 1))
            .unwrap_err()
            .message
            .contains("InputSaturated")
    );
    assert!(validate_frequency_response_bytes(MAX_FREQUENCY_RESPONSE_POINTS * 4).is_ok());
    assert!(
        validate_frequency_response_bytes(MAX_FREQUENCY_RESPONSE_POINTS * 4 + 4)
            .unwrap_err()
            .message
            .contains("InputSaturated")
    );
    assert!(
        validate_frequency_response_bytes(3)
            .unwrap_err()
            .message
            .contains("InvalidArgument")
    );
    assert!(validate_copy_to_channel_size(MAX_COPY_TO_CHANNEL_BYTES).is_ok());
    assert!(
        validate_copy_to_channel_size(MAX_COPY_TO_CHANNEL_BYTES + 4)
            .unwrap_err()
            .message
            .contains("InputSaturated")
    );
    assert!(
        validate_copy_to_channel_size(3)
            .unwrap_err()
            .message
            .contains("InvalidArgument")
    );
}

#[test]
fn iir_coefficients_are_checked_as_web_audio_requires() {
    let message =
        |ff: &[f64], fb: &[f64]| validate_iir_coefficients(ff, fb).err().map(|e| e.message);
    assert_eq!(message(&[1.0], &[1.0]), None);
    assert_eq!(
        message(&[], &[1.0]).as_deref(),
        Some("createIIRFilter: feedforward and feedback must be non-empty")
    );
    assert_eq!(
        message(&[1.0; 21], &[1.0]).as_deref(),
        Some("createIIRFilter: coefficient arrays must have at most 20 elements")
    );
    assert_eq!(
        message(&[1.0], &[0.0, 1.0]).as_deref(),
        Some("createIIRFilter: feedback[0] must not be zero")
    );
    assert_eq!(
        message(&[f64::NAN], &[1.0]).as_deref(),
        Some("createIIRFilter: coefficients must be finite")
    );
}

// ---------------------------------------------------------------------------
// Admission before copies
// ---------------------------------------------------------------------------

#[test]
fn a_string_command_is_checked_then_admitted_then_copied() {
    let source = include_str!("audio.rs");
    let body = section(source, "fn send_named(", "\n}\n");
    let validate = body.find("validate_audio_control_string").unwrap();
    let reserve = body.find("reserve(tx, value.len())").unwrap();
    let owned = body.find(".to_owned()").unwrap();
    assert!(validate < reserve && reserve < owned);
}

#[test]
fn an_oversized_string_is_refused_without_reaching_the_queue() {
    let (tx, rx) = hosted();
    let error = set_node_param(&tx, 1, &"x".repeat(MAX_AUDIO_CONTROL_STRING_BYTES + 1), 0.5)
        .expect_err("over the control-string bound");
    assert!(
        error.message.contains("AudioParam name is 4097 bytes"),
        "{}",
        error.message
    );
    assert!(rx.try_recv().is_err(), "nothing was sent");

    set_node_param(&tx, 1, "gain", 0.5).unwrap();
    assert!(matches!(
        rx.try_recv().unwrap(),
        AudioCmd::SetNodeParam { node_id: 1, ref param_name, value } if param_name == "gain" && value == 0.5
    ));
}

#[test]
fn byte_payloads_are_bounded_and_admitted_before_they_are_decoded() {
    let source = include_str!("audio.rs");
    for (name, validate, reserve) in [
        (
            "pub fn set_wave_shaper_curve(",
            "validate_wave_shaper_curve_size",
            "reserve(tx, byte_len)",
        ),
        (
            "pub fn get_frequency_response(",
            "validate_frequency_response_bytes",
            "reserve(tx, frequencies.len())",
        ),
    ] {
        let body = section(source, name, "\n}\n");
        let validate = body.find(validate).unwrap();
        let reserve = body.find(reserve).unwrap();
        // The conversion is an argument of the send, so it runs first.
        let convert = body.find("le_f32s").unwrap();
        assert!(body.contains("send_reserved("), "{name}");
        assert!(validate < reserve && reserve < convert, "{name}");
    }
}

#[test]
fn a_wave_shaper_curve_arrives_as_its_f32_values() {
    let (tx, rx) = hosted();
    let bytes: Vec<u8> = [0.5f32, -1.0]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    set_wave_shaper_curve(&tx, 3, Some(&bytes)).unwrap();
    assert!(matches!(
        rx.try_recv().unwrap(),
        AudioCmd::SetWaveShaperCurve { node_id: 3, curve: Some(ref curve) } if curve == &[0.5, -1.0]
    ));
    set_wave_shaper_curve(&tx, 3, None).unwrap();
    assert!(matches!(
        rx.try_recv().unwrap(),
        AudioCmd::SetWaveShaperCurve { curve: None, .. }
    ));
    assert!(set_wave_shaper_curve(&tx, 3, Some(&bytes[..5])).is_err());
}

// ---------------------------------------------------------------------------
// AudioBuffer
// ---------------------------------------------------------------------------

/// A decode is answered with samples and adopted as a frozen entry; starting
/// the buffer then publishes that very allocation, with no backing sent.
#[test]
fn a_decoded_buffer_plays_from_the_samples_the_decode_produced() {
    let (tx, rx) = hosted();
    let scope = BufferScope {
        tx: &tx,
        runtime_generation: 7,
    };
    let rt = runtime();

    let permit = reserve_encoded(&tx, 3).unwrap();
    let decoding = decode_audio_data(scope, 1, vec![1, 2, 3], permit).unwrap();
    let AudioCmd::DecodeAudioData { ctx_id, data, resp } = rx.try_recv().unwrap() else {
        panic!("a decode was sent");
    };
    assert_eq!((ctx_id, data.as_slice()), (1, &[1u8, 2, 3][..]));
    let samples = vec![0.25f32, -0.25, 0.5, -0.5];
    let samples_at = samples.as_ptr();
    resp.send(Ok(DecodedPcm {
        sample_rate: 48_000,
        channels: 2,
        frames: 2,
        samples,
    }))
    .unwrap();
    let info = rt.block_on(decoding.answer()).unwrap();
    assert_eq!(
        (info.channels, info.length, info.sample_rate),
        (2, 2, 48_000)
    );
    assert_eq!(info.duration, 2.0 / 48_000.0);

    publish_buffer(
        scope,
        1,
        9,
        info.id,
        None::<LittleEndianPlanar<'_>>,
        Some(StartTiming {
            when: 0.0,
            offset: 0.0,
            duration: -1.0,
        }),
        || {},
    )
    .unwrap();
    let AudioCmd::StartBuffer {
        buffer: Some(snapshot),
        duration: None,
        ..
    } = rx.try_recv().unwrap()
    else {
        panic!("a start with the frozen snapshot");
    };
    assert_eq!(snapshot.samples().as_ptr(), samples_at);

    // Reading it is what copies: a planar allocation for JavaScript.
    assert_eq!(
        materialize_buffer(scope, info.id).unwrap(),
        vec![0.25, 0.5, -0.25, -0.5]
    );
    release_buffer(scope, info.id);
}

#[test]
fn a_decode_is_bounded_before_its_bytes_are_admitted() {
    let (tx, rx) = hosted();
    assert!(
        reserve_encoded(&tx, MAX_ENCODED_AUDIO_BYTES + 1)
            .err()
            .unwrap()
            .message
            .contains("InputSaturated")
    );
    assert!(rx.try_recv().is_err());
}

#[test]
fn a_writable_buffer_sent_as_bytes_freezes_when_started() {
    let (tx, rx) = hosted();
    let scope = BufferScope {
        tx: &tx,
        runtime_generation: 8,
    };
    let id = reserve_buffer(scope, 2, 2, 48_000).unwrap();
    let planar: Vec<u8> = [1.0f32, 2.0, 10.0, 20.0]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();

    // The wrong size is refused, and the buffer is still writable.
    let mut published = false;
    assert!(
        publish_buffer(
            scope,
            1,
            2,
            id,
            Some(LittleEndianPlanar(&planar[..12])),
            None,
            || published = true,
        )
        .is_err()
    );
    assert!(!published);
    assert!(rx.try_recv().is_err());

    publish_buffer(
        scope,
        1,
        2,
        id,
        Some(LittleEndianPlanar(&planar)),
        None,
        || published = true,
    )
    .unwrap();
    assert!(published, "the caller learns the backing was taken");
    let AudioCmd::SetStartedBuffer {
        buffer: Some(snapshot),
        ..
    } = rx.try_recv().unwrap()
    else {
        panic!("the started buffer's snapshot");
    };
    assert_eq!(snapshot.samples(), &[1.0, 10.0, 2.0, 20.0]);
    release_buffer(scope, id);
}

#[test]
fn a_null_buffer_cannot_carry_a_backing_and_times_are_checked_first() {
    let (tx, rx) = hosted();
    let scope = BufferScope {
        tx: &tx,
        runtime_generation: 9,
    };
    assert_eq!(
        publish_buffer(scope, 1, 2, 0, Some(LittleEndianPlanar(&[])), None, || {})
            .unwrap_err()
            .message,
        "a null AudioBuffer id cannot carry a backing store"
    );
    assert_eq!(
        publish_buffer(
            scope,
            1,
            2,
            0,
            None::<LittleEndianPlanar<'_>>,
            Some(StartTiming {
                when: -1.0,
                offset: 0.0,
                duration: -1.0
            }),
            || {},
        )
        .unwrap_err()
        .message,
        "when must be a finite, non-negative number"
    );
    assert!(rx.try_recv().is_err());
}

#[test]
fn buffers_are_scoped_to_their_runtime_generation() {
    let (tx, _rx) = hosted();
    let old = BufferScope {
        tx: &tx,
        runtime_generation: 10,
    };
    let new = BufferScope {
        tx: &tx,
        runtime_generation: 11,
    };
    let id = reserve_buffer(old, 1, 4, 48_000).unwrap();
    // The same serial under another generation names nothing.
    assert!(materialize_buffer(new, id).is_err());
    release_buffer(new, id);
    assert_eq!(
        tx.resources().unwrap().format(AudioBufferKey {
            runtime_generation: 10,
            serial: id
        }),
        Some(AudioBufferFormat {
            channels: 1,
            frames: 4,
            sample_rate: 48_000
        })
    );
    release_buffer(old, id);
}

#[test]
fn an_unhosted_sender_has_no_buffers_and_says_so() {
    let (tx, _rx) = shared::audio_channel::channel();
    let tx = AudioSender::new(tx, ThreadWakeup::new());
    let scope = BufferScope {
        tx: &tx,
        runtime_generation: 1,
    };
    assert!(
        reserve_buffer(scope, 1, 1, 48_000)
            .unwrap_err()
            .message
            .contains("registry is unavailable")
    );
    release_buffer(scope, 1);
}

// ---------------------------------------------------------------------------
// Answers
// ---------------------------------------------------------------------------

#[test]
fn an_answer_is_the_audio_thread_s_and_a_dropped_one_says_so() {
    let (tx, rx) = hosted();
    let rt = runtime();

    let pending = close_context(&tx, 4).unwrap();
    let AudioCmd::CloseContext { ctx_id: 4, resp } = rx.try_recv().unwrap() else {
        panic!("a close");
    };
    resp.send(Err(EngineError::from_detail(ErrorCode::NotFound, "gone")))
        .unwrap();
    let error = rt.block_on(pending.answer()).unwrap_err();
    assert_eq!(
        error,
        engine_error(EngineError::from_detail(ErrorCode::NotFound, "gone"))
    );
    assert!(error.message.starts_with("[NotFound] "));

    // The audio thread dropping a command unanswered -- a context torn down
    // under it -- is an answer too, not a hang.
    let pending = resume_context(&tx, 4).unwrap();
    drop(rx.try_recv().unwrap());
    assert_eq!(
        rt.block_on(pending.answer()).unwrap_err().message,
        "Response channel closed"
    );
}

#[test]
fn analyser_reads_answer_in_their_own_element_type() {
    let (tx, rx) = hosted();
    let rt = runtime();
    let bytes = read_analyser(&tx, AnalyserRead::ByteFrequency, 5).unwrap();
    let AudioCmd::GetAnalyserByteFrequencyData { node_id: 5, resp } = rx.try_recv().unwrap() else {
        panic!("a byte frequency read");
    };
    resp.send(Ok(vec![1, 2])).unwrap();
    assert!(matches!(rt.block_on(bytes).unwrap(), AnalyserData::Bytes(ref b) if b == &[1, 2]));

    let floats = read_analyser(&tx, AnalyserRead::FloatTimeDomain, 5).unwrap();
    let AudioCmd::GetAnalyserFloatTimeDomainData { node_id: 5, resp } = rx.try_recv().unwrap()
    else {
        panic!("a float time-domain read");
    };
    resp.send(Ok(vec![0.5])).unwrap();
    assert!(matches!(rt.block_on(floats).unwrap(), AnalyserData::Floats(ref f) if f == &[0.5]));
}

#[test]
fn a_full_queue_is_saturation_not_disconnection() {
    let (tx, _rx) = hosted();
    for ctx_id in 0..shared::audio_channel::AUDIO_COMMAND_CAPACITY as u32 {
        create_context(&tx, ctx_id, 0).expect("fixture fills the data queue");
    }
    let error = close_context(&tx, 9)
        .err()
        .expect("limit + 1 must fail immediately");
    assert!(error.message.contains("InputSaturated"));
    assert!(error.message.contains("audio command queue is full"));
}

// ---------------------------------------------------------------------------
// Platform options
// ---------------------------------------------------------------------------

#[test]
fn without_a_platform_audio_service_the_options_are_refused_by_name() {
    assert_eq!(
        set_inner_audio_option(None, true, true, false)
            .unwrap_err()
            .message,
        "setInnerAudioOption:fail not supported"
    );
    assert_eq!(
        get_available_audio_sources(None).unwrap_err().message,
        "getAvailableAudioSources:fail not supported"
    );
}

// ---------------------------------------------------------------------------
// InnerAudioContext sources
// ---------------------------------------------------------------------------

#[test]
fn remote_audio_url_classification_is_case_insensitive_and_exact() {
    let http = parse_remote_audio_url("HTTP://media.example/a.mp3")
        .unwrap()
        .expect("HTTP URL should be remote");
    assert_eq!(http.scheme(), "http");
    let https = parse_remote_audio_url("https://media.example/b.mp3")
        .unwrap()
        .expect("HTTPS URL should be remote");
    assert_eq!(https.scheme(), "https");
    assert!(parse_remote_audio_url("audio/bgm.mp3").unwrap().is_none());
    assert!(
        parse_remote_audio_url("asset://audio/bgm.mp3")
            .unwrap()
            .is_none()
    );
    assert!(parse_remote_audio_url("http://").is_err());
}

#[test]
fn a_remote_source_is_admitted_before_it_reaches_the_queue() {
    let source = include_str!("audio.rs");
    let body = section(source, "pub async fn inner_audio_load_url", "\n}\n");
    let admit = body.find("(sources.admit_remote)(&url)?").unwrap();
    let enqueue = body.find("AudioCmd::InnerAudioLoadUrl").unwrap();
    assert!(admit < enqueue);

    let (tx, rx) = hosted();
    let refused = runtime().block_on(inner_audio_load_url(
        tx,
        InnerAudioSources {
            code_dir: None,
            vfs: None,
            admit_remote: |_: &url::Url| Err(audio_error("not on the allow list")),
        },
        1,
        "https://media.example/a.mp3".to_owned(),
    ));
    assert_eq!(refused.unwrap_err().message, "not on the allow list");
    assert!(rx.try_recv().is_err());
}

#[test]
fn a_local_source_is_admitted_before_it_is_opened_and_charged_what_it_keeps() {
    let source = include_str!("audio.rs");
    let body = section(source, "pub async fn inner_audio_load_url", "\n}\n");
    let reserve = body.find("reserve(&tx, MAX_ENCODED_AUDIO_BYTES)").unwrap();
    let open = body
        .find("open_local_audio_source(sources.vfs, source)")
        .unwrap();
    let read = body.find("read_capped_local_audio(file)").unwrap();
    let send = body.find("send_reserved").unwrap();
    assert!(reserve < open && open < read && read < send);
    assert!(body.contains("permit.shrink_to(data.capacity())"));
}

#[test]
fn a_packaged_sound_is_read_through_the_sandbox_and_loaded() {
    let (base, code, vfs) = sandbox("migo_audio_load_url");
    fs::write(code.join("clip.mp3"), b"encoded").unwrap();
    let (tx, rx) = hosted();
    let rt = runtime();

    let load = rt.spawn(inner_audio_load_url(
        tx,
        InnerAudioSources {
            code_dir: Some(code.to_string_lossy().into_owned()),
            vfs: Some(vfs),
            admit_remote: |_: &url::Url| -> Result<(), ServiceError> {
                panic!("a packaged sound is not remote")
            },
        },
        3,
        "clip.mp3".to_owned(),
    ));
    let command = rt.block_on(async {
        loop {
            if let Ok(command) = rx.try_recv() {
                return command;
            }
            tokio::task::yield_now().await;
        }
    });
    let AudioCmd::InnerAudioLoad { id: 3, data, resp } = command else {
        panic!("a load of the file's bytes");
    };
    assert_eq!(data, b"encoded");
    resp.send(Ok(InnerAudioInfo {
        duration: 1.0,
        sample_rate: 48_000,
        channels: 1,
    }))
    .unwrap();
    rt.block_on(load).unwrap().unwrap();
    let _ = fs::remove_dir_all(base);
}

#[test]
fn capped_local_audio_reader_accepts_exact_limit_and_rejects_limit_plus_one() {
    let dir = make_temp_dir("migo_audio_capped_reader");
    fs::create_dir_all(&dir).unwrap();
    let exact = dir.join("exact.mp3");
    let over = dir.join("over.mp3");
    fs::write(&exact, vec![0_u8; MAX_ENCODED_AUDIO_BYTES]).unwrap();
    fs::write(&over, vec![0_u8; MAX_ENCODED_AUDIO_BYTES + 1]).unwrap();
    let rt = runtime();

    let exact_data = rt.block_on(async {
        read_capped_local_audio(tokio::fs::File::open(&exact).await.unwrap()).await
    });
    assert_eq!(exact_data.unwrap().len(), MAX_ENCODED_AUDIO_BYTES);
    let over_limit = rt
        .block_on(async {
            read_capped_local_audio(tokio::fs::File::open(&over).await.unwrap()).await
        })
        .expect_err("limit + 1 local audio input must be rejected");
    assert!(over_limit.message.contains("InputSaturated"));
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn capped_local_audio_reader_uses_open_file_metadata_as_the_initial_capacity_hint() {
    let dir = make_temp_dir("migo_audio_small_reader");
    fs::create_dir_all(&dir).unwrap();
    let tiny = dir.join("tiny.mp3");
    fs::write(&tiny, b"tiny").unwrap();
    let data = runtime().block_on(async {
        read_capped_local_audio(tokio::fs::File::open(&tiny).await.unwrap())
            .await
            .unwrap()
    });
    assert!(data.capacity() < MAX_ENCODED_AUDIO_BYTES);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn capped_local_audio_reader_handles_metadata_underestimation_without_overcharging() {
    let dir = make_temp_dir("migo_audio_growing_reader");
    fs::create_dir_all(&dir).unwrap();
    let audio = dir.join("grown.mp3");
    fs::write(&audio, b"metadata was stale").unwrap();
    let data = runtime().block_on(async {
        read_capped_local_audio_with_capacity_hint(tokio::fs::File::open(&audio).await.unwrap(), 1)
            .await
            .unwrap()
    });
    assert_eq!(data, b"metadata was stale");
    assert!(data.capacity() <= MAX_ENCODED_AUDIO_BYTES);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn capped_local_audio_growth_is_geometric_and_never_exceeds_the_cap() {
    let first = next_local_audio_capacity(0, 0, 1)
        .unwrap()
        .expect("an empty buffer needs its first allocation");
    assert_eq!(first, 8 * 1024);
    let second = next_local_audio_capacity(first, first, 1)
        .unwrap()
        .expect("the next full chunk needs a growth step");
    assert_eq!(second, first * 2);
    assert!(
        next_local_audio_capacity(MAX_ENCODED_AUDIO_BYTES, MAX_ENCODED_AUDIO_BYTES, 1)
            .unwrap_err()
            .message
            .contains("InputSaturated")
    );
}

#[test]
fn local_audio_errors_do_not_disclose_the_resolved_absolute_path() {
    let source = include_str!("audio.rs");
    let body = section(source, "async fn read_capped_local_audio(", "#[cfg(test)]");
    assert!(body.contains("Failed to read local audio file"));
    assert!(
        !body.contains("path"),
        "reader errors must not format a resolved path"
    );
}

#[test]
fn resolve_path_maps_code_virtual_prefix() {
    let code_dir = make_temp_dir("migo_audio_code");
    fs::create_dir_all(code_dir.join("audio")).unwrap();
    let resolved = resolve_path(code_dir.to_str(), "/code/audio/bgm.mp3");
    assert_eq!(PathBuf::from(resolved), code_dir.join("audio/bgm.mp3"));
    let _ = fs::remove_dir_all(code_dir);
}

#[test]
fn resolve_local_src_preserves_user_virtual_path_for_descriptor_safe_open() {
    let (base, code, vfs) = sandbox("migo_audio_vfs");
    let source =
        resolve_local_src(code.to_str(), Some(&vfs), "/user/gamecaches/audio/bgm.mp3").unwrap();
    assert_eq!(
        source,
        LocalAudioSource::Sandboxed {
            virtual_path: "/user/gamecaches/audio/bgm.mp3".to_string(),
        }
    );
    let _ = fs::remove_dir_all(base);
}

#[test]
fn resolve_local_src_maps_relative_paths_to_the_code_virtual_root() {
    let (base, code, vfs) = sandbox("migo_audio_relative_vfs");
    let source = resolve_local_src(code.to_str(), Some(&vfs), "audio/bgm.mp3").unwrap();
    assert_eq!(
        source,
        LocalAudioSource::Sandboxed {
            virtual_path: "/code/audio/bgm.mp3".to_string(),
        }
    );
    let _ = fs::remove_dir_all(base);
}

#[test]
fn resolve_local_src_rejects_absolute_path_outside_sandbox() {
    let (base, code, vfs) = sandbox("migo_audio_escape");
    // An absolute path outside every virtual root (/code, /user, /cache, /tmp)
    // must NOT resolve to a real filesystem path -- that would escape the sandbox.
    let error = resolve_local_src(code.to_str(), Some(&vfs), "/etc/passwd").unwrap_err();
    assert_eq!(error.message, "Local audio path is not permitted");
    assert!(!error.message.contains(&base.to_string_lossy()[..]));
    let _ = fs::remove_dir_all(base);
}

#[test]
fn local_audio_vfs_branch_opens_the_descriptor_instead_of_reopening_a_resolved_path() {
    let source = include_str!("audio.rs");
    let body = section(source, "async fn open_local_audio_source", "\n}\n");
    assert!(body.contains("open_regular_for_read"));
    assert!(body.contains("spawn_blocking"));
    assert!(!body.contains("vfs.resolve"));
}

#[test]
fn local_audio_source_reads_through_the_already_open_vfs_descriptor() {
    let (base, code, vfs) = sandbox("migo_audio_strict_vfs_open");
    fs::write(base.join("user").join("clip.bin"), b"sandboxed-audio").unwrap();
    let source = resolve_local_src(code.to_str(), Some(&vfs), "/user/clip.bin").unwrap();
    let bytes = runtime().block_on(async {
        let file = open_local_audio_source(Some(vfs), source).await.unwrap();
        read_capped_local_audio(file).await.unwrap()
    });
    assert_eq!(bytes, b"sandboxed-audio");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn vendor_specific_local_uri_schemes_are_rejected_without_echoing_them() {
    let (base, code, vfs) = sandbox("migo_audio_vendor_uri");
    let original = "vendor-file://user/audio/clip.bin";
    let source = resolve_local_src(code.to_str(), Some(&vfs), original).unwrap();
    let error = runtime()
        .block_on(open_local_audio_source(Some(vfs), source))
        .unwrap_err();
    assert_eq!(error.message, "Failed to open local audio file");
    assert!(!error.message.contains(original));
    let _ = fs::remove_dir_all(base);
}

// ---------------------------------------------------------------------------
// The producer's copy of the command checks
// ---------------------------------------------------------------------------

/// A number as the answers file spells it: JSON has no NaN or infinities.
fn number(text: &str) -> f64 {
    match text {
        "NaN" => f64::NAN,
        "Infinity" => f64::INFINITY,
        "-Infinity" => f64::NEG_INFINITY,
        other => other.parse().expect("a number"),
    }
}

fn answer(result: Result<impl Sized, ServiceError>) -> serde_json::Value {
    match result {
        Ok(_) => serde_json::Value::Null,
        Err(error) => {
            assert_eq!(error.class, CLASS_AUDIO_ERROR);
            serde_json::Value::String(error.message)
        }
    }
}

/// What each check a command makes answers, for the values that tell its
/// branches apart.
///
/// A command has no answer, so a producer in another process makes the
/// embedded op's checks itself before sending -- and throws what the embedded
/// op throws. `test/audio-checks.test.mjs` runs the producer's checks over
/// these same cases and requires these same answers. Regenerate with
/// `MIGO_AUDIO_CHECKS_BLESS=1` after changing a check here; the diff is then
/// what the producer has to follow.
fn command_check_answers() -> serde_json::Value {
    use serde_json::json;
    const NUMBERS: [&str; 9] = [
        "0",
        "-0",
        "1.25",
        "-1",
        "-2",
        "NaN",
        "Infinity",
        "-Infinity",
        "1e308",
    ];
    let scheduled: Vec<_> = ["when", "offset", "duration"]
        .iter()
        .flat_map(|name| {
            NUMBERS.iter().map(move |value| {
                json!({
                    "name": name,
                    "value": value,
                    "error": answer(validate_scheduled_time(name, number(value))),
                })
            })
        })
        .collect();
    let duration: Vec<_> = NUMBERS
        .iter()
        .map(|value| {
            json!({ "value": value, "error": answer(validate_optional_duration(number(value))) })
        })
        .collect();
    let fields = [
        "AudioParam name",
        "oscillator type",
        "biquad filter type",
        "WaveShaper oversample",
        "panning model",
        "distance model",
        "analyser property",
        "panner property",
    ];
    let strings: Vec<_> = fields
        .iter()
        .flat_map(|field| {
            [0usize, 1, 4095, 4096, 4097, 4098, 4100, 4101, 12_289]
                .into_iter()
                .map(move |bytes| {
                    json!({
                        "field": field,
                        "utf8_bytes": bytes,
                        "error": answer(validate_audio_control_string(field, &"x".repeat(bytes))),
                    })
                })
        })
        .collect();
    let loops: Vec<_> = [
        ("0", "0"),
        ("-1", "2"),
        ("NaN", "1"),
        ("1", "Infinity"),
        ("-Infinity", "0"),
    ]
    .iter()
    .map(|(start, end)| {
        json!({
            "start": start,
            "end": end,
            "error": answer(validate_loop_points(number(start), number(end))),
        })
    })
    .collect();
    let iir_cases: [(&[&str], &[&str]); 7] = [
        (&["1"], &["1"]),
        (&[], &["1"]),
        (&["1"], &[]),
        (&["1"; 21], &["1"]),
        (&["1"], &["1"; 21]),
        (&["1"], &["0", "1"]),
        (&["1", "NaN"], &["1"]),
    ];
    let iir: Vec<_> = iir_cases
        .iter()
        .map(|(feedforward, feedback)| {
            let ff: Vec<f64> = feedforward.iter().map(|v| number(v)).collect();
            let fb: Vec<f64> = feedback.iter().map(|v| number(v)).collect();
            json!({
                "feedforward": feedforward,
                "feedback": feedback,
                "error": answer(validate_iir_coefficients(&ff, &fb)),
            })
        })
        .collect();
    let curves: Vec<_> = [
        0usize,
        3,
        4,
        6,
        MAX_WAVE_SHAPER_CURVE_BYTES,
        MAX_WAVE_SHAPER_CURVE_BYTES + 4,
    ]
    .into_iter()
    .map(|bytes| json!({ "bytes": bytes, "error": answer(validate_wave_shaper_curve_size(bytes)) }))
    .collect();
    let encoded: Vec<_> = [0usize, MAX_ENCODED_AUDIO_BYTES, MAX_ENCODED_AUDIO_BYTES + 1]
        .into_iter()
        .map(|bytes| json!({ "bytes": bytes, "error": answer(validate_encoded_audio_size(bytes)) }))
        .collect();
    json!({
        "class": CLASS_AUDIO_ERROR,
        "encoded_audio_bytes": encoded,
        "control_string_limit": MAX_AUDIO_CONTROL_STRING_BYTES,
        "scheduled_time": scheduled,
        "optional_duration": duration,
        "control_string": strings,
        "loop_points": loops,
        "iir_coefficients": iir,
        "wave_shaper_curve_bytes": curves,
    })
}

#[test]
fn the_producer_s_command_checks_answer_as_these_do() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../platforms/apple/WebContent/PerformancePlus/test/fixtures/audio-check-answers.json");
    let answers = command_check_answers();
    let text = serde_json::to_string_pretty(&answers).unwrap() + "\n";
    if std::env::var_os("MIGO_AUDIO_CHECKS_BLESS").is_some() {
        fs::write(&path, text).unwrap();
        return;
    }
    let committed = fs::read_to_string(&path).unwrap_or_default();
    assert!(
        committed == text,
        "{} is not what the checks answer; rerun with MIGO_AUDIO_CHECKS_BLESS=1 and make the \
         producer's checks follow the diff",
        path.display()
    );
}
