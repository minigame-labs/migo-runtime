//! Network service trait.

use crate::protocol::error::ServiceError;

/// Network status service.
pub trait NetworkService: Send + Sync {
    fn start_monitoring(&self) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "onNetworkStatusChange:fail not supported",
        ))
    }

    fn stop_monitoring(&self) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "offNetworkStatusChange:fail not supported",
        ))
    }

    /// Get network type JSON: `{"networkType": "wifi", "isConnected": true}`
    fn get_network_type_json(&self) -> Result<String, ServiceError> {
        Err(ServiceError::not_supported(
            "getNetworkType:fail not supported",
        ))
    }
}
