//! Revocation Status List (RSL) management.
//!
//! This module provides traits and implementations for managing RSLs,
//! including database-backed storage and in-memory caching.
//!
//! # The sequence number
//!
//! `sequence_number` is the anti-replay counter: a node that has accepted RSL
//! *n* from a CA must refuse anything from that CA numbered *n* or lower, or a
//! captured list can be served forever and look like current revocation data.
//! That only works if the number comes from state outliving the process that
//! issued it, so:
//!
//! * [`DatabaseRslManager`] reads `MAX(sequence_number)` from
//!   `revocation_status_lists`, whose `(issuer_id, sequence_number)` primary
//!   key makes a second list with an already-used number a rejected write.
//! * [`InMemoryRslManager`] keeps a counter that starts at 1 and never repeats
//!   for the life of the process, and nothing more — it is for tests and
//!   single-process deployments, and its numbers do not survive a restart.
//!
//! A list therefore starts at 1, and 0 means "this CA has never published
//! one" — which is also what `revocation_status_lists`' CHECK constraint
//! enforces.
//!
//! # Reading a list
//!
//! A published list reaches a consumer as a JSON document in a column: the same
//! row an operator can `UPDATE`, a replica can replay, a migration can rewrite.
//! What makes it the CA's statement rather than anybody's is the signature over
//! its bytes, so the only supported way to read one is
//! [`verified_checker`], which refuses a list whose signature does not come from
//! the configured anchor's key. There is deliberately no unverified accessor:
//! a checker built from an edited document answers `Valid` for exactly the nodes
//! the edit removed, which is the failure revocation exists to prevent.

use crate::anchor::TrustAnchor;
use crate::error::{CaError, Result};
use crate::revocation::{InMemoryRslChecker, RevocationStatus, RevocationStatusList, RevokedNode};
use async_trait::async_trait;
use chrono::Utc;

/// Rejects a list no CA has committed to.
///
/// Storing an unsigned list would make the storage the source of trust for a
/// document anybody could have written; signing is what makes a published list
/// the CA's, so it has to happen before the list is handed to a manager.
fn ensure_signed(rsl: &RevocationStatusList) -> Result<()> {
    if rsl.signature.is_empty() {
        return Err(CaError::UnsignedRsl(hex::encode(&rsl.issuer_id)));
    }

    Ok(())
}

