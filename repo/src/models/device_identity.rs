//! Models for the derived device-identity tables.
//!
//! These four rows are what the identity batch *believes*, derived from
//! `occurrences`; nothing in the capture path writes them and a reprocess rebuilds
//! them from scratch. That is why none of them carries a signature: they are not
//! assertions a node is attesting to, they are the local inference layered on top of
//! occurrences that are.
//!
//! [`DeviceIdentity`] is the stable entity, [`DeviceAddressLink`] the volatile
//! identifier-to-entity mapping, [`CoOccurrenceEvent`] a raw co-presence, and
//! [`AssociationEdge`] the rollup of those co-presences into something queryable.

use chrono::{DateTime, Utc};
use sqlx::FromRow;
use uuid::Uuid;

use crate::models::enums::{BleAddressType, IdentityResolutionMethod};
use crate::types::H3Index;

/// A stable device identity as the resolver currently infers it.
///
/// Keyed by [`Self::fingerprint_hash`] rather than by address: the address is the
/// thing that changes, and an identity that changed with it would not be one.
#[derive(Debug, Clone, FromRow)]
pub struct DeviceIdentity {
    /// Database-assigned identity. Deliberately not derived from the device: the
    /// whole problem is that no device-supplied value is stable enough to derive it
    /// from.
    pub identity_id: Uuid,
    /// The features the identity was built from. See the column comment in
    /// `202609271430_create_device_identity_tables.sql` for the required keys.
    pub fingerprint: serde_json::Value,
    /// SHA-256 over the canonical fingerprint — the natural key a re-run looks up by.
    pub fingerprint_hash: Vec<u8>,
    /// Resolver confidence, 0..1.
    pub confidence_score: f32,
    /// How the identity was established.
    pub resolution_method: IdentityResolutionMethod,
    /// Earliest occurrence folded into this identity.
    pub first_seen: DateTime<Utc>,
    /// Latest occurrence folded into this identity.
    pub last_seen: DateTime<Utc>,
    /// Occurrences folded into this identity by the pass that wrote the row.
    pub observation_count: i32,
    /// Resolver build that wrote this row, so a decision can be traced to the model
    /// that produced it.
    pub resolver_version: String,
    /// When this pass wrote the row.
    pub computed_at: DateTime<Utc>,
}

impl DeviceIdentity {
    /// Starts a builder.
    pub fn builder() -> DeviceIdentityBuilder {
        DeviceIdentityBuilder::default()
    }
}

/// Builder for [`DeviceIdentity`] rows, which the batch writes from scratch.
#[derive(Debug, Default)]
pub struct DeviceIdentityBuilder {
    identity_id: Option<Uuid>,
    fingerprint: Option<serde_json::Value>,
    fingerprint_hash: Option<Vec<u8>>,
    confidence_score: Option<f32>,
    resolution_method: Option<IdentityResolutionMethod>,
    first_seen: Option<DateTime<Utc>>,
    last_seen: Option<DateTime<Utc>>,
    observation_count: Option<i32>,
    resolver_version: Option<String>,
}

impl DeviceIdentityBuilder {
    /// Sets the identity id. Left unset, the column default assigns one.
    pub fn identity_id(mut self, identity_id: Uuid) -> Self {
        self.identity_id = Some(identity_id);
        self
    }

    /// Sets the fingerprint object.
    pub fn fingerprint(mut self, fingerprint: serde_json::Value) -> Self {
        self.fingerprint = Some(fingerprint);
        self
    }

    /// Sets the hash of the canonical fingerprint.
    pub fn fingerprint_hash(mut self, hash: Vec<u8>) -> Self {
        self.fingerprint_hash = Some(hash);
        self
    }

    /// Sets the resolver confidence.
    pub fn confidence(mut self, confidence: f32) -> Self {
        self.confidence_score = Some(confidence);
        self
    }

    /// Sets how the identity was established.
    pub fn resolution_method(mut self, method: IdentityResolutionMethod) -> Self {
        self.resolution_method = Some(method);
        self
    }

    /// Sets the first-seen/last-seen window together.
    pub fn seen_window(mut self, first_seen: DateTime<Utc>, last_seen: DateTime<Utc>) -> Self {
        self.first_seen = Some(first_seen);
        self.last_seen = Some(last_seen);
        self
    }

    /// Sets the number of observations folded in.
    pub fn observation_count(mut self, count: i32) -> Self {
        self.observation_count = Some(count);
        self
    }

    /// Sets the resolver build that produced this row.
    pub fn resolver_version(mut self, version: impl Into<String>) -> Self {
        self.resolver_version = Some(version.into());
        self
    }

    /// Builds the row.
    ///
    /// `computed_at` is not a field: the row is stamped by the database, so a batch
    /// that mis-told the clock cannot write a pass dated in the future.
    pub fn build(self) -> DeviceIdentity {
        DeviceIdentity {
            identity_id: self.identity_id.unwrap_or_else(Uuid::now_v7),
            fingerprint: self
                .fingerprint
                .expect("fingerprint is required: it is what the identity is"),
            fingerprint_hash: self
                .fingerprint_hash
                .expect("fingerprint_hash is required: it is the natural key"),
            confidence_score: self.confidence_score.unwrap_or(0.0),
            resolution_method: self
                .resolution_method
                .expect("resolution_method is required: an inference should say how it was made"),
            first_seen: self.first_seen.expect("first_seen is required"),
            last_seen: self.last_seen.expect("last_seen is required"),
            observation_count: self.observation_count.unwrap_or(1),
            resolver_version: self
                .resolver_version
                .expect("resolver_version is required: a decision must be traceable to its model")
                .to_string(),
            computed_at: Utc::now(),
        }
    }
}

