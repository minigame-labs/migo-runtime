//! Explicit host experiment over the actual transport, executor and codecs.
//! Duplex backpressure is deterministic; there is no TCP/TLS/device claim.
use super::*;
use std::pin::Pin;
use std::sync::atomic::AtomicU64;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

struct Counted<S> {
    stream: S,
    written: Arc<AtomicU64>,
}

impl<S: AsyncRead + Unpin> AsyncRead for Counted<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_read(cx, bytes)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Counted<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let result = Pin::new(&mut self.stream).poll_write(cx, bytes);
        if let Poll::Ready(Ok(count)) = result {
            self.written.fetch_add(count as u64, Ordering::Relaxed);
        }
        result
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

fn cpu_ns() -> u64 {
    let mut value = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // This Linux-only test samples the process, including its actual IO workers.
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut value) },
        0
    );
    value.tv_sec as u64 * 1_000_000_000 + value.tv_nsec as u64
}

fn pss_kib() -> u64 {
    std::fs::read_to_string("/proc/self/smaps_rollup")
        .unwrap()
        .lines()
        .find_map(|line| {
            line.strip_prefix("Pss:")
                .map(|value| value.split_whitespace().next().unwrap().parse().unwrap())
        })
        .unwrap()
}

async fn transfer<S, T>(tx: &mut Sender<S>, rx: &mut Receiver<T>, data: &bytes::Bytes)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let ((), ()) = tokio::join!(
        async {
            tx.send(
                Message::Binary(data.clone()),
                tokio::time::Instant::now() + WRITE_TIMEOUT,
            )
            .await
            .unwrap();
        },
        async {
            loop {
                match rx.next().await.unwrap() {
                    ReadStep::Event(WsEvent::Binary(bytes)) => {
                        assert!(
                            bytes.as_slice() == data.as_ref(),
                            "roundtrip payload mismatch"
                        );
                        break;
                    }
                    ReadStep::Control | ReadStep::Fragment => {}
                    _ => panic!("unexpected transport event"),
                }
            }
        }
    );
}

#[tokio::test]
#[ignore = "explicit fresh-process Release experiment; run tools/websocket-transport-bench.py"]
async fn actual_transport_cost() {
    use tokio_tungstenite::tungstenite::protocol::Role;
    let case = std::env::var("MIGO_WS_BENCH_CASE").expect("benchmark runner selects a corpus");
    let enabled = std::env::var("MIGO_WS_BENCH_DEFLATE").unwrap() == "true";
    let data = corpus(&case);
    let input_sha256 = {
        use sha2::Digest;
        hex::encode(sha2::Sha256::digest(&data))
    };
    let data = bytes::Bytes::from(data);
    let written = Arc::new(AtomicU64::new(0));
    let (client, server) = tokio::io::duplex(16 * 1024);
    let client = tokio_tungstenite::WebSocketStream::from_raw_socket(
        Counted {
            stream: client,
            written: written.clone(),
        },
        Role::Client,
        Some(if enabled {
            compression_config()
        } else {
            ws_config()
        }),
    )
    .await;
    let server = tokio_tungstenite::WebSocketStream::from_raw_socket(
        server,
        Role::Server,
        Some(if enabled {
            compression_config()
        } else {
            ws_config()
        }),
    )
    .await;
    let (client, _client_read) = client.split();
    let (_server_write, server) = server.split();
    let resources = ResourceTable::new();
    let slot = Arc::new(resources.web_socket_slot().unwrap());
    let parameters = enabled.then(compression_parameters);
    let pools = migo_io::pools::IoPools::new(7921);
    let mut tx = Sender::new(client, parameters, pools.clone(), slot.clone());
    let mut rx = Receiver::new(server, parameters, pools, slot);
    let initial_pss = pss_kib();
    for _ in 0..16 {
        transfer(&mut tx, &mut rx, &data).await;
    }
    let warm_pss = pss_kib();
    let before_wire = written.load(Ordering::Relaxed);
    let before_cpu = cpu_ns();
    let before_time = std::time::Instant::now();
    let messages = 512;
    for _ in 0..messages {
        transfer(&mut tx, &mut rx, &data).await;
    }
    let elapsed_ns = before_time.elapsed().as_nanos();
    let cpu_ns = cpu_ns() - before_cpu;
    let wire_bytes = written.load(Ordering::Relaxed) - before_wire;
    println!(
        "MIGO_WS_TRANSPORT_BENCH {}",
        serde_json::json!({
            "case":case,"deflate":enabled,"input_sha256":input_sha256,"input_bytes":data.len(),
            "messages":messages,"elapsed_ns":elapsed_ns,"cpu_ns":cpu_ns,"wire_bytes":wire_bytes,
            "pss_initial_kib":initial_pss,"pss_warm_kib":warm_pss,"pss_retained_kib":pss_kib(),
        })
    );
}

fn corpus(case: &str) -> Vec<u8> {
    match case {
        "tiny-state" => {
            br#"{"tick":123,"player":{"x":10.25,"y":18.5,"animation":"jump","hp":89}}"#.to_vec()
        }
        "state-json" => br#"{"id":15,"x":100,"y":120,"hp":89,"animation":"run"}"#.repeat(128),
        "binary-64k" => (0u8..=255).cycle().take(64 * 1024).collect(),
        "entropy-64k" => {
            let mut state = 0x19770612u32;
            (0..64 * 1024)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 17;
                    state ^= state << 5;
                    state as u8
                })
                .collect()
        }
        _ => panic!("unknown benchmark corpus"),
    }
}

#[path = "websocket_network_benchmarks.rs"]
mod network;