/// Turn a CA's published revocation data into a checker, verifying it first.
///
/// The issuer is not a parameter: `anchor` names the CA, and "give me the list
/// from someone else" is not a question this answers — the id is derived from the
/// key and compared against the document, so an anchor for CA A can never be used
/// to bless a list claiming to be from CA B.
///
/// # Errors
///
/// * [`CaError::RslNotFound`] — this CA has published nothing. That is the absence
///   of evidence, not a clean bill of health, and a caller that reads it as one
///   turns an outage at the CA into `Valid` for every node in the federation.
/// * [`CaError::Verification`] — the signature is not this key's, or the document
///   names a different CA. Tampering and misconfiguration both land here, and the
///   message says so rather than reporting an empty list.
/// * [`CaError::RslExpired`] — the newest list's validity window has closed, so
///   its silence about a node no longer means anything.
///
/// A list that verifies but is *stale* is returned as loaded: how much staleness
/// to tolerate is the caller's policy, expressed through `max_staleness` and acted
/// on by the [`crate::RevocationPolicy`]s, not a fact about authenticity.
///
/// [`crate::RevocationPolicy`]: crate::revocation::RevocationPolicy
pub async fn verified_checker(
    manager: &dyn RslManager,
    anchor: &TrustAnchor,
    max_staleness: chrono::Duration,
) -> Result<InMemoryRslChecker> {
    let rsl = manager
        .get_latest_rsl(anchor.ca_id())
        .await?
        .ok_or_else(|| CaError::RslNotFound(anchor.ca_id_hex()))?;

    if !anchor.verify_rsl(&rsl)? {
        return Err(CaError::Verification(format!(
            "the revocation list published for CA {} does not verify under that CA's key: it \
             was edited after publication, signed by someone else, or is not that CA's list at \
             all",
            anchor.ca_id_hex()
        )));
    }

    if rsl.is_expired() {
        return Err(CaError::RslExpired(rsl.expires_at));
    }

    Ok(InMemoryRslChecker::from_rsl(rsl, max_staleness))
}

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
    /// The list carries [`next_sequence_number`](Self::next_sequence_number)
    /// and is unsigned. Generating does not consume the number — publishing
    /// does, by way of [`store_rsl`](Self::store_rsl) — so generating twice
    /// without storing yields the same list rather than burning a sequence.
    ///
    /// # Arguments
    ///
    /// * `ca_id` - The CA identifier that will issue the RSL, as
    ///   `SHA-256(its public key)` — raw bytes, the same value
    ///   [`crate::TrustAnchor::ca_id`] returns
    /// * `validity_days` - How long the RSL should be valid
    ///
    /// # Returns
    ///
    /// * `Ok(RevocationStatusList)` - The generated (unsigned) RSL
    /// * `Err(CaError)` - If generation failed
    async fn generate_rsl(&self, ca_id: &[u8], validity_days: u64) -> Result<RevocationStatusList>;

    /// Store an RSL after it has been signed.
    ///
    /// Storing is what publishes a sequence number: a later
    /// [`generate_rsl`](Self::generate_rsl) reports the number after it, and a
    /// list reusing a number already stored is refused with
    /// [`CaError::RslNotNewer`].
    ///
    /// # Arguments
    ///
    /// * `rsl` - The signed RSL to store
    ///
    /// # Returns
    ///
    /// * `Ok(())` - RSL stored successfully
    /// * `Err(CaError::UnsignedRsl)` - The list carries no signature
    /// * `Err(CaError::RslNotNewer)` - Its sequence does not advance the
    ///   highest number already stored for its issuer
    /// * `Err(CaError)` - If storage failed
    async fn store_rsl(&self, rsl: &RevocationStatusList) -> Result<()>;

    /// Get the latest RSL for a CA.
    ///
    /// This is the most recently *published* list, byte-for-byte the document
    /// that was signed — not a fresh list rebuilt from current revocations,
    /// which would not verify against the signature the CA actually made.
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
    async fn get_latest_rsl(&self, ca_id: &[u8]) -> Result<Option<RevocationStatusList>>;

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
    async fn next_sequence_number(&self, ca_id: &[u8]) -> Result<u64>;

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
    /// # Returns
    ///
    /// - `Ok(RevocationStatus::Valid)` - Node is not revoked
    /// - `Ok(RevocationStatus::Revoked)` - Node is revoked
    /// - `Ok(RevocationStatus::Unknown)` - Cannot determine status
    /// - `Err(CaError)` - If check failed
    async fn is_revoked(&self, node_id: &[u8]) -> Result<RevocationStatus>;
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

/// NULL in `node_revocations.revoked_by`, for a revocation recorded but not yet
/// published in a signed list: there is no CA identifier to write yet, and
/// [`store_rsl`] attributes the row when a list carrying it is published. A
/// placeholder id of our own inventing would be a value in a column typed to hold
/// a real one, that every later reader has to know to exclude.
///
/// [`store_rsl`]: RslManager::store_rsl
#[cfg(feature = "database")]
const UNPUBLISHED_ISSUER: Option<&[u8]> = None;

/// One row of `node_revocations`, as the read below needs it.
///
/// Kept separate from the domain's [`RevokedNode`], which stores `reason` as a
/// `RevocationReason`: `FromRow` has to mirror the column list, and the reason
/// code is only turned into a domain variant after the read. Columns the RSL
/// does not carry (`revoked_by`, `ca_credential`, `rsl_sequence_number`) are not
/// selected.
#[cfg(feature = "database")]
#[derive(Debug, sqlx::FromRow)]
struct RevocationRow {
    node_id: Vec<u8>,
    revoked_at: chrono::DateTime<Utc>,
    reason: i32,
    signing_public_key: Vec<u8>,
    notes: Option<String>,
}

