//! The HTTP client every outbound request in this engine is made with.
//!
//! One builder, because the parts that make a request safe are the parts a
//! second builder would forget: a DNS resolver that refuses private and
//! loopback addresses, a redirect policy that runs the same gate on every hop,
//! and the connection pooling each protocol version wants. `fetch`, a streamed
//! `InnerAudioContext.src` and an `http(s)` image source all come through
//! here, in either execution -- the embedded runtime caches one per protocol
//! version in its `OpState`, the external session builds one for its audio.

use std::time::Duration;

use http::HeaderMap;
use http::header::USER_AGENT;
use reqwest::Client;
use reqwest::redirect::Policy;
use shared::error::{EngineError, EngineResult, ErrorCode};
use shared::op_state::NetworkPolicy;

use super::gate::{GateKind, GateReject, evaluate_policy};

/// Custom DNS resolver that checks ALL resolved addresses against the
/// blocked-address list before returning them to reqwest.  This is injected
/// into every `reqwest::Client` via `ClientBuilder::dns_resolver()`, so
/// reqwest connects **only** to addresses we have verified — no separate
/// pre-flight check needed, no double-resolution TOCTOU window.
///
/// Note: hyper-util bypasses the resolver for IP-literal hosts, so callers
/// must also call `reject_blocked_ip_literal()` before sending requests.
struct SsrfCheckingResolver {
    operation: &'static str,
}

impl reqwest::dns::Resolve for SsrfCheckingResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let operation = self.operation;
        Box::pin(async move {
            let host = name.as_str();
            let addr_str = format!("{}:0", host);
            let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host(&addr_str)
                .await
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?
                .collect();

            for addr in &addrs {
                if super::address_filter::is_blocked_address(addr) {
                    return Err(Box::new(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        format!(
                            "{}: connection to {} is not allowed (private/loopback address)",
                            operation,
                            addr.ip()
                        ),
                    ))
                        as Box<dyn std::error::Error + Send + Sync>);
                }
            }

            let addrs: reqwest::dns::Addrs = Box::new(addrs.into_iter());
            Ok(addrs)
        })
    }
}

/// The client a policy configured. Re-exported so a caller can hold one
/// without depending on the HTTP crate itself: which client library the engine
/// makes requests with is this module's business.
pub type PolicyHttpClient = Client;

/// Build a client that carries `net_policy`: its resolver refuses blocked
/// addresses, its redirect policy re-runs the gate under `redirect_kind`, and
/// `operation` names the caller in both refusals.
pub fn create_policy_http_client(
    user_agent: &str,
    enable_http2: bool,
    net_policy: &NetworkPolicy,
    redirect_kind: GateKind,
    operation: &'static str,
) -> EngineResult<Client> {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, user_agent.parse().unwrap());

    // Capture network policy for the redirect closure.  `Policy`
    // builds a boxed `Fn` internally, so we must move a *clone* of
    // the policy into the closure rather than borrowing from
    // `net_policy`.
    let redirect_policy = net_policy.clone();

    // Custom redirect policy: runs the shared gate on every redirect
    // target so `allowed.com -> 302 -> blocked.com` and
    // `https -> 302 -> http` are both rejected.  Centralising here
    // means redirect enforcement never drifts from the initial-URL
    // enforcement.
    let ssrf_redirect_policy = Policy::custom(move |attempt| {
        if attempt.previous().len() >= 10 {
            return attempt.stop();
        }
        match evaluate_policy(attempt.url(), &redirect_policy, redirect_kind) {
            Ok(()) => attempt.follow(),
            Err(reject) => attempt.error(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                redirect_reject_message(operation, &reject),
            )),
        }
    });

    let mut builder = Client::builder()
        .dns_resolver(std::sync::Arc::new(SsrfCheckingResolver { operation }))
        .redirect(ssrf_redirect_policy)
        .default_headers(headers)
        // Connect timeout applies to TCP + TLS handshake only; per-
        // request `RequestBuilder::timeout` still bounds the full
        // exchange. Mobile networks frequently stall at connect time
        // when going through captive portals, so cap that specifically
        // rather than waiting for the OS-level SYN retry window.
        .connect_timeout(Duration::from_secs(5));

    if enable_http2 {
        // HTTP/2 multiplexes many streams over a single TCP connection, so we
        // need fewer idle connections but want to keep them alive longer.
        builder = builder
            .pool_max_idle_per_host(3)
            .pool_idle_timeout(Duration::from_secs(120))
            .http2_adaptive_window(true)
            .http2_keep_alive_interval(Duration::from_secs(30))
            .http2_keep_alive_timeout(Duration::from_secs(10));
    } else {
        // HTTP/1.1 needs more idle connections since each handles one
        // request at a time. Shorter idle timeout to free resources.
        builder = builder
            .pool_max_idle_per_host(6)
            .pool_idle_timeout(Duration::from_secs(90))
            .http1_only();
    }

    builder.build().map_err(|error| {
        EngineError::from_detail(
            ErrorCode::IoError,
            format!("failed to build the HTTP client: {error}"),
        )
    })
}

fn redirect_reject_message(operation: &str, reject: &GateReject) -> String {
    let detail = match reject {
        GateReject::BlockedAddress { display } => {
            format!("connection to {display} is not allowed (private/loopback address)")
        }
        GateReject::NotWhitelisted { host } => {
            format!("'{host}' is not in the allowed domain list")
        }
        GateReject::HttpsRequired => "HTTPS required (enforce_https=true)".to_string(),
        GateReject::UnsupportedScheme { scheme } => {
            format!("scheme '{scheme}' is not allowed")
        }
        GateReject::MissingHost => "URL has no host".to_string(),
    };
    format!("{operation}: redirect rejected: {detail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The words a refused redirect carries. Games match on them, and the
    /// operation prefix is how an incident is attributed to fetch or audio.
    #[test]
    fn a_refused_redirect_says_which_request_and_why() {
        let reject = GateReject::NotWhitelisted {
            host: "blocked.example".to_string(),
        };
        assert_eq!(
            redirect_reject_message("fetch", &reject),
            "fetch: redirect rejected: 'blocked.example' is not in the allowed domain list"
        );
        assert_eq!(
            redirect_reject_message("audio", &reject),
            "audio: redirect rejected: 'blocked.example' is not in the allowed domain list"
        );
    }

    /// Building a client is not asynchronous work: the audio service builds
    /// one lazily, off any runtime, the first time a streamed source is asked
    /// for.
    #[test]
    fn a_policy_client_is_built_without_a_runtime() {
        let policy = NetworkPolicy {
            domain_whitelist: vec!["media.example".to_string()],
            enforce_https: true,
        };
        create_policy_http_client(
            "migo",
            true,
            &policy,
            GateKind::AudioStreamRedirect,
            "audio",
        )
        .expect("client construction must not require a runtime");
    }
}
