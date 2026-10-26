use super::*;
use futures::{SinkExt, StreamExt};

#[cfg(all(target_os = "linux", not(target_env = "ohos")))]
#[path = "websocket_benchmarks.rs"]
mod benchmarks;

fn compression_parameters() -> super::super::websocket_deflate::Negotiated {
    super::super::websocket_deflate::Negotiated {
        server_no_context_takeover: false,
        client_no_context_takeover: false,
        server_max_window_bits: 15,
    }
}

#[tokio::test]
async fn cancellation_before_connect_never_takes_a_slot_or_resolves_a_host() {
    let resources = ResourceTable::new();
    let cancel = Arc::new(CancelFlag::default());
    let rid = resources.add_cancel(cancel.clone());
    cancel.cancel();
    // All slots occupied proves cancellation happens before admission. The
    // reserved hostname must never need DNS or consume the original deadline.
    let held: Vec<_> = (0..super::super::resources::MAX_WEB_SOCKETS)
        .map(|_| resources.web_socket_slot().unwrap())
        .collect();
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        create(
            &NetworkPolicy::unrestricted(),
            &resources,
            "wss://not-resolved.invalid/game",
            &[],
            &[],
            ConnectOptions {
                cancel_rid: Some(rid),
                ..Default::default()
            },
            migo_io::pools::IoPools::new(7918),
        ),
    )
    .await
    .expect("a previously cancelled connection cannot wait");
    assert_eq!(result.unwrap_err().message, "WebSocket connect cancelled");
    assert_eq!(
        resources.len(),
        1,
        "only the caller's cancel handle remains"
    );
    resources.close(rid);
    assert!(resources.is_empty());
    drop(held);
}

#[tokio::test]
async fn a_websocket_fragment_is_refused_before_connection_admission() {
    let resources = ResourceTable::new();
    let held: Vec<_> = (0..super::super::resources::MAX_WEB_SOCKETS)
        .map(|_| resources.web_socket_slot().unwrap())
        .collect();
    for url in [
        "wss://not-resolved.invalid/game#",
        "ws://not-resolved.invalid/game#part",
    ] {
        let mut policy = NetworkPolicy::unrestricted();
        policy.allow_plain_http = true;
        let error = create(
            &policy,
            &resources,
            url,
            &[],
            &[],
            ConnectOptions::default(),
            migo_io::pools::IoPools::new(7923),
        )
        .await
        .unwrap_err();
        assert_eq!(error.class, "TypeError");
        assert!(error.message.contains("fragment"));
    }
    let error = create(
        &NetworkPolicy::unrestricted(),
        &resources,
        "wss://not-resolved.invalid/game%23part",
        &[],
        &[],
        ConnectOptions::default(),
        migo_io::pools::IoPools::new(7923),
    )
    .await
    .unwrap_err();
    assert!(
        error.message.contains("at most 5"),
        "an encoded hash is part of the path, not a fragment"
    );
    drop(held);
}

#[tokio::test]
async fn wss_configuration_reuses_tickets_within_one_session_and_isolates_other_sessions() {
    use shared::network_policy::TlsTrust;
    let first = ResourceTable::new();
    let second = ResourceTable::new();
    let pools = migo_io::pools::IoPools::new(7920);
    let config = first
        .web_socket_tls(TlsTrust::Mozilla, pools.clone())
        .config()
        .await
        .unwrap();
    let reused = first
        .web_socket_tls(TlsTrust::Mozilla, pools.clone())
        .config()
        .await
        .unwrap();
    let isolated = second
        .web_socket_tls(TlsTrust::Mozilla, pools)
        .config()
        .await
        .unwrap();
    assert!(Arc::ptr_eq(&config, &reused));
    assert!(!Arc::ptr_eq(&config, &isolated));
}

