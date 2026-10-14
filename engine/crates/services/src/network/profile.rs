//! A request's `profile`, as the reference reports it: the timestamps of its phases in epoch milliseconds, its
//! connection, its sizes, and the network estimates beside them.
//!
//! Each field keeps the reference's definition, which is what lets it be measured rather than filled in:
//! - a connection that existed before the request (`socketReused`) did no lookup, connecting or handshake for it, so
//!   those phases are the fetch start;
//! - a plain connection had no TLS handshake: its TLS phase is 0;
//! - redirects are timed only when they stayed on the request's host, and are 0 otherwise;
//! - a request waits its turn among the ten the reference lets run at once: its queue is that wait, which starts and
//!   ends together when it did not have to wait;
//! - an answer that needed no connection (a `data:` URL) has every phase at its fetch start, no peer, and no round trip.
//!
//! The RTT is the kernel's for the connection's socket, absent where the platform reports none.

use std::time::Instant;

use serde_json::{Value, json};

use super::http::trace::epoch_ms;
use super::http::{RequestTiming, Span};
use super::quality::Estimate;

fn ms(at: Instant) -> u64 {
    epoch_ms(at).round().max(0.0) as u64
}

/// The profile of a request content called at `invoked`, that waited for its turn over `queue`, and whose exchange
/// `timing` measured.
pub fn profile(invoked: Instant, queue: Span, timing: &RequestTiming, estimate: Estimate) -> Value {
    let fetch_start = ms(timing.fetch_start);
    let fresh = timing.connection.as_ref().filter(|_| !timing.socket_reused);
    let (lookup_start, lookup_end) = fresh
        .and_then(|connection| connection.dns)
        .map_or((fetch_start, fetch_start), |span| {
            (ms(span.start), ms(span.end))
        });
    let (connect_start, connect_end) = fresh.map_or((fetch_start, fetch_start), |connection| {
        // The reference's connectEnd is the end of the whole handshake, secure connection included.
        (ms(connection.connect.start), ms(connection.established()))
    });
    let (tls_start, tls_end) = match (
        &timing.connection,
        fresh.and_then(|connection| connection.tls),
    ) {
        (_, Some(tls)) => (ms(tls.start), ms(tls.end)),
        (Some(connection), None) if connection.tls.is_some() => (fetch_start, fetch_start),
        _ => (0, 0),
    };
    let (redirect_start, redirect_end) = timing
        .redirect
        .map_or((0, 0), |span| (ms(span.start), ms(span.end)));
    let response_end = timing.response_end.unwrap_or(timing.response_start);
    let transfer = response_end
        .saturating_duration_since(timing.response_start)
        .as_secs_f64();
    let throughput_kbps = if transfer > 0.0 {
        (timing.bytes_received as f64 * 8.0 / 1000.0 / transfer).round()
    } else {
        0.0
    };
    let protocol = match &timing.connection {
        None => "unknown",
        Some(_) if timing.version == http::Version::HTTP_2 => "h2",
        Some(_) => "http1.1",
    };

    let mut profile = json!({
        "invokeStart": ms(invoked),
        "queueStart": ms(queue.start),
        "queueEnd": ms(queue.end),
        "redirectStart": redirect_start,
        "redirectEnd": redirect_end,
        "fetchStart": fetch_start,
        "domainLookUpStart": lookup_start,
        "domainLookUpEnd": lookup_end,
        "connectStart": connect_start,
        "connectEnd": connect_end,
        "SSLconnectionStart": tls_start,
        "SSLconnectionEnd": tls_end,
        "requestStart": ms(timing.request_start),
        "requestEnd": ms(timing.request_end),
        "responseStart": ms(timing.response_start),
        "responseEnd": ms(response_end),
        "estimate_nettype": estimate.effective_type,
        "httpRttEstimate": estimate.http_rtt_ms.round(),
        "transportRttEstimate": estimate.transport_rtt_ms.round(),
        "downstreamThroughputKbpsEstimate": estimate.downstream_kbps.round(),
        "throughputKbps": throughput_kbps,
        "peerIP": timing.connection.as_ref().map_or_else(String::new, |connection| connection.peer.ip().to_string()),
        "port": timing.connection.as_ref().map_or(0, |connection| connection.peer.port()),
        "socketReused": timing.socket_reused,
        "sendBytesCount": timing.bytes_sent,
        // The reference's spelling.
        "receivedBytedCount": timing.bytes_received,
        "protocol": protocol,
        "usingHighPerformanceMode": false,
    });
    let rtt = match &timing.connection {
        None => Some(0.0),
        Some(connection) => connection
            .rtt()
            .map(|rtt| (rtt.as_secs_f64() * 1000.0).round()),
    };
    if let Some(rtt) = rtt {
        profile["rtt"] = json!(rtt);
    }
    profile
}
