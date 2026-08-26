//! Revocation Status List (RSL) management.
//!
//! This module provides traits and implementations for managing RSLs,
//! including database-backed storage and in-memory caching.

use crate::error::Result;
use crate::revocation::{
    InMemoryRslChecker, RevocationStatus, RevocationStatusList, RevokedNode,
};
use async_trait::async_trait;
use chrono::Utc;

/// Trait for managing Revocation Status Lists.
///
/// Implementations can use different backends:
/// - Database-backed: Persistent storage with query capability
/// - In-memory: Fast lookup but no persistence
/// - Hybrid: Cache with database fallback
#[async_trait]
pub trait RslManager: Send + Sync {
    /// Generate a new RSL with current revocations.
    ///
    /// # Arguments
    ///
    /// * `ca_id` - The CA identifier that will issue the RSL
    /// * `validity_days` - How long the RSL should be valid
    ///
    /// # Returns
    ///
    /// * `Ok(RevocationStatusList)` - The generated (unsigned) RSL
    /// * `Err(CaError)` - If generation failed
    async fn generate_rsl(
        &self,
        ca_id: &str,
        validity_days: u64,
    ) -> Result<RevocationStatusList>;

    /// Store an RSL after it has been signed.
    ///
    /// # Arguments
    ///
    /// * `rsl` - The signed RSL to store
    ///
    /// # Returns
    ///
    /// * `Ok(())` - RSL stored successfully
    /// * `Err(CaError)` - If storage failed
    async fn store_rsl(&self, rsl: &RevocationStatusList) -> Result<()>;

    /// Get the latest RSL for a CA.
    ///
    /// # Arguments
    ///
    /// * `ca_id` - The CA identifier
    ///
    /// # Returns
    ///
    /// * `Ok(Some(RevocationStatusList))` - The latest RSL
    /// * `Ok(None)` - No RSL found
    /// * `Err(CaError)` - If lookup failed
    async fn get_latest_rsl(&self, ca_id: &str) -> Result<Option<RevocationStatusList>>;

    /// Get the sequence number for the next RSL.
    ///
    /// # Arguments
    ///
    /// * `ca_id` - The CA identifier
    ///
    /// # Returns
    ///
    /// * `Ok(u64)` - The next sequence number
    /// * `Err(CaError)` - If lookup failed
    async fn next_sequence_number(&self, ca_id: &str) -> Result<u64>;

    /// Record a node revocation.
    ///
    /// # Arguments
    ///
    /// * `node_id` - The node to revoke
    /// * `reason` - Revocation reason code
    /// * `signing_public_key` - The node's signing public key
    /// * `ca_credential` - The CA credential being revoked
    /// * `notes` - Optional audit notes
    ///
    /// # Returns
    ///
    /// * `Ok(RevokedNode)` - The recorded revocation
    /// * `Err(CaError)` - If recording failed
    async fn revoke_node(
        &self,
        node_id: &[u8],
        reason: u8,
        signing_public_key: &[u8],
        ca_credential: &[u8],
        notes: Option<&str>,
    ) -> Result<RevokedNode>;

    /// Check if a node is revoked.
    ///
    /// # Arguments
    ///
    /// * `node_id` - The node ID to check
    ///
    /// # Returns
    ///
    /// * `Ok(RevocationStatus::Valid)` - Node is not revoked
    /// * `Ok(RevocationStatus::Revoked)` - Node is revoked
    /// * `Ok(RevocationStatus::Unknown)` - Cannot determine status
    /// * `Err(CaError)` - If check failed
    async fn is_revoked(&self, node_id: &[u8]) -> Result<RevocationStatus>;

    /// Get a revocation checker that can be used for fast in-memory checks.
    ///
    /// This loads the latest RSL into memory for efficient lookup.
    ///
    /// # Arguments
    ///
    /// * `ca_id` - The CA identifier
    /// * `max_staleness` - Maximum acceptable age of the cache
    ///
    /// # Returns
    ///
    /// * `Ok(InMemoryRslChecker)` - The in-memory checker
    /// * `Err(CaError)` - If loading failed
    async fn get_checker(&self, ca_id: &str, max_staleness: chrono::Duration)
        -> Result<InMemoryRslChecker>;
}

/// Database-backed RSL manager.
///
/// This implementation stores revocations and RSLs in a PostgreSQL database,
/// providing persistence and audit trail capabilities.
#[cfg(feature = "database")]
pub struct DatabaseRslManager {
    /// Database connection pool.
    pool: sqlx::PgPool,
}

