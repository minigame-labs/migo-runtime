//! Unified network-policy gate for every outbound-capable op.
//!
//! Every op that opens a network connection or performs a URL-based
//! request MUST funnel through [`enforce`] or [`enforce_host`].
//! This closes the gap the report flagged: previously `fetch()` had
//! full preflight (IP-literal block, domain whitelist, HTTPS enforce)
//! but `Image.src = http(s)://...` and `new WebSocket(...)` were
//! routed directly to the shared reqwest/tungstenite client pool
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
//! * [`GateKind`] selects which rule subset applies — a `fetch()`
//!   and a `connectSocket(tcp)` have different scheme whitelists.
//!
//! # Why a single enum for kinds?
//!
//! An open-coded bool matrix (`check_https: bool`, `check_wl: bool`,
//! …) invites drift: a new op forgets a flag and the whitelist goes
//! silently unenforced again. With a single `GateKind` every new
//! call site must pick a kind, and `evaluate_policy` centrally maps
//! the kind → rule set. Adding a new rule is one match arm.

use std::sync::Once;

use tracing::warn;
use url::Url;

use super::address_filter::is_blocked_address;
use shared::op_state::NetworkPolicy;

/// One-shot flag: warn exactly once if the host app shipped with an
/// empty `domain_whitelist` (which is "allow all"). Matches the
/// previous per-op warn, now centralised here so it fires no matter
/// which entry point (`fetch`, `WebSocket`, `Image.src`, …) triggers
/// the first request.
static EMPTY_WHITELIST_WARNED: Once = Once::new();

/// Classifies which network entry point is being guarded.
///
/// Every outbound-capable op must map itself to exactly one variant.
/// New entry points must be added here (and handled in
/// [`evaluate_policy`]) — a missing variant is a compile error, not
/// a silent bypass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateKind {
    /// Top-level `fetch()` (HTTP/HTTPS). Full preflight.
    Fetch,
    /// HTTP redirect hop targeted by reqwest's redirect policy.
    /// Same rule set as `Fetch`: a 302 into a blocked host is just
    /// as dangerous as the initial request.
    FetchRedirect,
    /// `InnerAudioContext.src` HTTP streaming request. Uses the same HTTP
    /// security rules as fetch, but keeps diagnostics attributable to audio.
    AudioStream,
    /// Redirect hop followed by an audio streaming request.
    AudioStreamRedirect,
    /// Body upload via `fetch` / `uploadFile`. Same as `Fetch`.
    FetchUpload,
    /// DNS prefetch / asset warmup. `data:` is not applicable; the
    /// rule set is the same as `Fetch`.
    Prefetch,
    /// `Image.src = "http(s)://..."`. `data:` is parsed separately
    /// before the gate runs, so only http/https reach here.
    ImageInlineSrc,
    /// `new WebSocket(url)`. Scheme must be `ws`/`wss`. HTTPS
    /// enforcement downgrades to "wss required" when
    /// `enforce_https == true`.
    WebSocket,
    /// `createTCPSocket().connect(host, port)`. Raw TCP has no URL;
    /// enforcement runs the shared domain whitelist and
    /// IP-literal address filter against the host argument.
    TcpSocket,
    /// `createUDPSocket().send(host, port, …)` and `.connect(...)`.
    /// Same rule set as `TcpSocket`.
    UdpSocket,
}

/// Ways a gate check can fail. Kept narrow so call sites can decide
/// error vocabulary without parsing the message.
#[derive(Debug, PartialEq, Eq)]
pub enum GateReject {
    /// URL scheme is not permitted for this [`GateKind`].
    UnsupportedScheme { scheme: String },
    /// URL host resolved to (or was already) a blocked IP range.
    /// The resolver also catches DNS names; this variant is the
    /// fallback for IP literals that bypass the resolver.
    BlockedAddress { display: String },
    /// Host was not in the configured domain whitelist.
    NotWhitelisted { host: String },
    /// HTTPS was required but the URL used HTTP (or `ws://` for
    /// WebSocket kinds).
    HttpsRequired,
    /// URL has no host component at all, so no rule can be applied.
    /// Happens for malformed input that reqwest would also reject.
    MissingHost,
}

impl GateReject {
    fn to_message(&self, kind: GateKind) -> String {
        let prefix = kind_prefix(kind);
        match self {
            GateReject::UnsupportedScheme { scheme } => {
                format!("{prefix}: scheme '{scheme}' is not allowed")
            }
            GateReject::BlockedAddress { display } => {
                format!(
                    "{prefix}: connection to {display} is not allowed (private/loopback address)"
                )
            }
            GateReject::NotWhitelisted { host } => {
                format!("{prefix}: domain '{host}' is not in the allowed list")
            }
            GateReject::HttpsRequired => {
                format!("{prefix}: HTTPS is required (enforce_https=true)")
            }
            GateReject::MissingHost => format!("{prefix}: URL has no host"),
        }
    }
}

