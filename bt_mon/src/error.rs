//! Error types for bt_mon.
//!
//! This module provides a unified error type that abstracts over backend-specific errors.

use crate::types::{CharacteristicUuid, DeviceId, ServiceUuid};
use std::fmt;

/// Backend kinds supported by bt_mon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    /// bluer backend (Linux/BlueZ)
    Bluer,
    /// btleplug backend (cross-platform)
    Btleplug,
}

impl fmt::Display for BackendKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BackendKind::Bluer => write!(f, "bluer"),
            BackendKind::Btleplug => write!(f, "btleplug"),
        }
    }
}

/// Error type for bt_mon operations.
#[derive(Debug)]
pub enum Error {
    /// Backend initialization failed.
    InitFailed(String),

    /// Operation failed on backend.
    BackendError {
        /// The backend that encountered the error.
        backend: BackendKind,
        /// Error message.
        message: String,
    },

    /// Device not found.
    DeviceNotFound(DeviceId),

    /// Device not connected.
    NotConnected(DeviceId),

    /// GATT service not discovered.
    ServiceNotFound(ServiceUuid),

    /// GATT characteristic not found.
    CharacteristicNotFound(CharacteristicUuid),

    /// Connection timeout.
    ConnectionTimeout,

    /// Operation cancelled.
    Cancelled,

    /// Invalid argument.
    InvalidArgument(String),

    /// Backend not available (feature not enabled).
    BackendUnavailable {
        /// Required backend.
        required: BackendKind,
        /// Available backends.
        available: Vec<BackendKind>,
    },

    /// Scan already in progress.
    ScanAlreadyInProgress,

    /// Not currently scanning.
    NotScanning,

    /// Device disconnected during operation.
    Disconnected(DeviceId),

    /// GATT operation failed.
    GattError {
        /// Device ID.
        device_id: DeviceId,
        /// Operation that failed.
        operation: String,
        /// Error message.
        message: String,
    },

    /// Internal error.
    Internal(String),
}

/// Type alias for bt_mon results.
pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InitFailed(msg) => write!(f, "Backend initialization failed: {}", msg),
            Error::BackendError { backend, message } => {
                write!(f, "Backend error ({}): {}", backend, message)
            }
            Error::DeviceNotFound(id) => write!(f, "Device not found: {}", id.0),
            Error::NotConnected(id) => write!(f, "Device not connected: {}", id.0),
            Error::ServiceNotFound(uuid) => write!(f, "Service not found: {}", uuid.0),
            Error::CharacteristicNotFound(uuid) => {
                write!(f, "Characteristic not found: {}", uuid.0)
            }
            Error::ConnectionTimeout => write!(f, "Connection timeout"),
            Error::Cancelled => write!(f, "Operation cancelled"),
            Error::InvalidArgument(msg) => write!(f, "Invalid argument: {}", msg),
            Error::BackendUnavailable { required, available } => {
                write!(
                    f,
                    "Backend {} unavailable. Available: {:?}",
                    required, available
                )
            }
            Error::ScanAlreadyInProgress => write!(f, "Scan already in progress"),
            Error::NotScanning => write!(f, "Not currently scanning"),
            Error::Disconnected(id) => write!(f, "Device disconnected: {}", id.0),
            Error::GattError {
                device_id,
                operation,
                message,
            } => write!(
                f,
                "GATT error on device {}: {} - {}",
                device_id.0, operation, message
            ),
            Error::Internal(msg) => write!(f, "Internal error: {}", msg),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(feature = "bluer")]
impl From<bluer::Error> for Error {
    fn from(err: bluer::Error) -> Self {
        Error::BackendError {
            backend: BackendKind::Bluer,
            message: err.to_string(),
        }
    }
}

#[cfg(feature = "btleplug")]
impl From<btleplug::Error> for Error {
    fn from(err: btleplug::Error) -> Self {
        Error::BackendError {
            backend: BackendKind::Btleplug,
            message: err.to_string(),
        }
    }
}

impl From<tokio::time::error::Elapsed> for Error {
    fn from(_: tokio::time::error::Elapsed) -> Self {
        Error::ConnectionTimeout
    }
}

impl From<uuid::Error> for Error {
    fn from(err: uuid::Error) -> Self {
        Error::InvalidArgument(format!("Invalid UUID: {}", err))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{DeviceId, ServiceUuid};

    #[test]
    fn test_backend_kind_display() {
        assert_eq!(format!("{}", BackendKind::Bluer), "bluer");
        assert_eq!(format!("{}", BackendKind::Btleplug), "btleplug");
    }

    #[test]
    fn test_error_display() {
        let err = Error::DeviceNotFound(DeviceId("AA:BB:CC:DD:EE:FF".to_string()));
        assert_eq!(format!("{}", err), "Device not found: AA:BB:CC:DD:EE:FF");

        let err = Error::ServiceNotFound(ServiceUuid(uuid::Uuid::parse_str("00001800-0000-1000-8000-00805f9b34fb").unwrap()));
        assert_eq!(format!("{}", err), "Service not found: 00001800-0000-1000-8000-00805f9b34fb");

        let err = Error::ConnectionTimeout;
        assert_eq!(format!("{}", err), "Connection timeout");
    }

    #[test]
    fn test_error_debug() {
        let err = Error::InitFailed("test error".to_string());
        let debug_str = format!("{:?}", err);
        assert!(debug_str.contains("InitFailed"));
    }

    #[test]
    fn test_uuid_error_conversion() {
        // Test with an invalid UUID string that will produce an error
        let result = uuid::Uuid::parse_str("invalid-uuid");
        assert!(result.is_err());
        let err: Error = result.unwrap_err().into();
        assert!(matches!(err, Error::InvalidArgument(_)));
    }
}