#[cfg(feature = "database")]
impl DatabaseRslManager {
    /// Create a new database-backed RSL manager.
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }

    /// Get all revoked nodes from the database.
    async fn get_all_revoked(&self) -> Result<Vec<RevokedNode>> {
        let rows = sqlx::query_as::<_, crate::revocation::RevokedNode>(
            r#"
            SELECT 
                node_id,
                revoked_at,
                revoked_by,
                reason,
                signing_public_key,
                ca_credential,
                rsl_sequence_number,
                notes
            FROM node_revocations
            ORDER BY revoked_at
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| CaError::Database(e.to_string()))?;

        // Convert database model to domain model
        Ok(rows
            .into_iter()
            .map(|row| RevokedNode {
                node_id: row.node_id,
                revoked_at: row.revoked_at,
                reason: crate::revocation::RevocationReason::from_u8(row.reason as u8)
                    .unwrap_or(crate::revocation::RevocationReason::Unspecified),
                signing_public_key: row.signing_public_key,
                notes: row.notes,
            })
            .collect())
    }

    /// Get the latest RSL sequence number for a CA.
    async fn get_latest_sequence(&self, ca_id: &str) -> Result<u64> {
        let seq: Option<i64> = sqlx::query_scalar(
            "SELECT COALESCE(MAX(rsl_sequence_number), 0) FROM node_revocations WHERE revoked_by = $1",
        )
        .bind(ca_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| CaError::Database(e.to_string()))?;

        Ok(seq as u64)
    }
}

#[cfg(feature = "database")]
#[async_trait]
impl RslManager for DatabaseRslManager {
    async fn generate_rsl(
        &self,
        ca_id: &str,
        validity_days: u64,
    ) -> Result<RevocationStatusList> {
        let revoked_nodes = self.get_all_revoked().await?;
        let sequence_number = self.get_latest_sequence(ca_id).await? + 1;

        let mut builder = RevocationStatusList::builder(ca_id)
            .sequence_number(sequence_number)
            .validity_days(validity_days);

        for revoked in revoked_nodes {
            builder = builder.add_revocation(revoked);
        }

        Ok(builder.build_unsigned())
    }

    async fn store_rsl(&self, rsl: &RevocationStatusList) -> Result<()> {
        // Store individual revocations from the RSL
        for revoked_node in &rsl.revocations {
            sqlx::query(
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
                ON CONFLICT (node_id) DO UPDATE SET
                    revoked_at = EXCLUDED.revoked_at,
                    reason = EXCLUDED.reason,
                    rsl_sequence_number = EXCLUDED.rsl_sequence_number
                "#,
            )
            .bind(&revoked_node.node_id)
            .bind(revoked_node.revoked_at)
            .bind(&rsl.issuer_id)
            .bind(revoked_node.reason.as_u8() as i32)
            .bind(&revoked_node.signing_public_key)
            .bind(Vec::<u8>::new()) // ca_credential not available in RSL
            .bind(rsl.sequence_number as i64)
            .bind(&revoked_node.notes)
            .execute(&self.pool)
            .await
            .map_err(|e| CaError::Database(e.to_string()))?;
        }

        Ok(())
    }

    async fn get_latest_rsl(&self, ca_id: &str) -> Result<Option<RevocationStatusList>> {
        // For now, we regenerate the RSL from the database
        // In a full implementation, we would store signed RSLs and retrieve them
        let rsl = self.generate_rsl(ca_id, 1).await?;
        Ok(Some(rsl))
    }

    async fn next_sequence_number(&self, ca_id: &str) -> Result<u64> {
        Ok(self.get_latest_sequence(ca_id).await? + 1)
    }

    async fn revoke_node(
        &self,
        node_id: &[u8],
        reason: u8,
        signing_public_key: &[u8],
        ca_credential: &[u8],
        notes: Option<&str>,
    ) -> Result<RevokedNode> {
        let reason_enum = crate::revocation::RevocationReason::from_u8(reason)
            .unwrap_or(crate::revocation::RevocationReason::Unspecified);

        let revoked_node = RevokedNode::with_notes(
            node_id.to_vec(),
            Utc::now(),
            reason_enum,
            signing_public_key.to_vec(),
            notes.unwrap_or("").to_string(),
        );

        // Insert into database
        sqlx::query(
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
            ) VALUES ($1, $2, $3, $4, $5, $6, 0, $7)
            ON CONFLICT (node_id) DO UPDATE SET
                revoked_at = EXCLUDED.revoked_at,
                reason = EXCLUDED.reason,
                notes = EXCLUDED.notes
            "#,
        )
        .bind(&revoked_node.node_id)
        .bind(revoked_node.revoked_at)
        .bind("pending") // Will be updated when RSL is generated
        .bind(reason as i32)
        .bind(&revoked_node.signing_public_key)
        .bind(ca_credential)
        .bind(&revoked_node.notes)
        .execute(&self.pool)
        .await
        .map_err(|e| CaError::Database(e.to_string()))?;

        Ok(revoked_node)
    }

    async fn is_revoked(&self, node_id: &[u8]) -> Result<RevocationStatus> {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM node_revocations WHERE node_id = $1)",
        )
        .bind(node_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| CaError::Database(e.to_string()))?;

        Ok(if exists {
            RevocationStatus::Revoked
        } else {
            RevocationStatus::Valid
        })
    }

    async fn get_checker(
        &self,
        ca_id: &str,
        max_staleness: chrono::Duration,
    ) -> Result<InMemoryRslChecker> {
        let rsl = self.generate_rsl(ca_id, 1).await?;
        Ok(InMemoryRslChecker::from_rsl(rsl, max_staleness))
    }
}