#[inline]
fn kind_prefix(kind: GateKind) -> &'static str {
    match kind {
        GateKind::Fetch => "fetch",
        GateKind::FetchRedirect => "fetch:redirect",
        GateKind::AudioStream => "audio",
        GateKind::AudioStreamRedirect => "audio:redirect",
        GateKind::FetchUpload => "uploadFile",
        GateKind::Prefetch => "prefetch",
        GateKind::ImageInlineSrc => "Image.src",
        GateKind::WebSocket => "WebSocket",
        GateKind::TcpSocket => "TCPSocket",
        GateKind::UdpSocket => "UDPSocket",
    }
}

/// Scheme policy matrix.
///
/// The explicit table makes it a 1-line diff to add new schemes or
/// kinds. `true` means the scheme is permitted **for that kind** —
/// additional runtime rules (whitelist, HTTPS enforcement) still
/// apply on top.
fn scheme_allowed(kind: GateKind, scheme: &str) -> bool {
    match (kind, scheme) {
        // HTTP entry points accept both; `check_https_required`
        // below rejects `http://` when `enforce_https` is set.
        (
            GateKind::Fetch
            | GateKind::FetchRedirect
            | GateKind::AudioStream
            | GateKind::AudioStreamRedirect
            | GateKind::FetchUpload
            | GateKind::Prefetch
            | GateKind::ImageInlineSrc,
            "http" | "https",
        ) => true,
        (GateKind::WebSocket, "ws" | "wss") => true,
        // Raw TCP/UDP use the synthetic `tcp://` / `udp://` scheme
        // produced by [`enforce_host_from_state`]; no other kind is
        // allowed to feed those schemes in.
        (GateKind::TcpSocket, "tcp") => true,
        (GateKind::UdpSocket, "udp") => true,
        _ => false,
    }
}

/// True if `host` is permitted by the domain whitelist. An empty
/// whitelist means "allow all" (dev / first-boot behaviour). A non-empty
/// list matches an exact host or any subdomain of a listed domain
/// (`cdn.mygame.com` is allowed by `mygame.com`, but `evilmygame.com`
/// is not). Shared by [`evaluate_policy`] and the DNS-prefetch op so
/// prefetch can't warm the resolver for off-whitelist hosts.
pub fn is_host_whitelisted(host: &str, policy: &NetworkPolicy) -> bool {
    if policy.domain_whitelist.is_empty() {
        return true;
    }
    policy.domain_whitelist.iter().any(|allowed| {
        // Suffix-strip rather than build `format!(".{allowed}")`: this runs once
        // per whitelist entry per request, per redirect hop and per audio stream
        // URL, and the allocation existed only to hold a leading dot. Requiring
        // the remainder to end in `.` is what keeps `evilmygame.com` out of
        // `mygame.com` -- the same rule, without the heap.
        host == allowed.as_str()
            || host
                .strip_suffix(allowed.as_str())
                .is_some_and(|prefix| prefix.ends_with('.'))
    })
}

