pub mod device_identity;
pub mod enums;
pub mod node;
pub mod occurrence;
pub mod revocation;

// Unified occurrence model (supports Bluetooth, WiFi, and future signal types)
//
// The device-identity models are the derived half of the schema: what the resolver
// currently believes, rebuildable from occurrences. `canonical_pair` comes along
// because two of those tables store an unordered pair under a CHECK, and the rule
// belongs in one place.
pub use device_identity::{
    canonical_pair, AssociationAggregate, AssociationEdge, CoOccurrenceEvent, DeviceAddressLink,
    DeviceIdentity, DeviceIdentityBuilder,
};
pub use enums::*;
pub use node::Node;
pub use occurrence::{Occurrence, OccurrenceBuilder, OccurrenceRelay, SignalType};
pub use revocation::RevokedNode;

/// Helper function to convert MAC address string to bytes
pub fn mac_address_from_string(s: &str) -> Result<Vec<u8>, std::num::ParseIntError> {
    s.split(':')
        .map(|byte| u8::from_str_radix(byte, 16))
        .collect()
}
