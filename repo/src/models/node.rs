//! Node registry models.
//!
//! This module provides models for the nodes table, which tracks
//! known peers in the network along with their credentials.

use chrono::{DateTime, Utc};
use sqlx::FromRow;

use crate::models::enums::{NodeStatus, NodeType};

/// A node in the BTMon network.
///
/// This represents a participant in the network, whether it's a
/// full node, aggregator, or signal node.
#[derive(Debug, Clone, FromRow)]
pub struct Node {
    /// The node's unique identifier (SHA-256 of signing public key).
    pub node_id: Vec<u8>,

    /// The type of node (full, aggregator, signal).
    pub node_type: NodeType,

    /// MTLS certificate fingerprint (for transport authentication).
    /// NULL for signal nodes which don't use TLS.
    pub mtls_cert_fingerprint: Option<String>,

    /// The node's Ed25519 signing public key (32 bytes).
    pub signing_public_key: Vec<u8>,

    /// The signing algorithm used (default: 'ed25519').
    pub signing_key_algo: String,

    /// CA credential issued at enrollment (CA signature over node data).
    pub ca_credential: Vec<u8>,

    /// Fixed latitude (for stationary nodes).
    pub fixed_lat: Option<f64>,

    /// Fixed longitude (for stationary nodes).
    pub fixed_lon: Option<f64>,

    /// H3 geo-cells owned by this node (for full nodes).
    pub owns_geo_cells: Vec<sqlx::postgres::types::Oid>,

    /// When the node was registered.
    pub registered_at: DateTime<Utc>,

    /// When the node was last seen.
    pub last_seen_at: Option<DateTime<Utc>>,

    /// Current status of the node (active, inactive, revoked).
    pub status: NodeStatus,
}

impl Node {
    /// Create a new node builder.
    pub fn builder() -> NodeBuilder {
        NodeBuilder::default()
    }

    /// Get the signing public key as a reference.
    pub fn signing_public_key(&self) -> &[u8] {
        &self.signing_public_key
    }

    /// Get the CA credential as a reference.
    pub fn ca_credential(&self) -> &[u8] {
        &self.ca_credential
    }

    /// Check if this node is revoked.
    pub fn is_revoked(&self) -> bool {
        self.status == NodeStatus::Revoked
    }

    /// Check if this node is active.
    pub fn is_active(&self) -> bool {
        self.status == NodeStatus::Active
    }
}

/// Builder for Node instances.
#[derive(Debug, Default)]
pub struct NodeBuilder {
    node_id: Option<Vec<u8>>,
    node_type: Option<NodeType>,
    mtls_cert_fingerprint: Option<String>,
    signing_public_key: Option<Vec<u8>>,
    signing_key_algo: Option<String>,
    ca_credential: Option<Vec<u8>>,
    fixed_lat: Option<f64>,
    fixed_lon: Option<f64>,
    owns_geo_cells: Option<Vec<sqlx::postgres::types::Oid>>,
    registered_at: Option<DateTime<Utc>>,
    last_seen_at: Option<DateTime<Utc>>,
    status: Option<NodeStatus>,
}

impl NodeBuilder {
    /// Set the node ID.
    pub fn node_id(mut self, node_id: &[u8]) -> Self {
        self.node_id = Some(node_id.to_vec());
        self
    }

    /// Set the node type.
    pub fn node_type(mut self, node_type: NodeType) -> Self {
        self.node_type = Some(node_type);
        self
    }

    /// Set the MTLS certificate fingerprint.
    pub fn mtls_cert_fingerprint(mut self, fingerprint: impl Into<String>) -> Self {
        self.mtls_cert_fingerprint = Some(fingerprint.into());
        self
    }

    /// Set the signing public key.
    pub fn signing_public_key(mut self, key: &[u8]) -> Self {
        self.signing_public_key = Some(key.to_vec());
        self
    }

    /// Set the signing key algorithm.
    pub fn signing_key_algo(mut self, algo: impl Into<String>) -> Self {
        self.signing_key_algo = Some(algo.into());
        self
    }

    /// Set the CA credential.
    pub fn ca_credential(mut self, credential: &[u8]) -> Self {
        self.ca_credential = Some(credential.to_vec());
        self
    }

    /// Set the fixed location.
    pub fn fixed_location(mut self, lat: f64, lon: f64) -> Self {
        self.fixed_lat = Some(lat);
        self.fixed_lon = Some(lon);
        self
    }

    /// Set the owned geo-cells.
    pub fn owns_geo_cells(mut self, cells: Vec<sqlx::postgres::types::Oid>) -> Self {
        self.owns_geo_cells = Some(cells);
        self
    }

    /// Set the registered at timestamp.
    pub fn registered_at(mut self, timestamp: DateTime<Utc>) -> Self {
        self.registered_at = Some(timestamp);
        self
    }

    /// Set the last seen at timestamp.
    pub fn last_seen_at(mut self, timestamp: DateTime<Utc>) -> Self {
        self.last_seen_at = Some(timestamp);
        self
    }

    /// Set the status.
    pub fn status(mut self, status: NodeStatus) -> Self {
        self.status = Some(status);
        self
    }

    /// Build the Node.
    pub fn build(self) -> Node {
        Node {
            node_id: self.node_id.expect("node_id is required"),
            node_type: self.node_type.expect("node_type is required"),
            mtls_cert_fingerprint: self.mtls_cert_fingerprint,
            signing_public_key: self.signing_public_key.expect("signing_public_key is required"),
            signing_key_algo: self.signing_key_algo.unwrap_or_else(|| "ed25519".to_string()),
            ca_credential: self.ca_credential.expect("ca_credential is required"),
            fixed_lat: self.fixed_lat,
            fixed_lon: self.fixed_lon,
            owns_geo_cells: self.owns_geo_cells.unwrap_or_default(),
            registered_at: self.registered_at.unwrap_or_else(Utc::now),
            last_seen_at: self.last_seen_at,
            status: self.status.unwrap_or(NodeStatus::Active),
        }
    }
}
