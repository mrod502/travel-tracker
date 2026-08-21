//! CBOR encoding and decoding for canonical payloads.
//!
//! This module implements deterministic CBOR encoding as specified in
//! `.knowledge/implementation/roadmap/phase_0/canonical-payload-spec.md`.
//!
//! # Deterministic Encoding
//!
//! The same `CanonicalPayload` instance will always produce identical
//! bytes when encoded. This is critical for cryptographic signing.
//!
//! # Example
//!
//! ```ignore
//! use app::provenance::{
//!     payload::CanonicalPayload,
//!     encode::{encode_payload, decode_payload},
//! };
//!
//! let payload = CanonicalPayload { /* ... */ };
//! let encoded = encode_payload(&payload)?;
//! let decoded = decode_payload(&encoded)?;
//! assert_eq!(payload, decoded);
//! ```

use ciborium::{de::from_reader, ser::into_writer};
use std::io::Cursor;

use super::payload::CanonicalPayload;

/// Error type for encoding/decoding operations.
#[derive(Debug, Clone, PartialEq)]
pub enum EncodeError {
    /// CBOR encoding failed.
    Encoding(String),

    /// CBOR decoding failed.
    Decoding(String),

    /// Invalid payload data.
    InvalidData(String),
}

impl std::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EncodeError::Encoding(msg) => write!(f, "Encoding error: {}", msg),
            EncodeError::Decoding(msg) => write!(f, "Decoding error: {}", msg),
            EncodeError::InvalidData(msg) => write!(f, "Invalid data: {}", msg),
        }
    }
}

impl std::error::Error for EncodeError {}

impl From<ciborium::ser::Error<std::io::Error>> for EncodeError {
    fn from(err: ciborium::ser::Error<std::io::Error>) -> Self {
        EncodeError::Encoding(err.to_string())
    }
}

impl From<ciborium::de::Error<std::io::Error>> for EncodeError {
    fn from(err: ciborium::de::Error<std::io::Error>) -> Self {
        EncodeError::Decoding(err.to_string())
    }
}

/// Result type for encoding operations.
pub type Result<T> = std::result::Result<T, EncodeError>;

/// Encode a canonical payload to CBOR bytes.
///
/// This function produces deterministic output - the same payload will
/// always produce identical bytes.
///
/// # Arguments
///
/// * `payload` - The canonical payload to encode
///
/// # Returns
///
/// * `Ok(Vec<u8>)` - The encoded CBOR bytes
/// * `Err(EncodeError)` - If encoding fails
///
/// # Example
///
/// ```ignore
/// use app::provenance::{payload::CanonicalPayload, encode::encode_payload};
///
/// let payload = CanonicalPayload { /* ... */ };
/// let encoded = encode_payload(&payload)?;
/// ```
pub fn encode_payload(payload: &CanonicalPayload) -> Result<Vec<u8>> {
    let mut buffer = Vec::new();
    into_writer(payload, &mut buffer)?;
    Ok(buffer)
}

/// Decode CBOR bytes to a canonical payload.
///
/// # Arguments
///
/// * `data` - The CBOR bytes to decode
///
/// # Returns
///
/// * `Ok(CanonicalPayload)` - The decoded payload
/// * `Err(EncodeError)` - If decoding fails
///
/// # Example
///
/// ```ignore
/// use app::provenance::{encode::{encode_payload, decode_payload}, payload::CanonicalPayload};
///
/// let payload = CanonicalPayload { /* ... */ };
/// let encoded = encode_payload(&payload)?;
/// let decoded = decode_payload(&encoded)?;
/// assert_eq!(payload, decoded);
/// ```
pub fn decode_payload(data: &[u8]) -> Result<CanonicalPayload> {
    let reader = Cursor::new(data);
    let payload = from_reader(reader)?;
    Ok(payload)
}

