//! The public URL parser also owns network URL values.
//!
//! This adapter gives the transport its existing borrowed component accessors;
//! parsing, relative resolution, IDNA and encoding all remain Ada's. A second
//! parser must not reinterpret the URL after policy has admitted it.

use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Url(ada_url::Url);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParseError;

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid URL")
    }
}
impl std::error::Error for ParseError {}

impl Url {
    pub fn parse(input: &str) -> Result<Self, ParseError> {
        Self::parse_with_base(input, None)
    }

    fn parse_with_base(input: &str, base: Option<&str>) -> Result<Self, ParseError> {
        ada_url::Url::parse(input, base)
            .map(Self)
            .map_err(|_| ParseError)
    }

    pub fn join(&self, input: &str) -> Result<Self, ParseError> {
        Self::parse_with_base(input, Some(self.as_str()))
    }

    /// Shared classification for a source that may also be a local/VFS path.
    /// Only HTTP(S) is remote; a malformed explicit HTTP scheme is an error.
    pub fn parse_http(input: &str) -> Result<Option<Self>, ParseError> {
        match Self::parse(input) {
            Ok(url) => Ok(matches!(url.scheme(), "http" | "https").then_some(url)),
            Err(error) => {
                let prefix = input.trim_start_matches(|character| character <= '\u{20}');
                let http = prefix
                    .get(..5)
                    .is_some_and(|scheme| scheme.eq_ignore_ascii_case("http:"));
                let https = prefix
                    .get(..6)
                    .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https:"));
                if http || https { Err(error) } else { Ok(None) }
            }
        }
    }

    pub fn as_str(&self) -> &str {
        self.0.href()
    }

    pub fn scheme(&self) -> &str {
        let protocol = self.0.protocol();
        protocol.strip_suffix(':').unwrap_or(protocol)
    }

    pub fn host_str(&self) -> Option<&str> {
        let host = self.0.hostname();
        (!host.is_empty()).then_some(host)
    }

    pub fn port(&self) -> Option<u16> {
        self.0
            .components()
            .port
            .and_then(|port| port.try_into().ok())
    }

    pub fn port_or_known_default(&self) -> Option<u16> {
        self.port().or(match self.scheme() {
            "http" | "ws" => Some(80),
            "https" | "wss" => Some(443),
            "ftp" => Some(21),
            _ => None,
        })
    }

    pub fn path(&self) -> &str {
        self.0.pathname()
    }

    pub fn query(&self) -> Option<&str> {
        self.0
            .has_search()
            .then(|| self.0.search().strip_prefix('?').unwrap_or(""))
    }

    pub fn fragment(&self) -> Option<&str> {
        self.0
            .has_hash()
            .then(|| self.0.hash().strip_prefix('#').unwrap_or(""))
    }

    pub fn set_username(&mut self, value: &str) -> Result<(), ParseError> {
        self.0.set_username(Some(value)).map_err(|_| ParseError)
    }

    pub fn set_password(&mut self, value: Option<&str>) -> Result<(), ParseError> {
        self.0.set_password(value).map_err(|_| ParseError)
    }

    pub fn set_fragment(&mut self, value: Option<&str>) {
        self.0.set_hash(value);
    }
}