#[cfg(all(target_os = "linux", not(target_env = "ohos")))]
#[test]
fn actual_wss_handshakes_use_platform_roots_without_a_mozilla_fallback() {
    let directory = std::env::temp_dir().join(format!(
        "migo-wss-trust-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&directory).unwrap();
    let certified = rcgen::generate_simple_self_signed(vec!["host-ca.example".into()]).unwrap();
    std::fs::write(directory.join("root.pem"), certified.cert.pem()).unwrap();
    std::fs::write(directory.join("leaf.der"), certified.cert.der()).unwrap();
    std::fs::write(
        directory.join("key.der"),
        certified.key_pair.serialize_der(),
    )
    .unwrap();
    // Native root selection is process-wide; isolate this fixture from cached
    // verifiers and the rest of the suite's environment.
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "network::websocket::tests::platform_wss_probe",
            "--nocapture",
        ])
        .env("SSL_CERT_FILE", directory.join("root.pem"))
        .env("SSL_CERT_DIR", &directory)
        .env("MIGO_WSS_TRUST_FIXTURE", &directory)
        .output()
        .unwrap();
    std::fs::remove_dir_all(directory).unwrap();
    assert!(
        output.status.success(),
        "WSS trust probe failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(all(target_os = "linux", not(target_env = "ohos")))]
#[tokio::test]
#[ignore = "run by actual_wss_handshakes_use_platform_roots_without_a_mozilla_fallback in a fresh process"]
async fn platform_wss_probe() {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    use shared::network_policy::TlsTrust;
    let directory = std::path::PathBuf::from(
        std::env::var_os("MIGO_WSS_TRUST_FIXTURE").expect("parent fixture"),
    );
    let certificate = CertificateDer::from(std::fs::read(directory.join("leaf.der")).unwrap());
    let key = std::fs::read(directory.join("key.der")).unwrap();
    for (trust, host, accepted) in [
        (TlsTrust::Platform, "host-ca.example", true),
        (TlsTrust::Mozilla, "host-ca.example", false),
        (TlsTrust::Platform, "wrong-host.example", false),
    ] {
        let pools = migo_io::pools::IoPools::new(7919);
        let server = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![certificate.clone()],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.clone())),
        )
        .unwrap();
        let server = Arc::new(server);
        let resources = ResourceTable::new();
        for attempt in 0..if accepted { 2 } else { 1 } {
            // Exactly the production session factory: no injected extra roots,
            // custom verifier or verification-failure fallback. Reconnect must
            // also reuse its TLS ticket store instead of paying for a full handshake.
            let config = resources
                .web_socket_tls(trust, pools.clone())
                .config()
                .await
                .unwrap();
            let (wire, peer) = tokio::io::duplex(16 * 1024);
            let connection = finish_handshake(
                tokio_tungstenite::client_async_tls_with_config(
                    format!("wss://{host}/game"),
                    wire,
                    Some(ws_config()),
                    Some(tokio_tungstenite::Connector::Rustls(config)),
                ),
                true,
                resources.web_socket_slot().unwrap(),
                pools.clone(),
                tokio::time::Instant::now() + std::time::Duration::from_secs(2),
            );
            let server = async {
                let peer = tokio_rustls::TlsAcceptor::from(server.clone())
                    .accept(peer)
                    .await
                    .map_err(|error| error.to_string())?;
                let _peer = tokio_tungstenite::accept_async(peer)
                    .await
                    .map_err(|error| error.to_string())?;
                Ok::<_, String>(())
            };
            let (connection, server) =
                tokio::time::timeout(std::time::Duration::from_secs(3), async {
                    tokio::join!(connection, server)
                })
                .await
                .expect("bounded WSS fixture");
            assert_eq!(connection.is_ok(), accepted, "{trust:?}/{host}");
            assert_eq!(server.is_ok(), accepted, "{trust:?}/{host}");
            if let Ok(((stream, response), slot)) = connection {
                assert_eq!(response.status(), 101);
                let MaybeTlsStream::Rustls(tls) = &stream.get_ref() else {
                    panic!("the WSS fixture must have negotiated TLS");
                };
                assert_eq!(
                    tls.get_ref().1.handshake_kind(),
                    Some(if attempt == 0 {
                        rustls::HandshakeKind::Full
                    } else {
                        rustls::HandshakeKind::Resumed
                    }),
                    "a reconnect within this session must resume its TLS connection"
                );
                drop((stream, slot));
            }
            assert!(resources.is_empty());
        }
    }
}

fn compression_config() -> WebSocketConfig {
    ws_config()
        .external_deflate(true)
        .external_deflate_max_wire_size(Some(super::super::websocket_deflate::encoded_limit(
            WS_MAX_MESSAGE_BYTES,
        )))
}

struct AcceptDeflate;
impl tokio_tungstenite::tungstenite::handshake::server::Callback for AcceptDeflate {
    fn on_request(
        self,
        request: &tokio_tungstenite::tungstenite::handshake::server::Request,
        mut response: tokio_tungstenite::tungstenite::handshake::server::Response,
    ) -> Result<
        tokio_tungstenite::tungstenite::handshake::server::Response,
        tokio_tungstenite::tungstenite::handshake::server::ErrorResponse,
    > {
        assert_eq!(
            request.headers()["Sec-WebSocket-Extensions"],
            super::super::websocket_deflate::OFFER
        );
        response.headers_mut().insert(
            "Sec-WebSocket-Extensions",
            HeaderValue::from_static("permessage-deflate"),
        );
        Ok(response)
    }
}