/// Verify that encoding is deterministic.
///
/// This function encodes the same payload multiple times and verifies
/// that all encodings produce identical bytes.
///
/// # Arguments
///
/// * `payload` - The payload to test
/// * `iterations` - Number of times to encode (default: 100)
///
/// # Returns
///
/// * `Ok(true)` - All encodings are identical
/// * `Ok(false)` - Encodings differ (determinism violation!)
/// * `Err(EncodeError)` - If encoding fails
pub fn verify_determinism(payload: &CanonicalPayload, iterations: usize) -> Result<bool> {
    if iterations == 0 {
        return Ok(true);
    }

    let first_encoding = encode_payload(payload)?;

    for _ in 1..iterations {
        let encoding = encode_payload(payload)?;
        if encoding != first_encoding {
            return Ok(false);
        }
    }

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_payload() -> CanonicalPayload {
        CanonicalPayload::builder()
            .schema_version(1)
            .signal_type(0)
            .origin_node_id(&vec![0u8; 32])
            .device_hash(&vec![1u8; 32])
            .device_address(&vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF])
            .observed_at_node_local("2026-08-15T12:00:00Z")
            .rssi(-67)
            .tx_power(0)
            .adv_type(0)
            .location(40.6892, -74.0445)
            .signal_payload(b"test payload")
            .advertised_name("TestDevice")
            .build()
    }

    #[test]
    fn test_encode_decode_round_trip() {
        let payload = create_test_payload();
        let encoded = encode_payload(&payload).unwrap();
        let decoded = decode_payload(&encoded).unwrap();

        assert_eq!(payload, decoded);
    }

    #[test]
    fn test_encoding_deterministic() {
        let payload = create_test_payload();
        let is_deterministic = verify_determinism(&payload, 100).unwrap();

        assert!(is_deterministic, "Encoding is not deterministic!");
    }

    #[test]
    fn test_encode_all_optional_fields_null() {
        let payload = CanonicalPayload::builder()
            .schema_version(1)
            .signal_type(0)
            .origin_node_id(&vec![0u8; 32])
            .device_hash(&vec![1u8; 32])
            .observed_at_node_local("2026-08-15T12:00:00Z")
            .rssi(-67)
            .no_tx_power()
            .no_adv_type()
            .no_location()
            .no_signal_payload()
            .no_advertised_name()
            .no_device_address()
            .build();

        let encoded = encode_payload(&payload).unwrap();
        let decoded = decode_payload(&encoded).unwrap();

        assert_eq!(payload, decoded);
    }

    #[test]
    fn test_encode_with_optional_fields() {
        let payload = CanonicalPayload::builder()
            .schema_version(1)
            .signal_type(0)
            .origin_node_id(&vec![0u8; 32])
            .device_hash(&vec![1u8; 32])
            .device_address(&vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF])
            .observed_at_node_local("2026-08-15T12:00:00Z")
            .rssi(-67)
            .tx_power(5)
            .adv_type(1)
            .location(51.5074, -0.1278)
            .signal_payload(b"payload data")
            .advertised_name("MyDevice")
            .build();

        let encoded = encode_payload(&payload).unwrap();
        let decoded = decode_payload(&encoded).unwrap();

        assert_eq!(payload, decoded);
        assert!(decoded.device_address.is_some());
        assert!(decoded.tx_power.is_some());
        assert!(decoded.adv_type.is_some());
        assert!(decoded.location.is_some());
        assert!(decoded.signal_payload.is_some());
        assert!(decoded.advertised_name.is_some());
    }

    #[test]
    fn test_invalid_cbor_decoding_fails() {
        let invalid_data = vec![0xFF, 0xFF, 0xFF, 0xFF]; // Invalid CBOR

        let result = decode_payload(&invalid_data);
        assert!(result.is_err());
    }

    #[test]
    fn test_empty_payload() {
        let payload = CanonicalPayload::builder()
            .signal_type(0)
            .origin_node_id(&vec![0u8; 32])
            .device_hash(&vec![1u8; 32])
            .observed_at_node_local("2026-08-15T12:00:00Z")
            .rssi(0)
            .build();

        let encoded = encode_payload(&payload).unwrap();
        let decoded = decode_payload(&encoded).unwrap();

        assert_eq!(payload, decoded);
    }

    #[test]
    fn test_different_payloads_produce_different_encoded_bytes() {
        let payload1 = CanonicalPayload::builder()
            .signal_type(0)
            .origin_node_id(&vec![0u8; 32])
            .device_hash(&vec![1u8; 32])
            .observed_at_node_local("2026-08-15T12:00:00Z")
            .rssi(-67)
            .build();

        let payload2 = CanonicalPayload::builder()
            .signal_type(0)
            .origin_node_id(&vec![0u8; 32])
            .device_hash(&vec![1u8; 32])
            .observed_at_node_local("2026-08-15T12:00:00Z")
            .rssi(-72) // Different RSSI
            .build();

        let encoded1 = encode_payload(&payload1).unwrap();
        let encoded2 = encode_payload(&payload2).unwrap();

        assert_ne!(encoded1, encoded2);
    }

    #[test]
    fn test_large_payload() {
        let large_payload = vec![0u8; 1024]; // 1KB payload

        let canonical = CanonicalPayload::builder()
            .signal_type(0)
            .origin_node_id(&vec![0u8; 32])
            .device_hash(&vec![1u8; 32])
            .observed_at_node_local("2026-08-15T12:00:00Z")
            .rssi(-67)
            .signal_payload(&large_payload)
            .build();

        let encoded = encode_payload(&canonical).unwrap();
        let decoded = decode_payload(&encoded).unwrap();

        assert_eq!(canonical, decoded);
    }
}
