//! Repository for `association_edges` — the queryable rollup of co-presence.
//!
//! The rollup runs in SQL (the grouping is over every event, so it belongs next to
//! the data); the *scoring* runs here, in Rust, where it can be read and unit-tested.
//! That split is deliberate: the SQL answers "how many times, how many places, how
//! many days", and only the Rust says what those three numbers mean together.

use sqlx::Executor;
use uuid::Uuid;

use crate::error::RepoError;
use crate::models::{canonical_pair, AssociationAggregate, AssociationEdge};

/// Database access for `association_edges`.
pub struct AssociationRepository;

/// Weight on raw co-presence volume.
///
/// The smallest of the three, on the reasoning in the table's own comment: two devices
/// seen together fifty times at one busy transit stop are not associated, they are
/// both parked there. Volume still counts, because a genuine association does produce
/// it.
const VOLUME_WEIGHT: f32 = 0.25;
/// Weight on the number of distinct macro cells the pair shared.
const CELL_WEIGHT: f32 = 0.35;
/// Weight on the number of distinct days the pair shared.
///
/// The largest: meeting repeatedly, on different days, is the part of co-presence a
/// chance encounter at one place cannot produce.
const DAY_WEIGHT: f32 = 0.40;

/// Paired observations at which the volume component saturates.
const VOLUME_SATURATION: f32 = 20.0;
/// Distinct cells at which the place component saturates.
const CELL_SATURATION: f32 = 3.0;
/// Distinct days at which the day component saturates.
const DAY_SATURATION: f32 = 5.0;

/// The composite strength of a pair, from the three counts.
///
/// Each component saturates on its own scale and the weighted sum is clamped to
/// `0..=1`, which is what the column's CHECK demands. The saturations are a first
/// calibration, not a finding — the identity design flags this formula as needing
/// prototyping against known-paired and known-incidental data, and the three counts
/// are stored on every edge so a recalibration can be computed from them rather than
/// from the events again.
pub fn strength_for(co_occurrence_count: i32, distinct_geo_cells: i32, distinct_days: i32) -> f32 {
    let component = |count: i32, saturation: f32| (count.max(0) as f32 / saturation).min(1.0);

    let strength = component(co_occurrence_count, VOLUME_SATURATION) * VOLUME_WEIGHT
        + component(distinct_geo_cells, CELL_SATURATION) * CELL_WEIGHT
        + component(distinct_days, DAY_SATURATION) * DAY_WEIGHT;

    strength.clamp(0.0, 1.0)
}

impl AssociationRepository {
    /// Groups every recorded co-presence into one aggregate per pair.
    ///
    /// Days are counted in UTC, matching the rest of the schema's time handling; a
    /// locally-midnight-to-midnight sighting straddling UTC counts as two days, which
    /// errs toward seeing *more* diversity, and is the safe direction for a score that
    /// is meant to be declined when it is weak.
    pub async fn aggregate<'e, E>(executor: E) -> Result<Vec<AssociationAggregate>, RepoError>
    where
        E: Executor<'e, Database = sqlx::Postgres>,
    {
        let aggregates = sqlx::query_as::<_, AssociationAggregate>(
            r#"
            SELECT
                identity_a,
                identity_b,
                SUM(sample_count)::INT AS co_occurrence_count,
                COUNT(DISTINCT geo_cell_macro)::INT AS distinct_geo_cells,
                COUNT(DISTINCT (window_start AT TIME ZONE 'UTC')::date)::INT AS distinct_days,
                MIN(window_start) AS first_seen,
                MAX(window_end) AS last_seen
            FROM co_occurrence_events
            GROUP BY identity_a, identity_b
            "#,
        )
        .fetch_all(executor)
        .await?;

        Ok(aggregates)
    }

