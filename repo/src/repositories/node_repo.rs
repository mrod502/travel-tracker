//! Repository for the `nodes` registry table.
//!
//! `occurrences.origin_node_id` has a foreign key to `nodes(node_id)`, so a node
//! must have a registry row before any occurrence it signs can be stored. Rows
//! are written at CA enrollment: `ca_credential` is NOT NULL by design because
//! it is what lets a verifier trace a signing key back to the CA without a live
//! CA call.

use sqlx::Executor;
use sqlx::Row;

use crate::error::RepoError;
use crate::models::{Node, NodeType};
use crate::types::H3Index;
use h3o::CellIndex;

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
    /// * `owns_geo_cells` - Res 6 cells the node is authoritative for. An empty
    ///   slice means "no claim made", which is the correct answer for a mobile
    ///   node — and, on a re-enrollment, means the existing claim survives:
    ///   ownership is granted out-of-band and renewing a credential must not
    ///   silently erase it.
    ///
    /// The cells bind as `h3index[]` through [`H3Index`], which is the one place
    /// that knows how that type crosses the wire.
    pub async fn register<'e, E>(
        executor: E,
        node_id: &[u8],
        node_type: NodeType,
        signing_public_key: &[u8],
        ca_credential: &[u8],
        fixed_location: Option<(f64, f64)>,
        owns_geo_cells: &[CellIndex],
    ) -> Result<(), RepoError>
    where
        E: Executor<'e, Database = sqlx::Postgres>,
    {
        let (fixed_lat, fixed_lon) = match fixed_location {
            Some((lat, lon)) => (Some(lat), Some(lon)),
            None => (None, None),
        };
        // NULL means "no claim made", which is what the column stores for that;
        // an empty array would be an assertion that the node owns nothing.
        let cells: Option<Vec<H3Index>> = (!owns_geo_cells.is_empty()).then(|| {
            owns_geo_cells
                .iter()
                .copied()
                .map(H3Index::from)
                .collect::<Vec<_>>()
        });

        sqlx::query(
            r#"
INSERT INTO nodes (
    node_id,
    node_type,
    signing_public_key,
    ca_credential,
    fixed_lat,
    fixed_lon,
    owns_geo_cells
) VALUES (
    $1, $2, $3, $4, $5, $6, $7
)
ON CONFLICT (node_id) DO UPDATE SET
    node_type = EXCLUDED.node_type,
    signing_public_key = EXCLUDED.signing_public_key,
    ca_credential = EXCLUDED.ca_credential,
    fixed_lat = EXCLUDED.fixed_lat,
    fixed_lon = EXCLUDED.fixed_lon,
    owns_geo_cells = COALESCE($7, nodes.owns_geo_cells)
            "#,
        )
        .bind(node_id)
        .bind(node_type)
        .bind(signing_public_key)
        .bind(ca_credential)
        .bind(fixed_lat)
        .bind(fixed_lon)
        .bind(cells)
        .execute(executor)
        .await?;

        Ok(())
    }

    /// The res 6 cells a node is recorded as owning.
    pub async fn owns_geo_cells<'e, E>(
        executor: E,
        node_id: &[u8],
    ) -> Result<Vec<CellIndex>, RepoError>
    where
        E: Executor<'e, Database = sqlx::Postgres>,
    {
        let row = sqlx::query("SELECT owns_geo_cells FROM nodes WHERE node_id = $1")
            .bind(node_id)
            .fetch_optional(executor)
            .await?
            .ok_or_else(|| RepoError::not_found("node"))?;

        let cells: Option<Vec<H3Index>> = row.try_get("owns_geo_cells")?;

        Ok(cells
            .unwrap_or_default()
            .into_iter()
            .map(CellIndex::from)
            .collect())
    }

    /// Load a node's registry row.
    ///
    /// `SELECT *` into [`Node`]'s `FromRow` derive on purpose: the model claims to
    /// mirror the table, and reading every column is what makes that claim fail
    /// loudly the moment the two disagree — which is how the `Vec<i64>` declaration
    /// for `owns_geo_cells` (an `h3index[]` column) would otherwise survive until
    /// something tried to load a node in production.
    pub async fn find_by_id<'e, E>(executor: E, node_id: &[u8]) -> Result<Option<Node>, RepoError>
    where
        E: Executor<'e, Database = sqlx::Postgres>,
    {
        let node = sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE node_id = $1")
            .bind(node_id)
            .fetch_optional(executor)
            .await?;

        Ok(node)
    }

    /// Nodes in the registry, whatever their status.
    pub async fn count<'e, E>(executor: E) -> Result<i64, RepoError>
    where
        E: Executor<'e, Database = sqlx::Postgres>,
    {
        sqlx::query_scalar("SELECT COUNT(*) FROM nodes")
            .fetch_one(executor)
            .await
            .map_err(RepoError::Database)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of the `Option` in `register` is that "no cells" and
    /// "no claim" are the same thing to the caller and both become NULL, so the
    /// `COALESCE` in the upsert leaves an existing claim alone.
    #[test]
    fn no_cells_binds_as_null_rather_than_an_empty_array() {
        let liberty_macro = CellIndex::try_from(604222352263217151).unwrap();

        let wrap = |cells: &[CellIndex]| -> Option<Vec<H3Index>> {
            (!cells.is_empty())
                .then(|| cells.iter().copied().map(H3Index::from).collect::<Vec<_>>())
        };

        assert_eq!(wrap(&[]), None);

        let claimed = wrap(&[liberty_macro]);
        assert_eq!(
            claimed.as_deref(),
            Some([H3Index::from(liberty_macro)].as_slice())
        );
        assert_eq!(CellIndex::from(claimed.unwrap()[0]), liberty_macro);
    }
}
