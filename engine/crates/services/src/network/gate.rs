//! Unified network-policy gate for every outbound-capable op.
//!
//! Every op that opens a network connection or performs a URL-based
//! request MUST funnel through [`enforce`] or [`enforce_host`].
//! This closes the gap the report flagged: previously `fetch()` had
//! full preflight (IP-literal block, domain whitelist, HTTPS enforce)
//! but `Image.src = http(s)://...` and `new WebSocket(...)` were
//! routed directly to the engine's HTTP client (network::http) and the tungstenite socket pool
//! without the preflight, which made the domain whitelist bypassable.
//!
//! # Design
//!
//! * [`evaluate_policy`] is a **pure** function over a URL and a
//!   snapshot of `NetworkPolicy`. All rule decisions live here so
//!   they can be unit-tested without standing up a runtime.
//! * [`enforce`] is what every caller uses: it evaluates the policy,
//!   counts the refusal, and answers the message content is shown. The
//!   embedded runtime wraps it in a `JsErrorBox`; the external session's
//!   service dispatcher answers with it directly.
//! * [`GateKind`] names the entry point; [`GateKind::channel`] says which of
//!   the policy's six allowlists decides it.
//!
//! # Why a single enum for kinds?
//!
//! An open-coded bool matrix (`check_https: bool`, `check_wl: bool`,
//! …) invites drift: a new op forgets a flag and the whitelist goes
//! silently unenforced again. With a single `GateKind` every new
//! call site must pick a kind, and every mapping from a kind -- its
//! channel, its schemes, its name -- is an exhaustive `match`, so a new
//! kind does not compile until each one places it.
//!
//! # Redirects
//!
//! A redirect hop is gated as the request that followed it: the HTTP client
//! carries the kind the request was made as and evaluates each hop under it,
//! so a download that is redirected is still held to the download allowlist.

use url::Url;

use super::address_filter::{LanAccess, Reach, datagram_reach, stream_reach};
use shared::network_policy::{NetworkChannel, NetworkPolicy};

/// Classifies which network entry point is being guarded.
///
/// Every outbound-capable op must map itself to exactly one variant, and every
/// variant to exactly one [`NetworkChannel`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateKind {
    /// `request` / `fetch()` (HTTP/HTTPS).
    Fetch,
    /// `downloadFile`.
    Download,
    /// `InnerAudioContext.src` HTTP streaming request.
    AudioStream,
    /// `uploadFile`.
    FetchUpload,
    /// `prefetchAssets`: a GET that warms the connection a later download,
    /// image or audio stream of the same asset reuses.
    Prefetch,
    /// `Image.src = "http(s)://..."`. `data:` is parsed separately
    /// before the gate runs, so only http/https reach here.
    ImageInlineSrc,
    /// An `http(s)` image or video content hands the host or the system media
    /// stack to fetch: `previewImage`, `previewMedia`, a share's image, a video
    /// decoder's source. Checked here, fetched there.
    HostMedia,
    /// `new WebSocket(url)`. Scheme must be `ws`/`wss`; `ws://` also needs
    /// `allow_plain_http`.
    WebSocket,
    /// `createTCPSocket().connect(host, port)`. Raw TCP has no URL;
    /// enforcement runs the tcp allowlist and the IP-literal rules against
    /// the host argument.
    TcpSocket,
    /// `createUDPSocket().send(host, port, …)` and `.connect(...)`.
    UdpSocket,
}

impl GateKind {
    /// The allowlist that decides this kind.
    pub const fn channel(self) -> NetworkChannel {
        match self {
            GateKind::Fetch => NetworkChannel::Request,
            GateKind::Download
            | GateKind::AudioStream
            | GateKind::Prefetch
            | GateKind::ImageInlineSrc
            | GateKind::HostMedia => NetworkChannel::Download,
            GateKind::FetchUpload => NetworkChannel::Upload,
            GateKind::WebSocket => NetworkChannel::Socket,
            GateKind::TcpSocket => NetworkChannel::Tcp,
            GateKind::UdpSocket => NetworkChannel::Udp,
        }
    }