async fn peer_message<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    peer: &mut tokio_tungstenite::WebSocketStream<S>,
    decoder: &mut super::super::websocket_deflate::Inflater,
) -> Message {
    use tokio_tungstenite::tungstenite::protocol::frame::coding::{Data, OpCode};
    let mut kind = None;
    loop {
        match peer.next().await.unwrap().unwrap() {
            Message::Frame(frame) => {
                if frame.header().rsv1 {
                    kind = Some(frame.header().opcode);
                }
                if let Some(bytes) = decoder
                    .fragment(frame.payload(), frame.header().is_final)
                    .unwrap()
                {
                    return match kind.unwrap() {
                        OpCode::Data(Data::Text) => {
                            Message::text(String::from_utf8(bytes).unwrap())
                        }
                        OpCode::Data(Data::Binary) => Message::Binary(bytes.into()),
                        _ => panic!("invalid original message type"),
                    };
                }
            }
            Message::Ping(_) | Message::Pong(_) => {}
            message => return message,
        }
    }
}

async fn exercise_compressed_transport<C, S>(
    client: tokio_tungstenite::WebSocketStream<C>,
    mut peer: tokio_tungstenite::WebSocketStream<S>,
    slot: tokio::sync::OwnedSemaphorePermit,
) where
    C: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    use super::super::websocket_deflate::{Deflater, Inflater};
    use tokio_tungstenite::tungstenite::protocol::frame::{
        Frame,
        coding::{Data, OpCode},
    };
    let parameters = Some(compression_parameters());
    let pools = migo_io::pools::IoPools::new(7910);
    let (tx, rx) = client.split();
    let slot = Arc::new(slot);
    let mut tx = Sender::new(tx, parameters, pools.clone(), slot.clone());
    let mut rx = Receiver::new(rx, parameters, pools, slot);
    let mut decoder = Inflater::new(false, WS_MAX_MESSAGE_BYTES);
    let text = "mini-game state: λ 🎮 ".repeat(8192);
    // Repeat the same input to exercise the negotiated outgoing dictionary;
    // a short uncompressed message between them must not update it.
    for payload in [
        Message::text(text.clone()),
        Message::text("plain"),
        Message::text(text.clone()),
    ] {
        let (sent, seen) = tokio::join!(
            tx.send(payload.clone(), tokio::time::Instant::now() + WRITE_TIMEOUT),
            peer_message(&mut peer, &mut decoder),
        );
        sent.unwrap();
        assert_eq!(seen, payload);
    }
    let encoded = Deflater::new(false)
        .message(text.as_bytes(), WS_MAX_MESSAGE_BYTES)
        .unwrap();
    let split = encoded.len() / 2;
    let (delivered, ()) = tokio::join!(
        async {
            let mut controls = 0;
            loop {
                match rx.next().await.unwrap() {
                    ReadStep::Control => controls += 1,
                    ReadStep::Fragment => {}
                    ReadStep::Event(event) => {
                        assert_eq!(controls, 1);
                        break event;
                    }
                }
            }
        },
        async {
            let mut first =
                Frame::message(encoded[..split].to_vec(), OpCode::Data(Data::Text), false);
            first.header_mut().rsv1 = true;
            peer.send(Message::Frame(first)).await.unwrap();
            peer.send(Message::Ping(b"ping".to_vec().into()))
                .await
                .unwrap();
            peer.send(Message::Frame(Frame::message(
                encoded[split..].to_vec(),
                OpCode::Data(Data::Continue),
                true,
            )))
            .await
            .unwrap();
        },
    );
    assert_eq!(delivered, WsEvent::Text(text));
    let (sent, ()) = tokio::join!(
        tx.send(
            Message::Close(None),
            tokio::time::Instant::now() + WRITE_TIMEOUT
        ),
        async {
            loop {
                if matches!(peer.next().await.unwrap().unwrap(), Message::Close(_)) {
                    break;
                }
            }
            let _ = peer.flush().await;
        },
    );
    sent.unwrap();
    assert!(matches!(
        rx.next().await.unwrap(),
        ReadStep::Event(WsEvent::Close { .. })
    ));
}

