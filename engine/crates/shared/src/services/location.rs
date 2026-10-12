//! Location service traits for GPS/network positioning.

use crate::protocol::error::ServiceError;

/// Location service for getting device geographic position.
///
/// Both methods are fire-and-forget: the host starts an async location request
/// and answers it later. A host answers in WGS-84, the datum every positioning
/// system reports; the engine converts to GCJ-02 when content asked for it, and
/// rounds getFuzzyLocation's position to 0.01 degrees itself.
pub trait LocationService: Send + Sync {
    /// Start a precise location request (getLocation).
    ///
    /// Options JSON fields:
    /// - `altitude`: bool (default false) -- include altitude
    /// - `isHighAccuracy`: bool (default false)
    /// - `highAccuracyExpireTime`: number in ms
    fn get_location(&self, _options_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "getLocation:fail not supported",
        ))
    }

    /// Start a fuzzy location request (getFuzzyLocation): the host's coarse
    /// position, which the engine rounds.
    fn get_fuzzy_location(&self, _options_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "getFuzzyLocation:fail not supported",
        ))
    }
}
