//! Bounded random reads of one pinned HTTP representation (RFC 9110 §§13–15).
//!
//! Container indexes must never join byte offsets from different encodings or
//! object versions. Use the existing policy/redirect client, a strong ETag and
//! the final URL. A server ignoring Range is an explicit fallback condition;
//! its full response is never collected here. The caller owns cancellation by
//! dropping the read future, which also latches this reader as failed.
use super::{HttpClient, Response};
use crate::network::{gate::GateKind, url::Url};
use http::{
    HeaderMap, HeaderValue, StatusCode,
    header::{
        ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, ETAG,
        IF_RANGE, RANGE,
    },
};
use tokio::time::Instant;

/// Limits control/index reads, independently of streaming media duration.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_read_bytes: usize,
    pub max_total_bytes: u64,
    pub max_requests: u32,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_read_bytes: 2 * 1024 * 1024,
            max_total_bytes: 4 * 1024 * 1024,
            max_requests: 64,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    NoStrongValidator,
    RangeUnsupported,
    RepresentationChanged,
    InvalidResponse(&'static str),
    Limit,
    Timeout,
    Http(String),
    PreviouslyFailed,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HTTP range source: {self:?}")
    }
}
impl std::error::Error for Error {}

/// Initial response identity. No body is consumed by capturing it.
pub struct Identity {
    url: Url,
    etag: HeaderValue,
    length: Option<u64>,
}
impl Identity {
    pub fn capture(response: &Response) -> Result<Self, Error> {
        if response.has_decoded_representation() {
            return Err(Error::InvalidResponse("initial representation was decoded"));
        }
        if response.status() != StatusCode::OK || response.headers().contains_key(CONTENT_RANGE) {
            return Err(Error::InvalidResponse(
                "initial response must be a full representation",
            ));
        }
        identity_encoding(response.headers())?;
        let tag = single(response.headers(), ETAG.as_str())?.ok_or(Error::NoStrongValidator)?;
        let bytes = tag.as_bytes();
        if bytes.len() < 2
            || bytes.len() > 1024
            || bytes[0] != b'"'
            || bytes[bytes.len() - 1] != b'"'
            || !bytes[1..bytes.len() - 1]
                .iter()
                .all(|b| *b == 0x21 || (0x23..=0x7e).contains(b) || *b >= 0x80)
        {
            return Err(Error::NoStrongValidator);
        }
        Ok(Self {
            url: response.url().clone(),
            etag: tag.clone(),
            // Response::content_length() is the remaining body's size hint,
            // not an object's identity once any bytes have been consumed.
            length: single(response.headers(), CONTENT_LENGTH.as_str())?
                .map(|n| {
                    decimal(n.as_bytes()).ok_or(Error::InvalidResponse("invalid object length"))
                })
                .transpose()?,
        })
    }

    pub fn length(&self) -> Option<u64> {
        self.length
    }
}

pub struct Reader {
    client: HttpClient,
    kind: GateKind,
    identity: Identity,
    limits: Limits,
    requests: u32,
    bytes: u64,
    failed: bool,
}
impl Reader {
    pub fn new(client: HttpClient, kind: GateKind, identity: Identity, limits: Limits) -> Self {
        Self {
            client,
            kind,
            identity,
            limits,
            requests: 0,
            bytes: 0,
            failed: false,
        }
    }
    pub fn length(&self) -> Option<u64> {
        self.identity.length
    }
    pub fn requests(&self) -> u32 {
        self.requests
    }
    pub fn bytes_received(&self) -> u64 {
        self.bytes
    }

    /// Collect exactly one bounded control/index extent. All subrequests share
    /// the supplied absolute deadline. Valid short 206 responses are continued
    /// contiguously; neither EOF nor a short body can silently satisfy a read.
    pub async fn read_exact_at(
        &mut self,
        offset: u64,
        len: usize,
        deadline: Instant,
    ) -> Result<Vec<u8>, Error> {
        if self.failed {
            return Err(Error::PreviouslyFailed);
        }
        // Set before the first suspension: dropping this future cannot leave a
        // partially completed read available for accidental reuse.
        self.failed = true;
        let result = self.read_inner(offset, len, deadline).await;
        if result.is_ok() {
            self.failed = false;
        }
        result
    }