#[cfg(feature = "database")]
impl DatabaseRslManager {
    /// Create a new database-backed RSL manager.
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }

    /// Get all revoked nodes from the database.
    async fn get_all_revoked(&self) -> Result<Vec<RevokedNode>> {
        let rows = sqlx::query_as::<_, RevocationRow>(
            r#"
            SELECT
                node_id,
                revoked_at,
                reason,
                signing_public_key,
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

    /// Highest sequence number this CA has already published, or 0 if it has
    /// never published one.
    ///
    /// Read from the published lists rather than from `node_revocations`: the
    /// revocation ledger records one number per *node*, so it cannot express
    /// "list 4 was published" on its own — and a CA with nothing to revoke
    /// would have no rows to read and could never move past its first list.
    async fn latest_published_sequence(&self, ca_id: &[u8]) -> Result<u64> {
        let seq: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(sequence_number), 0) FROM revocation_status_lists WHERE issuer_id = $1",
        )
        .bind(ca_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| CaError::Database(e.to_string()))?;

        Ok(seq.max(0) as u64)
    }
}

#[cfg(feature = "database")]
#[async_trait]
impl RslManager for DatabaseRslManager {
    async fn generate_rsl(&self, ca_id: &[u8], validity_days: u64) -> Result<RevocationStatusList> {
        let revoked_nodes = self.get_all_revoked().await?;
        let sequence_number = self.latest_published_sequence(ca_id).await? + 1;

        let mut builder = RevocationStatusList::builder(ca_id)
            .sequence_number(sequence_number)
            .validity_days(validity_days);

        for revoked in revoked_nodes {
            builder = builder.add_revocation(revoked);
        }

        Ok(builder.build_unsigned())
    }

    async fn store_rsl(&self, rsl: &RevocationStatusList) -> Result<()> {
        ensure_signed(rsl)?;

        // The primary key on (issuer_id, sequence_number) is what actually
        // refuses a replayed number; this read runs first so the caller is
        // told which number is already taken instead of tripping a constraint.
        let published = self.latest_published_sequence(&rsl.issuer_id).await?;
        if rsl.sequence_number <= published {
            return Err(CaError::RslNotNewer {
                issuer_id: hex::encode(&rsl.issuer_id),
                incoming: rsl.sequence_number,
                current: published,
            });
        }

        // The published list and the attribution of its entries are one fact:
        // half of it landing would leave a list on the record whose revocations
        // still point at an earlier one.
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| CaError::Database(e.to_string()))?;