/// In-memory RSL manager (for testing and single-node deployments).
///
/// This implementation keeps all revocations in memory and does not
/// provide persistence across restarts.
pub struct InMemoryRslManager {
    /// In-memory store of revoked nodes.
    revoked: std::sync::RwLock<std::collections::HashMap<Vec<u8>, RevokedNode>>,
    /// Sequence counter for RSLs.
    sequence: std::sync::atomic::AtomicU64,
}

impl InMemoryRslManager {
    /// Create a new in-memory RSL manager.
    pub fn new() -> Self {
        Self {
            revoked: std::sync::RwLock::new(std::collections::HashMap::new()),
            sequence: std::sync::atomic::AtomicU64::new(0),
        }
    }
}

impl Default for InMemoryRslManager {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl RslManager for InMemoryRslManager {
    async fn generate_rsl(
        &self,
        ca_id: &str,
        validity_days: u64,
    ) -> Result<RevocationStatusList> {
        let revoked_nodes: Vec<RevokedNode> = {
            let guard = self.revoked.read().unwrap();
            guard.values().cloned().collect()
        };

        let sequence_number = self.sequence.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        let mut builder = RevocationStatusList::builder(ca_id)
            .sequence_number(sequence_number)
            .validity_days(validity_days);

        for revoked in revoked_nodes {
            builder = builder.add_revocation(revoked);
        }

        Ok(builder.build_unsigned())
    }

    async fn store_rsl(&self, rsl: &RevocationStatusList) -> Result<()> {
        let mut guard = self.revoked.write().unwrap();
        for revoked_node in &rsl.revocations {
            guard.insert(revoked_node.node_id.clone(), revoked_node.clone());
        }
        Ok(())
    }

    async fn get_latest_rsl(&self, ca_id: &str) -> Result<Option<RevocationStatusList>> {
        let rsl = self.generate_rsl(ca_id, 1).await?;
        Ok(Some(rsl))
    }

    async fn next_sequence_number(&self, _ca_id: &str) -> Result<u64> {
        Ok(self.sequence.load(std::sync::atomic::Ordering::SeqCst) + 1)
    }

    async fn revoke_node(
        &self,
        node_id: &[u8],
        reason: u8,
        signing_public_key: &[u8],
        _ca_credential: &[u8],
        notes: Option<&str>,
    ) -> Result<RevokedNode> {
        let reason_enum = crate::revocation::RevocationReason::from_u8(reason)
            .unwrap_or(crate::revocation::RevocationReason::Unspecified);

        let revoked_node = RevokedNode::with_notes(
            node_id.to_vec(),
            Utc::now(),
            reason_enum,
            signing_public_key.to_vec(),
            notes.unwrap_or("").to_string(),
        );

        let mut guard = self.revoked.write().unwrap();
        guard.insert(node_id.to_vec(), revoked_node.clone());

        Ok(revoked_node)
    }

    async fn is_revoked(&self, node_id: &[u8]) -> Result<RevocationStatus> {
        let guard = self.revoked.read().unwrap();
        Ok(if guard.contains_key(node_id) {
            RevocationStatus::Revoked
        } else {
            RevocationStatus::Valid
        })
    }

    async fn get_checker(
        &self,
        ca_id: &str,
        max_staleness: chrono::Duration,
    ) -> Result<InMemoryRslChecker> {
        let rsl = self.generate_rsl(ca_id, 1).await?;
        Ok(InMemoryRslChecker::from_rsl(rsl, max_staleness))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_in_memory_rsl_manager() {
        let manager = InMemoryRslManager::new();

        // Revoke a node
        let node_id = vec![1u8; 32];
        let signing_key = vec![2u8; 32];
        let cred = vec![3u8; 100];

        let revoked = manager
            .revoke_node(&node_id, 1, &signing_key, &cred, Some("Test revocation"))
            .await
            .unwrap();

        assert_eq!(revoked.node_id, node_id);
        assert_eq!(revoked.reason.as_u8(), 1);

        // Check revocation status
        let status = manager.is_revoked(&node_id).await.unwrap();
        assert_eq!(status, RevocationStatus::Revoked);

        // Generate RSL
        let rsl = manager.generate_rsl("test-ca", 1).await.unwrap();
        assert_eq!(rsl.revocation_count(), 1);
        assert!(rsl.is_node_revoked(&node_id));
    }

    #[tokio::test]
    async fn test_in_memory_multiple_revocations() {
        let manager = InMemoryRslManager::new();

        // Revoke multiple nodes
        for i in 0..5 {
            let node_id = vec![i; 32];
            let signing_key = vec![i + 10; 32];
            let cred = vec![i + 20; 100];

            manager
                .revoke_node(&node_id, 0, &signing_key, &cred, None)
                .await
                .unwrap();
        }

        // Generate RSL
        let rsl = manager.generate_rsl("test-ca", 1).await.unwrap();
        assert_eq!(rsl.revocation_count(), 5);

        // Check each node
        for i in 0..5 {
            let node_id = vec![i; 32];
            let status = manager.is_revoked(&node_id).await.unwrap();
            assert_eq!(status, RevocationStatus::Revoked);
            assert!(rsl.is_node_revoked(&node_id));
        }
    }
}