    /// The API a refusal names, so an incident is attributed to the call content made.
    pub const fn label(self) -> &'static str {
        match self {
            GateKind::Fetch => "fetch",
            GateKind::Download => "downloadFile",
            GateKind::AudioStream => "audio",
            GateKind::FetchUpload => "uploadFile",
            GateKind::Prefetch => "prefetch",
            GateKind::ImageInlineSrc => "Image.src",
            GateKind::HostMedia => "media URL",
            GateKind::WebSocket => "WebSocket",
            GateKind::TcpSocket => "TCPSocket",
            GateKind::UdpSocket => "UDPSocket",
        }
    }
}

/// The two transports content reaches by host and port rather than by URL: the
/// only kinds [`enforce_host`] can be asked about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Tcp,
    Udp,
}

impl Transport {
    pub const fn kind(self) -> GateKind {
        match self {
            Transport::Tcp => GateKind::TcpSocket,
            Transport::Udp => GateKind::UdpSocket,
        }
    }

    /// The synthetic scheme its host is checked under.
    const fn scheme(self) -> &'static str {
        match self {
            Transport::Tcp => "tcp",
            Transport::Udp => "udp",
        }
    }
}

/// Whether `channel` may reach other devices on the local network under `policy`. A request, download or upload
/// never may, whatever the policy says: the HTTP client's resolver therefore refuses every private address without
/// asking.
pub fn lan_access(policy: &NetworkPolicy, channel: NetworkChannel) -> LanAccess {
    match channel {
        NetworkChannel::Request | NetworkChannel::Download | NetworkChannel::Upload => {
            LanAccess::Refused
        }
        NetworkChannel::Socket | NetworkChannel::Tcp | NetworkChannel::Udp if policy.lan => {
            LanAccess::LocalNetwork
        }
        NetworkChannel::Socket | NetworkChannel::Tcp | NetworkChannel::Udp => LanAccess::Refused,
    }
}

/// Ways a gate check can fail. Kept narrow so call sites can decide
/// error vocabulary without parsing the message.
#[derive(Debug, PartialEq, Eq)]
pub enum GateReject {
    /// URL scheme is not permitted for this [`GateKind`].
    UnsupportedScheme { scheme: String },
    /// The address is one content may not reach: loopback, link-local and the
    /// other reserved ranges always; a private address unless the local network
    /// is granted to this channel and the address is on it.
    BlockedAddress { display: String },
    /// A public IP address. Allowlists name domains, so no address is ever on one.
    IpLiteral { display: String },
    /// Host was not on the allowlist of the channel the request was made on.
    NotAllowed {
        host: String,
        channel: NetworkChannel,
    },
    /// `http://` or `ws://` while `allow_plain_http` is off.
    PlainTextNotAllowed { scheme: String },
    /// URL has no host component at all, so no rule can be applied.
    /// Happens for malformed input no client could send.
    MissingHost,
}

impl GateReject {
    /// Why, without saying which API asked.
    pub fn detail(&self) -> String {
        match self {
            GateReject::UnsupportedScheme { scheme } => {
                format!("scheme '{scheme}' is not allowed")
            }
            GateReject::BlockedAddress { display } => {
                format!("connection to {display} is not allowed (private/loopback address)")
            }
            GateReject::IpLiteral { display } => {
                format!("{display} is an IP address; content reaches servers by domain name")
            }
            GateReject::NotAllowed { host, channel } => {
                format!("domain '{host}' is not in the {} allowlist", channel.name())
            }
            GateReject::PlainTextNotAllowed { scheme } => {
                format!("plain-text {scheme}:// is not allowed (allowPlainHttp is off)")
            }
            GateReject::MissingHost => "URL has no host".to_string(),
        }
    }

    fn to_message(&self, kind: GateKind) -> String {
        format!("{}: {}", kind.label(), self.detail())
    }
}

