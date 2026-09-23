use super::*;

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
    let policy = NetworkPolicy {
        domain_whitelist: vec!["allowed.example".to_string()],
        enforce_https: true,
    };
    let resources = ResourceTable::new();
    let error = create(
        &policy,
        &resources,
        "wss://blocked.example/socket",
        &[],
        &[],
        None,
    )
    .await
    .expect_err("the allow list refuses it");
    assert!(error.message.contains("blocked.example"), "{}", error.message);
    assert!(resources.is_empty());

    // And `ws://` where the policy enforces HTTPS.
    let error = create(
        &policy,
        &resources,
        "ws://allowed.example/socket",
        &[],
        &[],
        None,
    )
    .await
    .expect_err("the policy enforces TLS");
    assert!(
        error.message.contains("HTTPS is required"),
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
        None,
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
