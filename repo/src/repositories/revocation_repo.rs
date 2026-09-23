//! Repository for node revocation data.
//!
//! This module provides database access for tracking revoked nodes
//! and generating Revocation Status Lists (RSLs).
//!
//! Every method takes an [`sqlx::Executor`], like the other repositories here, so
//! a revocation can be written as part of a larger transaction (a CA enrolling a
//! replacement key while revoking the old one, for instance) rather than only
//! against a pool.

use crate::error::RepoError;
use crate::models::revocation::RevokedNode;
use crate::models::NodeStatus;
use chrono::Utc;
use sqlx::Executor;
use std::collections::HashSet;

type Result<T> = std::result::Result<T, RepoError>;

/// Repository for managing node revocations.
pub struct RevocationRepository;

impl RevocationRepository {
    /// Record a node revocation in the database.
    ///
    /// # Arguments
    ///
    /// * `executor` - Pool or transaction
    /// * `revoked_node` - The revocation entry to record
    ///
    /// # Returns
    ///
    /// * `Ok(RevokedNode)` - The recorded revocation with database-generated fields
    /// * `Err(RepoError)` - If recording failed
    pub async fn create<'e, E>(executor: E, revoked_node: &RevokedNode) -> Result<RevokedNode>
    where
        E: Executor<'e, Database = sqlx::Postgres>,
    {
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
        .fetch_one(executor)
        .await?;

        Ok(row)
    }