/// Scheme policy matrix: which schemes reach each kind at all. The
/// allowlist and the plain-text rule apply on top.
fn scheme_allowed(kind: GateKind, scheme: &str) -> bool {
    match kind {
        GateKind::Fetch
        | GateKind::Download
        | GateKind::AudioStream
        | GateKind::FetchUpload
        | GateKind::Prefetch
        | GateKind::ImageInlineSrc
        | GateKind::HostMedia => matches!(scheme, "http" | "https"),
        GateKind::WebSocket => matches!(scheme, "ws" | "wss"),
        // Raw TCP/UDP use the synthetic `tcp://` / `udp://` scheme
        // produced by [`enforce_host`]; no other kind is
        // allowed to feed those schemes in.
        GateKind::TcpSocket => scheme == "tcp",
        GateKind::UdpSocket => scheme == "udp",
    }
}

/// Side-effect-free policy evaluator.
///
/// Returns `Ok(())` iff every rule applicable to `kind` passes.
/// Never performs DNS, never touches the filesystem, never logs; for an IP
/// literal it asks the system which networks the device is on.
pub fn evaluate_policy(
    url: &Url,
    policy: &NetworkPolicy,
    kind: GateKind,
) -> Result<(), GateReject> {
    // A datagram may also be broadcast or multicast; a stream or a request may not.
    let reach: &dyn Fn(std::net::IpAddr) -> Reach = match kind {
        GateKind::UdpSocket => &datagram_reach,
        GateKind::Fetch
        | GateKind::Download
        | GateKind::AudioStream
        | GateKind::FetchUpload
        | GateKind::Prefetch
        | GateKind::ImageInlineSrc
        | GateKind::HostMedia
        | GateKind::WebSocket
        | GateKind::TcpSocket => &stream_reach,
    };
    evaluate(url, policy, kind, reach)
}

/// [`evaluate_policy`] with `reach` saying where an address is: from the device's real networks in production,
/// given ones in a test.
fn evaluate(
    url: &Url,
    policy: &NetworkPolicy,
    kind: GateKind,
    reach: &dyn Fn(std::net::IpAddr) -> Reach,
) -> Result<(), GateReject> {
    let channel = kind.channel();

    // 1. Scheme whitelist.
    let scheme = url.scheme();
    if !scheme_allowed(kind, scheme) {
        return Err(GateReject::UnsupportedScheme {
            scheme: scheme.to_string(),
        });
    }

    // 2. Host must exist (the client would refuse it later; fail fast).
    let host = url.host_str().ok_or(GateReject::MissingHost)?;

    // 3. An address rather than a name. hyper-util routes IP literals around
    //    the custom DNS resolver, so the resolver's address rule never sees
    //    `http://127.0.0.1` -- it is decided here. No allowlist can name an
    //    address, so the only literal that passes is another device on the local
    //    network, on a channel the local network is granted to.
    let port = url.port_or_known_default().unwrap_or(match scheme {
        "https" | "wss" => 443,
        _ => 80,
    });
    if let Ok(sock_addr) = format!("{host}:{port}").parse::<std::net::SocketAddr>() {
        let display = sock_addr.ip().to_string();
        match reach(sock_addr.ip()) {
            Reach::Internet => return Err(GateReject::IpLiteral { display }),
            Reach::LocalNetwork if lan_access(policy, channel) == LanAccess::LocalNetwork => {
                // A local-network peer: there is no domain to hold to the allowlist.
            }
            Reach::LocalNetwork | Reach::Never => {
                return Err(GateReject::BlockedAddress { display });
            }
        }
    } else if !policy.channel(channel).permits(host) {
        // 4. The channel's allowlist.
        return Err(GateReject::NotAllowed {
            host: host.to_string(),
            channel,
        });
    }

    // 5. Plain-text transport. Raw TCP/UDP have no TLS analogue and are unaffected.
    let plain = match kind {
        GateKind::Fetch
        | GateKind::Download
        | GateKind::AudioStream
        | GateKind::FetchUpload
        | GateKind::Prefetch
        | GateKind::ImageInlineSrc
        | GateKind::HostMedia => scheme == "http",
        GateKind::WebSocket => scheme == "ws",
        GateKind::TcpSocket | GateKind::UdpSocket => false,
    };
    if plain && !policy.allow_plain_http {
        return Err(GateReject::PlainTextNotAllowed {
            scheme: scheme.to_string(),
        });
    }

    Ok(())
}