    async fn read_inner(
        &mut self,
        offset: u64,
        len: usize,
        deadline: Instant,
    ) -> Result<Vec<u8>, Error> {
        let end = offset
            .checked_add(len as u64)
            .filter(|end| *end > offset)
            .ok_or(Error::Limit)?;
        if len > self.limits.max_read_bytes
            || self
                .bytes
                .checked_add(len as u64)
                .is_none_or(|n| n > self.limits.max_total_bytes)
            || self.identity.length.is_some_and(|n| end > n)
        {
            return Err(Error::Limit);
        }
        let mut output = Vec::new();
        output.try_reserve_exact(len).map_err(|_| Error::Limit)?;
        while output.len() < len {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Error::Timeout);
            }
            self.requests = self
                .requests
                .checked_add(1)
                .filter(|n| *n <= self.limits.max_requests)
                .ok_or(Error::Limit)?;
            let start = offset + output.len() as u64;
            let value = HeaderValue::from_str(&format!("bytes={start}-{}", end - 1))
                .map_err(|_| Error::Limit)?;
            let mut response = self
                .client
                .get(self.kind, self.identity.url.clone())
                .header(RANGE, value)
                .header(IF_RANGE, self.identity.etag.clone())
                .header(ACCEPT_ENCODING, HeaderValue::from_static("identity"))
                .timeout(remaining)
                .send()
                .await
                .map_err(|e| {
                    if Instant::now() >= deadline {
                        Error::Timeout
                    } else {
                        Error::Http(e.to_string())
                    }
                })?;
            if response.url().as_str() != self.identity.url.as_str() {
                return Err(Error::RepresentationChanged);
            }
            identity_encoding(response.headers())?;
            let tag = single(response.headers(), ETAG.as_str())?;
            if tag.is_some_and(|tag| tag != self.identity.etag) {
                return Err(Error::RepresentationChanged);
            }
            if response.status() == StatusCode::OK {
                return Err(Error::RangeUnsupported);
            }
            if response.status() != StatusCode::PARTIAL_CONTENT || tag.is_none() {
                return Err(Error::InvalidResponse(
                    "expected a validated single-part 206",
                ));
            }
            if single(response.headers(), CONTENT_TYPE.as_str())?.is_some_and(|v| {
                v.as_bytes()
                    .get(..10)
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"multipart/"))
            }) {
                return Err(Error::InvalidResponse("multipart range response"));
            }
            let extent = single(response.headers(), CONTENT_RANGE.as_str())?
                .ok_or(Error::InvalidResponse("missing Content-Range"))?;
            let extent = ContentRange::parse(extent)?;
            if extent.start != start || extent.end >= end {
                return Err(Error::InvalidResponse(
                    "range does not match requested coordinates",
                ));
            }
            if self
                .identity
                .length
                .zip(extent.total)
                .is_some_and(|(a, b)| a != b)
            {
                return Err(Error::RepresentationChanged);
            }
            let total = self.identity.length.or(extent.total);
            if total.is_some_and(|n| end > n || extent.end >= n) {
                return Err(Error::InvalidResponse("range exceeds the representation"));
            }
            self.identity.length = total;
            let expected = extent.end - extent.start + 1;
            if single(response.headers(), CONTENT_LENGTH.as_str())?
                .is_some_and(|n| decimal(n.as_bytes()) != Some(expected))
            {
                return Err(Error::InvalidResponse(
                    "Content-Length disagrees with Content-Range",
                ));
            }
            let before = output.len();
            while let Some(chunk) = response.chunk().await.map_err(|e| {
                if Instant::now() >= deadline {
                    Error::Timeout
                } else {
                    Error::Http(e.to_string())
                }
            })? {
                self.bytes = self
                    .bytes
                    .checked_add(chunk.len() as u64)
                    .filter(|n| *n <= self.limits.max_total_bytes)
                    .ok_or(Error::Limit)?;
                let received = (output.len() - before) as u64;
                if received
                    .checked_add(chunk.len() as u64)
                    .is_none_or(|n| n > expected)
                {
                    return Err(Error::InvalidResponse("range body exceeds declared extent"));
                }
                output.extend_from_slice(&chunk);
            }
            if (output.len() - before) as u64 != expected {
                return Err(Error::InvalidResponse("truncated range body"));
            }
        }
        Ok(output)
    }
}

fn single<'a>(headers: &'a HeaderMap, name: &str) -> Result<Option<&'a HeaderValue>, Error> {
    let mut values = headers.get_all(name).iter();
    let first = values.next();
    if values.next().is_some() {
        return Err(Error::InvalidResponse("duplicate representation header"));
    }
    Ok(first)
}
fn identity_encoding(headers: &HeaderMap) -> Result<(), Error> {
    for value in headers.get_all(CONTENT_ENCODING) {
        let text = value
            .to_str()
            .map_err(|_| Error::InvalidResponse("invalid Content-Encoding"))?;
        if !text
            .split(',')
            .all(|v| v.trim().eq_ignore_ascii_case("identity"))
        {
            return Err(Error::InvalidResponse(
                "range source requires identity Content-Encoding",
            ));
        }
    }
    Ok(())
}
fn decimal(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() {
        return None;
    }
    bytes.iter().try_fold(0_u64, |n, b| {
        if !b.is_ascii_digit() {
            return None;
        }
        n.checked_mul(10)?.checked_add(u64::from(b - b'0'))
    })
}
struct ContentRange {
    start: u64,
    end: u64,
    total: Option<u64>,
}
impl ContentRange {
    fn parse(value: &HeaderValue) -> Result<Self, Error> {
        let bad = || Error::InvalidResponse("invalid Content-Range");
        let text = value.to_str().map_err(|_| bad())?;
        if text.len() > 128 {
            return Err(bad());
        }
        let (unit, extent) = text.split_once(' ').ok_or_else(bad)?;
        if !unit.eq_ignore_ascii_case("bytes") {
            return Err(bad());
        }
        let (extent, total) = extent.split_once('/').ok_or_else(bad)?;
        let (start, end) = extent.split_once('-').ok_or_else(bad)?;
        let start = decimal(start.as_bytes()).ok_or_else(bad)?;
        let end = decimal(end.as_bytes()).ok_or_else(bad)?;
        let total = if total == "*" {
            None
        } else {
            Some(decimal(total.as_bytes()).ok_or_else(bad)?)
        };
        if start > end || end == u64::MAX || total.is_some_and(|n| n <= end) {
            return Err(bad());
        }
        Ok(Self { start, end, total })
    }
}

#[cfg(test)]
mod tests;