    /// Check if a specific node is revoked.
    ///
    /// # Arguments
    ///
    /// * `executor` - Pool or transaction
    /// * `node_id` - The node ID to check
    ///
    /// # Returns
    ///
    /// * `Ok(true)` - Node is revoked
    /// * `Ok(false)` - Node is not revoked
    /// * `Err(RepoError)` - If the check failed
    pub async fn is_revoked<'e, E>(executor: E, node_id: &[u8]) -> Result<bool>
    where
        E: Executor<'e, Database = sqlx::Postgres>,
    {
        let exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM node_revocations WHERE node_id = $1)",
        )
        .bind(node_id)
        .fetch_one(executor)
        .await?;

        Ok(exists)
    }

    /// Get revocation details for a specific node.
    ///
    /// # Arguments
    ///
    /// * `executor` - Pool or transaction
    /// * `node_id` - The node ID to look up
    ///
    /// # Returns
    ///
    /// * `Ok(Some(RevokedNode))` - Revocation details found
    /// * `Ok(None)` - Node is not revoked
    /// * `Err(RepoError)` - If the lookup failed
    pub async fn get_revocation<'e, E>(executor: E, node_id: &[u8]) -> Result<Option<RevokedNode>>
    where
        E: Executor<'e, Database = sqlx::Postgres>,
    {
        let revocation =
            sqlx::query_as::<_, RevokedNode>("SELECT * FROM node_revocations WHERE node_id = $1")
                .bind(node_id)
                .fetch_optional(executor)
                .await?;

        Ok(revocation)
    }

    /// Get all currently revoked nodes.
    ///
    /// This is used for generating Revocation Status Lists (RSLs).
    ///
    /// # Arguments
    ///
    /// * `executor` - Pool or transaction
    ///
    /// # Returns
    ///
    /// * `Ok(Vec<RevokedNode>)` - List of all revoked nodes
    /// * `Err(RepoError)` - If the query failed
    pub async fn get_all_revoked<'e, E>(executor: E) -> Result<Vec<RevokedNode>>
    where
        E: Executor<'e, Database = sqlx::Postgres>,
    {
        let revocations =
            sqlx::query_as::<_, RevokedNode>("SELECT * FROM node_revocations ORDER BY revoked_at")
                .fetch_all(executor)
                .await?;

        Ok(revocations)
    }

    /// Get all revoked node IDs (for building in-memory sets).
    ///
    /// # Arguments
    ///
    /// * `executor` - Pool or transaction
    ///
    /// # Returns
    ///
    /// * `Ok(HashSet<Vec<u8>>)` - Set of all revoked node IDs
    /// * `Err(RepoError)` - If the query failed
    pub async fn get_revoked_node_ids<'e, E>(executor: E) -> Result<HashSet<Vec<u8>>>
    where
        E: Executor<'e, Database = sqlx::Postgres>,
    {
        let rows = sqlx::query_scalar::<_, Vec<u8>>("SELECT node_id FROM node_revocations")
            .fetch_all(executor)
            .await?;

        Ok(rows.into_iter().collect())
    }

    /// Get the latest RSL sequence number.
    ///
    /// # Arguments
    ///
    /// * `executor` - Pool or transaction
    /// * `ca_id` - The CA identifier
    ///
    /// # Returns
    ///
    /// * `Ok(Some(i64))` - Latest sequence number
    /// * `Ok(None)` - No RSLs issued yet
    /// * `Err(RepoError)` - If the query failed
    pub async fn get_latest_rsl_sequence<'e, E>(executor: E, ca_id: &str) -> Result<Option<i64>>
    where
        E: Executor<'e, Database = sqlx::Postgres>,
    {
        let seq = sqlx::query_scalar::<_, Option<i64>>(
            "SELECT COALESCE(MAX(rsl_sequence_number), 0) FROM node_revocations WHERE revoked_by = $1",
        )
        .bind(ca_id)
        .fetch_one(executor)
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
    /// * `executor` - Pool or transaction
    /// * `node_id` - The node ID to update
    /// * `status` - The new status
    ///
    /// # Returns
    ///
    /// * `Ok(())` - Status updated
    /// * `Err(RepoError)` - If the update failed
    pub async fn update_node_status<'e, E>(
        executor: E,
        node_id: &[u8],
        status: NodeStatus,
    ) -> Result<()>
    where
        E: Executor<'e, Database = sqlx::Postgres>,
    {
        sqlx::query("UPDATE nodes SET status = $1, last_seen_at = $2 WHERE node_id = $3")
            // Bound as the enum it is. `NodeStatus` declares `node_status` as its
            // SQL type name; casting it to `i16` here instead sends a smallint,
            // which the server rejects with "column status is of type node_status
            // but expression is of type smallint".
            .bind(status)
            .bind(Utc::now())
            .bind(node_id)
            .execute(executor)
            .await?;

        Ok(())
    }

    /// Revoke a node: record the revocation and mark the node revoked.
    ///
    /// The two writes are one fact — a revocation that exists while the registry
    /// still calls the node active is exactly the inconsistency a verifier trips
    /// over — so they happen in one transaction and either both land or neither
    /// does. That is why this takes a pool rather than an `Executor`: beginning a
    /// transaction is the one thing an `Executor` cannot do. A caller already
    /// inside a transaction should call [`create`](Self::create) and
    /// [`update_node_status`](Self::update_node_status) itself.
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
    pub async fn revoke_node(
        pool: &sqlx::PgPool,
        revoked_node: &RevokedNode,
    ) -> Result<RevokedNode> {
        let mut tx = pool.begin().await?;

        let revocation = Self::create(&mut *tx, revoked_node).await?;
        Self::update_node_status(&mut *tx, &revoked_node.node_id, NodeStatus::Revoked).await?;

        tx.commit().await?;

        Ok(revocation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::Pool;

    // `migrations` points at the crate that owns the schema: `#[sqlx::test]`
    // provisions a throwaway database per test, and the only schema worth testing
    // against is the one `db up` applies.
    #[sqlx::test(migrations = "../db/src/migrations")]
    async fn test_check_revoked_status(pool: sqlx::PgPool) {
        // First, ensure the node doesn't exist
        let wrapper = Pool::from_pool(pool.clone());
        let not_revoked = RevocationRepository::is_revoked(wrapper.as_pool(), &[99u8; 32])
            .await
            .unwrap();
        assert!(!not_revoked);
    }

    #[sqlx::test(migrations = "../db/src/migrations")]
    async fn test_get_all_revoked(pool: sqlx::PgPool) {
        // For now, just test that we can query an empty table
        let wrapper = Pool::from_pool(pool.clone());
        let revoked = RevocationRepository::get_all_revoked(wrapper.as_pool())
            .await
            .unwrap();
        assert_eq!(revoked.len(), 0);
    }

    /// Registers a node so the revocation has something to point at.
    async fn node(pool: &sqlx::PgPool) -> Vec<u8> {
        let node_id = vec![0x5au8; 32];
        crate::repositories::NodeRepository::register(
            pool,
            &node_id,
            crate::models::NodeType::Full,
            &[0x5bu8; 32],
            b"ca-credential",
            None,
            &[],
        )
        .await
        .unwrap();

        node_id
    }

    // The status column is a `node_status` enum. Binding the Rust enum sends it as
    // one; binding an integer made every status update fail at the server, which
    // is the defect this pins.
    #[sqlx::test(migrations = "../db/src/migrations")]
    async fn update_node_status_writes_the_enum_not_a_number(pool: sqlx::PgPool) {
        let node_id = node(&pool).await;

        RevocationRepository::update_node_status(&pool, &node_id, NodeStatus::Revoked)
            .await
            .expect("updating a node status should be accepted by the server");

        let status: NodeStatus = sqlx::query_scalar("SELECT status FROM nodes WHERE node_id = $1")
            .bind(&node_id)
            .fetch_one(&pool)
            .await
            .unwrap();

        assert_eq!(status, NodeStatus::Revoked);
    }

    #[sqlx::test(migrations = "../db/src/migrations")]
    async fn revoke_node_writes_the_record_and_the_status(pool: sqlx::PgPool) {
        let node_id = node(&pool).await;

        let entry = RevokedNode {
            node_id: node_id.clone(),
            revoked_at: Utc::now(),
            revoked_by: "ca-test".to_string(),
            reason: 1,
            signing_public_key: vec![0x5bu8; 32],
            ca_credential: b"ca-credential".to_vec(),
            rsl_sequence_number: 1,
            notes: Some("compromised in test".to_string()),
        };

        let recorded = RevocationRepository::revoke_node(&pool, &entry)
            .await
            .expect("a revocation should be recorded and the node marked revoked");

        assert_eq!(recorded.node_id, node_id);
        assert!(RevocationRepository::is_revoked(&pool, &node_id)
            .await
            .unwrap());

        let status: NodeStatus = sqlx::query_scalar("SELECT status FROM nodes WHERE node_id = $1")
            .bind(&node_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, NodeStatus::Revoked);
    }
}
