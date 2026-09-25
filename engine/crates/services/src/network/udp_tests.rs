use super::*;
use std::net::SocketAddr;

#[test]
fn length_zero_means_offset_to_end() {
    assert_eq!(send_range(10, 0, 0), Ok((0, 10)));
    assert_eq!(send_range(10, 3, 0), Ok((3, 10)));
}

#[test]
fn explicit_length_within_bounds() {
    assert_eq!(send_range(10, 0, 10), Ok((0, 10)));
    assert_eq!(send_range(10, 2, 5), Ok((2, 7)));
}

#[test]
fn offset_at_end_sends_empty_slice() {
    assert_eq!(send_range(10, 10, 0), Ok((10, 10)));
    assert_eq!(send_range(0, 0, 0), Ok((0, 0)));
}

#[test]
fn rejects_offset_past_buffer() {
    assert!(send_range(10, 11, 0).is_err());
}

#[test]
fn rejects_offset_plus_length_past_buffer_instead_of_clamping() {
    // Regression: this used to clamp to buf.len() and silently send
    // fewer bytes than requested.
    assert!(send_range(10, 5, 10).is_err());
    assert!(send_range(10, 0, 11).is_err());
}

#[test]
fn rejects_overflowing_length_without_panicking() {
    assert_eq!(send_range(10, 4, usize::MAX), Err("length overflow"));
}

#[test]
fn udp_peer_cache_reuses_same_peer_and_rebuilds_on_change() {
    let a: SocketAddr = "1.2.3.4:5000".parse().unwrap();
    let b: SocketAddr = "9.8.7.6:6000".parse().unwrap();
    let mut cache: Option<(SocketAddr, Arc<str>)> = None;

    // First datagram from peer A: builds and caches.
    let first = peer_address(&mut cache, a);
    assert_eq!(&*first, "1.2.3.4");

    // Same peer A: reuses the identical Arc allocation (refcount bump).
    let again = peer_address(&mut cache, a);
    assert_eq!(&*again, "1.2.3.4");
    assert!(Arc::ptr_eq(&first, &again));

    // Different peer B: rebuilds, and the event reflects the ACTUAL peer,
    // never a stale cached value.
    let other = peer_address(&mut cache, b);
    assert_eq!(&*other, "9.8.7.6");
    assert!(!Arc::ptr_eq(&first, &other));

    // Single-entry cache: A was evicted, so it rebuilds (new allocation).
    let a_third = peer_address(&mut cache, a);
    assert_eq!(&*a_third, "1.2.3.4");
    assert!(!Arc::ptr_eq(&first, &a_third));
}

/// A destination the policy refuses is never sent to, and neither connect nor
/// send reaches the kernel for it.
#[tokio::test]
async fn a_destination_the_policy_refuses_is_never_sent_to() {
    let policy = NetworkPolicy {
        domain_whitelist: vec!["allowed.example".to_string()],
        enforce_https: false,
    };
    let resources = ResourceTable::new();
    let bound = bind(&resources, 0, "udp4").expect("a system-assigned port");
    assert_ne!(bound.local.port, 0, "the system assigned a port");

    let error = connect(&policy, &resources, bound.rid, "blocked.example", 9000)
        .await
        .expect_err("the allow list refuses it");
    assert!(
        error.message.contains("blocked.example"),
        "{}",
        error.message
    );

    let error = send(
        &policy,
        &resources,
        bound.rid,
        "blocked.example",
        9000,
        Some("hi".into()),
        None,
        0,
        0,
        false,
    )
    .await
    .expect_err("the allow list refuses it");
    assert!(
        error.message.contains("blocked.example"),
        "{}",
        error.message
    );
}

/// Broadcast has no legitimate target once the address filter is on, so the
/// flag is refused rather than honoured.
#[tokio::test]
async fn broadcast_is_refused_outright() {
    let resources = ResourceTable::new();
    let bound = bind(&resources, 0, "udp4").unwrap();
    let error = send(
        &NetworkPolicy::default(),
        &resources,
        bound.rid,
        "255.255.255.255",
        9000,
        Some("hi".into()),
        None,
        0,
        0,
        true,
    )
    .await
    .expect_err("broadcast is not permitted");
    assert!(error.message.contains("broadcast"), "{}", error.message);
}

/// A handle of another kind is refused by name for every call that takes a
/// socket.
#[tokio::test]
async fn a_handle_that_is_not_a_socket_is_refused_by_name() {
    let resources = ResourceTable::new();
    let id = resources.add_cancel(Arc::new(super::super::resources::CancelFlag::default()));
    let error = set_ttl(&resources, id, 5).expect_err("a cancel handle is not a socket");
    assert_eq!(
        error.message,
        format!("resource {id} is a fetch cancel handle, not a UDP socket")
    );
    let error = close(&resources, id + 1).expect_err("nothing is open");
    assert!(error.message.contains("is not open"), "{}", error.message);
}

/// Two sockets on one host, talking: what the receiver is handed is the
/// datagram that was sent and the peer that sent it.
///
/// Loopback is what the address filter refuses, so this drives the socket
/// itself rather than `send`, which is the layer that filter belongs to.
#[tokio::test]
async fn a_datagram_arrives_whole_with_the_peer_that_sent_it() {
    let resources = ResourceTable::new();
    let receiver = bind(&resources, 0, "udp4").unwrap();
    let sender = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let to = format!("127.0.0.1:{}", receiver.local.port);
    sender.send_to(b"ping", &to).unwrap();

    let quiet = std::sync::atomic::AtomicBool::new(false);
    let event = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        next_event(&resources, receiver.rid, &quiet),
    )
    .await
    .expect("a datagram arrives")
    .expect("and is an event");
    match event {
        UdpEvent::Message {
            data,
            remote,
            local,
        } => {
            assert_eq!(&*data, b"ping");
            assert_eq!(&*remote.address, "127.0.0.1");
            assert_eq!(remote.port, sender.local_addr().unwrap().port());
            assert_eq!(local.port, receiver.local.port);
        }
        other => panic!("expected a datagram, not {other:?}"),
    }

    // Closing cancels the read that is waiting for the next one.
    close(&resources, receiver.rid).unwrap();
    let error = next_event(&resources, receiver.rid, &quiet)
        .await
        .expect_err("a closed socket is not read from");
    assert!(error.message.contains("is not open"), "{}", error.message);
}