    /// Recomputes every edge from the recorded events.
    ///
    /// Returns the number of edges written. `computed_through` is stamped with the
    /// latest window start the aggregate covered, so a reader can tell a stale edge
    /// from one the batch simply has not reached yet.
    ///
    /// Edges whose pair no longer has any events are left alone rather than deleted:
    /// this is a rollup over what is *recorded*, and clearing events is a deliberate
    /// act with its own statement, not a side effect of a recompute.
    ///
    /// Like [`crate::repositories::RevocationRepository::revoke_node`], this takes a pool
    /// rather than any `Executor`: it reads the aggregate and then writes one edge per
    /// pair, so the executor has to survive several statements.
    pub async fn recompute(pool: &sqlx::PgPool) -> Result<u64, RepoError> {
        let aggregates = Self::aggregate(pool).await?;
        let mut written = 0_u64;

        for aggregate in aggregates {
            let strength = strength_for(
                aggregate.co_occurrence_count,
                aggregate.distinct_geo_cells,
                aggregate.distinct_days,
            );

            let updated = sqlx::query(
                r#"
                INSERT INTO association_edges (
                    identity_a,
                    identity_b,
                    co_occurrence_count,
                    distinct_geo_cells,
                    distinct_days,
                    first_seen,
                    last_seen,
                    association_strength,
                    computed_through,
                    computed_at
                ) VALUES (
                    $1, $2, $3, $4, $5, $6, $7, $8, $9, now()
                )
                ON CONFLICT (identity_a, identity_b) DO UPDATE SET
                    co_occurrence_count = EXCLUDED.co_occurrence_count,
                    distinct_geo_cells = EXCLUDED.distinct_geo_cells,
                    distinct_days = EXCLUDED.distinct_days,
                    first_seen = EXCLUDED.first_seen,
                    last_seen = EXCLUDED.last_seen,
                    association_strength = EXCLUDED.association_strength,
                    computed_through = EXCLUDED.computed_through,
                    computed_at = EXCLUDED.computed_at
                "#,
            )
            .bind(aggregate.identity_a)
            .bind(aggregate.identity_b)
            .bind(aggregate.co_occurrence_count)
            .bind(aggregate.distinct_geo_cells)
            .bind(aggregate.distinct_days)
            .bind(aggregate.first_seen)
            .bind(aggregate.last_seen)
            .bind(strength)
            .bind(aggregate.last_seen)
            .execute(pool)
            .await?;

            written += updated.rows_affected();
        }

        Ok(written)
    }

    /// The strongest relationships first, at or above `min_strength`.
    pub async fn strongest<'e, E>(
        executor: E,
        min_strength: f32,
        limit: i64,
    ) -> Result<Vec<AssociationEdge>, RepoError>
    where
        E: Executor<'e, Database = sqlx::Postgres>,
    {
        let edges = sqlx::query_as::<_, AssociationEdge>(
            r#"
            SELECT * FROM association_edges
            WHERE association_strength >= $1
            ORDER BY association_strength DESC
            LIMIT $2
            "#,
        )
        .bind(min_strength)
        .bind(limit)
        .fetch_all(executor)
        .await?;

        Ok(edges)
    }

    /// The relationship between two identities, in either order.
    ///
    /// Canonicalises rather than asking the caller to: the pairs are stored under
    /// `identity_a < identity_b`, and a caller that guessed the order gets no row and
    /// no error, which is the worst possible outcome for a question that has an answer.
    pub async fn between<'e, E>(
        executor: E,
        one: Uuid,
        other: Uuid,
    ) -> Result<Option<AssociationEdge>, RepoError>
    where
        E: Executor<'e, Database = sqlx::Postgres>,
    {
        let Some((identity_a, identity_b)) = canonical_pair(one, other) else {
            return Ok(None);
        };

        let edge = sqlx::query_as::<_, AssociationEdge>(
            "SELECT * FROM association_edges WHERE identity_a = $1 AND identity_b = $2",
        )
        .bind(identity_a)
        .bind(identity_b)
        .fetch_optional(executor)
        .await?;

        Ok(edge)
    }

    /// How many edges exist.
    pub async fn count(
        executor: impl Executor<'_, Database = sqlx::Postgres>,
    ) -> Result<i64, RepoError> {
        sqlx::query_scalar("SELECT COUNT(*) FROM association_edges")
            .fetch_one(executor)
            .await
            .map_err(RepoError::Database)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strength_is_always_within_the_column_bounds() {
        // The CHECK on association_strength rejects anything outside 0..=1, so a
        // formula that could exceed it would fail the batch at write time rather than
        // here. Every combination the aggregate can produce has to be inside.
        for count in [0, 1, 7, 20, 21, 1_000_000] {
            for cells in [0, 1, 3, 99] {
                for days in [0, 1, 5, 400] {
                    let strength = strength_for(count, cells, days);
                    assert!(
                        (0.0..=1.0).contains(&strength),
                        "{count}/{cells}/{days} scored {strength}, outside the CHECK"
                    );
                }
            }
        }
    }

    #[test]
    fn nothing_seen_nothing_scores_nothing() {
        assert_eq!(strength_for(0, 0, 0), 0.0);
    }

    #[test]
    fn diversity_beats_volume() {
        // The whole point of the weighting: one place, one day, a hundred sightings is
        // a weaker claim than a handful of sightings across several places and days.
        let transit_stop = strength_for(100, 1, 1);
        let travelling_together = strength_for(6, 3, 5);
        assert!(
            travelling_together > transit_stop,
            "100 co-locations in one place on one day scored {transit_stop}, \
             6 across 3 places on 5 days scored {travelling_together}"
        );
    }

    #[test]
    fn more_days_are_worth_more_than_more_sightings() {
        let same_day_many = strength_for(20, 1, 1);
        let many_days_few = strength_for(4, 1, 5);
        assert!(many_days_few > same_day_many);
    }

    #[test]
    fn negative_counts_cannot_make_a_negative_score() {
        // The CHECK on the counts refuses them in the table, but the formula reads
        // aggregates from SQL and a negative would otherwise be written back out.
        assert_eq!(strength_for(-5, -5, -5), 0.0);
    }
}