#[tokio::test]
async fn compressed_transport_roundtrips_fragments_control_takeover_and_close() {
    use tokio_tungstenite::tungstenite::protocol::Role;
    let (client, peer) = tokio::io::duplex(16 * 1024);
    let client = tokio_tungstenite::WebSocketStream::from_raw_socket(
        client,
        Role::Client,
        Some(compression_config()),
    )
    .await;
    let peer = tokio_tungstenite::WebSocketStream::from_raw_socket(
        peer,
        Role::Server,
        Some(compression_config()),
    )
    .await;
    let resources = ResourceTable::new();
    exercise_compressed_transport(client, peer, resources.web_socket_slot().unwrap()).await;
}

#[tokio::test]
async fn real_tls_websocket_compression_runs_through_the_handshake_executor() {
    use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let certificate = certified.cert.der().clone();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let server = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![certificate.clone()],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(certified.key_pair.serialize_der())),
        )
        .unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(certificate).unwrap();
    let client = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let (wire, peer) = tokio::io::duplex(16 * 1024);
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut request = "wss://localhost/game".into_client_request().unwrap();
    request.headers_mut().insert(
        "Sec-WebSocket-Extensions",
        HeaderValue::from_static(super::super::websocket_deflate::OFFER),
    );
    let resources = ResourceTable::new();
    let (client, peer) = tokio::join!(
        finish_handshake(
            tokio_tungstenite::client_async_tls_with_config(
                request,
                wire,
                Some(compression_config()),
                Some(tokio_tungstenite::Connector::Rustls(Arc::new(client)))
            ),
            true,
            resources.web_socket_slot().unwrap(),
            migo_io::pools::IoPools::new(7911),
            tokio::time::Instant::now() + WRITE_TIMEOUT
        ),
        async {
            let peer = tokio_rustls::TlsAcceptor::from(Arc::new(server))
                .accept(peer)
                .await
                .unwrap();
            tokio_tungstenite::accept_hdr_async_with_config(
                peer,
                AcceptDeflate,
                Some(compression_config()),
            )
            .await
            .unwrap()
        },
    );
    let ((client, response), slot) = client.unwrap();
    assert_eq!(
        response.headers()["Sec-WebSocket-Extensions"],
        "permessage-deflate"
    );
    exercise_compressed_transport(client, peer, slot).await;
}

#[tokio::test]
async fn cancelled_compressed_send_keeps_its_message_until_the_next_writer_resumes() {
    cancelled_send_retains(Some(compression_parameters())).await;
}

#[tokio::test]
async fn cancelled_fragmented_plain_send_keeps_its_message_until_the_next_writer_resumes() {
    cancelled_send_retains(None).await;
}

async fn cancelled_send_retains(parameters: Option<super::super::websocket_deflate::Negotiated>) {
    use super::super::websocket_deflate::Inflater;
    use tokio_tungstenite::tungstenite::protocol::Role;
    let (client, peer) = tokio::io::duplex(64);
    let client = tokio_tungstenite::WebSocketStream::from_raw_socket(
        client,
        Role::Client,
        Some(compression_config()),
    )
    .await;
    let mut peer = tokio_tungstenite::WebSocketStream::from_raw_socket(
        peer,
        Role::Server,
        Some(compression_config()),
    )
    .await;
    let resources = ResourceTable::new();
    let (tx, _rx) = client.split();
    let mut tx = Sender::new(
        tx,
        parameters,
        migo_io::pools::IoPools::new(7912),
        Arc::new(resources.web_socket_slot().unwrap()),
    );
    let mut seed = 0x711709u32;
    let bytes: Vec<_> = (0..256 * 1024)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as u8
        })
        .collect();
    let first = Message::Binary(bytes.clone().into());
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(20),
            tx.send(first.clone(), tokio::time::Instant::now() + WRITE_TIMEOUT)
        )
        .await
        .is_err()
    );
    let mut decoder = Inflater::new(false, WS_MAX_MESSAGE_BYTES);
    let ((), seen) = tokio::join!(
        async {
            tx.send(
                Message::text("next"),
                tokio::time::Instant::now() + WRITE_TIMEOUT,
            )
            .await
            .expect("next writer must resume the retained message");
        },
        async {
            let first = peer_message(&mut peer, &mut decoder).await;
            let second = peer_message(&mut peer, &mut decoder).await;
            (first, second)
        },
    );
    assert_eq!(seen, (first, Message::text("next")));
}

