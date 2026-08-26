//! Revocation status management for CA credentials.
//!
//! This module provides structures and traits for managing node revocation,
//! allowing network participants to determine:
//! 1. Whether to record data from a node (occurrence verification)
//! 2. Whether to allow a node to connect (handshake/connection management)

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::error::CaError;

// Local Result type alias for this module
type RevResult<T> = std::result::Result<T, CaError>;

/// Revocation status for a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevocationStatus {
    /// Node is not revoked (credential is valid).
    Valid,
    
    /// Node has been revoked.
    Revoked,
    
    /// Cannot determine revocation status (cache expired, network error).
    Unknown,
}

/// Reasons for revocation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[repr(u8)]
pub enum RevocationReason {
    /// Unspecified reason.
    Unspecified = 0,
    
    /// Private key compromise suspected.
    KeyCompromise = 1,
    
    /// CA compromise (requires full re-issuance).
    CaCompromise = 2,
    
    /// Node ceased operation (decommissioned).
    CeasedOperation = 3,
    
    /// Policy violation (spam, attacks, etc.).
    PolicyViolation = 4,
    
    /// Superseded by new credential (key rotation).
    Superseded = 5,
    
    /// Temporary hold (pending investigation).
    Hold = 6,
}

impl RevocationReason {
    /// Convert from u8 to RevocationReason.
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Unspecified),
            1 => Some(Self::KeyCompromise),
            2 => Some(Self::CaCompromise),
            3 => Some(Self::CeasedOperation),
            4 => Some(Self::PolicyViolation),
            5 => Some(Self::Superseded),
            6 => Some(Self::Hold),
            _ => None,
        }
    }
    
    /// Convert to u8.
    pub fn as_u8(&self) -> u8 {
        *self as u8
    }
}

/// Individual revocation entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevokedNode {
    /// The node's identifier (SHA-256 of signing public key).
    pub node_id: Vec<u8>,
    
    /// When the node was revoked.
    pub revoked_at: DateTime<Utc>,
    
    /// Reason for revocation.
    pub reason: RevocationReason,
    
    /// The node's signing public key (for verification).
    pub signing_public_key: Vec<u8>,
    
    /// Optional notes about the revocation (for auditing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl RevokedNode {
    /// Create a new revocation entry.
    pub fn new(
        node_id: Vec<u8>,
        revoked_at: DateTime<Utc>,
        reason: RevocationReason,
        signing_public_key: Vec<u8>,
    ) -> Self {
        Self {
            node_id,
            revoked_at,
            reason,
            signing_public_key,
            notes: None,
        }
    }
    
    /// Create a new revocation entry with notes.
    pub fn with_notes(
        node_id: Vec<u8>,
        revoked_at: DateTime<Utc>,
        reason: RevocationReason,
        signing_public_key: Vec<u8>,
        notes: String,
    ) -> Self {
        Self {
            node_id,
            revoked_at,
            reason,
            signing_public_key,
            notes: Some(notes),
        }
    }
}

/// Revocation Status List (RSL).
///
/// A CA-signed snapshot of revoked node identifiers at a point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevocationStatusList {
    /// CA identifier that issued this RSL.
    pub issuer_id: String,
    
    /// When this RSL was signed.
    pub issued_at: DateTime<Utc>,
    
    /// When this RSL expires (must fetch fresh copy after this time).
    pub expires_at: DateTime<Utc>,
    
    /// Sequence number for incremental updates.
    pub sequence_number: u64,
    
    /// List of revoked node identifiers.
    pub revocations: Vec<RevokedNode>,
    
    /// CA signature over the entire structure.
    pub signature: Vec<u8>,
}

impl RevocationStatusList {
    /// Create a new RSL builder.
    pub fn builder(issuer_id: impl Into<String>) -> RslBuilder {
        RslBuilder::new(issuer_id)
    }
    
    /// Check if the RSL is currently valid (not expired).
    pub fn is_valid_now(&self) -> bool {
        let now = Utc::now();
        now >= self.issued_at && now < self.expires_at
    }
    
    /// Check if the RSL has expired.
    pub fn is_expired(&self) -> bool {
        Utc::now() >= self.expires_at
    }
    
    /// Get the age of the RSL.
    pub fn age(&self) -> chrono::Duration {
        Utc::now() - self.issued_at
    }
    
    /// Check if a specific node_id is in the revocation list.
    pub fn is_node_revoked(&self, node_id: &[u8]) -> bool {
        self.revocations.iter().any(|r| &r.node_id == node_id)
    }
    
