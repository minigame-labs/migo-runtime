use super::super::tungstenite;
use super::{Deflater, Fault, Inflater, OFFER, negotiate};
use std::io::{self, Cursor, Read, Write};
use tungstenite::{
    Message, WebSocket,
    protocol::{Role, WebSocketConfig},
};

struct Wire {
    input: Cursor<Vec<u8>>,
    output: Vec<u8>,
}
impl Read for Wire {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.input.read(bytes)
    }
}
impl Write for Wire {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.output.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn frame(opcode: u8, fin: bool, rsv1: bool, bytes: &[u8]) -> Vec<u8> {
    let mut wire = vec![opcode | if fin { 128 } else { 0 } | if rsv1 { 64 } else { 0 }];
    match bytes.len() {
        size @ 0..=125 => wire.push(size as u8),
        size @ 126..=65535 => {
            wire.push(126);
            wire.extend_from_slice(&(size as u16).to_be_bytes());
        }
        size => {
            wire.push(127);
            wire.extend_from_slice(&(size as u64).to_be_bytes());
        }
    }
    wire.extend_from_slice(bytes);
    wire
}
fn socket(bytes: Vec<u8>, enabled: bool, limit: usize) -> WebSocket<Wire> {
    WebSocket::from_raw_socket(
        Wire {
            input: Cursor::new(bytes),
            output: Vec::new(),
        },
        Role::Client,
        Some(
            WebSocketConfig::default()
                .external_deflate(enabled)
                .read_buffer_size(4096)
                .max_message_size(Some(limit))
                .max_frame_size(Some(limit)),
        ),
    )
}
fn raw(message: Message) -> tungstenite::protocol::frame::Frame {
    match message {
        Message::Frame(frame) => frame,
        other => panic!("expected compressed fragment, got {other:?}"),
    }
}

const HELLO: &[u8] = &[0xf2, 0x48, 0xcd, 0xc9, 0xc9, 0x07, 0x00];
const HELLO_SHARED: &[u8] = &[0xf2, 0x00, 0x11, 0x00, 0x00];
const HELLO_FINAL: &[u8] = &[0xf3, 0x48, 0xcd, 0xc9, 0xc9, 0x07, 0x00, 0x00];

#[test]
fn negotiation_accepts_only_the_single_offered_extension_and_valid_parameters() {
    assert!(negotiate(false, []).unwrap().is_none());
    assert!(negotiate(true, []).unwrap().is_none());
    assert!(negotiate(false, [OFFER]).is_err());
    assert!(negotiate(true, [OFFER, OFFER]).is_err());
    for invalid in [
        "",
        " ",
        "permessage-deflate, permessage-deflate",
        "permessage-deflate, other",
        "other",
        "permessage-deflate; unknown=1",
        "permessage-deflate; server_no_context_takeover=1",
        "permessage-deflate; client_no_context_takeover=1",
        "permessage-deflate; client_max_window_bits=15",
        "permessage-deflate; server_max_window_bits",
        "permessage-deflate; server_max_window_bits=7",
        "permessage-deflate; server_max_window_bits=16",
        "permessage-deflate; server_max_window_bits=999",
        "permessage-deflate; server_max_window_bits=bad",
        "permessage-deflate; server_max_window_bits=015",
        "permessage-deflate; server_max_window_bits=+15",
        "permessage-deflate; server_max_window_bits=\"15",
        "permessage-deflate; server_no_context_takeover; server_no_context_takeover",
        "permessage-deflate; server_max_window_bits=15; server_max_window_bits=14",
        "permessage-deflate;",
        "permessage-deflate;;server_no_context_takeover",
    ] {
        assert_eq!(
            negotiate(true, [invalid]),
            Err(Fault::Negotiation),
            "{invalid:?}"
        );
    }
    for bits in 8..=15 {
        let response = format!("permessage-deflate; server_max_window_bits={bits}");
        assert_eq!(
            negotiate(true, [response.as_str()])
                .unwrap()
                .unwrap()
                .server_max_window_bits,
            bits
        );
    }
    for response in [
        "permessage-deflate; server_max_window_bits=\"15\"",
        "permessage-deflate; server_max_window_bits=\"\\1\\5\"",
    ] {
        assert_eq!(
            negotiate(true, [response])
                .unwrap()
                .unwrap()
                .server_max_window_bits,
            15
        );
    }
    let response = negotiate(
        true,
        ["permessage-deflate; client_no_context_takeover; server_no_context_takeover"],
    )
    .unwrap()
    .unwrap();
    assert!(response.client_no_context_takeover && response.server_no_context_takeover);
}

#[test]
fn upstream_default_still_refuses_rsv1_and_opt_in_preserves_bytes() {
    let wire = frame(1, true, true, HELLO);
    assert!(socket(wire.clone(), false, 1024).read().is_err());
    let mut socket = socket(wire, true, 1024);
    let frame = raw(socket.read().unwrap());
    assert_eq!(frame.payload(), HELLO);
    assert!(frame.header().rsv1 && frame.header().is_final);
    assert_eq!(
        frame.header().opcode,
        tungstenite::protocol::frame::coding::OpCode::Data(
            tungstenite::protocol::frame::coding::Data::Text
        )
    );
}

#[test]
fn compressed_fragments_keep_ping_automatic_and_do_not_assemble_a_second_wire_buffer() {
    for split in 0..=HELLO.len() {
        let mut wire = frame(1, false, true, &HELLO[..split]);
        wire.extend(frame(9, true, false, b"ping"));
        wire.extend(frame(0, true, false, &HELLO[split..]));
        wire.extend(frame(1, true, false, b"plain"));
        let mut socket = socket(wire, true, 1024);
        let mut inflater = Inflater::new(false, 1024);
        let first = raw(socket.read().unwrap());
        assert!(inflater.fragment(first.payload(), false).unwrap().is_none());
        assert!(matches!(socket.read().unwrap(), Message::Ping(_)));
        let last = raw(socket.read().unwrap());
        assert!(!last.header().rsv1 && last.header().is_final);
        assert_eq!(
            inflater.fragment(last.payload(), true).unwrap().unwrap(),
            b"Hello"
        );
        assert_eq!(socket.read().unwrap(), Message::text("plain"));
        assert!(
            !socket.get_ref().output.is_empty(),
            "tungstenite answered the ping"
        );
    }
}

#[test]
fn extension_never_accepts_illegal_rsv_control_or_fragment_sequences() {
    let mut invalid = Vec::new();
    invalid.push(frame(9, true, true, b"ping"));
    invalid.push(frame(0, true, true, HELLO));
    invalid.push(frame(3, true, true, HELLO));
    let mut rsv2 = frame(1, true, true, HELLO);
    rsv2[0] |= 32;
    invalid.push(rsv2);
    let mut rsv3 = frame(1, true, true, HELLO);
    rsv3[0] |= 16;
    invalid.push(rsv3);
    invalid.push(frame(0, true, false, HELLO));
    for bytes in invalid {
        assert!(socket(bytes, true, 1024).read().is_err());
    }
    for next in [
        frame(1, true, false, b"text"),
        frame(2, true, true, HELLO),
        frame(0, true, true, HELLO),
    ] {
        let mut bytes = frame(1, false, true, &HELLO[..3]);
        bytes.extend(next);
        let mut socket = socket(bytes, true, 1024);
        raw(socket.read().unwrap());
        assert!(socket.read().is_err());
    }
    let mut bytes = frame(1, false, false, b"unfinished plain");
    bytes.extend(frame(2, true, true, HELLO));
    assert!(socket(bytes, true, 1024).read().is_err());
}

#[test]
fn fragmented_compressed_wire_still_obeys_the_aggregate_message_limit() {
    let mut bytes = frame(1, false, true, &HELLO[..3]);
    bytes.extend(frame(0, true, false, &HELLO[3..]));
    let mut socket = socket(bytes, true, HELLO.len() - 1);
    raw(socket.read().unwrap());
    assert!(matches!(
        socket.read(),
        Err(tungstenite::Error::Capacity(_))
    ));
}

#[test]
fn all_rfc_payload_examples_and_empty_messages_decode() {
    for bytes in [
        HELLO,
        HELLO_FINAL,
        &[
            0x00, 0x05, 0x00, 0xfa, 0xff, 0x48, 0x65, 0x6c, 0x6c, 0x6f, 0x00,
        ],
        &[
            0xf2, 0x48, 0x05, 0x00, 0x00, 0x00, 0xff, 0xff, 0xca, 0xc9, 0xc9, 0x07, 0x00,
        ],
    ] {
        for split in 0..=bytes.len() {
            let mut decoder = Inflater::new(false, 1024);
            assert_eq!(decoder.fragment(&bytes[..split], false).unwrap(), None);
            assert_eq!(
                decoder.fragment(&bytes[split..], true).unwrap().unwrap(),
                b"Hello",
                "split {split} of {bytes:?}"
            );
        }
    }
    assert_eq!(
        Inflater::new(false, 1024)
            .fragment(&[0], true)
            .unwrap()
            .unwrap(),
        b""
    );
}

#[test]
fn dictionary_takeover_survives_normal_and_bfinal_message_ends() {
    for initial in [HELLO, HELLO_FINAL] {
        let mut decoder = Inflater::new(false, 1024);
        for bytes in [initial, HELLO_SHARED, HELLO_SHARED] {
            assert_eq!(decoder.fragment(bytes, true).unwrap().unwrap(), b"Hello");
        }
    }
    let mut decoder = Inflater::new(true, 1024);
    decoder.fragment(HELLO, true).unwrap();
    assert_eq!(decoder.fragment(HELLO_SHARED, true), Err(Fault::Corrupt));
}

#[test]
fn truncated_and_corrupt_payloads_fail_and_exact_output_limit_is_accepted() {
    for truncated in 0..HELLO.len() {
        assert!(
            Inflater::new(false, 1024)
                .fragment(&HELLO[..truncated], true)
                .is_err(),
            "accepted prefix {truncated}"
        );
    }
    assert_eq!(
        Inflater::new(false, 1024).fragment(&[6], true),
        Err(Fault::Corrupt)
    );
    assert_eq!(
        Inflater::new(false, 5).fragment(&vec![0; 6 + super::CHUNK], true),
        Err(Fault::EncodedLimit)
    );
    let bytes = vec![42; 1024];
    let encoded = Deflater::new(true).message(&bytes, bytes.len()).unwrap();
    assert_eq!(
        Inflater::new(true, bytes.len())
            .fragment(&encoded, true)
            .unwrap()
            .unwrap(),
        bytes
    );
    let mut decoder = Inflater::new(true, bytes.len() - 1);
    assert_eq!(decoder.fragment(&encoded, true), Err(Fault::DecodedLimit));
    assert!(decoder.retained_capacity() <= bytes.len());
}

#[test]
fn expansion_ratio_is_bounded_and_each_native_step_has_fixed_input_and_output() {
    let bytes = vec![0; 8 * 1024 * 1024];
    let mut encoder = flate2::Compress::new(flate2::Compression::best(), false);
    let mut encoded = Vec::with_capacity(bytes.len() + 1024);
    encoder
        .compress_vec(&bytes, &mut encoded, flate2::FlushCompress::Sync)
        .unwrap();
    assert_eq!(encoder.total_in(), bytes.len() as u64);
    assert!(encoded.ends_with(&[0, 0, 255, 255]));
    encoded.truncate(encoded.len() - 4);
    assert!(encoded.len() * 256 + 1024 * 1024 < bytes.len());
    let mut decoder = Inflater::new(true, bytes.len());
    assert!(matches!(
        decoder.fragment(&encoded, true),
        Err(Fault::ExpansionLimit)
    ));
    assert!(decoder.steps() < 128);
}

#[test]
fn encoder_roundtrip_directional_takeover_and_invalid_decoded_utf8() {
    for no_takeover in [false, true] {
        let mut encoder = Deflater::new(no_takeover);
        let mut decoder = Inflater::new(no_takeover, 128 * 1024);
        for bytes in [
            vec![],
            b"tiny".to_vec(),
            b"game-state:\xce\xbb\xf0\x9f\x98\x80".repeat(4096),
            vec![255],
        ] {
            let encoded = encoder.message(&bytes, 128 * 1024).unwrap();
            let decoded = decoder.fragment(&encoded, true).unwrap().unwrap();
            assert_eq!(decoded, bytes);
            if bytes == [255] {
                assert!(String::from_utf8(decoded).is_err());
            }
        }
    }
}

#[test]
fn actual_client_handshake_retains_prefetched_compressed_fragments() {
    use tungstenite::client::IntoClientRequest;
    let mut request = "ws://example.invalid/game".into_client_request().unwrap();
    request.headers_mut().insert(
        "Sec-WebSocket-Key",
        "dGhlIHNhbXBsZSBub25jZQ==".parse().unwrap(),
    );
    request
        .headers_mut()
        .insert("Sec-WebSocket-Extensions", OFFER.parse().unwrap());
    let mut bytes = b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\nSec-WebSocket-Extensions: permessage-deflate\r\n\r\n".to_vec();
    bytes.extend(frame(1, true, true, HELLO));
    let (mut socket, response) = tungstenite::client::client_with_config(
        request,
        Wire {
            input: Cursor::new(bytes),
            output: Vec::new(),
        },
        Some(WebSocketConfig::default().external_deflate(true)),
    )
    .unwrap();
    let extensions = response
        .headers()
        .get_all("sec-websocket-extensions")
        .iter()
        .map(|value| value.to_str().unwrap());
    assert!(negotiate(true, extensions).unwrap().is_some());
    let frame = raw(socket.read().unwrap());
    assert_eq!(
        Inflater::new(false, 1024)
            .fragment(frame.payload(), true)
            .unwrap()
            .unwrap(),
        b"Hello"
    );
}

#[test]
fn a_large_inflate_yields_with_at_most_one_chunk_of_output_and_a_real_wakeup() {
    use std::{
        future::Future,
        pin::Pin,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        task::{Context, Poll, Wake, Waker},
    };
    struct Signal(AtomicUsize);
    impl Wake for Signal {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
    let bytes = b"game-state\0\x01\x02\x03".repeat(32 * 1024);
    let encoded = Deflater::new(true).message(&bytes, bytes.len()).unwrap();
    let mut decoder = Inflater::new(true, bytes.len());
    let signal = Arc::new(Signal(AtomicUsize::new(0)));
    let waker = Waker::from(signal.clone());
    let mut context = Context::from_waker(&waker);
    let mut job = decoder.fragment_job(&encoded, true).unwrap();
    assert!(matches!(
        Pin::new(&mut job).poll(&mut context),
        Poll::Pending
    ));
    assert_eq!(signal.0.load(Ordering::Relaxed), 1);
    let mut polls = 1;
    loop {
        polls += 1;
        match Pin::new(&mut job).poll(&mut context) {
            Poll::Ready(result) => {
                assert_eq!(result.unwrap().unwrap(), bytes);
                break;
            }
            Poll::Pending => assert!(polls < 32),
        }
    }
    assert!(polls >= bytes.len().div_ceil(super::CHUNK));
}

#[tokio::test]
async fn actual_shared_executor_retains_partial_inflate_across_a_cancelled_wait() {
    use migo_io::{
        polling::Offloaded,
        pools::IoPools,
        task::{PoolKind, PriorityClass},
    };
    use std::{future::Future, pin::Pin, time::Duration};
    let bytes = b"game-state\0\x01\x02\x03".repeat(32 * 1024);
    let encoded = Deflater::new(true).message(&bytes, bytes.len()).unwrap();
    let limit = bytes.len();
    let (started, running) = tokio::sync::oneshot::channel();
    let (release, gate) = std::sync::mpsc::channel();
    let caller = std::thread::current().id();
    let pools = IoPools::new(7903);
    let task = async move {
        let mut decoder = Inflater::new(true, limit);
        let mut fragment = decoder.fragment_job(&encoded, true).unwrap();
        let mut started = Some(started);
        std::future::poll_fn(move |cx| {
            assert_ne!(std::thread::current().id(), caller);
            let result = Pin::new(&mut fragment).poll(cx);
            if result.is_pending()
                && let Some(started) = started.take()
            {
                let _ = started.send(());
                let _ = gate.recv();
            }
            result
        })
        .await
    };
    let mut task = Offloaded::new(
        task,
        pools.clone(),
        PoolKind::Archive,
        (limit + 1024 * 1024) as u64,
    )
    .unwrap();
    tokio::select! {
        result = task.wait() => panic!("completed before native gate: {}", result.is_ok()),
        result = running => result.unwrap(),
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(20), task.wait())
            .await
            .is_err()
    );
    let fs = pools
        .submit_async(PoolKind::Fs, PriorityClass::ForegroundAsync, || 42)
        .unwrap();
    let fs_result = tokio::time::timeout(Duration::from_secs(1), fs).await;
    let _ = release.send(());
    assert_eq!(fs_result.unwrap().unwrap().unwrap(), 42);
    match tokio::time::timeout(Duration::from_secs(2), task.wait())
        .await
        .unwrap()
        .unwrap()
    {
        Ok(Some(decoded)) => assert_eq!(decoded, bytes),
        other => panic!(
            "partial inflate was lost: {:?}",
            other.map(|bytes| bytes.map(|bytes| bytes.len()))
        ),
    }
}

#[test]
fn an_exact_limit_incompressible_message_is_not_rejected_for_deflate_overhead() {
    let mut seed = 0x71c249a5u32;
    let bytes: Vec<_> = (0..256 * 1024)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as u8
        })
        .collect();
    let encoded = Deflater::new(true).message(&bytes, bytes.len()).unwrap();
    assert!(encoded.len() > bytes.len());
    let result = Inflater::new(true, bytes.len()).fragment(&encoded, true);
    assert!(
        result.is_ok(),
        "raw DEFLATE overhead must fit the independent encoded budget: {:?}",
        result.as_ref().err()
    );
    assert_eq!(result.unwrap().unwrap(), bytes);
}