#[tokio::test]
async fn cancelled_compressed_receive_keeps_the_partial_message_and_socket() {
    use super::super::websocket_deflate::Deflater;
    use tokio_tungstenite::tungstenite::protocol::{
        Role,
        frame::{
            Frame,
            coding::{Data, OpCode},
        },
    };
    let (client, peer) = tokio::io::duplex(16 * 1024);
    let client = tokio_tungstenite::WebSocketStream::from_raw_socket(
        client,
        Role::Client,
        Some(compression_config()),
    )
    .await;
    let mut peer = tokio_tungstenite::WebSocketStream::from_raw_socket(
        peer,
        Role::Server,
        Some(compression_config()),
    )
    .await;
    let resources = ResourceTable::new();
    let (_tx, rx) = client.split();
    let mut rx = Receiver::new(
        rx,
        Some(compression_parameters()),
        migo_io::pools::IoPools::new(7913),
        Arc::new(resources.web_socket_slot().unwrap()),
    );
    let text = "game-state λ 🎮 ".repeat(8192);
    let encoded = Deflater::new(false)
        .message(text.as_bytes(), WS_MAX_MESSAGE_BYTES)
        .unwrap();
    let split = encoded.len() / 2;
    let mut first = Frame::message(encoded[..split].to_vec(), OpCode::Data(Data::Text), false);
    first.header_mut().rsv1 = true;
    peer.send(Message::Frame(first)).await.unwrap();
    assert!(matches!(rx.next().await.unwrap(), ReadStep::Fragment));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), rx.next())
            .await
            .is_err()
    );
    peer.send(Message::Frame(Frame::message(
        encoded[split..].to_vec(),
        OpCode::Data(Data::Continue),
        true,
    )))
    .await
    .unwrap();
    assert!(
        matches!(rx.next().await.unwrap(), ReadStep::Event(WsEvent::Text(actual)) if actual == text)
    );
}

#[tokio::test]
async fn large_plain_messages_respect_the_same_frame_limit_as_the_receiver() {
    use super::super::websocket_deflate::Inflater;
    use tokio_tungstenite::tungstenite::protocol::Role;
    let (client, peer) = tokio::io::duplex(16 * 1024);
    let client = tokio_tungstenite::WebSocketStream::from_raw_socket(
        client,
        Role::Client,
        Some(ws_config()),
    )
    .await;
    let mut peer =
        tokio_tungstenite::WebSocketStream::from_raw_socket(peer, Role::Server, Some(ws_config()))
            .await;
    let resources = ResourceTable::new();
    let (tx, _rx) = client.split();
    let mut tx = Sender::new(
        tx,
        None,
        migo_io::pools::IoPools::new(7914),
        Arc::new(resources.web_socket_slot().unwrap()),
    );
    let expected = Message::Binary(vec![42; 4 * 1024 * 1024].into());
    let mut decoder = Inflater::new(false, WS_MAX_MESSAGE_BYTES);
    let (sent, delivered) = tokio::join!(
        tx.send(
            expected.clone(),
            tokio::time::Instant::now() + WRITE_TIMEOUT
        ),
        peer_message(&mut peer, &mut decoder),
    );
    sent.unwrap();
    assert_eq!(delivered, expected);
}

#[tokio::test]
async fn dropping_an_active_native_write_keeps_the_connection_slot_until_it_returns() {
    use std::{
        pin::Pin,
        task::{Context, Poll},
    };
    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
    use tokio_tungstenite::tungstenite::protocol::Role;
    struct HeldWrite {
        wire: tokio::io::DuplexStream,
        started: Option<tokio::sync::oneshot::Sender<()>>,
        gate: std::sync::mpsc::Receiver<()>,
    }
    impl AsyncRead for HeldWrite {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.wire).poll_read(cx, buf)
        }
    }
    impl AsyncWrite for HeldWrite {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            if let Some(started) = self.started.take() {
                let _ = started.send(());
                let _ = self.gate.recv();
            }
            Pin::new(&mut self.wire).poll_write(cx, bytes)
        }
        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.wire).poll_flush(cx)
        }
        fn poll_shutdown(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.wire).poll_shutdown(cx)
        }
    }
    let resources = ResourceTable::new();
    let mut held: Vec<_> = (0..super::super::resources::MAX_WEB_SOCKETS)
        .map(|_| resources.web_socket_slot().unwrap())
        .collect();
    let slot = Arc::new(held.pop().unwrap());
    let (wire, peer) = tokio::io::duplex(16 * 1024);
    let (started, running) = tokio::sync::oneshot::channel();
    let (release, gate) = std::sync::mpsc::channel();
    let client = tokio_tungstenite::WebSocketStream::from_raw_socket(
        HeldWrite {
            wire,
            started: Some(started),
            gate,
        },
        Role::Client,
        Some(compression_config()),
    )
    .await;
    let (tx, rx) = client.split();
    let mut tx = Sender::new(
        tx,
        Some(compression_parameters()),
        migo_io::pools::IoPools::new(7915),
        slot.clone(),
    );
    drop(slot);
    let mut sending = Box::pin(tx.send(
        Message::Binary(vec![42; 4096].into()),
        tokio::time::Instant::now() + WRITE_TIMEOUT,
    ));
    tokio::select! {
        result = &mut sending => panic!("write returned before its native gate: {result:?}"),
        result = running => result.unwrap(),
    }
    drop(sending);
    drop(tx);
    drop(rx);
    let premature = resources.web_socket_slot();
    let _ = release.send(());
    assert!(
        premature.is_none(),
        "an in-flight poll still owns the transport"
    );
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if let Some(slot) = resources.web_socket_slot() {
                drop(slot);
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("abandoned native completion releases the slot");
    drop(peer);
}