/// Pure, side-effect-free policy evaluator.
///
/// Returns `Ok(())` iff every rule applicable to `kind` passes.
/// Never performs DNS, never touches the filesystem, never logs.
///
/// Ordering is deliberate: cheapest / most-specific checks first so
/// we reject obviously bad URLs before looking up the whitelist.
pub fn evaluate_policy(
    url: &Url,
    policy: &NetworkPolicy,
    kind: GateKind,
) -> Result<(), GateReject> {
    // 1. Scheme whitelist.
    let scheme = url.scheme();
    if !scheme_allowed(kind, scheme) {
        return Err(GateReject::UnsupportedScheme {
            scheme: scheme.to_string(),
        });
    }

    // 2. Host must exist (reqwest would reject later, but fail fast).
    let host = url.host_str().ok_or(GateReject::MissingHost)?;

    // 3. IP-literal block. hyper-util routes IP literals around the
    //    custom DNS resolver, so the resolver's
    //    `is_blocked_address` gate never fires for `http://127.0.0.1`.
    //    We have to catch it before the connect happens.
    let port = url.port_or_known_default().unwrap_or(match scheme {
        "https" | "wss" => 443,
        _ => 80,
    });
    if let Ok(sock_addr) = format!("{host}:{port}").parse::<std::net::SocketAddr>() {
        if is_blocked_address(&sock_addr) {
            return Err(GateReject::BlockedAddress {
                display: sock_addr.ip().to_string(),
            });
        }
    }

    // 4. Domain whitelist. Empty whitelist means "allow all" (dev /
    //    first-boot behaviour — a dedicated warning path is the
    //    caller's responsibility and lives in `enforce_from_state`).
    if !is_host_whitelisted(host, policy) {
        return Err(GateReject::NotWhitelisted {
            host: host.to_string(),
        });
    }

    // 5. HTTPS enforcement. For HTTP kinds, reject `http://`.
    //    For WebSocket, reject `ws://` (the TLS-free form) when
    //    `enforce_https` is on — same security meaning. Raw TCP/UDP
    //    are unaffected: the "enforce_https" knob is about transport
    //    security for HTTP-flavoured traffic, and raw sockets have
    //    no native TLS analogue.
    if policy.enforce_https {
        let http_like = matches!(
            kind,
            GateKind::Fetch
                | GateKind::FetchRedirect
                | GateKind::AudioStream
                | GateKind::AudioStreamRedirect
                | GateKind::FetchUpload
                | GateKind::Prefetch
                | GateKind::ImageInlineSrc
        );
        let is_http = scheme == "http";
        let is_ws_plain = matches!(kind, GateKind::WebSocket) && scheme == "ws";
        if (http_like && is_http) || is_ws_plain {
            return Err(GateReject::HttpsRequired);
        }
    }

    Ok(())
}
/// Enforce the policy for a raw socket's host and port.
///
/// A socket has no URL, so one is synthesized (`tcp://host:port/`) and the
/// URL rules run unchanged: scheme whitelist, IP-literal block, domain
/// whitelist. Raw sockets therefore share exactly the allow/deny set `fetch`
/// and `WebSocket` have, closing the bypass where
/// `createTCPSocket().connect("evil.example")` ignored the domain whitelist.
pub fn enforce_host(
    host: &str,
    port: u16,
    policy: &NetworkPolicy,
    kind: GateKind,
) -> Result<(), String> {
    let scheme = match kind {
        GateKind::TcpSocket => "tcp",
        GateKind::UdpSocket => "udp",
        _ => return Err("enforce_host called with a non-raw-socket kind".to_string()),
    };
    // `Url::parse` needs a bracketed IPv6 literal; fall back to a
    // pre-bracketed form when the caller passed a literal IPv6.
    let host_for_url = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    let synthetic = format!("{scheme}://{host_for_url}:{port}/");
    let url = Url::parse(&synthetic)
        .map_err(|e| format!("{}:fail invalid host '{}': {}", kind_prefix(kind), host, e))?;
    enforce(&url, policy, kind)
}

