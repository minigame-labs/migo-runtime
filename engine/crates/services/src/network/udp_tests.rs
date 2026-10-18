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
    let policy = NetworkPolicy::from_json(r#"{"udp":["allowed.example"]}"#).unwrap();
    let resources = ResourceTable::new();
    let bound = bind(&policy, &resources, 0, "udp4").expect("a system-assigned port");
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

/// A broadcast is the reference's way to find peers on a LAN: with the local
/// network granted, setBroadcast turns SO_BROADCAST on and the datagram goes to
/// the kernel. Whether the kernel can deliver it is the machine's network's to
/// say (a host with no broadcast route answers so); the engine does not refuse it.
#[tokio::test]
async fn a_broadcast_is_handed_to_the_kernel() {
    let policy = NetworkPolicy {
        lan: true,
        ..NetworkPolicy::default()
    };
    let resources = ResourceTable::new();
    let bound = bind(&policy, &resources, 0, "udp4").unwrap();
    let sent = send(
        &policy,
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
    .await;
    if let Err(error) = &sent {
        assert!(
            !error.message.contains("not allowed") && !error.message.contains("policy"),
            "the engine refused it: {}",
            error.message
        );
    }
    assert!(
        resources
            .udp(bound.rid)
            .unwrap()
            .socket
            .broadcast()
            .unwrap()
    );
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
    let receiver = bind_hearing(&resources, 0, "udp4", Inbound::Any).unwrap();
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

/// A socket hears whoever can reach its port, so a policy under which content
/// could send to no one -- the default -- gets no socket at all.
#[tokio::test]
async fn a_policy_under_which_content_could_send_to_no_one_gets_no_socket() {
    let resources = ResourceTable::new();
    let error = bind(&NetworkPolicy::default(), &resources, 0, "udp4").unwrap_err();
    assert!(
        error.message.contains("grants the udp channel nothing"),
        "{}",
        error.message
    );

    let lan_only = NetworkPolicy {
        lan: true,
        ..NetworkPolicy::default()
    };
    assert!(
        bind(&lan_only, &resources, 0, "udp4").is_ok(),
        "the local network is someone"
    );
    let a_domain = NetworkPolicy::from_json(r#"{"udp":["game.example"]}"#).unwrap();
    assert!(
        bind(&a_domain, &resources, 0, "udp4").is_ok(),
        "a domain is someone"
    );
}

/// The device itself is a sender content may never send to, so what it sends
/// to content's port is dropped rather than heard.
#[tokio::test]
async fn a_sender_content_could_not_send_to_is_not_heard() {
    let policy = NetworkPolicy::from_json(r#"{"udp":"unrestricted"}"#).unwrap();
    let resources = ResourceTable::new();
    let receiver = bind(&policy, &resources, 0, "udp4").unwrap();
    let sender = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    sender
        .send_to(b"ping", format!("127.0.0.1:{}", receiver.local.port))
        .unwrap();

    let quiet = std::sync::atomic::AtomicBool::new(false);
    let heard = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        next_event(&resources, receiver.rid, &quiet),
    )
    .await;
    assert!(
        heard.is_err(),
        "a datagram from the loopback reached content: {heard:?}"
    );
}

fn at(a: u8, b: u8, c: u8, d: u8) -> SocketAddr {
    SocketAddr::from(([a, b, c, d], 9000))
}

/// A public sender is heard once the socket has addressed it, from any of its
/// ports and in either spelling of its address -- and no other public sender is.
#[test]
fn a_public_sender_is_heard_once_the_socket_has_addressed_it() {
    let inbound = Inbound::policy(LanAccess::Refused);
    inbound.addressed(SocketAddr::from(([8, 8, 8, 8], 53)));
    assert!(inbound.admits(&at(8, 8, 8, 8)), "a reply from another port");
    let mapped: SocketAddr = "[::ffff:8.8.8.8]:9000".parse().unwrap();
    assert!(
        inbound.admits(&mapped),
        "the same address, as a udp6 socket sees it"
    );
    assert!(!inbound.admits(&at(1, 1, 1, 1)), "an address never sent to");

    inbound.addressed(at(127, 0, 0, 1));
    inbound.addressed(at(192, 168, 1, 1));
    assert!(
        !inbound.admits(&at(127, 0, 0, 1)),
        "never the device itself"
    );
    assert!(
        !inbound.admits(&at(192, 168, 1, 1)),
        "the local network was not granted"
    );
}

/// Granting domains, or every domain, grants content the addresses it reaches
/// through them -- not the internet, which is what a fresh socket heard before.
#[tokio::test]
async fn no_policy_lets_a_fresh_socket_hear_a_public_sender() {
    let resources = ResourceTable::new();
    for policy in [
        r#"{"udp":["game.example"]}"#,
        r#"{"udp":"unrestricted"}"#,
        r#"{"lan":true}"#,
    ] {
        let policy = NetworkPolicy::from_json(policy).unwrap();
        let bound = bind(&policy, &resources, 0, "udp4").unwrap();
        let socket = resources.udp(bound.rid).unwrap();
        assert!(!socket.inbound.admits(&at(8, 8, 8, 8)), "{policy:?}");
    }
}

/// A send and a connect reach their peer through `destination`, so that is where
/// the peer becomes heard. Driven with a literal so it resolves without a lookup
/// and nothing leaves the machine.
#[tokio::test]
async fn resolving_a_destination_is_what_makes_its_replies_heard() {
    let policy = NetworkPolicy::from_json(r#"{"udp":["game.example"]}"#).unwrap();
    let resources = ResourceTable::new();
    let bound = bind(&policy, &resources, 0, "udp4").unwrap();
    let socket = resources.udp(bound.rid).unwrap();

    let resolved = socket
        .destination("8.8.8.8:53", LanAccess::Refused)
        .await
        .unwrap();
    assert_eq!(resolved, SocketAddr::from(([8, 8, 8, 8], 53)));
    assert!(socket.inbound.admits(&at(8, 8, 8, 8)));

    socket
        .destination("127.0.0.1:53", LanAccess::Refused)
        .await
        .expect_err("the datagram rule still refuses the loopback");
    assert!(!socket.inbound.admits(&at(127, 0, 0, 1)));
}

/// The kernel loops multicast back to the sending host unless told not to, and
/// the device itself is never content's to reach.
#[tokio::test]
async fn a_socket_does_not_loop_its_multicast_back_to_the_device() {
    let policy = NetworkPolicy {
        lan: true,
        ..NetworkPolicy::default()
    };
    let resources = ResourceTable::new();
    let bound = bind(&policy, &resources, 0, "udp4").unwrap();
    let socket = resources.udp(bound.rid).unwrap();
    assert!(!socket.socket.multicast_loop_v4().unwrap());
}