/// A rotating device identifier linked to the identity that currently explains it.
///
/// The link is scoped to the node whose occurrences justify it. That is not
/// bookkeeping: a platform-assigned identifier hashes to a different value on every
/// host, so a cross-node link between two of them would be an assertion that nothing
/// supports.
#[derive(Debug, Clone, FromRow)]
pub struct DeviceAddressLink {
    /// The identifier as `occurrences.device_hash` stores it — SHA-256 over the
    /// 6-byte MAC, or over a tagged platform identifier.
    pub device_hash: Vec<u8>,
    /// The node whose occurrences justify the link.
    pub observer_node_id: Vec<u8>,
    /// The identity the identifier was linked to.
    pub identity_id: Uuid,
    /// The MAC itself, when the identifier was one.
    pub address: Option<Vec<u8>>,
    /// Which kind of BLE address, when it was one.
    pub address_type: Option<BleAddressType>,
    /// How the link was justified.
    pub method: IdentityResolutionMethod,
    /// Confidence in the link, 0..1. Weaker than the identity's own confidence
    /// whenever the link came from an inference rather than the address.
    pub confidence: f32,
    /// Earliest occurrence of this identifier inside the identity's window.
    pub first_seen: DateTime<Utc>,
    /// Latest occurrence of this identifier inside the identity's window.
    pub last_seen: DateTime<Utc>,
    /// How many occurrences of this identifier were folded in.
    pub observation_count: i32,
    /// When the pass that wrote this row ran.
    pub computed_at: DateTime<Utc>,
}

/// One window in which two identities were seen together by one node.
///
/// The raw half of the association story: [`AssociationEdge`] is what a query reads,
/// this is what makes a strength score auditable back to the sightings behind it.
#[derive(Debug, Clone, FromRow)]
pub struct CoOccurrenceEvent {
    /// The lower of the two identity ids. See [`canonical_pair`].
    pub identity_a: Uuid,
    /// The higher of the two identity ids.
    pub identity_b: Uuid,
    /// The node that saw both. Two devices seen by different nodes are not known to
    /// be together, however close their cells.
    pub node_id: Vec<u8>,
    /// The macro cell the overlap falls in.
    pub geo_cell_macro: H3Index,
    /// Start of the overlap window.
    pub window_start: DateTime<Utc>,
    /// End of the overlap window.
    pub window_end: DateTime<Utc>,
    /// Paired observations inside the window.
    pub sample_count: i32,
    /// Distance between the two devices at the overlap, when both were located.
    pub distance_m: Option<f32>,
    /// When this event was derived.
    pub generated_at: DateTime<Utc>,
}

/// The rollup of [`CoOccurrenceEvent`]s between two identities.
#[derive(Debug, Clone, FromRow)]
pub struct AssociationEdge {
    /// The lower of the two identity ids. See [`canonical_pair`].
    pub identity_a: Uuid,
    /// The higher of the two identity ids.
    pub identity_b: Uuid,
    /// Total paired observations.
    pub co_occurrence_count: i32,
    /// Distinct macro cells the co-presence was seen in.
    pub distinct_geo_cells: i32,
    /// Distinct UTC days the co-presence was seen on.
    pub distinct_days: i32,
    /// Earliest co-presence.
    pub first_seen: DateTime<Utc>,
    /// Latest co-presence.
    pub last_seen: DateTime<Utc>,
    /// Composite of the three counts, weighting diversity over volume.
    pub association_strength: f32,
    /// How far through the events this aggregate runs — what a reader compares
    /// against `max(window_start)` to tell a stale edge from an unreached one.
    pub computed_through: DateTime<Utc>,
    /// When this aggregate was recomputed.
    pub computed_at: DateTime<Utc>,
}

/// Co-presence counts for one pair, before they are turned into a score.
///
/// The rollup and the score are kept apart on purpose: the SQL says what the events
/// were, [`crate::repositories::association_repo::strength_for`] says what they mean,
/// and only the second one is a policy someone might want to argue with.
#[derive(Debug, Clone, FromRow)]
pub struct AssociationAggregate {
    /// The lower of the two identity ids.
    pub identity_a: Uuid,
    /// The higher of the two identity ids.
    pub identity_b: Uuid,
    /// Total paired observations.
    pub co_occurrence_count: i32,
    /// Distinct macro cells.
    pub distinct_geo_cells: i32,
    /// Distinct UTC days.
    pub distinct_days: i32,
    /// Earliest co-presence.
    pub first_seen: DateTime<Utc>,
    /// Latest co-presence.
    pub last_seen: DateTime<Utc>,
}

/// Orders a pair of identities, so an unordered relationship is stored once.
///
/// `co_occurrence_events` and `association_edges` both keep `identity_a < identity_b`
/// under a CHECK. Canonicalising here rather than in every query is what stops each
/// new reader from re-deriving the rule — and getting it wrong in a way that silently
/// double-counts. Returns `None` for a pair of one identity with itself, which is not
/// a relationship.
pub fn canonical_pair(a: Uuid, b: Uuid) -> Option<(Uuid, Uuid)> {
    match a.cmp(&b) {
        std::cmp::Ordering::Less => Some((a, b)),
        std::cmp::Ordering::Greater => Some((b, a)),
        std::cmp::Ordering::Equal => None,
    }
}
