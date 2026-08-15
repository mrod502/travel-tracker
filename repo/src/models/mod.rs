pub mod enums;
pub mod occurrence;

// Unified occurrence model (supports Bluetooth, WiFi, and future signal types)
pub use occurrence::{Occurrence, OccurrenceBuilder, OccurrenceRelay, SignalType};
pub use enums::*;

/// Helper function to convert MAC address string to bytes
pub fn mac_address_from_string(s: &str) -> Result<Vec<u8>, std::num::ParseIntError> {
    s.split(':')
        .map(|byte| u8::from_str_radix(byte, 16))
        .collect()
}