    /// Get revocation entry for a specific node_id.
    pub fn get_revocation(&self, node_id: &[u8]) -> Option<&RevokedNode> {
        self.revocations.iter().find(|r| &r.node_id == node_id)
    }
    
    /// Get the number of revoked nodes.
    pub fn revocation_count(&self) -> usize {
        self.revocations.len()
    }
}

/// Builder for creating RevocationStatusList instances.
pub struct RslBuilder {
    issuer_id: String,
    sequence_number: u64,
    revocations: Vec<RevokedNode>,
    validity_days: u64,
}

impl RslBuilder {
    /// Create a new RSL builder.
    pub fn new(issuer_id: impl Into<String>) -> Self {
        Self {
            issuer_id: issuer_id.into(),
            sequence_number: 0,
            revocations: Vec::new(),
            validity_days: 1, // Default: RSL valid for 1 day
        }
    }
    
    /// Set the sequence number.
    pub fn sequence_number(mut self, seq: u64) -> Self {
        self.sequence_number = seq;
        self
    }
    
    /// Add a revocation entry.
    pub fn add_revocation(mut self, revocation: RevokedNode) -> Self {
        self.revocations.push(revocation);
        self
    }
    
    /// Add multiple revocation entries.
    pub fn add_revocations(mut self, revocations: impl IntoIterator<Item = RevokedNode>) -> Self {
        for rev in revocations {
            self.revocations.push(rev);
        }
        self
    }
    
    /// Set RSL validity period in days.
    pub fn validity_days(mut self, days: u64) -> Self {
        self.validity_days = days;
        self
    }
    
    /// Build the RSL (unsigned - caller must sign it).
    pub fn build_unsigned(self) -> RevocationStatusList {
        let issued_at = Utc::now();
        let expires_at = issued_at + chrono::Duration::days(self.validity_days as i64);
        
        RevocationStatusList {
            issuer_id: self.issuer_id,
            issued_at,
            expires_at,
            sequence_number: self.sequence_number,
            revocations: self.revocations,
            signature: Vec::new(), // To be filled by signer
        }
    }
}

/// Context for revocation check.
#[derive(Debug, Clone)]
pub struct CheckContext {
    /// Where this check is being performed.
    pub location: CheckLocation,
    
    /// Age of the revocation cache.
    pub cache_age: chrono::Duration,
    
    /// Maximum acceptable staleness.
    pub max_staleness: chrono::Duration,
}

impl CheckContext {
    /// Create a new check context.
    pub fn new(location: CheckLocation, cache_age: chrono::Duration, max_staleness: chrono::Duration) -> Self {
        Self {
            location,
            cache_age,
            max_staleness,
        }
    }
    
    /// Check if the cache is fresh enough.
    pub fn is_cache_fresh(&self) -> bool {
        let age_std = self.cache_age.to_std().unwrap_or_else(|_| chrono::Duration::MAX.to_std().unwrap());
        let max_std = self.max_staleness.to_std().unwrap_or_else(|_| chrono::Duration::MAX.to_std().unwrap());
        age_std <= max_std
    }
}

/// Where the revocation check is happening.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckLocation {
    /// During occurrence verification (recording data).
    OccurrenceVerification,
    
    /// During P2P handshake (establishing connection).
    Handshake,
    
    /// During credential enrollment.
    Enrollment,
    
    /// Administrative check (manual review).
    Admin,
}

/// Revocation check decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Accept the node.
    Accept,
    
    /// Reject the node.
    Reject,
    
    /// Accept but log warning (for Unknown status with lenient policy).
    AcceptWithWarning,
    
    /// Defer decision (need fresh revocation data).
    Defer,
}

/// Revocation policy determines how to handle uncertain revocation status.
pub trait RevocationPolicy: Send + Sync {
    /// Decide whether to accept a node based on revocation status.
    fn evaluate(&self, node_id: &[u8], status: RevocationStatus, context: &CheckContext) -> Decision;
}

/// Policy for occurrence verification: prefer availability, accept unknown.
pub struct DataRecordingPolicy {
    /// Fail-closed if cache is too stale.
    pub fail_on_stale_cache: bool,
    
    /// Log warning for unknown status but still accept.
    pub warn_on_unknown: bool,
    
    /// Maximum cache staleness before refusing to check.
    pub max_cache_age: chrono::Duration,
}

impl Default for DataRecordingPolicy {
    fn default() -> Self {
        Self {
            fail_on_stale_cache: true,
            warn_on_unknown: true,
            max_cache_age: chrono::Duration::hours(24),
        }
    }
}

