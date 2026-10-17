//! Error types for bt_mon.
//!
//! This module provides a unified error type that abstracts over backend-specific errors.
//!
//! # Who raises what
//!
//! Every variant here is one a backend actually produces, because a variant nobody
//! produces is a promise no consumer can rely on — and the inverse trap is real: a
//! failure that arrives as a generic variant sends the consumer down the wrong
//! recovery path. A platform error is mapped to the most specific row that fits, not
//! to a catch-all by reflex.
//!
//! | Variant | Raised by | Situation |
//! |---|---|---|
//! | [`Error::InitFailed`] | btleplug, bluer | the monitor could not be built: no manager or session, no adapter at all, or [`MonitorConfig::adapter`](crate::MonitorConfig::adapter) naming one that matched nothing or several |
//! | [`Error::BackendError`] | all three | a platform call failed and nothing more specific fits; what the mock's `error_on_scan` / `error_on_connect` switches raise |
//! | [`Error::DeviceNotFound`] | all three | the id is not one this monitor has discovered |
//! | [`Error::NotConnected`] | all three | a GATT operation on a device this monitor knows but is not connected to |
//! | [`Error::CharacteristicNotFound`] | all three | no characteristic with that uuid in the device's discovered GATT table |
//! | [`Error::ConnectionTimeout`] | btleplug, bluer | the connect attempt gave up after the backend's connect timeout |
//! | [`Error::InvalidArgument`] | all three | a uuid that does not parse; an operation the object cannot serve, such as reading a characteristic whose properties do not include read |
//! | [`Error::BackendUnavailable`] | callers choosing a backend, and btleplug when the platform has no radio | the requested backend was not compiled in — which a factory mapping a configured name onto a monitor reports, as `app`'s does — or the platform says there is no adapter for this monitor to hold at all, in which case [`BackendKind::compiled`] lists what this build could produce instead |
//! | [`Error::ScanAlreadyInProgress`] | all three | `start_scan` while a scan is running |
//! | [`Error::NotScanning`] | all three | `stop_scan` with no scan running |
//! | [`Error::Disconnected`] | btleplug, bluer, mock | the link dropped, or the peripheral stopped existing, while an operation was in flight |
//! | [`Error::GattError`] | btleplug, bluer, mock | a read, write, subscribe or discovery failed at the GATT layer: `operation` names the call, `message` is the platform's own words |
//!
//! Three variants that used to sit in this enum are gone rather than documented
//! here: `ServiceNotFound` (no operation in this API takes a service uuid, so nothing
//! could ever raise it), `Cancelled` (no call in the crate is cancellable by a
//! caller) and `Internal` (which meant the same thing as `BackendError`, less the
//! backend name).
//!
//! A dropped connection is *also* reported as
//! [`DeviceEvent::DeviceRemoved`](crate::DeviceEvent::DeviceRemoved) or
//! [`DeviceUpdated`](crate::DeviceEvent::DeviceUpdated) naming
//! [`Connected`](crate::UpdateField::Connected) on the event stream; the error tells
//! the caller who asked, the event tells everyone else.

use crate::types::{CharacteristicUuid, DeviceId};
use std::fmt;

/// Backend kinds supported by bt_mon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    /// bluer backend (Linux/BlueZ)
    Bluer,
    /// btleplug backend (cross-platform)
    Btleplug,
    /// mock backend (testing/development)
    Mock,
}

impl fmt::Display for BackendKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BackendKind::Bluer => write!(f, "bluer"),
            BackendKind::Btleplug => write!(f, "btleplug"),
            BackendKind::Mock => write!(f, "mock"),
        }
    }
}

impl BackendKind {
    /// The backends this build can actually produce a monitor for.
    ///
    /// [`Error::BackendUnavailable`]'s `available` field is a fallback list, so it has
    /// to be the truth about the build rather than something a caller guesses: "the
    /// radio you were using is gone, and bluer is compiled in if you want to try the
    /// other BlueZ front-end" is actionable, an empty list is not.
    // The pushes are cfg-gated: which of them exist at all is the fact this function
    // reports, so a `vec![]` literal cannot express it.
    #[allow(clippy::vec_init_then_push)]
    pub fn compiled() -> Vec<BackendKind> {
        let mut kinds = Vec::new();
        #[cfg(feature = "btleplug")]
        kinds.push(BackendKind::Btleplug);
        #[cfg(feature = "bluer")]
        kinds.push(BackendKind::Bluer);
        #[cfg(feature = "mock")]
        kinds.push(BackendKind::Mock);
        kinds
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
    ///
    /// The monitor knows the device; it just has no link to it. See
    /// [`Error::Disconnected`] for a link that went away mid-operation.
    NotConnected(DeviceId),

    /// GATT characteristic not found.
    ///
    /// Means the device's discovered GATT table has no such characteristic. On
    /// btleplug that includes the case where discovery has not run yet — its
    /// characteristic lookup is a table the caller populates with
    /// [`discover_services`](crate::monitor::GattClient::discover_services).
    CharacteristicNotFound(CharacteristicUuid),

    /// Connection timeout.
    ConnectionTimeout,

    /// Invalid argument.
    ///
    /// Also what the crate raises when an operation asks a GATT object for something
    /// its properties do not support (reading a characteristic that is not readable,
    /// subscribing to one that does not notify): the uuid resolved, the object just
    /// cannot do this.
    InvalidArgument(String),

    /// Backend not available (not compiled into this build).
    ///
    /// Raised by whoever maps a configured backend name onto a monitor — a monitor
    /// of a backend that was not compiled in cannot exist to raise it. See the
    /// module doc's table.
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
    ///
    /// The link was there and went: the peripheral dropped out, powered off, or its
    /// D-Bus object disappeared while a connect, read, write or subscription was in
    /// flight. The event stream reports the same moment as
    /// [`DeviceEvent::DeviceRemoved`](crate::DeviceEvent::DeviceRemoved) or a
    /// [`DeviceUpdated`](crate::DeviceEvent::DeviceUpdated) naming
    /// [`Connected`](crate::UpdateField::Connected).
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
            Error::CharacteristicNotFound(uuid) => {
                write!(f, "Characteristic not found: {}", uuid.0)
            }
            Error::ConnectionTimeout => write!(f, "Connection timeout"),
            Error::InvalidArgument(msg) => write!(f, "Invalid argument: {}", msg),
            Error::BackendUnavailable {
                required,
                available,
            } => {
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
        }
    }
}