/// Evaluate the policy, count the refusal, and answer the message content is
/// shown. The one entry point both executions call.
pub fn enforce(url: &Url, policy: &NetworkPolicy, kind: GateKind) -> Result<(), String> {
    if policy.domain_whitelist.is_empty() {
        EMPTY_WHITELIST_WARNED.call_once(|| {
            warn!(
                "network_policy.domain_whitelist is empty -- all domains are permitted. \
                 Populate the whitelist with allowed server domains to restrict \
                 outbound access."
            );
        });
    }
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
                // Fetch/upload/prefetch/redirect rejects already surface
                // through fetch error paths; keep them out of the binary
                // telemetry tail to avoid double counting with the existing
                // HTTP error metrics.
                GateKind::Fetch
                | GateKind::FetchRedirect
                | GateKind::FetchUpload
                | GateKind::Prefetch => {}
                GateKind::AudioStream | GateKind::AudioStreamRedirect => {}
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

    fn policy(whitelist: &[&str], enforce_https: bool) -> NetworkPolicy {
        NetworkPolicy {
            domain_whitelist: whitelist.iter().map(|s| s.to_string()).collect(),
            enforce_https,
        }
    }

    #[test]
    fn audio_stream_accepts_http_and_https_schemes() {
        let policy = policy(&[], false);
        assert!(
            evaluate_policy(
                &u("http://media.example/track.mp3"),
                &policy,
                GateKind::AudioStream,
            )
            .is_ok()
        );
        assert!(
            evaluate_policy(
                &u("https://media.example/track.mp3"),
                &policy,
                GateKind::AudioStream,
            )
            .is_ok()
        );
        assert!(matches!(
            evaluate_policy(
                &u("ws://media.example/track.mp3"),
                &policy,
                GateKind::AudioStream,
            ),
            Err(GateReject::UnsupportedScheme { .. })
        ));
    }

    #[test]
    fn audio_stream_enforces_allowlist_https_and_ip_literal_rules() {
        let strict_policy = policy(&["media.example"], true);
        assert!(
            evaluate_policy(
                &u("https://cdn.media.example/track.mp3"),
                &strict_policy,
                GateKind::AudioStream,
            )
            .is_ok()
        );
        assert!(matches!(
            evaluate_policy(
                &u("https://other.example/track.mp3"),
                &strict_policy,
                GateKind::AudioStream,
            ),
            Err(GateReject::NotWhitelisted { .. })
        ));
        assert!(matches!(
            evaluate_policy(
                &u("http://media.example/track.mp3"),
                &strict_policy,
                GateKind::AudioStream,
            ),
            Err(GateReject::HttpsRequired)
        ));
        assert!(matches!(
            evaluate_policy(
                &u("https://127.0.0.1/track.mp3"),
                &policy(&[], false),
                GateKind::AudioStream,
            ),
            Err(GateReject::BlockedAddress { .. })
        ));
    }

    #[test]
    fn audio_redirect_uses_the_same_policy_as_initial_audio_url() {
        let policy = policy(&["media.example"], true);
        for kind in [GateKind::AudioStream, GateKind::AudioStreamRedirect] {
            assert!(matches!(
                evaluate_policy(&u("https://blocked.example/next.mp3"), &policy, kind,),
                Err(GateReject::NotWhitelisted { .. })
            ));
            assert!(matches!(
                evaluate_policy(&u("http://media.example/next.mp3"), &policy, kind),
                Err(GateReject::HttpsRequired)
            ));
        }
    }

    fn u(s: &str) -> Url {
        Url::parse(s).expect("test url parse")
    }

    // ---- scheme whitelist ----

    #[test]
    fn fetch_rejects_ws_scheme() {
        let r = evaluate_policy(
            &u("ws://example.com/"),
            &policy(&[], false),
            GateKind::Fetch,
        );
        assert!(matches!(r, Err(GateReject::UnsupportedScheme { .. })));
    }

    #[test]
    fn websocket_rejects_http_scheme() {
        let r = evaluate_policy(
            &u("https://example.com/"),
            &policy(&[], false),
            GateKind::WebSocket,
        );
        assert!(matches!(r, Err(GateReject::UnsupportedScheme { .. })));
    }

    #[test]
    fn image_inline_src_rejects_file_scheme() {
        let r = evaluate_policy(
            &u("file:///etc/passwd"),
            &policy(&[], false),
            GateKind::ImageInlineSrc,
        );
        assert!(matches!(r, Err(GateReject::UnsupportedScheme { .. })));
    }

    #[test]
    fn fetch_accepts_http_and_https() {
        assert!(
            evaluate_policy(
                &u("http://example.com/"),
                &policy(&[], false),
                GateKind::Fetch
            )
            .is_ok()
        );
        assert!(
            evaluate_policy(
                &u("https://example.com/"),
                &policy(&[], false),
                GateKind::Fetch
            )
            .is_ok()
        );
    }

    #[test]
    fn websocket_accepts_ws_and_wss() {
        assert!(
            evaluate_policy(
                &u("ws://example.com/"),
                &policy(&[], false),
                GateKind::WebSocket
            )
            .is_ok()
        );
        assert!(
            evaluate_policy(
                &u("wss://example.com/"),
                &policy(&[], false),
                GateKind::WebSocket
            )
            .is_ok()
        );
    }

    // ---- IP-literal block ----

    #[test]
    fn blocks_http_to_loopback_ipv4_literal() {
        let r = evaluate_policy(
            &u("http://127.0.0.1/secret"),
            &policy(&[], false),
            GateKind::ImageInlineSrc,
        );
        assert!(matches!(r, Err(GateReject::BlockedAddress { .. })));
    }

    #[test]
    fn blocks_wss_to_ipv6_loopback_literal() {
        let r = evaluate_policy(
            &u("wss://[::1]:8443/"),
            &policy(&[], false),
            GateKind::WebSocket,
        );
        assert!(matches!(r, Err(GateReject::BlockedAddress { .. })));
    }

    #[test]
    fn blocks_cloud_metadata_endpoint() {
        // 169.254.169.254 is the AWS/GCP metadata service — classic SSRF target.
        let r = evaluate_policy(
            &u("http://169.254.169.254/latest/meta-data/"),
            &policy(&[], false),
            GateKind::Fetch,
        );
        assert!(matches!(r, Err(GateReject::BlockedAddress { .. })));
    }

    #[test]
    fn allows_public_ip_literal() {
        // 93.184.215.14 is example.com's published IP; not in any
        // blocked range. The rule is about *private* blocks, not
        // about blanket-rejecting IP literals.
        let r = evaluate_policy(
            &u("https://93.184.215.14/"),
            &policy(&[], false),
            GateKind::Fetch,
        );
        assert_eq!(r, Ok(()));
    }

    // ---- domain whitelist ----

    #[test]
    fn empty_whitelist_allows_any_public_domain() {
        assert!(
            evaluate_policy(
                &u("https://random-host.example.org/"),
                &policy(&[], false),
                GateKind::Fetch
            )
            .is_ok()
        );
    }

    #[test]
    fn whitelist_rejects_off_list_host() {
        let r = evaluate_policy(
            &u("https://attacker.example/"),
            &policy(&["mygame.com"], false),
            GateKind::Fetch,
        );
        assert!(matches!(r, Err(GateReject::NotWhitelisted { .. })));
    }

    #[test]
    fn whitelist_matches_exact_and_subdomain() {
        let p = policy(&["mygame.com"], false);
        assert!(evaluate_policy(&u("https://mygame.com/"), &p, GateKind::Fetch).is_ok());
        assert!(evaluate_policy(&u("https://cdn.mygame.com/a"), &p, GateKind::Fetch).is_ok());
        // Suffix-match must not accept "othermygame.com".
        let r = evaluate_policy(&u("https://evilmygame.com/"), &p, GateKind::Fetch);
        assert!(matches!(r, Err(GateReject::NotWhitelisted { .. })));
    }

    #[test]
    fn whitelist_applies_to_websocket_kind() {
        let r = evaluate_policy(
            &u("wss://attacker.example/"),
            &policy(&["mygame.com"], false),
            GateKind::WebSocket,
        );
        assert!(matches!(r, Err(GateReject::NotWhitelisted { .. })));
    }

    #[test]
    fn whitelist_applies_to_image_inline_src() {
        // Was the actual pre-gate bypass: this used to succeed.
        let r = evaluate_policy(
            &u("http://attacker.example/1x1.png"),
            &policy(&["mygame.com"], false),
            GateKind::ImageInlineSrc,
        );
        assert!(matches!(r, Err(GateReject::NotWhitelisted { .. })));
    }

    // ---- HTTPS enforcement ----

    #[test]
    fn enforce_https_rejects_plain_http() {
        let r = evaluate_policy(
            &u("http://mygame.com/"),
            &policy(&["mygame.com"], true),
            GateKind::Fetch,
        );
        assert_eq!(r, Err(GateReject::HttpsRequired));
    }

    #[test]
    fn enforce_https_rejects_plain_ws() {
        let r = evaluate_policy(
            &u("ws://mygame.com/"),
            &policy(&["mygame.com"], true),
            GateKind::WebSocket,
        );
        assert_eq!(r, Err(GateReject::HttpsRequired));
    }

    #[test]
    fn enforce_https_allows_wss_and_https() {
        let p = policy(&["mygame.com"], true);
        assert!(evaluate_policy(&u("https://mygame.com/"), &p, GateKind::Fetch).is_ok());
        assert!(evaluate_policy(&u("wss://mygame.com/"), &p, GateKind::WebSocket).is_ok());
    }

    // ---- ordering / precedence ----

    #[test]
    fn ip_literal_block_precedes_whitelist() {
        // If an attacker somehow registers "example.com" and points
        // it at 127.0.0.1, they still can't punch the loopback hole.
        let r = evaluate_policy(
            &u("http://127.0.0.1/"),
            &policy(&["127.0.0.1"], false),
            GateKind::Fetch,
        );
        assert!(matches!(r, Err(GateReject::BlockedAddress { .. })));
    }

    // ---- error-message formatting ----

    #[test]
    fn reject_messages_include_kind_prefix() {
        let r = evaluate_policy(
            &u("http://attacker.example/"),
            &policy(&["mygame.com"], false),
            GateKind::ImageInlineSrc,
        )
        .unwrap_err();
        let msg = r.to_message(GateKind::ImageInlineSrc);
        assert!(msg.starts_with("Image.src:"), "msg={msg}");
        assert!(msg.contains("attacker.example"), "msg={msg}");
    }

    #[test]
    fn websocket_reject_message_uses_ws_prefix() {
        let r = evaluate_policy(
            &u("ws://mygame.com/"),
            &policy(&["mygame.com"], true),
            GateKind::WebSocket,
        )
        .unwrap_err();
        let msg = r.to_message(GateKind::WebSocket);
        assert!(msg.starts_with("WebSocket:"), "msg={msg}");
    }
}
