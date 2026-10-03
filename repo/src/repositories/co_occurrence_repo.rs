//! Repository for `co_occurrence_events` — the raw co-presence records.
//!
//! These rows are the audit trail behind an [`crate::models::AssociationEdge`]: the
//! edge says two devices are associated, these say which sightings made that up. They
//! are written by the identity batch from occurrences, never by the capture path.

use chrono::{DateTime, Utc};
use sqlx::Executor;
use uuid::Uuid;

use crate::error::RepoError;
use crate::models::CoOccurrenceEvent;

/// Database access for `co_occurrence_events`.
pub struct CoOccurrenceRepository;

impl CoOccurrenceRepository {
    /// Records one co-presence window.
    ///
    /// Re-recording the same `(pair, node, cell, window_start)` widens the window and
    /// keeps the better sample count rather than creating a second row for one fact.
    /// The distance takes the *smaller* value when both passes computed one: two
    /// devices reported 4 m apart and 40 m apart were 4 m apart, and the larger number
    /// is the one that says less.
    pub async fn record<'e, E>(executor: E, event: &CoOccurrenceEvent) -> Result<(), RepoError>
    where
        E: Executor<'e, Database = sqlx::Postgres>,
    {
        sqlx::query(
            r#"
            INSERT INTO co_occurrence_events (
                identity_a,
                identity_b,
                node_id,
                geo_cell_macro,
                window_start,
                window_end,
                sample_count,
                distance_m,
                generated_at
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9
            )
            ON CONFLICT (identity_a, identity_b, node_id, geo_cell_macro, window_start)
            DO UPDATE SET
                window_end = GREATEST(
                    co_occurrence_events.window_end,
                    EXCLUDED.window_end
                ),
                sample_count = GREATEST(
                    co_occurrence_events.sample_count,
                    EXCLUDED.sample_count
                ),
                distance_m = LEAST(
                    co_occurrence_events.distance_m,
                    EXCLUDED.distance_m
                ),
                generated_at = EXCLUDED.generated_at
            "#,
        )
        .bind(event.identity_a)
        .bind(event.identity_b)
        .bind(&event.node_id)
        .bind(event.geo_cell_macro)
        .bind(event.window_start)
        .bind(event.window_end)
        .bind(event.sample_count)
        .bind(event.distance_m)
        .bind(event.generated_at)
        .execute(executor)
        .await?;

        Ok(())
    }

    /// Every co-presence one identity appears in, as either side of the pair.
    ///
    /// Both columns have to be consulted — the canonical ordering that keeps a pair
    /// stored once is exactly why "everything involving this identity" cannot filter
    /// on `identity_a` alone.
    pub async fn for_identity<'e, E>(
        executor: E,
        identity_id: Uuid,
        limit: i64,
    ) -> Result<Vec<CoOccurrenceEvent>, RepoError>
    where
        E: Executor<'e, Database = sqlx::Postgres>,
    {
        let events = sqlx::query_as::<_, CoOccurrenceEvent>(
            r#"
            SELECT * FROM co_occurrence_events
            WHERE identity_a = $1 OR identity_b = $1
            ORDER BY window_start DESC
            LIMIT $2
            "#,
        )
        .bind(identity_id)
        .bind(limit)
        .fetch_all(executor)
        .await?;

        Ok(events)
    }

    /// Co-presences whose windows overlap `[from, to]`, oldest-last.
    ///
    /// The scan a backfill or a period review reads; `window_start DESC` matches how
    /// an association is examined — most recent first.
    pub async fn in_window<'e, E>(
        executor: E,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        limit: i64,
    ) -> Result<Vec<CoOccurrenceEvent>, RepoError>
    where
        E: Executor<'e, Database = sqlx::Postgres>,
    {
        let events = sqlx::query_as::<_, CoOccurrenceEvent>(
            r#"
            SELECT * FROM co_occurrence_events
            WHERE window_end >= $1 AND window_start <= $2
            ORDER BY window_start DESC
            LIMIT $3
            "#,
        )
        .bind(from)
        .bind(to)
        .bind(limit)
        .fetch_all(executor)
        .await?;

        Ok(events)
    }

    /// How many co-presence windows are recorded.
    pub async fn count(
        executor: impl Executor<'_, Database = sqlx::Postgres>,
    ) -> Result<i64, RepoError> {
        sqlx::query_scalar("SELECT COUNT(*) FROM co_occurrence_events")
            .fetch_one(executor)
            .await
            .map_err(RepoError::Database)
    }
}