        sqlx::query(
            r#"
            INSERT INTO revocation_status_lists (
                issuer_id,
                sequence_number,
                issued_at,
                expires_at,
                revocation_count,
                signature,
                rsl
            ) VALUES ($1, $2, $3, $4, $5, $6, $7)
            "#,
        )
        .bind(&rsl.issuer_id)
        .bind(rsl.sequence_number as i64)
        .bind(rsl.issued_at)
        .bind(rsl.expires_at)
        .bind(rsl.revocation_count() as i32)
        .bind(&rsl.signature)
        .bind(sqlx::types::Json(rsl))
        .execute(&mut *tx)
        .await
        .map_err(|e| CaError::Database(e.to_string()))?;

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
                    revoked_by = EXCLUDED.revoked_by,
                    -- "First list this revocation appeared in", per the column's
                    -- meaning: a row already attributed (non-zero) keeps that
                    -- number, and only one published from pending (0) takes this
                    -- list's.
                    rsl_sequence_number = CASE
                        WHEN node_revocations.rsl_sequence_number = 0
                            THEN EXCLUDED.rsl_sequence_number
                        ELSE node_revocations.rsl_sequence_number
                    END
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
            .execute(&mut *tx)
            .await
            .map_err(|e| CaError::Database(e.to_string()))?;
        }

        tx.commit()
            .await
            .map_err(|e| CaError::Database(e.to_string()))?;

        Ok(())
    }

    async fn get_latest_rsl(&self, ca_id: &[u8]) -> Result<Option<RevocationStatusList>> {
        let stored = sqlx::query_scalar::<_, sqlx::types::Json<RevocationStatusList>>(
            r#"
            SELECT rsl
            FROM revocation_status_lists
            WHERE issuer_id = $1
            ORDER BY sequence_number DESC
            LIMIT 1
            "#,
        )
        .bind(ca_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| CaError::Database(e.to_string()))?;

        Ok(stored.map(|stored| stored.0))
    }

    async fn next_sequence_number(&self, ca_id: &[u8]) -> Result<u64> {
        Ok(self.latest_published_sequence(ca_id).await? + 1)
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

        // Insert into database, unattributed: this revocation is recorded, but
        // no list carrying it has been signed or published yet, so it has no
        // issuer and no sequence number until store_rsl gives it both.
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
        .bind(UNPUBLISHED_ISSUER)
        // The mapped variant, not the raw argument: the column checks that the
        // code is a reason this system knows, and an unmapped one would fail
        // that check instead of being recorded the way it was understood here.
        .bind(revoked_node.reason.as_u8() as i32)
        .bind(&revoked_node.signing_public_key)
        .bind(ca_credential)
        .bind(&revoked_node.notes)
        .execute(&self.pool)
        .await
        .map_err(|e| CaError::Database(e.to_string()))?;

        Ok(revoked_node)
    }

    async fn is_revoked(&self, node_id: &[u8]) -> Result<RevocationStatus> {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM node_revocations WHERE node_id = $1)")
                .bind(node_id)
                .fetch_one(&self.pool)
                .await
                .map_err(|e| CaError::Database(e.to_string()))?;

        // Absence here is a direct read of the authoritative ledger, which is
        // what makes it mean something; a cached list answers Unknown instead
        // once its own age disqualifies it (see `InMemoryRslChecker`).
        Ok(if exists {
            RevocationStatus::Revoked
        } else {
            RevocationStatus::Valid
        })
    }
}

/// In-memory RSL manager (for testing and single-node deployments).
///
/// This implementation keeps all revocations in memory and does not
/// provide persistence across restarts — including the sequence counter, which
/// is the part that matters: see the module docs for why a deployment that
/// needs replay protection uses [`DatabaseRslManager`] instead.
pub struct InMemoryRslManager {
    /// In-memory store of revoked nodes.
    revoked: std::sync::RwLock<std::collections::HashMap<Vec<u8>, RevokedNode>>,

    /// Latest list published per CA.
    ///
    /// This *is* the sequence counter: the next number is one past whatever is
    /// here, the same reading the database manager takes of
    /// `revocation_status_lists`. Holding a separate counter would be a second
    /// answer to the same question, and the two disagreeing is exactly how a
    /// replayed list gets through.
    published: std::sync::RwLock<std::collections::HashMap<Vec<u8>, RevocationStatusList>>,
}

impl InMemoryRslManager {
    /// Create a new in-memory RSL manager.
    pub fn new() -> Self {
        Self {
            revoked: std::sync::RwLock::new(std::collections::HashMap::new()),
            published: std::sync::RwLock::new(std::collections::HashMap::new()),
        }
    }

    /// Highest sequence number published for `ca_id`, 0 if none.
    fn highest_published(&self, ca_id: &[u8]) -> u64 {
        self.published
            .read()
            .unwrap()
            .get(ca_id)
            .map(|rsl| rsl.sequence_number)
            .unwrap_or(0)
    }
}

