//! What network a host grants content: one domain policy per channel, refused unless granted.
//!
//! Every field defaults to refusal -- an empty allowlist on every channel, no plain-text transport, no local
//! network -- so a host that configures nothing grants nothing. `DATA-HANDLING.md` promises exactly that, and before
//! this type it was false: an empty list meant "allow all", and no platform ever set one.
//!
//! The host hands the policy over as JSON (`RuntimeConfig.Builder.setNetworkPolicy` on Android serialises to it, a
//! C-ABI host writes it into `MigoSessionConfig.network_policy_json_utf8`), so the grammar below is parsed in exactly
//! one place:
//!
//! ```json
//! { "request": ["api.example.com"], "download": ["*.cdn.example.com"], "socket": "unrestricted",
//!   "allowPlainHttp": false, "lan": false }
//! ```
//!
//! A channel left out is refused; an unknown key is an error rather than ignored, because a misspelt channel would
//! otherwise read as a deliberate refusal.

use std::fmt;

use serde_json::{Map, Value};

/// The six kinds of traffic a policy grants separately, as the reference platform's domain lists do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NetworkChannel {
    /// `request` / `fetch`.
    Request,
    /// `downloadFile`, remote images, streamed audio, asset prefetch.
    Download,
    /// `uploadFile`.
    Upload,
    /// WebSocket.
    Socket,
    /// Raw TCP.
    Tcp,
    /// Raw UDP.
    Udp,
}

impl NetworkChannel {
    pub const ALL: [Self; 6] = [
        Self::Request,
        Self::Download,
        Self::Upload,
        Self::Socket,
        Self::Tcp,
        Self::Udp,
    ];

    /// The channel's key in the JSON form and its name in a refusal.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Download => "download",
            Self::Upload => "upload",
            Self::Socket => "socket",
            Self::Tcp => "tcp",
            Self::Udp => "udp",
        }
    }
}

/// A domain an allowlist names: `example.com` matches that host only; `*.example.com` matches its subdomains and not
/// `example.com` itself.
///
/// Never an IP address: every label is letters, digits and hyphens, and the last one cannot be read as a number, so
/// no pattern can match the host of a URL that names an address. Content reaches servers by name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DomainPattern {
    /// Lowercase, without the `*.`.
    domain: Box<str>,
    subdomains_only: bool,
}

impl DomainPattern {
    pub fn parse(text: &str) -> Result<Self, NetworkPolicyError> {
        let refuse = |why: &str| {
            Err(NetworkPolicyError(format!(
                "domain pattern '{text}': {why}"
            )))
        };
        let (subdomains_only, domain) = match text.strip_prefix("*.") {
            Some(rest) => (true, rest),
            None => (false, text),
        };
        if domain.is_empty() {
            return refuse("is empty");
        }
        if !domain.is_ascii() {
            return refuse("must be ASCII; write an internationalised name in its xn-- form");
        }
        let domain = domain.to_ascii_lowercase();
        let labels: Vec<&str> = domain.split('.').collect();
        if labels.len() < 2 {
            return refuse("needs at least two labels, e.g. example.com");
        }
        for label in &labels {
            if label.is_empty() || label.len() > 63 {
                return refuse("has an empty or over-long label");
            }
            if !label
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            {
                return refuse(
                    "may contain only letters, digits, hyphens and dots, with '*.' only at the start",
                );
            }
            if label.starts_with('-') || label.ends_with('-') {
                return refuse("has a label that starts or ends with a hyphen");
            }
        }
        // The URL standard reads a host whose last label is a number as an IPv4 address (`1.2.3.4`, `0x7f.1`).
        let last = labels[labels.len() - 1];
        let hex = last
            .strip_prefix("0x")
            .is_some_and(|digits| digits.bytes().all(|b| b.is_ascii_hexdigit()));
        if last.bytes().all(|b| b.is_ascii_digit()) || hex {
            return refuse("is an IP address; allowlists name domains");
        }
        Ok(Self {
            domain: domain.into_boxed_str(),
            subdomains_only,
        })
    }

    /// Whether `host` -- a URL's host, or a socket's -- is one this pattern names. ASCII case does not matter: a
    /// raw socket's host is not lowercased by URL parsing the way an `http` host is.
    pub fn matches(&self, host: &str) -> bool {
        let domain = &*self.domain;
        if !self.subdomains_only {
            return host.eq_ignore_ascii_case(domain);
        }
        let Some(split) = host.len().checked_sub(domain.len() + 1) else {
            return false;
        };
        // At least one character of subdomain before the dot.
        split > 0
            && host.is_char_boundary(split)
            && host.as_bytes()[split] == b'.'
            && host[split + 1..].eq_ignore_ascii_case(domain)
    }
}

