//! Repository for the unified `Occurrence` model
//!
//! Provides type-safe CRUD operations for wireless signal occurrences.
//!
//! `occurrences` is `PARTITION BY RANGE (observed_at)` and the partitions only
//! run so far ahead of now, so every write here can meet
//! `no partition of relation "occurrences" found for row`. [`create`] repairs
//! that on the spot; [`insert_once`] does not, and exists for the callers that
//! cannot afford to.

use chrono::{DateTime, Utc};
use h3o::CellIndex;
use sqlx::Executor;

use crate::error::RepoError;
use crate::models::{Occurrence, SignalType};
use crate::types::H3Index;

/// SQLSTATE of a row that no partition of the target table will accept.
///
/// Postgres reports partition routing failures as a generic check-constraint
/// violation, so this code alone is not enough to act on — see
/// [`is_missing_partition`].
const CHECK_VIOLATION: &str = "23514";

/// True when `error` is Postgres saying "no partition of relation
/// 'occurrences' found for row", and not some other check-constraint failure
/// wearing the same SQLSTATE.
fn is_missing_partition(error: &RepoError) -> bool {
    match error {
        RepoError::Database(sqlx::Error::Database(db_err)) => {
            db_err.code() == Some(CHECK_VIOLATION.into())
                && db_err.message().contains("no partition of relation")
        }
        _ => false,
    }
}

/// Generic repository for `Occurrence` records
pub struct OccurrenceRepository;