impl RevocationPolicy for DataRecordingPolicy {
    fn evaluate(&self, _node_id: &[u8], status: RevocationStatus, ctx: &CheckContext) -> Decision {
        match status {
            RevocationStatus::Valid => Decision::Accept,
            RevocationStatus::Revoked => Decision::Reject,
            RevocationStatus::Unknown => {
                if ctx.cache_age > self.max_cache_age && self.fail_on_stale_cache {
                    Decision::Defer // Need fresh data before recording
                } else if self.warn_on_unknown {
                    Decision::AcceptWithWarning
                } else {
                    Decision::Accept
                }
            }
        }
    }
}

/// Policy for P2P handshakes: fail-closed on any uncertainty.
pub struct ConnectionPolicy {
    /// Reject if cache is stale (even slightly).
    pub require_fresh_cache: bool,
    
    /// Reject if revocation status is unknown.
    pub reject_unknown: bool,
}

impl Default for ConnectionPolicy {
    fn default() -> Self {
        Self {
            require_fresh_cache: true,
            reject_unknown: true,
        }
    }
}

impl RevocationPolicy for ConnectionPolicy {
    fn evaluate(&self, _node_id: &[u8], status: RevocationStatus, ctx: &CheckContext) -> Decision {
        match status {
            RevocationStatus::Valid => Decision::Accept,
            RevocationStatus::Revoked => Decision::Reject,
            RevocationStatus::Unknown => {
                if self.reject_unknown {
                    Decision::Reject
                } else if ctx.cache_age > chrono::Duration::hours(1) {
                    Decision::Defer
                } else {
                    Decision::Accept
                }
            }
        }
    }
}

/// Trait for checking node revocation status.
///
/// Implementations can use different strategies:
/// - Full RSL (complete list of revoked nodes)
/// - Bloom filter (space-efficient, probabilistic)
/// - CRL (Certificate Revocation List, X.509 compatible)
/// - OCSP-like (online query to CA)
pub trait RevocationChecker: Send + Sync {
    /// Check if a node is revoked.
    ///
    /// # Returns
    ///
    /// - `Ok(RevocationStatus::Valid)` - Node is not revoked
    /// - `Ok(RevocationStatus::Revoked)` - Node is revoked
    /// - `Ok(RevocationStatus::Unknown)` - Cannot determine status
    /// - `Err(...)` - Error checking status
    fn is_revoked(&self, node_id: &[u8]) -> RevResult<RevocationStatus>;
    
    /// Check if the revocation data is fresh enough for policy.
    fn is_fresh(&self) -> bool;
    
    /// Get the age of the current revocation data.
    fn cache_age(&self) -> chrono::Duration;
}

/// In-memory RSL-based revocation checker.
///
/// Loads complete RSL into memory and performs lookup for revocation checks.
#[derive(Debug, Clone)]
pub struct InMemoryRslChecker {
    /// In-memory index of revoked node_ids for fast lookup.
    revoked_set: std::collections::HashSet<Vec<u8>>,
    
    /// Current RSL (for metadata).
    current_rsl: Option<Arc<RevocationStatusList>>,
    
    /// When the RSL was loaded.
    loaded_at: DateTime<Utc>,
    
    /// Maximum acceptable staleness.
    max_staleness: chrono::Duration,
}

impl InMemoryRslChecker {
    /// Create a new in-memory RSL checker.
    pub fn new(max_staleness: chrono::Duration) -> Self {
        Self {
            revoked_set: std::collections::HashSet::new(),
            current_rsl: None,
            loaded_at: Utc::now(),
            max_staleness,
        }
    }
    
    /// Create from a loaded RSL.
    pub fn from_rsl(rsl: RevocationStatusList, max_staleness: chrono::Duration) -> Self {
        let mut revoked_set = std::collections::HashSet::new();
        for revocation in &rsl.revocations {
            revoked_set.insert(revocation.node_id.clone());
        }
        
        Self {
            revoked_set,
            current_rsl: Some(Arc::new(rsl)),
            loaded_at: Utc::now(),
            max_staleness,
        }
    }
    
    /// Update with a new RSL.
    pub fn update_rsl(&mut self, rsl: RevocationStatusList) {
        self.revoked_set.clear();
        for revocation in &rsl.revocations {
            self.revoked_set.insert(revocation.node_id.clone());
        }
        
        self.current_rsl = Some(Arc::new(rsl));
        self.loaded_at = Utc::now();
    }
    
