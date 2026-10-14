//! How good the network is, as the requests the engine has made measured it.
//!
//! A request's `profile` reports estimates beside its own measurements: the HTTP round trip (request sent to first
//! byte), the transport round trip (the kernel's smoothed RTT), the downstream throughput, and the effective connection
//! type they amount to. Each completed response is a sample; an estimate is the exponentially weighted mean of the
//! samples so far, so it follows the network as it changes without one outlier deciding it. The connection type comes
//! from the HTTP RTT through the thresholds browsers use for the Network Information API (Chromium's network quality
//! estimator), in its numbering: 0 unknown, 1 offline, 2 slow-2g, 3 2g, 4 3g, 5 4g.
//!
//! One estimator per process: every client's requests share the network they measure.

use std::sync::OnceLock;
use std::time::Duration;

use parking_lot::Mutex;

use super::http::RequestTiming;

/// Weight of the newest sample in each mean.
const WEIGHT: f64 = 0.3;

/// A transfer shorter or smaller than this says more about latency than about bandwidth, so it is not a throughput
/// sample.
const MIN_THROUGHPUT_BYTES: u64 = 32 * 1024;
const MIN_THROUGHPUT_TIME: Duration = Duration::from_millis(20);

/// An estimate a profile reports, or `-1` where nothing has been measured yet (the value the reference platform's
/// network stack reports for an unknown metric).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Estimate {
    pub effective_type: u8,
    pub http_rtt_ms: f64,
    pub transport_rtt_ms: f64,
    pub downstream_kbps: f64,
}

#[derive(Debug, Default)]
pub struct Estimator {
    http_rtt_ms: Option<f64>,
    transport_rtt_ms: Option<f64>,
    downstream_kbps: Option<f64>,
}

fn mean(current: Option<f64>, sample: f64) -> Option<f64> {
    Some(current.map_or(sample, |current| current + WEIGHT * (sample - current)))
}

impl Estimator {
    /// Take what one completed response measured. A response that came on no connection measured nothing.
    pub fn record(&mut self, timing: &RequestTiming) {
        let Some(connection) = &timing.connection else {
            return;
        };
        let to_first_byte = timing
            .response_start
            .saturating_duration_since(timing.request_start);
        self.http_rtt_ms = mean(self.http_rtt_ms, to_first_byte.as_secs_f64() * 1000.0);
        if let Some(rtt) = connection.rtt() {
            self.transport_rtt_ms = mean(self.transport_rtt_ms, rtt.as_secs_f64() * 1000.0);
        }
        if let Some(end) = timing.response_end {
            let transfer = end.saturating_duration_since(timing.response_start);
            if timing.bytes_received >= MIN_THROUGHPUT_BYTES && transfer >= MIN_THROUGHPUT_TIME {
                let kbps = timing.bytes_received as f64 * 8.0 / 1000.0 / transfer.as_secs_f64();
                self.downstream_kbps = mean(self.downstream_kbps, kbps);
            }
        }
    }

    pub fn estimate(&self) -> Estimate {
        Estimate {
            effective_type: self.http_rtt_ms.map_or(0, effective_type),
            http_rtt_ms: self.http_rtt_ms.unwrap_or(-1.0),
            transport_rtt_ms: self.transport_rtt_ms.unwrap_or(-1.0),
            downstream_kbps: self.downstream_kbps.unwrap_or(-1.0),
        }
    }
}

/// The effective connection type an HTTP RTT amounts to: the Network Information API's thresholds.
fn effective_type(http_rtt_ms: f64) -> u8 {
    if http_rtt_ms >= 2010.0 {
        2 // slow-2g
    } else if http_rtt_ms >= 1420.0 {
        3 // 2g
    } else if http_rtt_ms >= 272.0 {
        4 // 3g
    } else {
        5 // 4g
    }
}

/// The process's estimator.
pub fn estimator() -> &'static Mutex<Estimator> {
    static ESTIMATOR: OnceLock<Mutex<Estimator>> = OnceLock::new();
    ESTIMATOR.get_or_init(|| Mutex::new(Estimator::default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nothing measured is unknown, not zero: an RTT of zero would read as an instant network.
    #[test]
    fn an_estimator_with_no_samples_knows_nothing() {
        assert_eq!(
            Estimator::default().estimate(),
            Estimate {
                effective_type: 0,
                http_rtt_ms: -1.0,
                transport_rtt_ms: -1.0,
                downstream_kbps: -1.0
            }
        );
    }

    /// The thresholds, at their edges.
    #[test]
    fn the_connection_type_follows_the_http_rtt() {
        assert_eq!(effective_type(2010.0), 2);
        assert_eq!(effective_type(2009.0), 3);
        assert_eq!(effective_type(1420.0), 3);
        assert_eq!(effective_type(272.0), 4);
        assert_eq!(effective_type(271.0), 5);
        assert_eq!(effective_type(0.0), 5);
    }

    /// Each sample moves the mean by its weight: the first is taken whole, the next pulls part of the way.
    #[test]
    fn samples_move_the_mean_by_their_weight() {
        let first = mean(None, 100.0);
        assert_eq!(first, Some(100.0));
        assert_eq!(mean(first, 200.0), Some(130.0));
    }
}
