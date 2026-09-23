//! Node revocation models.
//!
//! This module provides models for tracking node revocations.

use chrono::{DateTime, Utc};
use sqlx::FromRow;

/// A revoked node in the BTMon network.
#[derive(Debug, Clone, FromRow)]
pub struct RevokedNode {
    /// The node's unique identifier (SHA-256 of signing public key).
    pub node_id: Vec<u8>,

    /// When the node was revoked.
    pub revoked_at: DateTime<Utc>,

    /// The CA identifier that issued the revocation.
    pub revoked_by: String,

    /// Revocation reason code (0-6 per RFC 5280 adaptation).
    /// 0 = unspecified, 1 = key_compromise, 2 = ca_compromise,
    /// 3 = ceased_operation, 4 = policy_violation, 5 = superseded, 6 = hold
    pub reason: i32,

    /// The node's Ed25519 signing public key (32 bytes).
    pub signing_public_key: Vec<u8>,

    /// The CA credential that was revoked (for audit trail).
    pub ca_credential: Vec<u8>,

    /// RSL sequence number where this was first revoked.
    pub rsl_sequence_number: i64,

    /// Optional audit notes.
    pub notes: Option<String>,
}

impl RevokedNode {
    /// Create a new revocation entry builder.
    pub fn builder() -> RevokedNodeBuilder {
        RevokedNodeBuilder::default()
    }

    /// Get the revocation reason as a descriptive string.
    pub fn reason_description(&self) -> &'static str {
        match self.reason {
            0 => "Unspecified",
            1 => "Key Compromise",
            2 => "CA Compromise",
            3 => "Ceased Operation",
            4 => "Policy Violation",
            5 => "Superseded",
            6 => "On Hold",
            _ => "Unknown",
        }
    }

    /// Check if this is a key compromise revocation.
    pub fn is_key_compromise(&self) -> bool {
        self.reason == 1
    }

    /// Check if this is a policy violation.
    pub fn is_policy_violation(&self) -> bool {
        self.reason == 4
    }
}

/// Builder for RevokedNode instances.
#[derive(Debug, Default)]
pub struct RevokedNodeBuilder {
    node_id: Option<Vec<u8>>,
    revoked_at: Option<DateTime<Utc>>,
    revoked_by: Option<String>,
    reason: Option<i32>,
    signing_public_key: Option<Vec<u8>>,
    ca_credential: Option<Vec<u8>>,
    rsl_sequence_number: Option<i64>,
    notes: Option<String>,
}

impl RevokedNodeBuilder {
    /// Set the node ID.
    pub fn node_id(mut self, node_id: &[u8]) -> Self {
        self.node_id = Some(node_id.to_vec());
        self
    }

    /// Set the revoked at timestamp.
    pub fn revoked_at(mut self, timestamp: DateTime<Utc>) -> Self {
        self.revoked_at = Some(timestamp);
        self
    }

    /// Set the revoked by (CA identifier).
    pub fn revoked_by(mut self, ca_id: impl Into<String>) -> Self {
        self.revoked_by = Some(ca_id.into());
        self
    }

    /// Set the revocation reason.
    pub fn reason(mut self, reason: i32) -> Self {
        self.reason = Some(reason);
        self
    }

    /// Set the signing public key.
    pub fn signing_public_key(mut self, key: &[u8]) -> Self {
        self.signing_public_key = Some(key.to_vec());
        self
    }

    /// Set the CA credential.
    pub fn ca_credential(mut self, credential: &[u8]) -> Self {
        self.ca_credential = Some(credential.to_vec());
        self
    }

    /// Set the RSL sequence number.
    pub fn rsl_sequence_number(mut self, seq: i64) -> Self {
        self.rsl_sequence_number = Some(seq);
        self
    }

    /// Set the notes.
    pub fn notes(mut self, notes: impl Into<String>) -> Self {
        self.notes = Some(notes.into());
        self
    }

    /// Build the RevokedNode.
    pub fn build(self) -> RevokedNode {
        RevokedNode {
            node_id: self.node_id.expect("node_id is required"),
            revoked_at: self.revoked_at.unwrap_or_else(Utc::now),
            revoked_by: self.revoked_by.expect("revoked_by is required"),
            reason: self.reason.expect("reason is required"),
            signing_public_key: self
                .signing_public_key
                .expect("signing_public_key is required"),
            ca_credential: self.ca_credential.expect("ca_credential is required"),
            rsl_sequence_number: self
                .rsl_sequence_number
                .expect("rsl_sequence_number is required"),
            notes: self.notes,
        }
    }
}
