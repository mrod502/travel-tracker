//! Repository for node revocation data.
//!
//! This module provides database access for tracking revoked nodes
//! and generating Revocation Status Lists (RSLs).

use crate::error::RepoError;
use crate::models::revocation::RevokedNode;
use crate::pool::Pool;
use chrono::Utc;
use std::collections::HashSet;

type Result<T> = std::result::Result<T, RepoError>;

/// Repository for managing node revocations.
pub struct RevocationRepository;

impl RevocationRepository {
    /// Record a node revocation in the database.
    ///
    /// # Arguments
    ///
    /// * `pool` - Database connection pool
    /// * `revoked_node` - The revocation entry to record
    ///
    /// # Returns
    ///
    /// * `Ok(RevokedNode)` - The recorded revocation with database-generated fields
    /// * `Err(RepoError)` - If recording failed
    pub async fn create(pool: &Pool, revoked_node: &RevokedNode) -> Result<RevokedNode> {
        let row = sqlx::query_as::<_, RevokedNode>(
            r#"
            INSERT INTO node_revocations (
                node_id,
                revoked_at,
                revoked_by,
                reason,
                signing_public_key,
                ca_credential,
                rsl_sequence_number,
                notes
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            RETURNING *
            "#,
        )
        .bind(&revoked_node.node_id)
        .bind(revoked_node.revoked_at)
        .bind(&revoked_node.revoked_by)
        .bind(revoked_node.reason)
        .bind(&revoked_node.signing_public_key)
        .bind(&revoked_node.ca_credential)
        .bind(revoked_node.rsl_sequence_number)
        .bind(&revoked_node.notes)
        .fetch_one(pool.as_pool())
        .await?;

        Ok(row)
    }

    /// Check if a specific node is revoked.
    ///
    /// # Arguments
    ///
    /// * `pool` - Database connection pool
    /// * `node_id` - The node ID to check
    ///
    /// # Returns
    ///
    /// * `Ok(true)` - Node is revoked
    /// * `Ok(false)` - Node is not revoked
    /// * `Err(RepoError)` - If the check failed
    pub async fn is_revoked(pool: &Pool, node_id: &[u8]) -> Result<bool> {
        let exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM node_revocations WHERE node_id = $1)",
        )
        .bind(node_id)
        .fetch_one(pool.as_pool())
        .await?;

        Ok(exists)
    }

    /// Get revocation details for a specific node.
    ///
    /// # Arguments
    ///
    /// * `pool` - Database connection pool
    /// * `node_id` - The node ID to look up
    ///
    /// # Returns
    ///
    /// * `Ok(Some(RevokedNode))` - Revocation details found
    /// * `Ok(None)` - Node is not revoked
    /// * `Err(RepoError)` - If the lookup failed
    pub async fn get_revocation(pool: &Pool, node_id: &[u8]) -> Result<Option<RevokedNode>> {
        let revocation = sqlx::query_as::<_, RevokedNode>(
            "SELECT * FROM node_revocations WHERE node_id = $1",
        )
        .bind(node_id)
        .fetch_optional(pool.as_pool())
        .await?;

        Ok(revocation)
    }

    /// Get all currently revoked nodes.
    ///
    /// This is used for generating Revocation Status Lists (RSLs).
    ///
    /// # Arguments
    ///
    /// * `pool` - Database connection pool
    ///
    /// # Returns
    ///
    /// * `Ok(Vec<RevokedNode>)` - List of all revoked nodes
    /// * `Err(RepoError)` - If the query failed
    pub async fn get_all_revoked(pool: &Pool) -> Result<Vec<RevokedNode>> {
        let revocations = sqlx::query_as::<_, RevokedNode>(
            "SELECT * FROM node_revocations ORDER BY revoked_at",
        )
        .fetch_all(pool.as_pool())
        .await?;

        Ok(revocations)
    }

    /// Get all revoked node IDs (for building in-memory sets).
    ///
    /// # Arguments
    ///
    /// * `pool` - Database connection pool
    ///
    /// # Returns
    ///
    /// * `Ok(HashSet<Vec<u8>>)` - Set of all revoked node IDs
    /// * `Err(RepoError)` - If the query failed
    pub async fn get_revoked_node_ids(pool: &Pool) -> Result<HashSet<Vec<u8>>> {
        let rows = sqlx::query_scalar::<_, Vec<u8>>(
            "SELECT node_id FROM node_revocations",
        )
        .fetch_all(pool.as_pool())
        .await?;

        Ok(rows.into_iter().collect())
    }

    /// Get the latest RSL sequence number.
    ///
    /// # Arguments
    ///
    /// * `pool` - Database connection pool
    /// * `ca_id` - The CA identifier
    ///
    /// # Returns
    ///
    /// * `Ok(Some(i64))` - Latest sequence number
    /// * `Ok(None)` - No RSLs issued yet
    /// * `Err(RepoError)` - If the query failed
    pub async fn get_latest_rsl_sequence(pool: &Pool, ca_id: &str) -> Result<Option<i64>> {
        let seq = sqlx::query_scalar::<_, Option<i64>>(
            "SELECT COALESCE(MAX(rsl_sequence_number), 0) FROM node_revocations WHERE revoked_by = $1",
        )
        .bind(ca_id)
        .fetch_one(pool.as_pool())
        .await?;

        Ok(seq)
    }

    /// Update a node's status in the nodes table.
    ///
    /// This should be called when a node is revoked to keep the
    /// local node cache in sync.
    ///
    /// # Arguments
    ///
    /// * `pool` - Database connection pool
    /// * `node_id` - The node ID to update
    /// * `status` - The new status
    ///
    /// # Returns
    ///
    /// * `Ok(())` - Status updated
    /// * `Err(RepoError)` - If the update failed
    pub async fn update_node_status(
        pool: &Pool,
        node_id: &[u8],
        status: crate::models::NodeStatus,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE nodes SET status = $1, last_seen_at = $2 WHERE node_id = $3",
        )
        .bind(status as i16)
        .bind(Utc::now())
        .bind(node_id)
        .execute(pool.as_pool())
        .await?;

        Ok(())
    }

    /// Revoke a node (convenience method that does both revocation record and status update).
    ///
    /// # Arguments
    ///
    /// * `pool` - Database connection pool
    /// * `revoked_node` - The revocation entry
    ///
    /// # Returns
    ///
    /// * `Ok(RevokedNode)` - The recorded revocation
    /// * `Err(RepoError)` - If recording failed
    pub async fn revoke_node(pool: &Pool, revoked_node: &RevokedNode) -> Result<RevokedNode> {
        // Record the revocation
        let revocation = Self::create(pool, revoked_node).await?;

        // Update the node status
        Self::update_node_status(pool, &revoked_node.node_id, crate::models::NodeStatus::Revoked).await?;

        Ok(revocation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::Pool;

    #[sqlx::test]
    async fn test_check_revoked_status(pool: sqlx::PgPool) {
        // First, ensure the node doesn't exist
        let wrapper = Pool::from_pool(pool.clone());
        let not_revoked = RevocationRepository::is_revoked(&wrapper, &[99u8; 32]).await.unwrap();
        assert!(!not_revoked);

        // Create and record a revocation (skip this part since we need a valid node first)
        // For now, just test that non-existent nodes are not revoked
    }

    #[sqlx::test]
    async fn test_get_all_revoked(pool: sqlx::PgPool) {
        // For now, just test that we can query an empty table
        let wrapper = Pool::from_pool(pool.clone());
        let revoked = RevocationRepository::get_all_revoked(&wrapper).await.unwrap();
        assert_eq!(revoked.len(), 0);
    }
}