impl fmt::Display for Url {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl From<Url> for String {
    fn from(url: Url) -> Self {
        url.as_str().to_owned()
    }
}

/// A raw socket takes a host argument, not a URL or authority. Validate that
/// boundary, then let the same special-host parser canonicalize it. The fixed
/// nondefault port makes an injected port fail instead of being normalized away.
pub(super) fn canonical_host(host: &str) -> Result<String, ParseError> {
    if host.is_empty()
        || host.bytes().any(|byte| {
            byte <= b' ' || byte == 127 || matches!(byte, b'/' | b'\\' | b'?' | b'#' | b'@')
        })
    {
        return Err(ParseError);
    }
    let parsed = Url::parse(&format!("http://{host}:65535/"))?;
    if parsed.0.has_credentials()
        || parsed.port() != Some(65535)
        || parsed.path() != "/"
        || parsed.query().is_some()
        || parsed.0.has_hash()
    {
        return Err(ParseError);
    }
    parsed
        .host_str()
        .filter(|host| !host.is_empty())
        .map(str::to_owned)
        .ok_or(ParseError)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "run by test-ada-url-vendor-contract.sh with the unchanged WPT fixture converted to WebIDL USVString"]
    fn network_url_wpt_constructors() {
        let file = std::env::var_os("MIGO_NETWORK_URL_WPT_FILE")
            .expect("the contract gate prepares USVString inputs");
        let data: serde_json::Value =
            serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
        let mut failures = Vec::new();
        let mut cases = 0;
        for row in data
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row.is_object())
        {
            cases += 1;
            let input = row["input"].as_str().unwrap();
            let parsed = Url::parse_with_base(input, row["base"].as_str());
            if row["failure"] == true {
                if parsed.is_ok() {
                    failures.push(format!("accepted invalid URL: {input:?}"));
                }
                continue;
            }
            let Ok(parsed) = parsed else {
                failures.push(format!("rejected valid URL: {input:?}"));
                continue;
            };
            let port = parsed
                .port()
                .map(|port| port.to_string())
                .unwrap_or_default();
            let host = match (parsed.host_str(), parsed.port()) {
                (Some(host), Some(port)) => format!("{host}:{port}"),
                (Some(host), None) => host.to_owned(),
                _ => String::new(),
            };
            let search = parsed
                .query()
                .filter(|query| !query.is_empty())
                .map(|query| format!("?{query}"))
                .unwrap_or_default();
            for (key, actual) in [
                ("href", parsed.as_str().to_owned()),
                ("protocol", format!("{}:", parsed.scheme())),
                ("hostname", parsed.host_str().unwrap_or_default().to_owned()),
                ("host", host),
                ("port", port),
                ("pathname", parsed.path().to_owned()),
                ("search", search),
            ] {
                if let Some(expected) = row[key].as_str() {
                    if actual != expected {
                        failures.push(format!("{input:?}/{key}: {actual:?} != {expected:?}"));
                    }
                }
            }
        }
        assert_eq!(cases, 896, "the pinned constructor corpus must not shrink");
        assert!(failures.is_empty(), "{}", failures.join("\n"));
        println!("network URL WPT: {cases} constructors matched");
    }

    #[test]
    fn an_http_redirect_with_extra_slashes_has_the_standard_host() {
        let base = Url::parse("https://example.org/game/start").unwrap();
        assert_eq!(
            base.join("///cdn.example.org/asset").unwrap().as_str(),
            "https://cdn.example.org/asset"
        );
        assert_eq!(
            base.join("///example.org/../asset").unwrap().as_str(),
            "https://example.org/asset"
        );
    }

    #[test]
    fn network_paths_encode_caret_without_rewriting_the_query() {
        for scheme in ["http", "https", "ws", "wss"] {
            let parsed = Url::parse(&format!(
                "{scheme}://example.org/path^asset?query=^#fragment^"
            ))
            .unwrap();
            assert_eq!(parsed.path(), "/path%5Easset");
            assert_eq!(parsed.query(), Some("query=^"));
            assert_eq!(
                parsed.as_str(),
                format!("{scheme}://example.org/path%5Easset?query=^#fragment^")
            );
        }
    }

    #[test]
    fn ascii_domain_handling_agrees_with_the_public_url_parser() {
        for host in ["a.b.c.xn--pokxncvks", "a.b.c.XN--pokxncvks", "xn--"] {
            let parsed = Url::parse(&format!("https://{host}/game")).unwrap();
            assert_eq!(parsed.host_str(), Some(host.to_ascii_lowercase().as_str()));
        }
    }

    #[test]
    fn empty_query_and_nondefault_port_survive_without_an_extra_parser() {
        let parsed = Url::parse("https://example.org:8443/game?#fragment").unwrap();
        assert_eq!(parsed.query(), Some(""));
        assert_eq!(parsed.port(), Some(8443));
        assert_eq!(parsed.port_or_known_default(), Some(8443));
        assert_eq!(
            Url::parse("wss://example.org/game")
                .unwrap()
                .port_or_known_default(),
            Some(443)
        );
    }

    #[test]
    fn a_raw_host_argument_cannot_smuggle_url_or_authority_fields() {
        for input in [
            "",
            "example.org:80",
            "example.org:",
            "user@example.org",
            "@example.org",
            "example.org/path",
            "example.org\\path",
            "example.org?x",
            "example.org#x",
            "\texample.org",
            "example\n.org",
            "example.org\0",
            "[::1]:80",
            "%40example.org",
            "example%2f.org",
            "example%3a.org",
            "example%23.org",
            "example%00.org",
        ] {
            assert!(
                canonical_host(input).is_err(),
                "accepted a host with extra URL fields: {input:?}"
            );
        }
    }

    #[test]
    fn raw_hosts_share_special_host_idna_ipv4_and_ipv6_canonicalization() {
        for (input, expected) in [
            ("EXAMPLE.org", "example.org"),
            ("%65xample.org", "example.org"),
            ("bücher.example", "xn--bcher-kva.example"),
            ("0x08080808", "8.8.8.8"),
            ("010.010.010.010", "8.8.8.8"),
            ("[2001:0DB8:0:0:0:0:0:1]", "[2001:db8::1]"),
        ] {
            assert_eq!(canonical_host(input).unwrap(), expected);
        }
    }
}
