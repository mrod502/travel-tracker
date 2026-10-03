//! Repository for the derived `device_identities` table and its address links.
//!
//! Both tables are written by the identity batch and by nothing else. Every method
//! here is an upsert-or-read: the batch is re-runnable, so a second pass over the
//! same occurrences has to converge on the same rows rather than duplicate them.
//!
//! The identity's `identity_id` is never taken from the caller on conflict — the
//! existing row keeps its id, which is what lets links written in an earlier pass
//! stay valid across a reprocess.

use sqlx::Executor;
use uuid::Uuid;

use crate::error::RepoError;
use crate::models::{DeviceAddressLink, DeviceIdentity};

/// Database access for `device_identities` and `device_address_links`.
pub struct DeviceIdentityRepository;

impl DeviceIdentityRepository {
    /// Inserts an identity, or refreshes the one with the same fingerprint.
    ///
    /// The fingerprint hash is the natural key: a re-run that derives the same
    /// features must land on the same identity, and the returned row's `identity_id`
    /// is the one to use from then on — which may be the one an earlier pass chose.
    ///
    /// The window widens (`LEAST`/`GREATEST`) rather than being replaced, because a
    /// pass over a *newer* slice of occurrences must not forget when the device was
    /// first seen; the counts and confidence are replaced, because they describe the
    /// pass that just ran.
    ///
    /// # Arguments
    ///
    /// * `executor` - Pool or transaction
    /// * `identity` - The identity as the resolver just derived it
    pub async fn upsert_by_fingerprint<'e, E>(
        executor: E,
        identity: &DeviceIdentity,
    ) -> Result<DeviceIdentity, RepoError>
    where
        E: Executor<'e, Database = sqlx::Postgres>,
    {
        let row = sqlx::query_as::<_, DeviceIdentity>(
            r#"
            INSERT INTO device_identities (
                identity_id,
                fingerprint,
                fingerprint_hash,
                confidence_score,
                resolution_method,
                first_seen,
                last_seen,
                observation_count,
                resolver_version,
                computed_at
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10
            )
            ON CONFLICT (fingerprint_hash) DO UPDATE SET
                fingerprint = EXCLUDED.fingerprint,
                confidence_score = EXCLUDED.confidence_score,
                resolution_method = EXCLUDED.resolution_method,
                first_seen = LEAST(device_identities.first_seen, EXCLUDED.first_seen),
                last_seen = GREATEST(device_identities.last_seen, EXCLUDED.last_seen),
                observation_count = EXCLUDED.observation_count,
                resolver_version = EXCLUDED.resolver_version,
                computed_at = EXCLUDED.computed_at
            RETURNING *
            "#,
        )
        .bind(identity.identity_id)
        .bind(&identity.fingerprint)
        .bind(&identity.fingerprint_hash)
        .bind(identity.confidence_score)
        .bind(identity.resolution_method)
        .bind(identity.first_seen)
        .bind(identity.last_seen)
        .bind(identity.observation_count)
        .bind(&identity.resolver_version)
        .bind(identity.computed_at)
        .fetch_one(executor)
        .await?;

        Ok(row)
    }

    /// Looks an identity up by the hash of its canonical fingerprint.
    ///
    /// This is the lookup a batch does once per device before deciding whether it is
    /// something new.
    pub async fn find_by_fingerprint_hash<'e, E>(
        executor: E,
        fingerprint_hash: &[u8],
    ) -> Result<Option<DeviceIdentity>, RepoError>
    where
        E: Executor<'e, Database = sqlx::Postgres>,
    {
        let identity = sqlx::query_as::<_, DeviceIdentity>(
            "SELECT * FROM device_identities WHERE fingerprint_hash = $1",
        )
        .bind(fingerprint_hash)
        .fetch_optional(executor)
        .await?;

        Ok(identity)
    }

    /// Fetches one identity by id.
    pub async fn find_by_id<'e, E>(
        executor: E,
        identity_id: Uuid,
    ) -> Result<Option<DeviceIdentity>, RepoError>
    where
        E: Executor<'e, Database = sqlx::Postgres>,
    {
        let identity = sqlx::query_as::<_, DeviceIdentity>(
            "SELECT * FROM device_identities WHERE identity_id = $1",
        )
        .bind(identity_id)
        .fetch_optional(executor)
        .await?;

        Ok(identity)
    }

    /// The weakest identities first, up to `limit`.
    ///
    /// A review pass starts here: the merges worth arguing about are the ones the
    /// resolver was least sure of, and they are a bounded list, unlike every identity.
    pub async fn list_weakest<'e, E>(
        executor: E,
        max_confidence: f32,
        limit: i64,
    ) -> Result<Vec<DeviceIdentity>, RepoError>
    where
        E: Executor<'e, Database = sqlx::Postgres>,
    {
        let identities = sqlx::query_as::<_, DeviceIdentity>(
            r#"
            SELECT * FROM device_identities
            WHERE confidence_score <= $1
            ORDER BY confidence_score, last_seen DESC
            LIMIT $2
            "#,
        )
        .bind(max_confidence)
        .bind(limit)
        .fetch_all(executor)
        .await?;

        Ok(identities)
    }

    /// Links a rotating device identifier to an identity.
    ///
    /// Re-linking the same triple refreshes the window rather than adding a row. Two
    /// justifications are reconciled when they disagree: the stronger method wins
    /// (`LEAST` over the enum, whose declaration order is the reliability order), so a
    /// link first inferred from a fingerprint and later confirmed by an exact address
    /// stops being reported as an inference.
    pub async fn link_address<'e, E>(executor: E, link: &DeviceAddressLink) -> Result<(), RepoError>
    where
        E: Executor<'e, Database = sqlx::Postgres>,
    {
        sqlx::query(
            r#"
            INSERT INTO device_address_links (
                device_hash,
                observer_node_id,
                identity_id,
                address,
                address_type,
                method,
                confidence,
                first_seen,
                last_seen,
                observation_count,
                computed_at
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11
            )
            ON CONFLICT (device_hash, observer_node_id, identity_id) DO UPDATE SET
                address = COALESCE(EXCLUDED.address, device_address_links.address),
                address_type = COALESCE(
                    EXCLUDED.address_type,
                    device_address_links.address_type
                ),
                method = LEAST(device_address_links.method, EXCLUDED.method),
                confidence = GREATEST(device_address_links.confidence, EXCLUDED.confidence),
                first_seen = LEAST(device_address_links.first_seen, EXCLUDED.first_seen),
                last_seen = GREATEST(device_address_links.last_seen, EXCLUDED.last_seen),
                observation_count = EXCLUDED.observation_count,
                computed_at = EXCLUDED.computed_at
            "#,
        )
        .bind(&link.device_hash)
        .bind(&link.observer_node_id)
        .bind(link.identity_id)
        .bind(&link.address)
        .bind(link.address_type)
        .bind(link.method)
        .bind(link.confidence)
        .bind(link.first_seen)
        .bind(link.last_seen)
        .bind(link.observation_count)
        .bind(link.computed_at)
        .execute(executor)
        .await?;

        Ok(())
    }

    /// Every identity a device identifier is currently linked to, for one observer.
    ///
    /// A Vec, not an Option: an identifier linked to two identities by the same node
    /// is a recorded split, and a caller that assumed one would silently pick a side.
    pub async fn links_for_hash<'e, E>(
        executor: E,
        device_hash: &[u8],
        observer_node_id: &[u8],
    ) -> Result<Vec<DeviceAddressLink>, RepoError>
    where
        E: Executor<'e, Database = sqlx::Postgres>,
    {
        let links = sqlx::query_as::<_, DeviceAddressLink>(
            r#"
            SELECT * FROM device_address_links
            WHERE device_hash = $1 AND observer_node_id = $2
            ORDER BY confidence DESC, last_seen DESC
            "#,
        )
        .bind(device_hash)
        .bind(observer_node_id)
        .fetch_all(executor)
        .await?;

        Ok(links)
    }

    /// Every identifier an identity currently answers to.
    pub async fn links_for_identity<'e, E>(
        executor: E,
        identity_id: Uuid,
    ) -> Result<Vec<DeviceAddressLink>, RepoError>
    where
        E: Executor<'e, Database = sqlx::Postgres>,
    {
        let links = sqlx::query_as::<_, DeviceAddressLink>(
            "SELECT * FROM device_address_links WHERE identity_id = $1 ORDER BY first_seen",
        )
        .bind(identity_id)
        .fetch_all(executor)
        .await?;

        Ok(links)
    }

    /// How many identities exist. Used by the replay report's summary line.
    pub async fn count(
        executor: impl Executor<'_, Database = sqlx::Postgres>,
    ) -> Result<i64, RepoError> {
        sqlx::query_scalar("SELECT COUNT(*) FROM device_identities")
            .fetch_one(executor)
            .await
            .map_err(RepoError::Database)
    }
}
