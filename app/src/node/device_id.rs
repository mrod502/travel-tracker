//! Device identifier normalization across Bluetooth backends.
//!
//! Backends expose device identifiers in platform-specific forms:
//!
//! - BlueZ / `bluer`: colon-separated MAC address (`AA:BB:CC:DD:EE:FF`)
//! - CoreBluetooth (macOS): host-local UUID — CoreBluetooth never exposes the
//!   peripheral's MAC address
//!
//! The canonical payload spec pins `device_address` to `bytes[6]` for BLE, so a
//! platform identifier must not be stuffed into that column. Non-MAC
//! identifiers are hashed into `device_hash` under a domain-separation tag and
//! stored with a NULL address, with the source recorded in `signal_payload`.
//!
//! # Cross-node comparability
//!
//! A MAC-derived hash is globally comparable: two nodes that see the same
//! peripheral report the same `device_hash`. A platform-assigned identifier is
//! unique to the host that assigned it, so its hash is only comparable between
//! occurrences captured by that same host. [`IdentifierSource`] makes the
//! distinction queryable downstream.
//!
//! # Example
//!
//! ```ignore
//! use app::node::device_id::derive_device_identity;
//!
//! let mac = derive_device_identity("AA:BB:CC:DD:EE:FF");
//! assert_eq!(mac.address, Some(vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]));
//!
//! // CoreBluetooth yields a host-local UUID, so there is no address to store.
//! let cb = derive_device_identity("9cc22dd0-ff98-0ed2-b717-fe430544851b");
//! assert_eq!(cb.address, None);
//! assert_eq!(cb.hash.len(), 32);
//! ```

use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Domain-separation tag for platform UUID identifiers.
///
/// MAC-derived hashes are deliberately left untagged to stay compatible with
/// occurrences written before this module existed. The tag also guarantees the
/// two namespaces cannot collide: a 6-byte MAC can never begin with this ASCII
/// prefix.
const UUID_TAG: &[u8] = b"ble:id:uuid:v1:";

/// Domain-separation tag for identifiers that are neither MAC nor UUID.
const OPAQUE_TAG: &[u8] = b"ble:id:opaque:v1:";

/// Where a device identifier came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentifierSource {
    /// Colon-separated MAC address reported by the backend.
    BleMac,

    /// Platform-assigned UUID (e.g. the CoreBluetooth peripheral identifier).
    Uuid,

    /// Any other backend-assigned identifier.
    Opaque,
}

impl IdentifierSource {
    /// Stable string form for `signal_payload`, so stored rows stay
    /// interpretable regardless of Rust enum changes.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BleMac => "ble_mac",
            Self::Uuid => "uuid",
            Self::Opaque => "opaque_id",
        }
    }
}

/// Address and pseudonymous hash derived from a backend device identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceIdentity {
    /// Value for the `device_address` column: the 6-byte MAC when the backend
    /// exposes one, otherwise `None`.
    pub address: Option<Vec<u8>>,

    /// Value for the `device_hash` column (32-byte SHA-256).
    pub hash: Vec<u8>,

    /// How [`DeviceIdentity::hash`] was derived.
    pub source: IdentifierSource,
}

impl DeviceIdentity {
    /// True when the hash is comparable across nodes because it came from a MAC.
    pub fn mac_derived(&self) -> bool {
        self.source == IdentifierSource::BleMac
    }
}

/// Normalize a backend device identifier into an address and device hash.
///
/// Never fails: an unrecognized identifier falls back to a stable hash over its
/// UTF-8 bytes rather than dropping the observation.
///
/// # Arguments
///
/// * `raw_id` - The identifier string as reported by the backend
pub fn derive_device_identity(raw_id: &str) -> DeviceIdentity {
    if let Some(address) = parse_mac_address(raw_id) {
        return DeviceIdentity {
            hash: sha256(&address),
            address: Some(address),
            source: IdentifierSource::BleMac,
        };
    }

    // Hash the UUID's 16 canonical bytes so that hyphenation and letter case in
    // the string form do not change the identity.
    if let Ok(parsed) = Uuid::parse_str(raw_id) {
        return DeviceIdentity {
            hash: sha256(&tagged(UUID_TAG, parsed.as_bytes())),
            address: None,
            source: IdentifierSource::Uuid,
        };
    }

    DeviceIdentity {
        hash: sha256(&tagged(OPAQUE_TAG, raw_id.as_bytes())),
        address: None,
        source: IdentifierSource::Opaque,
    }
}