    /// Get the current RSL.
    pub fn current_rsl(&self) -> Option<&RevocationStatusList> {
        self.current_rsl.as_deref()
    }
    
    /// Check if the cache is fresh.
    pub fn is_cache_fresh(&self) -> bool {
        let age = Utc::now() - self.loaded_at;
        age <= self.max_staleness
    }
}

impl RevocationChecker for InMemoryRslChecker {
    fn is_revoked(&self, node_id: &[u8]) -> RevResult<RevocationStatus> {
        Ok(if self.revoked_set.contains(node_id) {
            RevocationStatus::Revoked
        } else {
            RevocationStatus::Valid
        })
    }
    
    fn is_fresh(&self) -> bool {
        self.is_cache_fresh()
    }
    
    fn cache_age(&self) -> chrono::Duration {
        Utc::now() - self.loaded_at
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_revocation_reason_conversion() {
        for i in 0..=7 {
            let reason = RevocationReason::from_u8(i);
            if i <= 6 {
                assert!(reason.is_some());
                assert_eq!(reason.unwrap().as_u8(), i);
            } else {
                assert!(reason.is_none());
            }
        }
    }
    
    #[test]
    fn test_rsl_builder() {
        let revoked_node = RevokedNode::new(
            vec![1u8; 32],
            Utc::now(),
            RevocationReason::KeyCompromise,
            vec![2u8; 32],
        );
        
        let rsl = RevocationStatusList::builder("test-ca")
            .sequence_number(1)
            .add_revocation(revoked_node)
            .validity_days(7)
            .build_unsigned();
        
        assert_eq!(rsl.issuer_id, "test-ca");
        assert_eq!(rsl.sequence_number, 1);
        assert_eq!(rsl.revocation_count(), 1);
        assert!(rsl.is_valid_now());
    }
    
    #[test]
    fn test_rsl_node_revocation_check() {
        let revoked_node_id = vec![1u8; 32];
        let active_node_id = vec![2u8; 32];
        
        let revoked_node = RevokedNode::new(
            revoked_node_id.clone(),
            Utc::now(),
            RevocationReason::KeyCompromise,
            vec![3u8; 32],
        );
        
        let rsl = RevocationStatusList::builder("test-ca")
            .add_revocation(revoked_node)
            .build_unsigned();
        
        assert!(rsl.is_node_revoked(&revoked_node_id));
        assert!(!rsl.is_node_revoked(&active_node_id));
    }
    
    #[test]
    fn test_data_recording_policy() {
        let policy = DataRecordingPolicy::default();
        
        let ctx = CheckContext::new(
            CheckLocation::OccurrenceVerification,
            chrono::Duration::hours(1),
            chrono::Duration::hours(24),
        );
        
        assert_eq!(policy.evaluate(&[1u8; 32], RevocationStatus::Valid, &ctx), Decision::Accept);
        assert_eq!(policy.evaluate(&[1u8; 32], RevocationStatus::Revoked, &ctx), Decision::Reject);
        assert_eq!(policy.evaluate(&[1u8; 32], RevocationStatus::Unknown, &ctx), Decision::AcceptWithWarning);
    }
    
    #[test]
    fn test_connection_policy() {
        let policy = ConnectionPolicy::default();
        
        let ctx = CheckContext::new(
            CheckLocation::Handshake,
            chrono::Duration::hours(1),
            chrono::Duration::hours(24),
        );
        
        assert_eq!(policy.evaluate(&[1u8; 32], RevocationStatus::Valid, &ctx), Decision::Accept);
        assert_eq!(policy.evaluate(&[1u8; 32], RevocationStatus::Revoked, &ctx), Decision::Reject);
        assert_eq!(policy.evaluate(&[1u8; 32], RevocationStatus::Unknown, &ctx), Decision::Reject);
    }
    
    #[test]
    fn test_in_memory_checker() {
        let revoked_node = RevokedNode::new(
            vec![1u8; 32],
            Utc::now(),
            RevocationReason::KeyCompromise,
            vec![2u8; 32],
        );
        
        let rsl = RevocationStatusList::builder("test-ca")
            .add_revocation(revoked_node)
            .build_unsigned();
        
        let checker = InMemoryRslChecker::from_rsl(rsl, chrono::Duration::hours(24));
        
        assert_eq!(checker.is_revoked(&[1u8; 32]).unwrap(), RevocationStatus::Revoked);
        assert_eq!(checker.is_revoked(&[2u8; 32]).unwrap(), RevocationStatus::Valid);
        assert!(checker.is_fresh());
    }
}