#[tokio::test]
async fn invalid_compressed_text_and_unnegotiated_data_are_terminal() {
    use super::super::websocket_deflate::Deflater;
    use tokio_tungstenite::tungstenite::protocol::{
        Role,
        frame::{
            Frame,
            coding::{Data, OpCode},
        },
    };
    for parameters in [None, Some(compression_parameters())] {
        let (client, peer) = tokio::io::duplex(16 * 1024);
        let client = tokio_tungstenite::WebSocketStream::from_raw_socket(
            client,
            Role::Client,
            Some(compression_config()),
        )
        .await;
        let mut peer = tokio_tungstenite::WebSocketStream::from_raw_socket(
            peer,
            Role::Server,
            Some(compression_config()),
        )
        .await;
        let (_tx, rx) = client.split();
        let resources = ResourceTable::new();
        let mut rx = Receiver::new(
            rx,
            parameters,
            migo_io::pools::IoPools::new(7916),
            Arc::new(resources.web_socket_slot().unwrap()),
        );
        let mut frame = Frame::message(
            Deflater::new(true).message(&[255], 1024).unwrap(),
            OpCode::Data(Data::Text),
            true,
        );
        frame.header_mut().rsv1 = true;
        peer.send(Message::Frame(frame)).await.unwrap();
        let refused = match rx.next().await {
            Err(error) => error,
            Ok(_) => panic!("invalid text reached content"),
        };
        if parameters.is_some() {
            assert!(refused.message.contains("UTF-8"));
        } else {
            assert!(refused.message.contains("unnegotiated"));
        }
        let next = tokio::time::timeout(std::time::Duration::from_millis(100), rx.next())
            .await
            .expect("a failed decoder must be terminal instead of waiting for more data");
        assert!(next.is_err(), "a failed decoder cannot resume");
    }
}

#[tokio::test]
async fn a_local_oversize_send_does_not_poison_a_healthy_transport() {
    use super::super::websocket_deflate::Inflater;
    use tokio_tungstenite::tungstenite::protocol::Role;
    let (client, peer) = tokio::io::duplex(16 * 1024);
    let client = tokio_tungstenite::WebSocketStream::from_raw_socket(
        client,
        Role::Client,
        Some(ws_config()),
    )
    .await;
    let mut peer =
        tokio_tungstenite::WebSocketStream::from_raw_socket(peer, Role::Server, Some(ws_config()))
            .await;
    let (tx, _rx) = client.split();
    let resources = ResourceTable::new();
    let mut tx = Sender::new(
        tx,
        None,
        migo_io::pools::IoPools::new(7917),
        Arc::new(resources.web_socket_slot().unwrap()),
    );
    assert!(
        tx.send(
            Message::Binary(vec![42; WS_MAX_MESSAGE_BYTES + 1].into()),
            tokio::time::Instant::now() + WRITE_TIMEOUT
        )
        .await
        .is_err()
    );
    let mut decoder = Inflater::new(false, WS_MAX_MESSAGE_BYTES);
    let (sent, delivered) = tokio::join!(
        tx.send(
            Message::text("still open"),
            tokio::time::Instant::now() + WRITE_TIMEOUT
        ),
        peer_message(&mut peer, &mut decoder)
    );
    sent.unwrap();
    assert_eq!(delivered, Message::text("still open"));
}

