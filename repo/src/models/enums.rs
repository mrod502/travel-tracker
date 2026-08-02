//! PostgreSQL enum type definitions for the database schema
//!
//! These Rust enums map to PostgreSQL enum types and provide type-safe
//! interaction with the database via sqlx's Encode/Decode traits.

use serde::{Deserialize, Serialize};
use sqlx::Type;

// ============================================================================
// NODE ENUMS
// ============================================================================

/// Type of node in the distributed network
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "node_type", rename_all = "snake_case")]
pub enum NodeType {
    Full,
    Light,
    Aggregator,
    Signal,
}

impl NodeType {
    /// Returns all possible node types
    pub fn all() -> &'static [NodeType] {
        &[NodeType::Full, NodeType::Light, NodeType::Aggregator, NodeType::Signal]
    }
}

/// Status of a node in the network
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "node_status", rename_all = "snake_case")]
pub enum NodeStatus {
    Active,
    Suspected,
    Down,
    Revoked,
}

impl NodeStatus {
    /// Returns all possible node statuses
    pub fn all() -> &'static [NodeStatus] {
        &[NodeStatus::Active, NodeStatus::Suspected, NodeStatus::Down, NodeStatus::Revoked]
    }

    /// Returns true if this status indicates the node is operational
    pub fn is_operational(&self) -> bool {
        matches!(self, NodeStatus::Active)
    }
}

// ============================================================================
// BLUETOOTH ENUMS
// ============================================================================

/// Bluetooth LE address type per Bluetooth SIG specification
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "ble_address_type", rename_all = "snake_case")]
pub enum BleAddressType {
    Public,
    RandomStatic,
    RandomResolvable,
    RandomNonresolvable,
}

impl BleAddressType {
    /// Returns all possible address types
    pub fn all() -> &'static [BleAddressType] {
        &[
            BleAddressType::Public,
            BleAddressType::RandomStatic,
            BleAddressType::RandomResolvable,
            BleAddressType::RandomNonresolvable,
        ]
    }

    /// Returns true if this address type is resolvable to an identity
    pub fn is_resolvable(&self) -> bool {
        matches!(self, BleAddressType::RandomResolvable)
    }

    /// Returns true if this is a random address type
    pub fn is_random(&self) -> bool {
        matches!(self, BleAddressType::RandomStatic | BleAddressType::RandomResolvable | BleAddressType::RandomNonresolvable)
    }
}

/// Bluetooth LE advertisement type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "adv_type", rename_all = "snake_case")]
pub enum AdvType {
    ConnectableAdv,
    ScannableAdv,
    BroadcastAdv,
    ExtendedAdv,
}

impl AdvType {
    /// Returns all possible advertisement types
    pub fn all() -> &'static [AdvType] {
        &[AdvType::ConnectableAdv, AdvType::ScannableAdv, AdvType::BroadcastAdv, AdvType::ExtendedAdv]
    }

    /// Returns true if this advertisement type allows connection requests
    pub fn is_connectable(&self) -> bool {
        matches!(self, AdvType::ConnectableAdv | AdvType::ExtendedAdv)
    }

    /// Returns true if this advertisement type allows scan requests
    pub fn is_scannable(&self) -> bool {
        matches!(self, AdvType::ScannableAdv | AdvType::ExtendedAdv)
    }
}

/// Source of location data for an occurrence
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "location_source", rename_all = "snake_case")]
pub enum LocationSource {
    NodeFixed,
    NodeGps,
    Interpolated,
    AggregatorFixed,
}

impl LocationSource {
    /// Returns all possible location sources
    pub fn all() -> &'static [LocationSource] {
        &[LocationSource::NodeFixed, LocationSource::NodeGps, LocationSource::Interpolated, LocationSource::AggregatorFixed]
    }

    /// Returns true if this source indicates a fixed/stationary location
    pub fn is_fixed(&self) -> bool {
        matches!(self, LocationSource::NodeFixed | LocationSource::AggregatorFixed)
    }

    /// Returns true if this source indicates GPS-derived location
    pub fn is_gps(&self) -> bool {
        matches!(self, LocationSource::NodeGps)
    }
}

// ============================================================================
// SYNC ENUMS
// ============================================================================

/// Direction of synchronization between nodes
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "sync_direction", rename_all = "snake_case")]
pub enum SyncDirection {
    Inbound,
    Outbound,
}