impl Default for InMemoryRslManager {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl RslManager for InMemoryRslManager {
    async fn generate_rsl(&self, ca_id: &[u8], validity_days: u64) -> Result<RevocationStatusList> {
        let revoked_nodes: Vec<RevokedNode> = {
            let guard = self.revoked.read().unwrap();
            guard.values().cloned().collect()
        };

        // Same rule as the database-backed manager: one past the highest list
        // already published. Generating alone does not publish, so generating
        // twice before storing yields the same number twice rather than
        // silently skipping one.
        let sequence_number = self.next_sequence_number(ca_id).await?;

        let mut builder = RevocationStatusList::builder(ca_id)
            .sequence_number(sequence_number)
            .validity_days(validity_days);

        for revoked in revoked_nodes {
            builder = builder.add_revocation(revoked);
        }

        Ok(builder.build_unsigned())
    }

    async fn store_rsl(&self, rsl: &RevocationStatusList) -> Result<()> {
        ensure_signed(rsl)?;

        let current = self.highest_published(&rsl.issuer_id);
        if rsl.sequence_number <= current {
            return Err(CaError::RslNotNewer {
                issuer_id: hex::encode(&rsl.issuer_id),
                incoming: rsl.sequence_number,
                current,
            });
        }

        {
            let mut guard = self.revoked.write().unwrap();
            for revoked_node in &rsl.revocations {
                guard.insert(revoked_node.node_id.clone(), revoked_node.clone());
            }
        }

        self.published
            .write()
            .unwrap()
            .insert(rsl.issuer_id.clone(), rsl.clone());

        Ok(())
    }

    async fn get_latest_rsl(&self, ca_id: &[u8]) -> Result<Option<RevocationStatusList>> {
        Ok(self.published.read().unwrap().get(ca_id).cloned())
    }

    async fn next_sequence_number(&self, ca_id: &[u8]) -> Result<u64> {
        Ok(self.highest_published(ca_id) + 1)
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::revocation::RevocationChecker;

    /// Signs a generated list the way a publisher does: the manager's
    /// sequence and validity window are kept, not rebuilt at the signing site.
    fn publish(ca: &crate::CaRoot, unsigned: RevocationStatusList) -> RevocationStatusList {
        ca.sign_rsl(unsigned).unwrap()
    }

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
        let rsl = manager.generate_rsl(b"test-ca", 1).await.unwrap();
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
        let rsl = manager.generate_rsl(b"test-ca", 1).await.unwrap();
        assert_eq!(rsl.revocation_count(), 5);

        // Check each node
        for i in 0..5 {
            let node_id = vec![i; 32];
            let status = manager.is_revoked(&node_id).await.unwrap();
            assert_eq!(status, RevocationStatus::Revoked);
            assert!(rsl.is_node_revoked(&node_id));
        }
    }

    #[tokio::test]
    async fn sequence_numbers_start_at_one_and_advance_when_published() {
        let ca = crate::CaRoot::generate();
        let manager = InMemoryRslManager::new();
        let ca_id = ca.ca_id();

        // 0 means "nothing published yet", so the first real list cannot be 0:
        // a list numbered 0 is what a replayed piece of pre-history looks like.
        assert_eq!(manager.next_sequence_number(&ca_id).await.unwrap(), 1);

        // Generating is not publishing: the number is whatever the next list
        // would carry, and asking twice cannot burn one.
        let first = manager.generate_rsl(&ca_id, 1).await.unwrap();
        let same = manager.generate_rsl(&ca_id, 1).await.unwrap();
        assert_eq!(first.sequence_number, 1);
        assert_eq!(same.sequence_number, first.sequence_number);

        let first = publish(&ca, first);
        manager.store_rsl(&first).await.unwrap();
        assert_eq!(manager.next_sequence_number(&ca_id).await.unwrap(), 2);

        let second = publish(&ca, manager.generate_rsl(&ca_id, 1).await.unwrap());
        manager.store_rsl(&second).await.unwrap();

        assert_eq!(second.sequence_number, 2);
        assert_eq!(manager.next_sequence_number(&ca_id).await.unwrap(), 3);
    }

