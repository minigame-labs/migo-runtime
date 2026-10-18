//! The HTTP client every outbound request in this engine is made with.
//!
//! One constructor, because the parts that make a request safe are the parts a
//! second one would forget: a resolver that refuses private and loopback
//! addresses, a redirect gate that runs on every hop, and the connection
//! pooling each protocol version wants. `fetch`, `uploadFile`, a streamed
//! `InnerAudioContext.src` and an `http(s)` image source all come through
//! here, in either execution -- the embedded runtime caches one per protocol
//! version in its `OpState`, the external session builds one for its audio.
//! A client is not tied to one API: each request names the kind it is made as.
//! The client itself is [`super::http`].

use shared::error::{EngineError, EngineResult, ErrorCode};
use shared::network_policy::NetworkPolicy;

use super::gate::{GateKind, GateReject};
use super::http::{HttpClient, HttpClientBuilder};

/// The client a policy configured. Named here so a caller holds one without
/// knowing how it is built.
pub type PolicyHttpClient = HttpClient;

/// Build a client that carries `net_policy`: its resolver refuses blocked
/// addresses, every redirect is gated as the request that followed it, and
/// `operation` names the caller in the resolver's refusals.
///
/// Proxies are the device's: the environment's proxy settings apply, as they
/// do to the system WebView this runtime stands in for. A proxy is the machine
/// owner's egress, not something content can name, so it is not held to the
/// destination rules -- and through one the engine never resolves or connects
/// to the destination at all, which is why `fetch_send` has no check on the
/// address a response came from (see the note there).
pub fn create_policy_http_client(
    user_agent: &str,
    enable_http2: bool,
    net_policy: &NetworkPolicy,
    operation: &'static str,
) -> EngineResult<HttpClient> {
    policy_client_builder(user_agent, enable_http2, net_policy, operation)
        .build()
        .map_err(|error| {
            EngineError::from_detail(
                ErrorCode::IoError,
                format!("failed to build the HTTP client: {error}"),
            )
        })
}

/// Everything `create_policy_http_client` configures, before it is built: a test
/// that needs a proxy or no proxy adds that and builds.
pub(crate) fn policy_client_builder(
    user_agent: &str,
    enable_http2: bool,
    net_policy: &NetworkPolicy,
    operation: &'static str,
) -> HttpClientBuilder {
    HttpClientBuilder::new(user_agent, enable_http2, net_policy, operation)
}

pub(crate) fn redirect_reject_message(kind: GateKind, reject: &GateReject) -> String {
    format!("{}: redirect rejected: {}", kind.label(), reject.detail())
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::network_policy::NetworkChannel;

    /// The words a refused redirect carries. Games match on them, and the
    /// operation prefix is how an incident is attributed to fetch or audio.
    #[test]
    fn a_refused_redirect_says_which_request_and_why() {
        let reject = |channel| GateReject::NotAllowed {
            host: "blocked.example".to_string(),
            channel,
        };
        assert_eq!(
            redirect_reject_message(GateKind::Fetch, &reject(NetworkChannel::Request)),
            "fetch: redirect rejected: domain 'blocked.example' is not in the request allowlist"
        );
        assert_eq!(
            redirect_reject_message(GateKind::AudioStream, &reject(NetworkChannel::Download)),
            "audio: redirect rejected: domain 'blocked.example' is not in the download allowlist"
        );
    }

    /// Building a client is not asynchronous work: the audio service builds
    /// one lazily, off any runtime, the first time a streamed source is asked
    /// for.
    #[test]
    fn a_policy_client_is_built_without_a_runtime() {
        create_policy_http_client("migo", true, &NetworkPolicy::default(), "audio")
            .expect("client construction must not require a runtime");
    }
}
