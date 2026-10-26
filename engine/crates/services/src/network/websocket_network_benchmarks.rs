//! Explicit host TCP/TLS experiment, including the real handshake offloader,
//! RFC7692 negotiation, Sender/Receiver, fair codec executor and bounded link
//! emulator. CPU/PSS include peer and emulator; ciphertext accepted by TCP
//! excludes TCP/IP/ACK overhead. The proxy terminates TCP, so ACKs/congestion
//! control still see loopback, not the emulated byte-stream RTT. No
//! device/internet/platform-CA claim.
use super::*;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Each direction has its own rate and one-way propagation delay. Bounded
/// read-ahead pipelines propagation instead of adding an RTT to every chunk.
async fn forward<R, W>(mut reader: R, mut writer: W, rate: u64, delay: Duration)
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let (tx, mut rx) = tokio::sync::mpsc::channel::<(tokio::time::Instant, Vec<u8>)>(2);
    let producer = async move {
        let mut buffer = vec![0u8; 16 * 1024];
        let mut tail = tokio::time::Instant::now();
        loop {
            let count = match reader.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(count) => count,
            };
            let serialization = if rate == 0 {
                Duration::ZERO
            } else {
                Duration::from_secs_f64(count as f64 / rate as f64)
            };
            tail = tail.max(tokio::time::Instant::now()) + serialization;
            if tx
                .send((tail + delay, buffer[..count].to_vec()))
                .await
                .is_err()
            {
                break;
            }
        }
    };
    let consumer = async move {
        while let Some((deadline, bytes)) = rx.recv().await {
            // Even sleep_until(now) can round up to the next timer tick.
            // A zero-delay control must not gain a synthetic ~1ms per write.
            if deadline > tokio::time::Instant::now() {
                tokio::time::sleep_until(deadline).await;
            }
            if writer.write_all(&bytes).await.is_err() {
                break;
            }
        }
        let _ = writer.shutdown().await;
    };
    tokio::join!(producer, consumer);
}

async fn tcp_link(rate: u64, rtt: Duration, nodelay: bool) -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (proxy_in, _) = listener.accept().await.unwrap();
    let proxy_out = TcpStream::connect(peer_listener.local_addr().unwrap())
        .await
        .unwrap();
    let (server, _) = peer_listener.accept().await.unwrap();
    client.set_nodelay(nodelay).unwrap();
    assert_eq!(client.nodelay().unwrap(), nodelay);
    for socket in [&proxy_in, &proxy_out, &server] {
        socket.set_nodelay(true).unwrap();
    }
    let (client_read, client_write) = proxy_in.into_split();
    let (server_read, server_write) = proxy_out.into_split();
    tokio::spawn(async move {
        tokio::join!(
            forward(client_read, server_write, rate, rtt / 2),
            forward(server_read, client_write, rate, rtt / 2),
        );
    });
    (client, server)
}

struct Extensions(bool);
impl tokio_tungstenite::tungstenite::handshake::server::Callback for Extensions {
    fn on_request(
        self,
        request: &tokio_tungstenite::tungstenite::handshake::server::Request,
        mut response: tokio_tungstenite::tungstenite::handshake::server::Response,
    ) -> Result<
        tokio_tungstenite::tungstenite::handshake::server::Response,
        tokio_tungstenite::tungstenite::handshake::server::ErrorResponse,
    > {
        let offered = request.headers().get("Sec-WebSocket-Extensions");
        assert_eq!(offered.is_some(), self.0);
        if self.0 {
            assert_eq!(
                offered.unwrap(),
                super::super::super::super::websocket_deflate::OFFER
            );
            response.headers_mut().insert(
                "Sec-WebSocket-Extensions",
                HeaderValue::from_static("permessage-deflate"),
            );
        }
        Ok(response)
    }
}