impl fmt::Display for DomainPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.subdomains_only {
            f.write_str("*.")?;
        }
        f.write_str(&self.domain)
    }
}

/// What one channel may reach.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DomainPolicy {
    /// Only these domains. Empty -- the default -- is nothing at all.
    Allowlist(Vec<DomainPattern>),
    /// Any domain. Set only by saying so: a developer tool, or a host that has already checked the content.
    Unrestricted,
}

impl Default for DomainPolicy {
    fn default() -> Self {
        Self::Allowlist(Vec::new())
    }
}

impl DomainPolicy {
    /// No domain at all: the default, and what a channel left out of the JSON gets.
    pub fn grants_nothing(&self) -> bool {
        matches!(self, Self::Allowlist(patterns) if patterns.is_empty())
    }

    pub fn permits(&self, host: &str) -> bool {
        match self {
            Self::Unrestricted => true,
            Self::Allowlist(patterns) => patterns.iter().any(|pattern| pattern.matches(host)),
        }
    }
}

/// Certificate trust is a host decision, fixed before the first connection.
/// A verification failure never switches modes or disables hostname checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TlsTrust {
    Platform,
    Mozilla,
}

impl Default for TlsTrust {
    fn default() -> Self {
        // OHOS has no supported platform-verifier integration. Android native
        // C ABI hosts without a JVM explicitly choose Mozilla in their policy.
        if cfg!(target_env = "ohos") {
            Self::Mozilla
        } else {
            Self::Platform
        }
    }
}

/// The network a session's content may reach. Immutable for the session's life.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetworkPolicy {
    pub request: DomainPolicy,
    pub download: DomainPolicy,
    pub upload: DomainPolicy,
    pub socket: DomainPolicy,
    pub tcp: DomainPolicy,
    pub udp: DomainPolicy,
    /// `http://` and `ws://`. Off by default; raw TCP and UDP have no TLS analogue and are not affected.
    pub allow_plain_http: bool,
    /// The other devices on the device's own networks, whatever their addresses' range, for the socket, TCP
    /// and UDP channels only -- a request, download or upload never reaches one. Off by default.
    pub lan: bool,
    pub tls_trust: TlsTrust,
}

impl NetworkPolicy {
    /// Every channel unrestricted; plain-text transport and the local network still off.
    pub fn unrestricted() -> Self {
        Self {
            request: DomainPolicy::Unrestricted,
            download: DomainPolicy::Unrestricted,
            upload: DomainPolicy::Unrestricted,
            socket: DomainPolicy::Unrestricted,
            tcp: DomainPolicy::Unrestricted,
            udp: DomainPolicy::Unrestricted,
            allow_plain_http: false,
            lan: false,
            tls_trust: TlsTrust::default(),
        }
    }

    pub fn channel(&self, channel: NetworkChannel) -> &DomainPolicy {
        match channel {
            NetworkChannel::Request => &self.request,
            NetworkChannel::Download => &self.download,
            NetworkChannel::Upload => &self.upload,
            NetworkChannel::Socket => &self.socket,
            NetworkChannel::Tcp => &self.tcp,
            NetworkChannel::Udp => &self.udp,
        }
    }

    fn channel_mut(&mut self, channel: NetworkChannel) -> &mut DomainPolicy {
        match channel {
            NetworkChannel::Request => &mut self.request,
            NetworkChannel::Download => &mut self.download,
            NetworkChannel::Upload => &mut self.upload,
            NetworkChannel::Socket => &mut self.socket,
            NetworkChannel::Tcp => &mut self.tcp,
            NetworkChannel::Udp => &mut self.udp,
        }
    }

    /// Whether any channel permits `host`: the question a DNS pre-resolve asks, since it connects to nothing.
    pub fn permits_on_any_channel(&self, host: &str) -> bool {
        NetworkChannel::ALL
            .iter()
            .any(|&channel| self.channel(channel).permits(host))
    }

    /// Parse the host's JSON form (see the module documentation).
    pub fn from_json(text: &str) -> Result<Self, NetworkPolicyError> {
        let value: Value = serde_json::from_str(text)
            .map_err(|error| NetworkPolicyError(format!("not valid JSON: {error}")))?;
        let Value::Object(fields) = value else {
            return Err(NetworkPolicyError("must be a JSON object".to_string()));
        };
        Self::from_fields(fields)
    }