impl OccurrenceRepository {
    /// Insert one occurrence, creating the monthly partition it needs if the
    /// database does not have one yet.
    ///
    /// Partitions are provisioned a fixed horizon ahead (`db up` extends it on
    /// every deploy), so a node that outlives that horizon — or that is handed
    /// a row dated outside it, from a synced backlog or a clock that was wrong
    /// when the row was captured — meets a partition error on a perfectly good
    /// row. That is a schema condition the caller can neither see nor fix, so
    /// it is handled here: on the first such failure this calls
    /// [`ensure_partition`] for `observed_at` and repeats the insert once. The
    /// repair is not silent — a refusal to create a partition (the timestamp is
    /// outside the window the schema will accept, or the maintenance function
    /// has not been migrated in yet) comes back as
    /// [`RepoError::PartitionHealFailed`] carrying both the insert failure and
    /// the reason healing failed.
    ///
    /// The executor has to survive two statements, hence `Copy`: in practice a
    /// `&PgPool`, which hands each statement its own connection. A transaction
    /// is not `Copy` and could not help anyway — the failed insert aborts it.
    /// Use [`insert_once`] inside one, after ensuring the partition.
    ///
    /// # Errors
    ///
    /// Returns [`RepoError::Duplicate`] if an occurrence with the same ID
    /// already exists, [`RepoError::PartitionHealFailed`] if the partition
    /// could not be created, and [`RepoError::Database`] for other database
    /// errors.
    pub async fn create<'e, E>(
        executor: E,
        occurrence: &Occurrence,
    ) -> Result<Occurrence, RepoError>
    where
        E: Executor<'e, Database = sqlx::Postgres> + Copy,
    {
        match Self::insert_once(executor, occurrence).await {
            Err(missing) if is_missing_partition(&missing) => {
                Self::ensure_partition(executor, occurrence.observed_at)
                    .await
                    .map_err(|heal| RepoError::PartitionHealFailed {
                        observed_at: occurrence.observed_at,
                        insert: Box::new(missing),
                        heal: Box::new(heal),
                    })?;

                // Exactly one retry. If the row still will not route, something
                // is wrong beyond this function's knowledge and the second
                // error is the one to report.
                Self::insert_once(executor, occurrence).await
            }
            other => other,
        }
    }

    /// Insert one occurrence exactly once, with no partition healing.
    ///
    /// The transaction-capable form of [`create`]: it accepts any executor — a
    /// `PgTransaction` included, passed as `&mut *tx` — and issues a single
    /// statement. In return it cannot repair a missing partition: the failed
    /// insert aborts the transaction, so there is nothing left to run the repair
    /// with. Callers that can be handed an `observed_at` outside the
    /// provisioned horizon should call [`ensure_partition`] on the same
    /// executor first, and accept the raw partition error as a genuine failure
    /// otherwise.
    ///
    /// # Errors
    ///
    /// Returns [`RepoError::Duplicate`] if an occurrence with the same ID
    /// already exists and [`RepoError::Database`] for other database errors.
    pub async fn insert_once<'e, E>(
        executor: E,
        occurrence: &Occurrence,
    ) -> Result<Occurrence, RepoError>
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

    /// Create the monthly `occurrences` partition that holds `observed_at`.
    ///
    /// Delegates to `ensure_occurrence_partition(timestamptz)` (migration
    /// `202609141353`), which is idempotent and returns the partition's name.
    /// [`create`] calls this when an insert needs it; call it directly to open
    /// a month ahead of writing — at the start of a transaction, where the
    /// repair cannot happen mid-insert, or before a bulk backfill.
    ///
    /// The schema bounds what it will create to roughly the last twelve months
    /// and the next twenty-four, so a row with a wild `observed_at` cannot fill
    /// the catalog from the network. A refusal, and a schema that predates the
    /// function, both surface as [`RepoError::Database`].
    pub async fn ensure_partition<'e, E>(
        executor: E,
        observed_at: DateTime<Utc>,
    ) -> Result<String, RepoError>
    where
        E: Executor<'e, Database = sqlx::Postgres>,
    {
        sqlx::query_scalar("SELECT ensure_occurrence_partition($1)")
            .bind(observed_at)
            .fetch_one(executor)
            .await
            .map_err(RepoError::Database)
    }

    /// Find recent occurrences of one signal type, newest first.
    ///
    /// The `observed_at` bound is what keeps this partition-pruned: `LIMIT` alone
    /// on an unbounded `ORDER BY observed_at DESC` still walks every partition.
    pub async fn find_recent<'e, E>(
        executor: E,
        signal_type: SignalType,
        since: DateTime<Utc>,
        limit: i64,
    ) -> Result<Vec<Occurrence>, RepoError>
    where
        E: Executor<'e, Database = sqlx::Postgres>,
    {
        sqlx::query_as::<_, Occurrence>(
            r#"
SELECT * FROM occurrences
WHERE signal_type = $1 AND observed_at >= $2
ORDER BY observed_at DESC
LIMIT $3
            "#,
        )
        .bind(signal_type)
        .bind(since)
        .bind(limit)
        .fetch_all(executor)
        .await
        .map_err(RepoError::Database)
    }

    /// Find occurrences by signal type, newest first.
    ///
    /// Unbounded: no `observed_at` predicate means no partition pruning, so this
    /// is a "what have we got" read for a small database, not a hot path.
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

    /// Find occurrences by H3 macro cell, newest first.
    ///
    /// Unbounded on `observed_at`, like [`find_by_signal_type`](Self::find_by_signal_type).
    ///
    /// `geo_cell` is matched against `geo_cell_macro`, so a cell at any
    /// resolution other than [`RESOLUTION_MACRO`](crate::geo::RESOLUTION_MACRO)
    /// matches nothing — Postgres will not complain, it will simply agree that no
    /// occurrence happened there.
    pub async fn find_by_geo_cell<'e, E>(
        executor: E,
        geo_cell: CellIndex,
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
        // `h3index` has no implicit cast from `bigint`, so an `int8` parameter
        // here fails to plan: `operator does not exist: h3index = bigint`.
        .bind(H3Index::from(geo_cell))
        .bind(limit)
        .fetch_all(executor)
        .await
        .map_err(RepoError::Database)
    }

    /// Total rows in `occurrences`, across every partition.
    pub async fn count_all<'e, E>(executor: E) -> Result<i64, RepoError>
    where
        E: Executor<'e, Database = sqlx::Postgres>,
    {
        sqlx::query_scalar("SELECT COUNT(*) FROM occurrences")
            .fetch_one(executor)
            .await
            .map_err(RepoError::Database)
    }

    /// Row counts per signal type, ordered by type.
    ///
    /// Typed as [`SignalType`] rather than read as text: `signal_type` is a
    /// Postgres enum, and asking for an enum column as a `String` is the same
    /// class of mistake [`H3Index`] exists to avoid on the geo columns.
    pub async fn count_by_signal_type<'e, E>(
        executor: E,
    ) -> Result<Vec<(SignalType, i64)>, RepoError>
    where
        E: Executor<'e, Database = sqlx::Postgres>,
    {
        sqlx::query_as(
            "SELECT signal_type, COUNT(*) FROM occurrences GROUP BY signal_type ORDER BY signal_type",
        )
        .fetch_all(executor)
        .await
        .map_err(RepoError::Database)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_repo_compiles() {
        // Just a compilation test
        assert!(true);
    }
}
