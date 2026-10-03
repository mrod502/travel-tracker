//! The `migrations` table — what the runner knows it has already applied.
//!
//! The registry is itself a migration
//! (`202607312100_create_migrations_registry.sql`), not DDL embedded in the
//! CLI, so it is inspectable as part of schema history and its definition has
//! exactly one source. What lives here is only the bookkeeping: how to
//! bootstrap the table by applying that file, and how to read and write rows.
//!
//! Every write goes through a `Transaction`, because a migration and the row
//! recording it must commit or roll back together.

use crate::exec::{self, Bookkeeping};
use crate::up::{MigrationError, parse_file_name};
use chrono::{DateTime, Local, TimeZone};
use sqlx::{AssertSqlSafe, Pool, Postgres, Row, Transaction};
use std::path::Path;

/// The migration that creates the registry. Its `name` as recorded in the
/// registry itself, and its file name, must agree with what `db up` would
/// derive from the path — so they are derived, not written out.
pub(crate) const REGISTRY_FILE: &str = "202607312100_create_migrations_registry.sql";

/// Value `latest_applied` reports for a database with no applied migration, so
/// that every discovered migration sorts after it.
fn epoch() -> DateTime<Local> {
    Local.with_ymd_and_hms(0, 1, 1, 0, 0, 0).unwrap()
}

/// A migration the runner has applied, as recorded in the registry.
#[derive(Debug, Clone)]
pub(crate) struct AppliedMigration {
    pub(crate) name: String,
    pub(crate) created_at: DateTime<Local>,
}

/// Does the registry table exist in the target database?
pub(crate) async fn registry_exists(conn: &Pool<Postgres>) -> Result<bool, MigrationError> {
    let found: Option<String> = sqlx::query_scalar("SELECT to_regclass('public.migrations')::text")
        .fetch_one(conn)
        .await
        .map_err(|e| MigrationError::new_from("failed to look for the migrations table", e))?;
    Ok(found.is_some())
}

/// Ensure the registry exists, applying the migration that owns it when it
/// does not.
///
/// A database created by an older runner already has the table but no row for
/// the migration that owns it; that row is backfilled so history is complete
/// and `db down` can describe where the floor is.
pub(crate) async fn ensure_registry(
    conn: &Pool<Postgres>,
    migrations_path: &str,
) -> Result<(), MigrationError> {
    let attrs = parse_file_name(&Path::new(migrations_path).join(REGISTRY_FILE))?;

    if registry_exists(conn).await? {
        let recorded: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM migrations WHERE name = $1)")
                .bind(&attrs.name)
                .fetch_one(conn)
                .await
                .map_err(|e| {
                    MigrationError::new_from("failed to read the migrations registry", e)
                })?;
        if !recorded {
            let mut tx = conn
                .begin()
                .await
                .map_err(|e| MigrationError::new_from("failed to begin tx", e))?;
            record_applied(&mut tx, &attrs.name, attrs.created_at).await?;
            tx.commit()
                .await
                .map_err(|e| MigrationError::new_from("failed to commit tx", e))?;
            log::info!(
                "registry predates migration {REGISTRY_FILE}; recorded it as applied so history is complete"
            );
        }
        return Ok(());
    }

    // The registry is created by applying its own migration, through the same
    // path every other migration takes: statements plus the row that records
    // them, one commit. The row is written last, which is the only order that
    // can work when the table creating it is in the same transaction.
    let path = Path::new(migrations_path).join(REGISTRY_FILE);
    let migration = exec::parse_migration(&path)?;
    exec::apply_group(
        conn,
        &migration.up,
        Bookkeeping::Record {
            name: &attrs.name,
            created_at: attrs.created_at,
        },
    )
    .await?;
    log::info!("created the migrations registry from {REGISTRY_FILE}");
    Ok(())
}

/// Insert a migration's registry row. Callers own the transaction, so the row
/// and the migration's own statements commit or roll back together.
pub(crate) async fn record_applied(
    tx: &mut Transaction<'_, Postgres>,
    name: &str,
    created_at: DateTime<Local>,
) -> Result<(), MigrationError> {
    sqlx::query("INSERT INTO migrations (name, created_at) VALUES ($1,$2)")
        .bind(name)
        .bind(created_at)
        .execute(&mut **tx)
        .await
        .map_err(|e| MigrationError::new_from("failed to register migration", e))?;
    Ok(())
}