    fn from_fields(fields: Map<String, Value>) -> Result<Self, NetworkPolicyError> {
        let mut policy = Self::default();
        for (key, value) in fields {
            if key == "tlsTrust" {
                policy.tls_trust = match value.as_str() {
                    Some("platform") => TlsTrust::Platform,
                    Some("mozilla") => TlsTrust::Mozilla,
                    _ => {
                        return Err(NetworkPolicyError(
                            "'tlsTrust' must be \"platform\" or \"mozilla\"".into(),
                        ));
                    }
                };
                continue;
            }
            if let Some(channel) = NetworkChannel::ALL.into_iter().find(|c| c.name() == key) {
                *policy.channel_mut(channel) = parse_channel(channel, value)?;
                continue;
            }
            let flag = match key.as_str() {
                "allowPlainHttp" => &mut policy.allow_plain_http,
                "lan" => &mut policy.lan,
                _ => {
                    let channels: Vec<&str> =
                        NetworkChannel::ALL.iter().map(|c| c.name()).collect();
                    return Err(NetworkPolicyError(format!(
                        "unknown key '{key}'; the keys are {}, allowPlainHttp, lan and tlsTrust",
                        channels.join(", ")
                    )));
                }
            };
            *flag = value
                .as_bool()
                .ok_or_else(|| NetworkPolicyError(format!("'{key}' must be true or false")))?;
        }
        Ok(policy)
    }
}

fn parse_channel(
    channel: NetworkChannel,
    value: Value,
) -> Result<DomainPolicy, NetworkPolicyError> {
    let name = channel.name();
    match value {
        Value::String(word) if word == "unrestricted" => Ok(DomainPolicy::Unrestricted),
        Value::Array(entries) => entries
            .into_iter()
            .map(|entry| match entry {
                Value::String(text) => DomainPattern::parse(&text),
                other => Err(NetworkPolicyError(format!(
                    "'{name}' lists {other}, which is not a string"
                ))),
            })
            .collect::<Result<Vec<_>, _>>()
            .map(DomainPolicy::Allowlist),
        other => Err(NetworkPolicyError(format!(
            "'{name}' must be an array of domains or \"unrestricted\", not {other}"
        ))),
    }
}

/// A policy the host wrote that cannot be the one it meant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkPolicyError(pub String);

impl fmt::Display for NetworkPolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "network policy {}", self.0)
    }
}

