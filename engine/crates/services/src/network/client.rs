//! The HTTP client every outbound request in this engine is made with.
//!
//! One constructor, because the parts that make a request safe are the parts a
//! second one would forget: a resolver that refuses private and loopback
//! addresses, a redirect gate that runs on every hop, and the connection
//! pooling each protocol version wants. `fetch`, `uploadFile`, a streamed
//! `InnerAudioContext.src` and an `http(s)` image source all come through
//! here, in either execution -- the embedded runtime caches one per protocol
//! version in its `OpState`, the external session builds one for its audio.
//! The client itself is [`super::http`].

use shared::error::{EngineError, EngineResult, ErrorCode};
use shared::op_state::NetworkPolicy;

use super::gate::{GateKind, GateReject};
use super::http::{HttpClient, HttpClientBuilder};

/// The client a policy configured. Named here so a caller holds one without
/// knowing how it is built.
pub type PolicyHttpClient = HttpClient;

/// Build a client that carries `net_policy`: its resolver refuses blocked
/// addresses, its redirects are gated as `redirect_kind`, and `operation`
/// names the caller in both refusals.
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
    redirect_kind: GateKind,
    operation: &'static str,
) -> EngineResult<HttpClient> {
    policy_client_builder(
        user_agent,
        enable_http2,
        net_policy,
        redirect_kind,
        operation,
    )
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
    redirect_kind: GateKind,
    operation: &'static str,
) -> HttpClientBuilder {
    HttpClientBuilder::new(
        user_agent,
        enable_http2,
        net_policy,
        redirect_kind,
        operation,
    )
}

pub(crate) fn redirect_reject_message(operation: &str, reject: &GateReject) -> String {
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