#[tokio::test]
async fn cancelled_native_handshake_keeps_its_connection_slot_until_poll_returns() {
    use std::time::Duration;
    let resources = ResourceTable::new();
    let mut held: Vec<_> = (0..super::super::resources::MAX_WEB_SOCKETS)
        .map(|_| resources.web_socket_slot().unwrap())
        .collect();
    let slot = held.pop().unwrap();
    let (started, running) = tokio::sync::oneshot::channel();
    let (release, gate) = std::sync::mpsc::channel();
    let handshake = async move {
        let _ = started.send(());
        let _ = gate.recv();
        Ok::<_, String>(())
    };
    let mut connect = Box::pin(finish_handshake(
        handshake,
        true,
        slot,
        migo_io::pools::IoPools::new(7701),
        tokio::time::Instant::now() + Duration::from_secs(5),
    ));
    tokio::select! {
        result = &mut connect => panic!("native fixture completed before release: {result:?}"),
        result = running => result.expect("native poll started"),
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut connect)
            .await
            .is_err()
    );
    drop(connect);
    let prematurely_released = resources.web_socket_slot();
    // Always unblock the executor, including the failing pre-fix control.
    let _ = release.send(());
    assert!(
        prematurely_released.is_none(),
        "a cancelled caller released the still-running connection's slot"
    );
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if let Some(slot) = resources.web_socket_slot() {
                drop(slot);
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("native completion releases the connection slot");
}

/// The headers content may not set on a handshake: the ones that would forge a
/// hop, and the ones the upgrade itself is made of.
#[test]
fn the_handshake_s_own_headers_are_not_content_s_to_set() {
    for name in [
        "connection",
        "upgrade",
        "sec-websocket-key",
        "sec-websocket-version",
        "sec-websocket-accept",
        "sec-websocket-protocol",
        "sec-websocket-extensions",
    ] {
        assert!(
            is_reserved_header(&HeaderName::from_bytes(name.as_bytes()).unwrap()),
            "{name} belongs to the handshake"
        );
    }
    assert!(!is_reserved_header(
        &HeaderName::from_bytes(b"x-game").unwrap()
    ));
    // And the forwarding headers a request must not carry, which are the same
    // set `fetch` refuses.
    assert!(super::super::fetch::is_blocked_header(
        &HeaderName::from_bytes(b"x-forwarded-for").unwrap()
    ));
}

/// A bare IPv6 literal is bracketed, or its own colons make the target
/// ambiguous and it never resolves.
#[test]
fn an_ipv6_literal_is_bracketed_for_the_resolver() {
    assert_eq!(join_host_port("::1", 80), "[::1]:80");
    assert_eq!(join_host_port("[::1]", 80), "[::1]:80");
    assert_eq!(join_host_port("example.com", 443), "example.com:443");
}

/// The limits are the engine's, not tungstenite's desktop defaults.
#[test]
fn a_connection_is_bounded_well_below_the_library_s_defaults() {
    let config = ws_config();
    assert_eq!(config.max_message_size, Some(8 * 1024 * 1024));
    assert_eq!(config.max_frame_size, Some(2 * 1024 * 1024));
    assert_eq!(config.max_write_buffer_size, 8 * 1024 * 1024);
}