/// Enforce the policy for a raw socket's host and port, and answer the host as
/// the gate read it. The socket must resolve that one, not what content wrote:
/// what was checked is then what is connected to.
///
/// A socket has no URL, so one is synthesized (`tcp://host:port/`) and the
/// URL rules run unchanged: scheme whitelist, IP-literal rules, the channel's
/// allowlist.
pub fn enforce_host(
    host: &str,
    port: u16,
    policy: &NetworkPolicy,
    transport: Transport,
) -> Result<String, String> {
    let (kind, scheme) = (transport.kind(), transport.scheme());
    let bracketed = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    // Read as an `http://` host would be, not as the opaque host `tcp://` gets.
    // getaddrinfo connects `134744072`, `0x08080808` and `010.010.010.010` to
    // 8.8.8.8, and this parser reads them the same way, so a numeric spelling
    // meets the IP-literal rule the dotted one does. It also lowercases and
    // IDNA-encodes a name, as every `http://` host already is.
    let canonical = url::Host::parse(&bracketed)
        .map_err(|e| format!("{}:fail invalid host '{}': {}", kind.label(), host, e))?
        .to_string();
    let synthetic = format!("{scheme}://{canonical}:{port}/");
    let url = Url::parse(&synthetic)
        .map_err(|e| format!("{}:fail invalid host '{}': {}", kind.label(), host, e))?;
    enforce(&url, policy, kind)?;
    Ok(canonical)
}