impl std::error::Error for Error {}

/// Fallback mapping for a `?` over a BlueZ call.
///
/// This is as specific as a mapping can be without knowing which device and call
/// produced the failure; the operations a caller can act on — connect, read, write,
/// subscribe, discovery — map through `bluer::map_gatt_error` instead, where both are
/// known. `ErrorKind::NotSupported` is the one kind worth naming here: it means the
/// operation is not one this object offers, which is a bad request rather than a
/// radio failure.
#[cfg(feature = "bluer")]
impl From<bluer::Error> for Error {
    fn from(err: bluer::Error) -> Self {
        // `kind` is a field, not a method.
        match err.kind {
            bluer::ErrorKind::InvalidArguments | bluer::ErrorKind::NotSupported => {
                Error::InvalidArgument(err.message)
            }
            kind => Error::BackendError {
                backend: BackendKind::Bluer,
                message: format!("{kind}: {}", err.message),
            },
        }
    }
}

/// Fallback mapping for a `?` over a btleplug call; see the bluer equivalent.
#[cfg(feature = "btleplug")]
impl From<btleplug::Error> for Error {
    fn from(err: btleplug::Error) -> Self {
        match err {
            btleplug::Error::Uuid(e) => Error::from(e),
            btleplug::Error::InvalidBDAddr(e) => {
                Error::InvalidArgument(format!("Invalid Bluetooth address: {e}"))
            }
            e => Error::BackendError {
                backend: BackendKind::Btleplug,
                message: e.to_string(),
            },
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
    use crate::types::{CharacteristicUuid, DeviceId};

    #[test]
    fn test_backend_kind_display() {
        assert_eq!(format!("{}", BackendKind::Bluer), "bluer");
        assert_eq!(format!("{}", BackendKind::Btleplug), "btleplug");
    }

    #[test]
    fn test_error_display() {
        let err = Error::DeviceNotFound(DeviceId("AA:BB:CC:DD:EE:FF".to_string()));
        assert_eq!(format!("{}", err), "Device not found: AA:BB:CC:DD:EE:FF");

        let err = Error::CharacteristicNotFound(
            CharacteristicUuid::parse_str("00002a00-0000-1000-8000-00805f9b34fb").unwrap(),
        );
        assert_eq!(
            format!("{}", err),
            "Characteristic not found: 00002a00-0000-1000-8000-00805f9b34fb"
        );

        let err = Error::ConnectionTimeout;
        assert_eq!(format!("{}", err), "Connection timeout");

        let err = Error::Disconnected(DeviceId("AA:BB:CC:DD:EE:FF".to_string()));
        assert_eq!(format!("{}", err), "Device disconnected: AA:BB:CC:DD:EE:FF");

        let err = Error::GattError {
            device_id: DeviceId("AA:BB:CC:DD:EE:FF".to_string()),
            operation: "read_characteristic".to_string(),
            message: "not permitted".to_string(),
        };
        assert_eq!(
            format!("{}", err),
            "GATT error on device AA:BB:CC:DD:EE:FF: read_characteristic - not permitted"
        );
    }

    #[test]
    fn every_variant_names_the_device_or_argument_it_failed_on() {
        // The reason `Internal` is gone: it said neither which device nor which
        // call, so a consumer could not route it anywhere `BackendError` could not.
        let id = DeviceId("AA:BB:CC:DD:EE:FF".to_string());
        let variants: Vec<Error> = vec![
            Error::InitFailed("no adapter".into()),
            Error::BackendError {
                backend: BackendKind::Mock,
                message: "radio refused".into(),
            },
            Error::DeviceNotFound(id.clone()),
            Error::NotConnected(id.clone()),
            Error::CharacteristicNotFound(
                CharacteristicUuid::parse_str("00002a00-0000-1000-8000-00805f9b34fb").unwrap(),
            ),
            Error::ConnectionTimeout,
            Error::InvalidArgument("not readable".into()),
            Error::BackendUnavailable {
                required: BackendKind::Bluer,
                available: vec![BackendKind::Mock],
            },
            Error::ScanAlreadyInProgress,
            Error::NotScanning,
            Error::Disconnected(id),
            Error::GattError {
                device_id: DeviceId("AA:BB:CC:DD:EE:FF".to_string()),
                operation: "subscribe".into(),
                message: "session stopped".into(),
            },
        ];

        for variant in variants {
            let rendered = variant.to_string();
            assert!(
                !rendered.is_empty() && rendered != "Internal error: ",
                "every variant has to say something a log line can act on: {variant:?}"
            );
        }
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
