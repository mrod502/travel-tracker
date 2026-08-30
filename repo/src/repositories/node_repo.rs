//! Repository for the `nodes` registry table.
//!
//! `occurrences.origin_node_id` has a foreign key to `nodes(node_id)`, so a node
//! must have a registry row before any occurrence it signs can be stored. Rows
//! are written at CA enrollment: `ca_credential` is NOT NULL by design because
//! it is what lets a verifier trace a signing key back to the CA without a live
//! CA call.

use sqlx::Executor;

use crate::error::RepoError;
use crate::models::NodeType;

/// Database access for the `nodes` registry.
pub struct NodeRepository;

impl NodeRepository {
    /// Insert a node's registry row, refreshing credentials if it already exists.
    ///
    /// Enrollment is idempotent, so re-enrolling after a credential renewal
    /// updates the row instead of failing on the primary key. `registered_at` and
    /// `status` are deliberately left untouched — renewing a credential must not
    /// resurrect a revoked node.
    ///
    /// # Arguments
    ///
    /// * `executor` - Pool or transaction
    /// * `node_id` - 32-byte SHA-256 of the signing public key
    /// * `node_type` - Role of the node (full, light, aggregator, signal)
    /// * `signing_public_key` - Raw Ed25519 public key (32 bytes)
    /// * `ca_credential` - Serialized CA credential issued at enrollment
    /// * `fixed_location` - `(latitude, longitude)` for a stationary node
    pub async fn register<'e, E>(
        executor: E,
        node_id: &[u8],
        node_type: NodeType,
        signing_public_key: &[u8],
        ca_credential: &[u8],
        fixed_location: Option<(f64, f64)>,
    ) -> Result<(), RepoError>
    where
        E: Executor<'e, Database = sqlx::Postgres>,
    {
        let (fixed_lat, fixed_lon) = match fixed_location {
            Some((lat, lon)) => (Some(lat), Some(lon)),
            None => (None, None),
        };

        sqlx::query(
            r#"
INSERT INTO nodes (
    node_id,
    node_type,
    signing_public_key,
    ca_credential,
    fixed_lat,
    fixed_lon
) VALUES (
    $1, $2, $3, $4, $5, $6
)
ON CONFLICT (node_id) DO UPDATE SET
    node_type = EXCLUDED.node_type,
    signing_public_key = EXCLUDED.signing_public_key,
    ca_credential = EXCLUDED.ca_credential,
    fixed_lat = EXCLUDED.fixed_lat,
    fixed_lon = EXCLUDED.fixed_lon
            "#,
        )
        .bind(node_id)
        .bind(node_type)
        .bind(signing_public_key)
        .bind(ca_credential)
        .bind(fixed_lat)
        .bind(fixed_lon)
        .execute(executor)
        .await?;

        Ok(())
    }

    /// Check whether a node has a registry row.
    ///
    /// An unregistered node cannot store anything: the `origin_node_id` foreign
    /// key rejects every insert.
    pub async fn is_registered<'e, E>(executor: E, node_id: &[u8]) -> Result<bool, RepoError>
    where
        E: Executor<'e, Database = sqlx::Postgres>,
    {
        let row = sqlx::query("SELECT 1 FROM nodes WHERE node_id = $1")
            .bind(node_id)
            .fetch_optional(executor)
            .await?;

        Ok(row.is_some())
    }
}
