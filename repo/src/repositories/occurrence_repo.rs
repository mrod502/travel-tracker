//! Repository for the unified `Occurrence` model
//!
//! Provides type-safe CRUD operations for wireless signal occurrences.

use sqlx::Executor;
use uuid::Uuid;

use crate::error::RepoError;
use crate::models::{Occurrence, SignalType};

/// Generic repository for `Occurrence` records
pub struct OccurrenceRepository;

impl OccurrenceRepository {
    /// Create a new occurrence record (append-only).
    ///
    /// # Errors
    ///
    /// Returns `RepoError::Duplicate` if an occurrence with the same ID already exists.
    /// Returns `RepoError::Database` for other database errors.
    pub async fn create<'e, E>(executor: E, occurrence: &Occurrence) -> Result<Occurrence, RepoError>
    where
        E: Executor<'e, Database = sqlx::Postgres>,
    {
        let record = sqlx::query_as::<_, Occurrence>(
            r#"
INSERT INTO occurrences (
    occurrence_id,
    signal_type,
    origin_node_id,
    observed_at,
    observed_at_node_local,
    device_address,
    device_hash,
    advertised_name,
    adv_type,
    rssi,
    tx_power,
    signal_payload,
    location,
    alt_m,
    accuracy_m,
    location_source,
    signed_payload,
    signature,
    schema_version,
    ingested_at
) VALUES (
    $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20
)
RETURNING *
            "#,
        )
        .bind(occurrence.occurrence_id)
        .bind(occurrence.signal_type)
        .bind(&occurrence.origin_node_id)
        .bind(occurrence.observed_at)
        .bind(occurrence.observed_at_node_local)
        .bind(&occurrence.device_address)
        .bind(&occurrence.device_hash)
        .bind(&occurrence.advertised_name)
        .bind(&occurrence.adv_type)
        .bind(occurrence.rssi)
        .bind(&occurrence.tx_power)
        .bind(&occurrence.signal_payload)
        .bind(&occurrence.location)
        .bind(&occurrence.alt_m)
        .bind(&occurrence.accuracy_m)
        .bind(&occurrence.location_source)
        .bind(&occurrence.signed_payload)
        .bind(&occurrence.signature)
        .bind(occurrence.schema_version)
        .bind(occurrence.ingested_at);

        record.fetch_one(executor).await.map_err(|e| {
            if let sqlx::Error::Database(db_err) = &e {
                // Check for unique constraint violation
                if db_err.code() == Some("23505".into()) {
                    return RepoError::Duplicate(occurrence.occurrence_id);
                }
            }
            RepoError::Database(e)
        })
    }

    /// Find an occurrence by ID.
    ///
    /// Returns `None` if not found.
    pub async fn find_by_id<'e, E>(executor: E, id: Uuid) -> Result<Option<Occurrence>, RepoError>
    where
        E: Executor<'e, Database = sqlx::Postgres>,
    {
        sqlx::query_as::<_, Occurrence>(
            r#"
SELECT * FROM occurrences WHERE occurrence_id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(executor)
        .await
        .map_err(RepoError::Database)
    }

    /// Find occurrences by device address (MAC).
    pub async fn find_by_device_address<'e, E>(
        executor: E,
        address: &[u8],
        limit: i64,
    ) -> Result<Vec<Occurrence>, RepoError>
    where
        E: Executor<'e, Database = sqlx::Postgres>,
    {
        sqlx::query_as::<_, Occurrence>(
            r#"
SELECT * FROM occurrences 
WHERE device_address = $1 
ORDER BY observed_at DESC 
LIMIT $2
            "#,
        )
        .bind(address)
        .bind(limit)
        .fetch_all(executor)
        .await
        .map_err(RepoError::Database)
    }

    /// Find occurrences by signal type.
    pub async fn find_by_signal_type<'e, E>(
        executor: E,
        signal_type: SignalType,
        limit: i64,
    ) -> Result<Vec<Occurrence>, RepoError>
    where
        E: Executor<'e, Database = sqlx::Postgres>,
    {
        sqlx::query_as::<_, Occurrence>(
            r#"
SELECT * FROM occurrences 
WHERE signal_type = $1 
ORDER BY observed_at DESC 
LIMIT $2
            "#,
        )
        .bind(signal_type)
        .bind(limit)
        .fetch_all(executor)
        .await
        .map_err(RepoError::Database)
    }

    /// Find occurrences by H3 macro cell.
    pub async fn find_by_geo_cell<'e, E>(
        executor: E,
        geo_cell: i64,
        limit: i64,
    ) -> Result<Vec<Occurrence>, RepoError>
    where
        E: Executor<'e, Database = sqlx::Postgres>,
    {
        sqlx::query_as::<_, Occurrence>(
            r#"
SELECT * FROM occurrences
WHERE geo_cell_macro = $1
ORDER BY observed_at DESC
LIMIT $2
            "#,
        )
        .bind(geo_cell)
        .bind(limit)
        .fetch_all(executor)
        .await
        .map_err(RepoError::Database)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_repo_compiles() {
        // Just a compilation test
        assert!(true);
    }
}