/// Drop a migration's registry row, as part of reverting it.
pub(crate) async fn forget(
    tx: &mut Transaction<'_, Postgres>,
    name: &str,
) -> Result<(), MigrationError> {
    sqlx::query("DELETE FROM migrations WHERE name = $1")
        .bind(name)
        .execute(&mut **tx)
        .await
        .map_err(|e| MigrationError::new_from("failed to un-register migration", e))?;
    Ok(())
}

/// Newest `created_at` among applied migrations, or an epoch sentinel when
/// nothing has been applied (including when the registry does not exist yet).
pub(crate) async fn latest_applied(
    conn: &Pool<Postgres>,
) -> Result<DateTime<Local>, MigrationError> {
    if !registry_exists(conn).await? {
        return Ok(epoch());
    }
    let newest: Option<DateTime<Local>> =
        sqlx::query_scalar("SELECT MAX(created_at) FROM migrations")
            .fetch_one(conn)
            .await
            .map_err(|e| MigrationError::new_from("failed to fetch latest migration", e))?;
    Ok(newest.unwrap_or_else(epoch))
}

/// Applied migrations, newest first — the order `db down` reverts them in.
pub(crate) async fn applied_newest_first(
    conn: &Pool<Postgres>,
    limit: Option<usize>,
) -> Result<Vec<AppliedMigration>, MigrationError> {
    // `limit` is a usize rendered by Rust, never user text; None means "all".
    let sql = match limit {
        Some(n) => format!(
            "SELECT name, created_at FROM migrations ORDER BY created_at DESC, name DESC LIMIT {n}"
        ),
        None => String::from(
            "SELECT name, created_at FROM migrations ORDER BY created_at DESC, name DESC",
        ),
    };
    let rows = sqlx::query(AssertSqlSafe(sql))
        .fetch_all(conn)
        .await
        .map_err(|e| MigrationError::new_from("failed to read applied migrations", e))?;
    rows.iter()
        .map(|row| {
            let name: String = row
                .try_get(0)
                .map_err(|e| MigrationError::new_from("failed to read migrations.name", e))?;
            let created_at: DateTime<Local> = row
                .try_get(1)
                .map_err(|e| MigrationError::new_from("failed to read migrations.created_at", e))?;
            Ok(AppliedMigration { name, created_at })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The registry has to sort before every real migration, otherwise a fresh
    /// database would try to record migrations in a table that does not exist.
    #[test]
    fn registry_migration_sorts_before_every_committed_migration() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/migrations");
        let registry_ts = parse_file_name(&dir.join(REGISTRY_FILE))
            .expect("registry migration file name must parse")
            .created_at;

        let earlier: Vec<String> = std::fs::read_dir(&dir)
            .expect("migrations directory must exist")
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                let path = entry.path();
                let is_up_migration = path.extension().and_then(|s| s.to_str()) == Some("sql")
                    && !path.to_str().is_some_and(|s| s.ends_with(".down.sql"));
                if !is_up_migration {
                    return None;
                }
                let attrs = parse_file_name(&path).ok()?;
                if attrs.created_at >= registry_ts {
                    return None;
                }
                Some(path.file_name()?.to_string_lossy().into())
            })
            .collect();

        assert!(
            earlier.is_empty(),
            "registry migration must be first; these sort earlier: {earlier:?}"
        );
    }

    /// Bootstrapping is the one migration path that has to work on a database
    /// with nothing in it, so its file must parse as a migration and must not
    /// ask to leave the transaction: the table and the row recording it have to
    /// commit together.
    #[test]
    fn registry_migration_is_a_migration_that_fits_one_transaction() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/migrations");
        let migration =
            exec::parse_migration(&dir.join(REGISTRY_FILE)).expect("registry file must parse");

        assert_eq!(migration.up.len(), 2, "{:?}", migration.up.statements);
        assert!(
            migration.skip_tx_statements().is_empty(),
            "the registry cannot be created outside the transaction that records it"
        );
    }

    /// The name recorded for the registry is derived from the file name, so a
    /// renamed file cannot leave a row nothing matches.
    #[test]
    fn registry_name_matches_its_file_name() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/migrations");
        let attrs = parse_file_name(&dir.join(REGISTRY_FILE)).unwrap();
        assert_eq!(attrs.name, "create_migrations_registry");
    }
}