    #[tokio::test]
    async fn storing_a_list_that_does_not_advance_the_sequence_is_refused() {
        let ca = crate::CaRoot::generate();
        let manager = InMemoryRslManager::new();
        let ca_id = ca.ca_id();

        let first = publish(&ca, manager.generate_rsl(&ca_id, 1).await.unwrap());
        manager.store_rsl(&first).await.unwrap();

        // A captured copy of the list already stored, offered again: it carries
        // no information the receiver does not already have, and accepting it
        // would let a stale cache pass for a current one.
        let replay = first.clone();
        let err = manager.store_rsl(&replay).await.unwrap_err();
        assert!(
            matches!(
                &err,
                CaError::RslNotNewer {
                    incoming: 1,
                    current: 1,
                    ..
                }
            ),
            "expected the replay to be refused as not newer, got {err}"
        );

        // And the counter is unchanged by the attempt.
        assert_eq!(manager.next_sequence_number(&ca_id).await.unwrap(), 2);
    }

    #[tokio::test]
    async fn an_unsigned_list_cannot_be_stored() {
        let manager = InMemoryRslManager::new();

        let unsigned = manager.generate_rsl(b"test-ca", 1).await.unwrap();
        let err = manager.store_rsl(&unsigned).await.unwrap_err();

        assert!(
            matches!(err, CaError::UnsignedRsl(issuer) if issuer == hex::encode(b"test-ca")),
            "an unsigned list must not become the record of revocations"
        );
    }

    #[tokio::test]
    async fn a_checker_needs_a_list_that_verifies() {
        let ca = crate::CaRoot::generate();
        let manager = InMemoryRslManager::new();
        let anchor = crate::TrustAnchor::from_public_key(ca.public_key_slice()).unwrap();
        let ca_id = ca.ca_id();

        // Nothing published yet. This is an error rather than a checker that
        // answers `Unknown`, because handing out an empty checker lets a caller
        // mistake "no data" for data — and the CA being down then reads as an
        // all-clear.
        let err = verified_checker(&manager, &anchor, chrono::Duration::hours(24))
            .await
            .expect_err("there is no list to load yet");
        assert!(matches!(err, CaError::RslNotFound(_)), "{err}");

        let node_id = vec![7u8; 32];
        manager
            .revoke_node(&node_id, 1, &[8u8; 32], &[], None)
            .await
            .unwrap();
        let published = publish(&ca, manager.generate_rsl(&ca_id, 7).await.unwrap());
        manager.store_rsl(&published).await.unwrap();

        let checker = verified_checker(&manager, &anchor, chrono::Duration::hours(24))
            .await
            .expect("a list this CA signed verifies under its own anchor");
        assert_eq!(
            checker.is_revoked(&node_id).unwrap(),
            RevocationStatus::Revoked
        );
        assert_eq!(checker.sequence_number(), Some(1));
        // A second node nobody revoked, against a list that is current.
        assert_eq!(
            checker.is_revoked(&[9u8; 32]).unwrap(),
            RevocationStatus::Valid
        );

        // The same manager read under a different CA's anchor finds nothing to
        // load: the issuer comes from the key, so one CA's list is never offered
        // as another's answer.
        let other = crate::CaRoot::generate();
        let other_anchor = crate::TrustAnchor::from_public_key(other.public_key_slice()).unwrap();
        let err = verified_checker(&manager, &other_anchor, chrono::Duration::hours(24))
            .await
            .expect_err("this anchor speaks for a CA that has published nothing");
        assert!(
            matches!(err, CaError::RslNotFound(ref id) if *id == other.ca_id_hex()),
            "{err}"
        );
    }
}