#[test]
fn independent_miniz_streams_and_dictionary_rollover_agree() {
    for len in [0, 1, 127, 1024, 32768, 65535, 128 * 1024] {
        let bytes: Vec<_> = (0..len).map(|i| ((i * 37 + i / 101) % 256) as u8).collect();
        let mut encoded = miniz_oxide::deflate::compress_to_vec(&bytes, 6);
        encoded.push(0); // RFC7692 BFINAL padding plus empty stored-block header.
        assert_eq!(
            Inflater::new(true, bytes.len().max(1))
                .fragment(&encoded, true)
                .unwrap()
                .unwrap(),
            bytes
        );
        let mut encoded = Deflater::new(true).message(&bytes, bytes.len()).unwrap();
        encoded.extend_from_slice(&[0, 0, 255, 255, 1, 0, 0, 255, 255]);
        assert_eq!(
            miniz_oxide::inflate::decompress_to_vec(&encoded).unwrap(),
            bytes
        );
    }
    let mut encoder = Deflater::new(false);
    let mut decoder = Inflater::new(false, 65536);
    let mut seed = 17u32;
    let mut previous = Vec::new();
    for turn in 0..128 {
        let len = 256 + turn * 37;
        let mut bytes: Vec<_> = (0..len)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed as u8
            })
            .collect();
        bytes.extend_from_slice(&previous);
        let encoded = encoder.message(&bytes, 65536).unwrap();
        let split = encoded.len() / 2;
        assert!(
            decoder
                .fragment(&encoded[..split], false)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            decoder.fragment(&encoded[split..], true).unwrap().unwrap(),
            bytes
        );
        previous = bytes[bytes.len().saturating_sub(2048)..].to_vec();
    }
}

#[test]
fn a_large_deflate_send_yields_and_roundtrips_after_resume() {
    use std::{
        future::Future,
        pin::Pin,
        task::{Context, Poll, Waker},
    };
    let bytes = b"game-state\0\x01\x02\x03".repeat(32 * 1024);
    let mut encoder = Deflater::new(true);
    let mut job = encoder.message_job(&bytes, bytes.len()).unwrap();
    let mut context = Context::from_waker(Waker::noop());
    assert!(matches!(
        Pin::new(&mut job).poll(&mut context),
        Poll::Pending
    ));
    let mut polls = 1;
    let encoded = loop {
        polls += 1;
        match Pin::new(&mut job).poll(&mut context) {
            Poll::Ready(result) => break result.unwrap(),
            Poll::Pending => assert!(polls < 32),
        }
    };
    assert!(polls >= bytes.len().div_ceil(super::CHUNK));
    assert_eq!(
        Inflater::new(true, bytes.len())
            .fragment(&encoded, true)
            .unwrap()
            .unwrap(),
        bytes
    );
}