impl std::error::Error for NetworkPolicyError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(text: &str) -> DomainPattern {
        DomainPattern::parse(text).unwrap_or_else(|error| panic!("{text}: {error}"))
    }

    #[test]
    fn the_default_grants_nothing() {
        let policy = NetworkPolicy::default();
        for channel in NetworkChannel::ALL {
            assert!(
                !policy.channel(channel).permits("example.com"),
                "{channel:?}"
            );
        }
        assert!(!policy.allow_plain_http);
        assert!(!policy.lan);
        assert_eq!(NetworkPolicy::from_json("{}"), Ok(NetworkPolicy::default()));
    }

    #[test]
    fn tls_trust_is_explicit_and_never_an_insecure_mode() {
        assert_eq!(
            NetworkPolicy::from_json(r#"{"tlsTrust":"mozilla"}"#)
                .unwrap()
                .tls_trust,
            TlsTrust::Mozilla
        );
        assert_eq!(
            NetworkPolicy::from_json(r#"{"tlsTrust":"platform"}"#)
                .unwrap()
                .tls_trust,
            TlsTrust::Platform
        );
        for value in [r#""insecure""#, r#""Platform""#, "true", "null", "[]"] {
            assert!(
                NetworkPolicy::from_json(&format!(r#"{{"tlsTrust":{value}}}"#))
                    .unwrap_err()
                    .0
                    .contains("tlsTrust")
            );
        }
    }

    #[test]
    fn an_exact_pattern_matches_only_that_host() {
        let exact = pattern("api.example.com");
        assert!(exact.matches("api.example.com"));
        assert!(exact.matches("API.Example.COM"));
        assert!(!exact.matches("cdn.api.example.com"));
        assert!(!exact.matches("example.com"));
        assert!(!exact.matches("evilapi.example.com"));
    }

    #[test]
    fn a_wildcard_matches_subdomains_and_not_the_domain_itself() {
        let wildcard = pattern("*.example.com");
        assert!(wildcard.matches("cdn.example.com"));
        assert!(wildcard.matches("a.b.example.com"));
        assert!(!wildcard.matches("example.com"));
        assert!(!wildcard.matches(".example.com"));
        assert!(!wildcard.matches("evilexample.com"));
        assert!(!wildcard.matches("example.com.evil.net"));
    }

    #[test]
    fn a_pattern_is_never_an_address() {
        for text in [
            "1.2.3.4",
            "*.1.2.3.4",
            "127.1",
            "0x7f.0.0.1",
            "[::1]",
            "::1",
            "10.0.0.1",
        ] {
            assert!(DomainPattern::parse(text).is_err(), "{text} was accepted");
        }
    }

    #[test]
    fn malformed_patterns_are_refused_with_the_entry_named() {
        for text in [
            "",
            "*.",
            "*",
            "*.com",
            "localhost",
            "a..b",
            "-a.com",
            "a-.com",
            "a_b.com",
            "*.*.a.com",
            "a.*.com",
            "exa mple.com",
            "例子.com",
            "example.com.",
        ] {
            let error = DomainPattern::parse(text).expect_err(text);
            assert!(error.0.contains(&format!("'{text}'")), "{error}");
        }
        assert_eq!(pattern("API.Example.com").to_string(), "api.example.com");
        assert_eq!(pattern("*.xn--fiqs8s.cn").to_string(), "*.xn--fiqs8s.cn");
    }

    /// The grammar the Android builder also holds itself to (`NetworkPolicyTest`), from the one file both read.
    #[test]
    fn the_shared_corpus_is_the_grammar() {
        let corpus: Value = serde_json::from_str(include_str!(
            "../../../../contracts/runtime/domain-patterns.json"
        ))
        .unwrap();
        for pair in corpus["accepted"].as_array().unwrap() {
            let (entry, stored) = (pair[0].as_str().unwrap(), pair[1].as_str().unwrap());
            assert_eq!(pattern(entry).to_string(), stored, "{entry}");
        }
        for entry in corpus["refused"].as_array().unwrap() {
            let entry = entry.as_str().unwrap();
            assert!(DomainPattern::parse(entry).is_err(), "{entry} was accepted");
        }
        // What the Android builder writes when its test grants every channel, alternately listed and unrestricted --
        // read as the policy the same rule gives over this side's channels. A channel only one side has fails here
        // or in `NetworkPolicyTest`.
        let sample = NetworkPolicy::from_json(corpus["builderSample"].as_str().unwrap()).unwrap();
        let mut expected = NetworkPolicy {
            allow_plain_http: true,
            lan: true,
            ..NetworkPolicy::default()
        };
        for (index, channel) in NetworkChannel::ALL.into_iter().enumerate() {
            *expected.channel_mut(channel) = if index % 2 == 0 {
                DomainPolicy::Allowlist(vec![pattern(&format!("{}.example.com", channel.name()))])
            } else {
                DomainPolicy::Unrestricted
            };
        }
        assert_eq!(sample, expected);
    }

    #[test]
    fn json_sets_each_channel_and_flag() {
        let policy = NetworkPolicy::from_json(
            r#"{"request":["api.example.com"],"download":["*.cdn.example.com"],"socket":"unrestricted",
                "allowPlainHttp":true,"lan":true}"#,
        )
        .unwrap();
        assert!(policy.request.permits("api.example.com"));
        assert!(!policy.request.permits("x.cdn.example.com"));
        assert!(policy.download.permits("x.cdn.example.com"));
        assert_eq!(policy.socket, DomainPolicy::Unrestricted);
        assert_eq!(policy.upload, DomainPolicy::default());
        assert_eq!(policy.tcp, DomainPolicy::default());
        assert_eq!(policy.udp, DomainPolicy::default());
        assert!(policy.allow_plain_http && policy.lan);
        assert!(policy.permits_on_any_channel("api.example.com"));
        assert!(policy.permits_on_any_channel("anything.example.org"));
        assert!(!NetworkPolicy::default().permits_on_any_channel("api.example.com"));
    }

    #[test]
    fn json_that_is_not_the_grammar_is_an_error_not_a_refusal() {
        for (text, says) in [
            (r#"{"requests":["a.com"]}"#, "unknown key 'requests'"),
            (r#"{"request":"a.com"}"#, "'request' must be an array"),
            (
                r#"{"request":"Unrestricted"}"#,
                "'request' must be an array",
            ),
            (r#"{"request":[1]}"#, "not a string"),
            (r#"{"request":["1.2.3.4"]}"#, "is an IP address"),
            (r#"{"lan":"yes"}"#, "'lan' must be true or false"),
            (r#"["a.com"]"#, "must be a JSON object"),
            ("{", "not valid JSON"),
        ] {
            let error = NetworkPolicy::from_json(text).expect_err(text);
            assert!(error.to_string().contains(says), "{text}: {error}");
        }
    }
}