/// The policy is enforced before anything network-visible happens, and a
/// refusal leaves no resource behind.
#[tokio::test]
async fn a_host_the_policy_refuses_is_never_connected_to() {
    let policy = NetworkPolicy::from_json(r#"{"socket":["allowed.example"]}"#).unwrap();
    let resources = ResourceTable::new();
    let error = create(
        &policy,
        &resources,
        "wss://blocked.example/socket",
        &[],
        &[],
        ConnectOptions::default(),
        migo_io::pools::IoPools::new(0),
    )
    .await
    .expect_err("the allow list refuses it");
    assert!(
        error.message.contains("blocked.example"),
        "{}",
        error.message
    );
    assert!(resources.is_empty());

    // And `ws://` where plain text is not allowed.
    let error = create(
        &policy,
        &resources,
        "ws://allowed.example/socket",
        &[],
        &[],
        ConnectOptions::default(),
        migo_io::pools::IoPools::new(0),
    )
    .await
    .expect_err("the policy refuses plain text");
    assert!(
        error.message.contains("plain-text ws:// is not allowed"),
        "{}",
        error.message
    );
    assert!(resources.is_empty());
}

/// A URL that is not one is a TypeError, before the gate: there is no host to
/// hold to a policy.
#[tokio::test]
async fn a_url_that_is_not_one_is_a_type_error() {
    let resources = ResourceTable::new();
    let error = create(
        &NetworkPolicy::default(),
        &resources,
        "not a url",
        &[],
        &[],
        ConnectOptions::default(),
        migo_io::pools::IoPools::new(0),
    )
    .await
    .expect_err("a URL is a URL");
    assert_eq!(error.class, "TypeError");
    assert!(
        error.message.starts_with("Invalid WebSocket URL"),
        "{}",
        error.message
    );
}

/// Every call names a resource, and one that names the wrong kind is refused
/// by name rather than misread.
#[tokio::test]
async fn a_handle_of_another_kind_is_refused_by_name() {
    let resources = ResourceTable::new();
    let id = resources.add_cancel(Arc::new(CancelFlag::default()));
    let error = send(&resources, id, Some("hi".into()), None)
        .await
        .expect_err("a cancel handle is not a socket");
    assert_eq!(
        error.message,
        format!("resource {id} is a fetch cancel handle, not a web socket")
    );
    let error = close(&resources, id + 1, 1000, String::new())
        .await
        .expect_err("nothing is open");
    assert!(error.message.contains("is not open"), "{}", error.message);
}

/// Neither text nor bytes is the TypeError the embedded op throws, and it is
/// thrown before the connection is looked up.
#[tokio::test]
async fn a_send_of_neither_text_nor_bytes_is_refused() {
    let resources = ResourceTable::new();
    let error = send(&resources, 1, None, None)
        .await
        .expect_err("a send sends something");
    assert_eq!(error.class, "TypeError");
    assert_eq!(error.message, "No data provided");
}

/// Characterization for the `bytes` conversion a binary message takes:
/// `Vec::<u8>::from(Bytes)`.
///
/// A uniquely owned, full-length `Bytes` reclaims its original allocation --
/// pointer transfer, no copy. Contents are always exact; the pointer equality
/// records the current opportunistic-transfer behaviour, so a future `bytes`
/// upgrade that regresses it trips here rather than silently doubling what a
/// message costs.
#[test]
fn a_unique_full_length_message_transfers_its_allocation() {
    let original = vec![1u8, 2, 3, 4, 255, 0, 7];
    let address = original.as_ptr();
    let taken = Vec::<u8>::from(bytes::Bytes::from(original));
    assert_eq!(taken, [1u8, 2, 3, 4, 255, 0, 7], "exact contents");
    assert_eq!(
        taken.as_ptr(),
        address,
        "a unique full Bytes transfers its allocation rather than copying"
    );
}

/// The transfer is opportunistic, not guaranteed: a shared `Bytes` cannot
/// reclaim, so it copies, and so does a slice at a non-zero offset. Contents
/// stay exact either way.
#[test]
fn a_shared_or_sliced_message_copies_and_stays_exact() {
    let data = bytes::Bytes::from(vec![9u8, 9, 9]);
    // Held across the conversion so the backing stays shared.
    let held = data.clone();
    let backing = held.as_ptr();
    let taken = Vec::<u8>::from(data);
    assert_eq!(taken, [9u8, 9, 9], "exact contents on the copy path");
    assert_ne!(
        taken.as_ptr(),
        backing,
        "a shared Bytes copies while a clone is alive"
    );
    // Held past the comparison, so the backing is not freed under the check.
    assert_eq!(&held[..], &[9u8, 9, 9]);

    let sliced = bytes::Bytes::from(vec![10u8, 11, 12, 13, 14]).slice(1..4);
    assert_eq!(
        Vec::<u8>::from(sliced),
        [11u8, 12, 13],
        "a slice yields its exact contents"
    );
}

/// Five WebSocket connections may exist at once; a sixth fails before it connects, saying why, and a place frees
/// when one goes.
#[tokio::test]
async fn a_sixth_web_socket_fails_and_a_closed_one_frees_its_place() {
    let resources = ResourceTable::new();
    let held: Vec<_> = (0..super::super::resources::MAX_WEB_SOCKETS)
        .map(|_| resources.web_socket_slot().expect("a place"))
        .collect();
    let error = create(
        &NetworkPolicy::unrestricted(),
        &resources,
        "wss://example.com/",
        &[],
        &[],
        ConnectOptions {
            timeout_ms: Some(1000),
            ..Default::default()
        },
        migo_io::pools::IoPools::new(0),
    )
    .await
    .expect_err("a sixth connection");
    assert!(
        error.message.contains("at most 5 WebSocket connections"),
        "{}",
        error.message
    );
    drop(held);
    assert!(resources.web_socket_slot().is_some());
}
