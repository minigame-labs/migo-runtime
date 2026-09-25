use super::*;

/// The policy a raw socket is held to is the one `fetch` is held to: a host
/// content cannot reach one way it cannot reach the other.
#[tokio::test]
async fn a_host_the_policy_refuses_is_never_connected_to() {
    let policy = NetworkPolicy {
        domain_whitelist: vec!["allowed.example".to_string()],
        enforce_https: false,
    };
    let resources = ResourceTable::new();
    let error = connect(&policy, &resources, "blocked.example", 443, 1)
        .await
        .expect_err("the allow list refuses it");
    assert!(
        error.message.contains("blocked.example"),
        "{}",
        error.message
    );
    assert!(resources.is_empty(), "a refused connect leaves no handle");
}

/// A port that is not one is refused rather than wrapped: `70000 as u16` is
/// 4464, a different port than the caller asked for.
#[tokio::test]
async fn a_port_out_of_range_is_refused_before_the_gate() {
    let resources = ResourceTable::new();
    let error = connect(
        &NetworkPolicy::default(),
        &resources,
        "example.com",
        70_000,
        1,
    )
    .await
    .expect_err("a port is sixteen bits");
    assert_eq!(error.class, "TypeError");
    assert_eq!(error.message, "port 70000 out of range (0-65535)");
}

/// A handle of another kind is refused by name, and one that names nothing
/// says so -- for every call that takes a socket.
#[tokio::test]
async fn a_handle_that_is_not_a_socket_is_refused_by_name() {
    let resources = ResourceTable::new();
    let id = resources.add_cancel(Arc::new(CancelFlag::default()));
    let pools = IoPools::new(9601);
    let error = write(&resources, &pools, id, Some("hi".into()), None)
        .await
        .expect_err("a cancel handle is not a socket");
    assert_eq!(
        error.message,
        format!("resource {id} is a fetch cancel handle, not a TCP socket")
    );
    let error = close(&resources, id + 1).expect_err("nothing is open");
    assert!(error.message.contains("is not open"), "{}", error.message);
}

/// Neither text nor bytes is the TypeError the embedded op throws, before the
/// socket is even looked up.
#[tokio::test]
async fn a_write_of_neither_text_nor_bytes_is_refused() {
    let resources = ResourceTable::new();
    let pools = IoPools::new(9602);
    let error = write(&resources, &pools, 1, None, None)
        .await
        .expect_err("a write writes something");
    assert_eq!(error.class, "TypeError");
    assert_eq!(error.message, "write:fail no data provided");
}

/// A payload larger than the session's whole byte budget is refused before any
/// native work, so an oversized write costs nothing but the refusal.
#[test]
fn a_write_larger_than_the_budget_is_refused_before_it_starts() {
    let pools = IoPools::new(9603);
    let error = match reserve_write_bytes(&pools, 256 * 1024 * 1024 + 1) {
        Ok(_) => panic!("a write larger than the process byte limit must be refused"),
        Err(error) => error,
    };
    assert!(
        error.message.contains("byte limit") && error.message.contains("requested"),
        "{}",
        error.message
    );
}

/// A cancelled write releases what it was holding -- its payload and its byte
/// credit -- rather than pinning both behind a peer that stopped reading.
#[tokio::test]
async fn a_cancelled_write_releases_its_payload_and_its_credit() {
    let pools = IoPools::new(9604);
    let ticket = reserve_write_bytes(&pools, 4096).unwrap();
    let payload = Arc::new(vec![0u8; 16 * 1024 * 1024]);
    let watched = Arc::downgrade(&payload);
    let cancel = Arc::new(CancelFlag::default());

    let flag = Arc::clone(&cancel);
    let waiting = tokio::spawn(async move {
        flag.until_cancelled(async move {
            let _held = (ticket, payload);
            std::future::pending::<()>().await;
        })
        .await
    });
    tokio::task::yield_now().await;
    cancel.cancel();
    assert!(
        waiting.await.unwrap().is_none(),
        "a cancelled write does not finish"
    );
    assert!(
        watched.upgrade().is_none(),
        "a cancelled write releases its payload"
    );
    reserve_write_bytes(&pools, 4096).expect("and its byte credit, exactly once");
}

/// A peer that stops reading must not hold the writer forever: the deadline
/// reclaims it, and the next write can take the lock.
#[tokio::test]
async fn a_stalled_peer_loses_the_writer_at_the_deadline() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback listener");
    let address = listener.local_addr().unwrap();
    let client = TcpStream::connect(address).await.expect("loopback connect");
    // Accepted and held, never read from: the socket's buffers fill and the
    // write blocks.
    let (_server, _) = listener.accept().await.expect("accept");
    let connection = TcpConn::over(client, Duration::from_millis(100), address);

    let pools = IoPools::new(9605);
    let payload = vec![0u8; 64 * 1024 * 1024];
    let ticket = reserve_write_bytes(&pools, payload.len()).unwrap();
    let error = tokio::time::timeout(
        Duration::from_secs(5),
        write_payload(&connection, payload, ticket),
    )
    .await
    .expect("the deadline reclaims a stalled write")
    .expect_err("a stalled peer is not written to");
    assert!(
        error.message.contains("deadline exceeded"),
        "{}",
        error.message
    );
    // The writer is free again: this would hang if the deadline had left it
    // taken.
    connection.tx.lock().await;
}

/// And closing reclaims it too, without waiting for the deadline.
#[tokio::test]
async fn closing_reclaims_the_writer_from_a_stalled_peer() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback listener");
    let address = listener.local_addr().unwrap();
    let client = TcpStream::connect(address).await.expect("loopback connect");
    let (_server, _) = listener.accept().await.expect("accept");
    let connection = Arc::new(TcpConn::over(client, Duration::from_secs(30), address));

    let pools = IoPools::new(9606);
    let payload = vec![0u8; 64 * 1024 * 1024];
    let ticket = reserve_write_bytes(&pools, payload.len()).unwrap();
    let closing = Arc::clone(&connection);
    let closed = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        closing.cancel();
    });
    let error = tokio::time::timeout(
        Duration::from_secs(5),
        write_payload(&connection, payload, ticket),
    )
    .await
    .expect("the close reclaims a stalled write")
    .expect_err("a closed socket is not written to");
    closed.await.unwrap();
    assert_eq!(error.message, "TCPSocket closed");
    connection.tx.lock().await;
}