/// Parse a colon-separated MAC address into 6 bytes.
///
/// Each group must be exactly two hex digits. The strict length check keeps
/// `u8::from_str_radix` quirks (it accepts a leading `+`) out of the MAC path.
fn parse_mac_address(raw_id: &str) -> Option<Vec<u8>> {
    let groups: Vec<&str> = raw_id.split(':').collect();
    if groups.len() != 6 {
        return None;
    }

    groups
        .iter()
        .map(|&group| {
            let mut chars = group.chars();
            match (chars.next(), chars.next(), chars.next()) {
                (Some(hi), Some(lo), None) if hi.is_ascii_hexdigit() && lo.is_ascii_hexdigit() => {
                    u8::from_str_radix(group, 16).ok()
                }
                _ => None,
            }
        })
        .collect()
}

fn tagged(tag: &[u8], identifier: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(tag.len() + identifier.len());
    buf.extend_from_slice(tag);
    buf.extend_from_slice(identifier);
    buf
}

fn sha256(bytes: &[u8]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The MAC path must stay byte-for-byte identical to the original
    /// `SHA-256(mac_bytes)` scheme, otherwise existing rows stop matching.
    #[test]
    fn mac_hash_is_untagged_sha256_of_address_bytes() {
        let identity = derive_device_identity("AA:BB:CC:DD:EE:FF");

        assert_eq!(
            identity.address,
            Some(vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF])
        );
        assert_eq!(identity.source, IdentifierSource::BleMac);
        assert!(identity.mac_derived());
        assert_eq!(identity.hash, sha256(&[0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]));
    }

    #[test]
    fn mac_parsing_is_case_insensitive() {
        assert_eq!(
            derive_device_identity("aa:bb:cc:dd:ee:ff").hash,
            derive_device_identity("AA:BB:CC:DD:EE:FF").hash
        );
    }

    /// Regression: CoreBluetooth reports a host-local UUID, which used to fail
    /// MAC parsing outright and drop every occurrence.
    #[test]
    fn core_bluetooth_uuid_stores_no_address() {
        let identity = derive_device_identity("9cc22dd0-ff98-0ed2-b717-fe430544851b");

        assert_eq!(identity.address, None);
        assert_eq!(identity.source, IdentifierSource::Uuid);
        assert!(!identity.mac_derived());
        assert_eq!(identity.hash.len(), 32);
    }

    #[test]
    fn uuid_hash_ignores_string_formatting() {
        let hyphenated = derive_device_identity("9CC22DD0-FF98-0ED2-B717-FE430544851B");
        let simple = derive_device_identity("9cc22dd0ff980ed2b717fe430544851b");

        assert_eq!(hyphenated.hash, simple.hash);
    }

    #[test]
    fn distinct_uuids_yield_distinct_hashes() {
        let a = derive_device_identity("9cc22dd0-ff98-0ed2-b717-fe430544851b");
        let b = derive_device_identity("598dc612-d610-f89a-3eb8-9d4bd70b1aaa");

        assert_ne!(a.hash, b.hash);
    }

    #[test]
    fn unrecognized_identifier_falls_back_to_opaque() {
        for raw_id in ["some-device-path", ""] {
            let identity = derive_device_identity(raw_id);

            assert_eq!(identity.address, None);
            assert_eq!(identity.source, IdentifierSource::Opaque);
            assert_eq!(
                identity.hash,
                sha256(&tagged(OPAQUE_TAG, raw_id.as_bytes()))
            );
        }
    }

    #[test]
    fn malformed_mac_is_not_treated_as_mac() {
        for raw_id in [
            "AA:BB:CC:DD:EE",
            "AA:BB:CC:DD:EE:FF:00",
            "GG:BB:CC:DD:EE:FF",
            "+1:BB:CC:DD:EE:FF",
            "aabbccddeeff",
        ] {
            let identity = derive_device_identity(raw_id);

            assert_ne!(identity.source, IdentifierSource::BleMac, "{raw_id}");
            assert_eq!(identity.address, None, "{raw_id}");
        }
    }

    /// An identifier that spells out MAC bytes as free text must not land in
    /// the MAC hash namespace, and a UUID hash must not be a bare SHA-256 of
    /// the UUID bytes.
    #[test]
    fn identifier_namespaces_do_not_collide() {
        let mac = derive_device_identity("AA:BB:CC:DD:EE:FF");
        let opaque = derive_device_identity("aabbccddeeff");

        assert_eq!(mac.source, IdentifierSource::BleMac);
        assert_eq!(opaque.source, IdentifierSource::Opaque);
        assert_ne!(mac.hash, opaque.hash);

        let raw_id = "9cc22dd0-ff98-0ed2-b717-fe430544851b";
        let uuid = derive_device_identity(raw_id);
        let raw_bytes = Uuid::parse_str(raw_id).unwrap();

        assert_ne!(uuid.hash, sha256(raw_bytes.as_bytes()));
    }

    #[test]
    fn source_strings_are_stable() {
        assert_eq!(IdentifierSource::BleMac.as_str(), "ble_mac");
        assert_eq!(IdentifierSource::Uuid.as_str(), "uuid");
        assert_eq!(IdentifierSource::Opaque.as_str(), "opaque_id");
    }
}