/// Evaluate the policy, count the refusal, and answer the message content is
/// shown. The one entry point both executions call.
pub fn enforce(url: &Url, policy: &NetworkPolicy, kind: GateKind) -> Result<(), String> {
    match evaluate_policy(url, policy, kind) {
        Ok(()) => Ok(()),
        Err(reject) => {
            // Observability: per-kind reject counters are useful for spotting
            // games that silently violate the policy after an app update.
            use std::sync::atomic::Ordering;
            let metrics = shared::stats::io_metrics_global();
            match kind {
                GateKind::ImageInlineSrc => {
                    metrics
                        .inline_image_policy_rejects
                        .fetch_add(1, Ordering::Relaxed);
                }
                GateKind::WebSocket => {
                    metrics.ws_policy_rejects.fetch_add(1, Ordering::Relaxed);
                }
                // Fetch/download/upload/prefetch rejects already surface
                // through their error paths; keep them out of the binary
                // telemetry tail to avoid double counting with the existing
                // HTTP error metrics. A media URL's refusal fails the call that
                // named it the same way.
                GateKind::Fetch
                | GateKind::Download
                | GateKind::FetchUpload
                | GateKind::Prefetch
                | GateKind::AudioStream
                | GateKind::HostMedia => {}
                GateKind::TcpSocket | GateKind::UdpSocket => {
                    // Fold into the ws bucket for now: both are raw transport
                    // rejects and the binary snapshot protocol has no
                    // dedicated slot yet. The per-kind prefix in the error
                    // message keeps incident triage unambiguous.
                    metrics.ws_policy_rejects.fetch_add(1, Ordering::Relaxed);
                }
            }
            Err(reject.to_message(kind))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::device::local_address::LocalNetwork;
    use shared::network_policy::{DomainPattern, DomainPolicy};

    const ALL_KINDS: [GateKind; 10] = [
        GateKind::Fetch,
        GateKind::Download,
        GateKind::AudioStream,
        GateKind::FetchUpload,
        GateKind::Prefetch,
        GateKind::ImageInlineSrc,
        GateKind::HostMedia,
        GateKind::WebSocket,
        GateKind::TcpSocket,
        GateKind::UdpSocket,
    ];

    fn u(s: &str) -> Url {
        Url::parse(s).expect("test url parse")
    }

    fn allow(patterns: &[&str]) -> DomainPolicy {
        DomainPolicy::Allowlist(
            patterns
                .iter()
                .map(|p| DomainPattern::parse(p).unwrap())
                .collect(),
        )
    }

    /// One channel granted `patterns`, the rest refused.
    fn only(channel: NetworkChannel, patterns: &[&str]) -> NetworkPolicy {
        let mut policy = NetworkPolicy::default();
        let slot = match channel {
            NetworkChannel::Request => &mut policy.request,
            NetworkChannel::Download => &mut policy.download,
            NetworkChannel::Upload => &mut policy.upload,
            NetworkChannel::Socket => &mut policy.socket,
            NetworkChannel::Tcp => &mut policy.tcp,
            NetworkChannel::Udp => &mut policy.udp,
        };
        *slot = allow(patterns);
        policy
    }

    /// A URL `kind` accepts the scheme of, at `host`.
    fn url_for(kind: GateKind, host: &str) -> Url {
        let scheme = match kind {
            GateKind::WebSocket => "wss",
            GateKind::TcpSocket => "tcp",
            GateKind::UdpSocket => "udp",
            _ => "https",
        };
        u(&format!("{scheme}://{host}:443/"))
    }

    /// The device as if it were on 10.0.2.15/24, so what LAN assertions say does not depend on the network the
    /// machine running them is on.
    fn on_lan(url: &Url, policy: &NetworkPolicy, kind: GateKind) -> Result<(), GateReject> {
        let lan = [LocalNetwork {
            address: "10.0.2.15".parse().unwrap(),
            prefix: 24,
        }];
        let datagram = kind == GateKind::UdpSocket;
        let reach = |ip| super::super::address_filter::reach_on(ip, datagram, &lan);
        evaluate(url, policy, kind, &reach)
    }

    /// A literal is judged by where it is from the device: the device itself is refused, another device on its
    /// network passes only with the local network granted, and a server on the internet is named by domain -- an
    /// IPv6 neighbour's global address included.
    #[test]
    fn a_literal_is_refused_or_passed_by_where_it_is_from_the_device() {
        let networks = [LocalNetwork {
            address: "2001:470:1:2::15".parse().unwrap(),
            prefix: 64,
        }];
        let reach = |ip| super::super::address_filter::reach_on(ip, false, &networks);
        let mut lan = NetworkPolicy::unrestricted();
        lan.lan = true;
        let check = |url: &str, policy: &NetworkPolicy| {
            evaluate(&u(url), policy, GateKind::TcpSocket, &reach)
        };
        assert_eq!(check("tcp://[2001:470:1:2::99]:9000/", &lan), Ok(()));
        assert!(matches!(
            check(
                "tcp://[2001:470:1:2::99]:9000/",
                &NetworkPolicy::unrestricted()
            ),
            Err(GateReject::BlockedAddress { .. })
        ));
        assert!(matches!(
            check("tcp://[2001:470:1:2::15]:9000/", &lan),
            Err(GateReject::BlockedAddress { .. })
        ));
        assert!(matches!(
            check("tcp://[2001:470:1:3::1]:9000/", &lan),
            Err(GateReject::IpLiteral { .. })
        ));
    }

    // ---- the default refuses ----

    #[test]
    fn with_no_policy_set_every_kind_is_refused_and_the_message_names_channel_and_domain() {
        let policy = NetworkPolicy::default();
        for kind in ALL_KINDS {
            let url = url_for(kind, "api.example.com");
            let reject = evaluate_policy(&url, &policy, kind).unwrap_err();
            assert_eq!(
                reject,
                GateReject::NotAllowed {
                    host: "api.example.com".to_string(),
                    channel: kind.channel()
                },
                "{kind:?}"
            );
            let message = enforce(&url, &policy, kind).unwrap_err();
            assert!(
                message.contains(kind.channel().name()) && message.contains("api.example.com"),
                "{message}"
            );
        }
    }

    // ---- channels ----

    #[test]
    fn each_kind_is_decided_by_its_own_channel_and_no_other() {
        for kind in ALL_KINDS {
            for channel in NetworkChannel::ALL {
                let granted = evaluate_policy(
                    &url_for(kind, "api.example.com"),
                    &only(channel, &["api.example.com"]),
                    kind,
                );
                assert_eq!(
                    granted.is_ok(),
                    channel == kind.channel(),
                    "{kind:?} under {channel:?}: {granted:?}"
                );
            }
        }
    }

    #[test]
    fn a_download_grant_does_not_open_fetch_and_a_request_grant_does_not_open_websocket() {
        let cdn = only(NetworkChannel::Download, &["cdn.example.com"]);
        assert!(
            evaluate_policy(
                &u("https://cdn.example.com/a.png"),
                &cdn,
                GateKind::Download
            )
            .is_ok()
        );
        assert!(matches!(
            evaluate_policy(&u("https://cdn.example.com/a.png"), &cdn, GateKind::Fetch),
            Err(GateReject::NotAllowed {
                channel: NetworkChannel::Request,
                ..
            })
        ));
        let api = only(NetworkChannel::Request, &["api.example.com"]);
        assert!(matches!(
            evaluate_policy(&u("wss://api.example.com/"), &api, GateKind::WebSocket),
            Err(GateReject::NotAllowed {
                channel: NetworkChannel::Socket,
                ..
            })
        ));
    }

    #[test]
    fn the_channel_mapping_is_the_documented_table() {
        use NetworkChannel::*;
        let table = [
            (GateKind::Fetch, Request),
            (GateKind::Download, Download),
            (GateKind::AudioStream, Download),
            (GateKind::Prefetch, Download),
            (GateKind::ImageInlineSrc, Download),
            (GateKind::HostMedia, Download),
            (GateKind::FetchUpload, Upload),
            (GateKind::WebSocket, Socket),
            (GateKind::TcpSocket, Tcp),
            (GateKind::UdpSocket, Udp),
        ];
        assert_eq!(table.len(), ALL_KINDS.len());
        for (kind, channel) in table {
            assert_eq!(kind.channel(), channel, "{kind:?}");
        }
    }

    // ---- unrestricted ----

    #[test]
    fn unrestricted_is_any_domain_and_still_no_address_and_no_plain_text() {
        let open = NetworkPolicy::unrestricted();
        for kind in ALL_KINDS {
            assert_eq!(
                evaluate_policy(&url_for(kind, "anything.example.org"), &open, kind),
                Ok(()),
                "{kind:?}"
            );
            assert!(
                matches!(
                    evaluate_policy(&url_for(kind, "93.184.215.14"), &open, kind),
                    Err(GateReject::IpLiteral { .. })
                ),
                "{kind:?}"
            );
        }
        assert_eq!(
            evaluate_policy(&u("http://a.example.org/"), &open, GateKind::Fetch),
            Err(GateReject::PlainTextNotAllowed {
                scheme: "http".to_string()
            })
        );
    }

    // ---- plain text ----

    #[test]
    fn plain_http_and_ws_are_refused_until_allowed() {
        let mut policy = NetworkPolicy::unrestricted();
        for (url, kind) in [
            ("http://a.example.com/", GateKind::Fetch),
            ("http://a.example.com/", GateKind::Download),
            ("http://a.example.com/", GateKind::FetchUpload),
            ("http://a.example.com/", GateKind::AudioStream),
            ("http://a.example.com/", GateKind::ImageInlineSrc),
            ("http://a.example.com/", GateKind::Prefetch),
            ("ws://a.example.com/", GateKind::WebSocket),
        ] {
            assert!(
                matches!(
                    evaluate_policy(&u(url), &policy, kind),
                    Err(GateReject::PlainTextNotAllowed { .. })
                ),
                "{kind:?}"
            );
        }
        policy.allow_plain_http = true;
        assert_eq!(
            evaluate_policy(&u("http://a.example.com/"), &policy, GateKind::Fetch),
            Ok(())
        );
        assert_eq!(
            evaluate_policy(&u("ws://a.example.com/"), &policy, GateKind::WebSocket),
            Ok(())
        );
    }

    #[test]
    fn plain_ws_needs_both_the_domain_and_the_flag() {
        let mut policy = only(NetworkChannel::Socket, &["game.example.com"]);
        policy.allow_plain_http = true;
        assert!(matches!(
            evaluate_policy(&u("ws://other.example.com/"), &policy, GateKind::WebSocket),
            Err(GateReject::NotAllowed { .. })
        ));
        assert_eq!(
            evaluate_policy(&u("ws://game.example.com/"), &policy, GateKind::WebSocket),
            Ok(())
        );
        policy.allow_plain_http = false;
        assert!(matches!(
            evaluate_policy(&u("ws://game.example.com/"), &policy, GateKind::WebSocket),
            Err(GateReject::PlainTextNotAllowed { .. })
        ));
    }

    // ---- addresses ----

    #[test]
    fn a_public_ip_literal_is_refused_on_every_channel() {
        let open = NetworkPolicy::unrestricted();
        for kind in ALL_KINDS {
            assert_eq!(
                evaluate_policy(&url_for(kind, "8.8.8.8"), &open, kind),
                Err(GateReject::IpLiteral {
                    display: "8.8.8.8".to_string()
                }),
                "{kind:?}"
            );
        }
        assert!(matches!(
            evaluate_policy(&u("https://[2606:4700::1111]/"), &open, GateKind::Fetch),
            Err(GateReject::IpLiteral { .. })
        ));
    }

    #[test]
    fn the_local_network_is_refused_by_default_and_never_reaches_http() {
        let mut policy = NetworkPolicy::unrestricted();
        for kind in ALL_KINDS {
            assert!(
                matches!(
                    on_lan(&url_for(kind, "10.0.2.2"), &policy, kind),
                    Err(GateReject::BlockedAddress { .. })
                ),
                "{kind:?} with lan off"
            );
        }
        policy.lan = true;
        for kind in ALL_KINDS {
            let verdict = on_lan(&url_for(kind, "10.0.2.2"), &policy, kind);
            let lan_channel = matches!(
                kind.channel(),
                NetworkChannel::Socket | NetworkChannel::Tcp | NetworkChannel::Udp
            );
            if lan_channel {
                assert_eq!(verdict, Ok(()), "{kind:?} with lan on");
            } else {
                assert!(
                    matches!(verdict, Err(GateReject::BlockedAddress { .. })),
                    "{kind:?} with lan on: {verdict:?}"
                );
            }
        }
        // Another network's private address, and the device itself, stay out of reach.
        assert!(on_lan(&u("tcp://10.0.3.2:80/"), &policy, GateKind::TcpSocket).is_err());
        assert!(on_lan(&u("tcp://10.0.2.15:80/"), &policy, GateKind::TcpSocket).is_err());
    }

    #[test]
    fn a_lan_peer_needs_no_allowlist_entry_but_a_lan_grant_opens_no_domain() {
        let mut policy = NetworkPolicy::default();
        policy.lan = true;
        assert_eq!(
            on_lan(&u("tcp://10.0.2.2:9000/"), &policy, GateKind::TcpSocket),
            Ok(())
        );
        assert!(matches!(
            on_lan(
                &u("tcp://peer.example.com:9000/"),
                &policy,
                GateKind::TcpSocket
            ),
            Err(GateReject::NotAllowed { .. })
        ));
    }

    #[test]
    fn reserved_addresses_are_refused_whatever_the_policy() {
        let mut policy = NetworkPolicy::unrestricted();
        policy.lan = true;
        policy.allow_plain_http = true;
        for url in [
            "http://127.0.0.1/secret",
            "http://169.254.169.254/latest/meta-data/",
            "http://[::1]:8080/",
        ] {
            assert!(
                matches!(
                    evaluate_policy(&u(url), &policy, GateKind::Fetch),
                    Err(GateReject::BlockedAddress { .. })
                ),
                "{url}"
            );
        }
        assert!(matches!(
            evaluate_policy(&u("wss://[::1]:8443/"), &policy, GateKind::WebSocket),
            Err(GateReject::BlockedAddress { .. })
        ));
    }

    #[test]
    fn a_datagram_may_broadcast_only_with_the_local_network() {
        let mut policy = NetworkPolicy::default();
        let broadcast = u("udp://255.255.255.255:9/");
        assert!(matches!(
            evaluate_policy(&broadcast, &policy, GateKind::UdpSocket),
            Err(GateReject::BlockedAddress { .. })
        ));
        policy.lan = true;
        assert_eq!(
            evaluate_policy(&broadcast, &policy, GateKind::UdpSocket),
            Ok(())
        );
        assert!(matches!(
            evaluate_policy(&u("tcp://255.255.255.255:9/"), &policy, GateKind::TcpSocket),
            Err(GateReject::BlockedAddress { .. })
        ));
    }

    // ---- patterns ----

    #[test]
    fn exact_and_wildcard_patterns_hold_on_the_gate() {
        let policy = only(NetworkChannel::Request, &["mygame.com", "*.cdn.mygame.com"]);
        let check = |url: &str| evaluate_policy(&u(url), &policy, GateKind::Fetch);
        assert_eq!(check("https://mygame.com/"), Ok(()));
        assert_eq!(check("https://a.cdn.mygame.com/"), Ok(()));
        for refused in [
            "https://www.mygame.com/",
            "https://cdn.mygame.com/",
            "https://evilmygame.com/",
        ] {
            assert!(
                matches!(check(refused), Err(GateReject::NotAllowed { .. })),
                "{refused}"
            );
        }
    }

    #[test]
    fn a_raw_socket_host_matches_regardless_of_case() {
        // `tcp://` is not a special scheme, so its host would keep the case content wrote.
        let policy = only(NetworkChannel::Tcp, &["game.example.com"]);
        assert_eq!(
            enforce_host("GAME.Example.com", 9000, &policy, Transport::Tcp),
            Ok("game.example.com".to_string())
        );
        assert!(enforce_host("other.example.com", 9000, &policy, Transport::Tcp).is_err());
    }

    /// getaddrinfo reads a decimal, hex or octal IPv4 the way inet_aton does, and
    /// `tcp://` keeps such a host opaque, so these reached the allowlist as names
    /// and an unrestricted channel let them through to 8.8.8.8.
    #[test]
    fn a_numeric_ipv4_spelling_is_an_ip_literal_on_a_raw_socket() {
        let open = NetworkPolicy::unrestricted();
        for transport in [Transport::Tcp, Transport::Udp] {
            for spelling in ["134744072", "0x08080808", "010.010.010.010", "8.8.2056"] {
                let refusal = enforce_host(spelling, 9000, &open, transport).unwrap_err();
                assert!(
                    refusal.contains("8.8.8.8"),
                    "{spelling} as {transport:?}: {refusal}"
                );
            }
            let loopback = enforce_host("2130706433", 9000, &open, transport).unwrap_err();
            assert!(loopback.contains("127.0.0.1"), "{transport:?}: {loopback}");
        }
    }

    // ---- schemes ----

    #[test]
    fn each_kind_accepts_only_its_schemes() {
        let open = {
            let mut policy = NetworkPolicy::unrestricted();
            policy.allow_plain_http = true;
            policy
        };
        let rejects = [
            ("ws://example.com/", GateKind::Fetch),
            ("https://example.com/", GateKind::WebSocket),
            ("file:///etc/passwd", GateKind::ImageInlineSrc),
            ("ftp://example.com/", GateKind::Download),
            ("udp://example.com:1/", GateKind::TcpSocket),
        ];
        for (url, kind) in rejects {
            assert!(
                matches!(
                    evaluate_policy(&u(url), &open, kind),
                    Err(GateReject::UnsupportedScheme { .. })
                ),
                "{url} as {kind:?}"
            );
        }
    }

    // ---- messages ----

    #[test]
    fn a_refusal_names_the_api_the_channel_and_the_domain() {
        let message = enforce(
            &u("https://attacker.example/1x1.png"),
            &only(NetworkChannel::Download, &["mygame.com"]),
            GateKind::ImageInlineSrc,
        )
        .unwrap_err();
        assert_eq!(
            message,
            "Image.src: domain 'attacker.example' is not in the download allowlist"
        );
        let message = enforce(
            &u("ws://mygame.com/"),
            &only(NetworkChannel::Socket, &["mygame.com"]),
            GateKind::WebSocket,
        )
        .unwrap_err();
        assert_eq!(
            message,
            "WebSocket: plain-text ws:// is not allowed (allowPlainHttp is off)"
        );
    }
}