#[tokio::test]
#[ignore = "explicit fresh-process Release TCP/TLS/link experiment; never part of normal correctness timing"]
async fn actual_tls_link_cost() {
    use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
    use sha2::Digest;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let case = std::env::var("MIGO_WS_BENCH_CASE").unwrap();
    let enabled = std::env::var("MIGO_WS_BENCH_DEFLATE").unwrap() == "true";
    let rate: u64 = std::env::var("MIGO_WS_LINK_BYTES_PER_SEC")
        .unwrap()
        .parse()
        .unwrap();
    let rtt_ms: u64 = std::env::var("MIGO_WS_LINK_RTT_MS")
        .unwrap()
        .parse()
        .unwrap();
    let nodelay = std::env::var("MIGO_WS_LINK_NODELAY").unwrap() == "true";
    let messages: usize = std::env::var("MIGO_WS_LINK_MESSAGES")
        .unwrap()
        .parse()
        .unwrap();
    assert!(messages > 0 && messages <= 1024);
    let data = bytes::Bytes::from(corpus(&case));
    let input_sha256 = hex::encode(sha2::Sha256::digest(&data));
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let certificate = certified.cert.der().clone();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let server_config = rustls::ServerConfig::builder_with_provider(provider.clone())
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
    let client_config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let (client, server) = tcp_link(rate, Duration::from_millis(rtt_ms), nodelay).await;
    let written = Arc::new(AtomicU64::new(0));
    let mut request = "wss://localhost/game".into_client_request().unwrap();
    if enabled {
        request.headers_mut().insert(
            "Sec-WebSocket-Extensions",
            HeaderValue::from_static(super::super::super::super::websocket_deflate::OFFER),
        );
    }
    let resources = ResourceTable::new();
    let pools = migo_io::pools::IoPools::new(7922);
    let started = std::time::Instant::now();
    let (client, server) = tokio::join!(
        finish_handshake(
            tokio_tungstenite::client_async_tls_with_config(
                request,
                Counted {
                    stream: client,
                    written: written.clone()
                },
                Some(if enabled {
                    compression_config()
                } else {
                    ws_config()
                }),
                Some(tokio_tungstenite::Connector::Rustls(Arc::new(
                    client_config
                )))
            ),
            true,
            resources.web_socket_slot().unwrap(),
            pools.clone(),
            tokio::time::Instant::now() + WRITE_TIMEOUT,
        ),
        async {
            let server = tokio_rustls::TlsAcceptor::from(Arc::new(server_config))
                .accept(server)
                .await
                .unwrap();
            tokio_tungstenite::accept_hdr_async_with_config(
                server,
                Extensions(enabled),
                Some(if enabled {
                    compression_config()
                } else {
                    ws_config()
                }),
            )
            .await
            .unwrap()
        },
    );
    let handshake_ns = started.elapsed().as_nanos();
    let ((client, response), slot) = client.unwrap();
    assert_eq!(
        response.headers().contains_key("Sec-WebSocket-Extensions"),
        enabled
    );
    if enabled {
        assert_eq!(
            response.headers()["Sec-WebSocket-Extensions"],
            "permessage-deflate"
        );
    }
    let (client, _client_read) = client.split();
    let (_server_write, server) = server.split();
    let values: Vec<_> = response
        .headers()
        .get_all("Sec-WebSocket-Extensions")
        .iter()
        .map(|value| value.to_str().unwrap())
        .collect();
    let parameters =
        super::super::super::super::websocket_deflate::negotiate(enabled, values).unwrap();
    assert_eq!(parameters.is_some(), enabled);
    if let Some(parameters) = parameters {
        assert!(!parameters.client_no_context_takeover && !parameters.server_no_context_takeover);
        assert_eq!(parameters.server_max_window_bits, 15);
    }
    let slot = Arc::new(slot);
    let mut tx = Sender::new(client, parameters, pools.clone(), slot.clone());
    let mut rx = Receiver::new(server, parameters, pools, slot);
    let initial_pss = pss_kib();
    for _ in 0..4 {
        transfer(&mut tx, &mut rx, &data).await;
    }
    let warm_pss = pss_kib();
    let before_wire = written.load(Ordering::Relaxed);
    let before_cpu = cpu_ns();
    let before_time = std::time::Instant::now();
    let mut elapsed_samples = Vec::with_capacity(messages);
    for _ in 0..messages {
        let started = std::time::Instant::now();
        transfer(&mut tx, &mut rx, &data).await;
        elapsed_samples.push(started.elapsed().as_nanos());
    }
    let elapsed_ns = before_time.elapsed().as_nanos();
    let cpu_ns = cpu_ns() - before_cpu;
    let tcp_tls_bytes = written.load(Ordering::Relaxed) - before_wire;
    // Fail visibly if the fixture accidentally bypasses its shaped link.
    // These are conservative physical lower bounds, not timing upper limits.
    if rate != 0 {
        assert!(
            elapsed_ns >= tcp_tls_bytes as u128 * 1_000_000_000 / rate as u128 * 99 / 100,
            "link serialization lower bound was bypassed"
        );
    }
    assert!(
        elapsed_ns >= rtt_ms as u128 * 500_000 * messages as u128 * 99 / 100,
        "sequential message propagation lower bound was bypassed"
    );
    elapsed_samples.sort_unstable();
    println!(
        "MIGO_WS_NETWORK_BENCH {}",
        serde_json::json!({
            "case":case,"deflate":enabled,"input_sha256":input_sha256,"input_bytes":data.len(),
            "messages":messages,"rate_bytes_per_sec_per_direction":rate,"emulated_stream_rtt_ms":rtt_ms,
            "tcp_nodelay":nodelay,"handshake_ns":handshake_ns,"elapsed_ns":elapsed_ns,
            "message_p50_ns":elapsed_samples[messages / 2],
            "message_p95_ns":elapsed_samples[(messages * 95 / 100).min(messages - 1)],
            "whole_fixture_cpu_ns":cpu_ns,"tcp_tls_bytes":tcp_tls_bytes,
            "pss_initial_kib":initial_pss,"pss_warm_kib":warm_pss,"pss_retained_kib":pss_kib(),
        })
    );
}