impl SyncDirection {
    /// Returns all possible sync directions
    pub fn all() -> &'static [SyncDirection] {
        &[SyncDirection::Inbound, SyncDirection::Outbound]
    }

    /// Returns the opposite direction
    pub fn opposite(&self) -> SyncDirection {
        match self {
            SyncDirection::Inbound => SyncDirection::Outbound,
            SyncDirection::Outbound => SyncDirection::Inbound,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Type;
    use sqlx::postgres::Postgres;

    // ========================================================================
    // NodeType tests
    // ========================================================================

    #[test]
    fn test_node_type_all() {
        let types = NodeType::all();
        assert_eq!(types.len(), 4);
        assert!(types.contains(&NodeType::Full));
        assert!(types.contains(&NodeType::Light));
        assert!(types.contains(&NodeType::Aggregator));
        assert!(types.contains(&NodeType::Signal));
    }

    // ========================================================================
    // NodeStatus tests
    // ========================================================================

    #[test]
    fn test_node_status_all() {
        let statuses = NodeStatus::all();
        assert_eq!(statuses.len(), 4);
        assert!(statuses.contains(&NodeStatus::Active));
        assert!(statuses.contains(&NodeStatus::Suspected));
        assert!(statuses.contains(&NodeStatus::Down));
        assert!(statuses.contains(&NodeStatus::Revoked));
    }

    #[test]
    fn test_node_status_is_operational() {
        assert!(NodeStatus::Active.is_operational());
        assert!(!NodeStatus::Suspected.is_operational());
        assert!(!NodeStatus::Down.is_operational());
        assert!(!NodeStatus::Revoked.is_operational());
    }

    // ========================================================================
    // BleAddressType tests
    // ========================================================================

    #[test]
    fn test_ble_address_type_all() {
        let types = BleAddressType::all();
        assert_eq!(types.len(), 4);
        assert!(types.contains(&BleAddressType::Public));
        assert!(types.contains(&BleAddressType::RandomStatic));
        assert!(types.contains(&BleAddressType::RandomResolvable));
        assert!(types.contains(&BleAddressType::RandomNonresolvable));
    }

    #[test]
    fn test_ble_address_type_is_resolvable() {
        assert!(!BleAddressType::Public.is_resolvable());
        assert!(!BleAddressType::RandomStatic.is_resolvable());
        assert!(BleAddressType::RandomResolvable.is_resolvable());
        assert!(!BleAddressType::RandomNonresolvable.is_resolvable());
    }

    #[test]
    fn test_ble_address_type_is_random() {
        assert!(!BleAddressType::Public.is_random());
        assert!(BleAddressType::RandomStatic.is_random());
        assert!(BleAddressType::RandomResolvable.is_random());
        assert!(BleAddressType::RandomNonresolvable.is_random());
    }

    // ========================================================================
    // AdvType tests
    // ========================================================================

    #[test]
    fn test_adv_type_all() {
        let types = AdvType::all();
        assert_eq!(types.len(), 4);
        assert!(types.contains(&AdvType::ConnectableAdv));
        assert!(types.contains(&AdvType::ScannableAdv));
        assert!(types.contains(&AdvType::BroadcastAdv));
        assert!(types.contains(&AdvType::ExtendedAdv));
    }

    #[test]
    fn test_adv_type_is_connectable() {
        assert!(AdvType::ConnectableAdv.is_connectable());
        assert!(!AdvType::ScannableAdv.is_connectable());
        assert!(!AdvType::BroadcastAdv.is_connectable());
        assert!(AdvType::ExtendedAdv.is_connectable());
    }

    #[test]
    fn test_adv_type_is_scannable() {
        assert!(!AdvType::ConnectableAdv.is_scannable());
        assert!(AdvType::ScannableAdv.is_scannable());
        assert!(!AdvType::BroadcastAdv.is_scannable());
        assert!(AdvType::ExtendedAdv.is_scannable());
    }

    // ========================================================================
    // LocationSource tests
    // ========================================================================

    #[test]
    fn test_location_source_all() {
        let sources = LocationSource::all();
        assert_eq!(sources.len(), 4);
        assert!(sources.contains(&LocationSource::NodeFixed));
        assert!(sources.contains(&LocationSource::NodeGps));
        assert!(sources.contains(&LocationSource::Interpolated));
        assert!(sources.contains(&LocationSource::AggregatorFixed));
    }

    #[test]
    fn test_location_source_is_fixed() {
        assert!(LocationSource::NodeFixed.is_fixed());
        assert!(!LocationSource::NodeGps.is_fixed());
        assert!(!LocationSource::Interpolated.is_fixed());
        assert!(LocationSource::AggregatorFixed.is_fixed());
    }

    #[test]
    fn test_location_source_is_gps() {
        assert!(!LocationSource::NodeFixed.is_gps());
        assert!(LocationSource::NodeGps.is_gps());
        assert!(!LocationSource::Interpolated.is_gps());
        assert!(!LocationSource::AggregatorFixed.is_gps());
    }

    // ========================================================================
    // SyncDirection tests
    // ========================================================================

    #[test]
    fn test_sync_direction_all() {
        let directions = SyncDirection::all();
        assert_eq!(directions.len(), 2);
        assert!(directions.contains(&SyncDirection::Inbound));
        assert!(directions.contains(&SyncDirection::Outbound));
    }

    #[test]
    fn test_sync_direction_opposite() {
        assert_eq!(SyncDirection::Inbound.opposite(), SyncDirection::Outbound);
        assert_eq!(SyncDirection::Outbound.opposite(), SyncDirection::Inbound);
    }

    // ========================================================================
    // SQLX type mapping tests (compile-time only - verifies Type trait impl)
    // ========================================================================

    #[test]
    fn test_type_mappings_compile() {
        // These assertions are compile-time only - if they compile,
        // the Type<Postgres> trait is correctly implemented
        fn assert_type<T: Type<Postgres>>() {}

        assert_type::<NodeType>();
        assert_type::<NodeStatus>();
        assert_type::<BleAddressType>();
        assert_type::<AdvType>();
        assert_type::<LocationSource>();
        assert_type::<SyncDirection>();
    }
}
