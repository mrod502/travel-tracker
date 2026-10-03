//! Applying a parsed migration to a live database.
//!
//! The library half of this crate turns a file into a [`Migration`]: statements
//! in order, each carrying the options its directives asked for. This module is
//! the other half — running those statements and writing the `migrations`
//! registry row that tells the next run what happened. Both directions go
//! through here so `db up` and `db down` cannot drift into two different ideas
//! of when a migration counts as applied.
//!
//! # One transaction, unless the file says otherwise
//!
//! A group's statements and its registry row commit together in a single
//! transaction: a migration that fails halfway leaves neither, and a migration
//! that is recorded was applied in full.
//!
//! [`StatementOption::OptSkipTx`] cannot keep that promise — PostgreSQL rejects
//! `CREATE INDEX CONCURRENTLY` inside a transaction block — so a group carrying
//! it runs in three commits instead of one:
//!
//! 1. a transaction holding the group's transactional statements;
//! 2. each `skipTx` statement, in file order, autocommitted one at a time;
//! 3. a final transaction holding the registry row.
//!
//! A failure in step 2 or 3 therefore leaves step 1 committed *and the row
//! unwritten*: the migration is not marked applied, and the next run replays it
//! against a schema it has partly changed. That is the deliberate trade — a
//! half-applied migration that announces itself loudly on the next `db up`
//! beats one quietly recorded as done. The error names the phase that failed.
//!
//! [`Migration`]: db::Migration

use std::path::Path;

use chrono::{DateTime, Local};
use db::{
    DirectiveRegistry, Migration, MigrationParser, StandardMigrationParser, Statement,
    StatementGroup, StatementOption,
};
use sqlx::{AssertSqlSafe, Executor, PgPool, Transaction};

use crate::registry;
use crate::up::{MigrationError, read_migration_file};

/// What happens to the `migrations` row alongside a group's statements.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Bookkeeping<'a> {
    /// Record the migration as applied (`db up`).
    Record {
        name: &'a str,
        created_at: DateTime<Local>,
    },
    /// Stop recording it (`db down`).
    Forget { name: &'a str },
}

impl Bookkeeping<'_> {
    /// The registry change, applied inside the caller's transaction.
    async fn write(&self, tx: &mut Transaction<'_, sqlx::Postgres>) -> Result<(), MigrationError> {
        match self {
            Bookkeeping::Record { name, created_at } => {
                registry::record_applied(tx, name, *created_at).await
            }
            Bookkeeping::Forget { name } => registry::forget(tx, name).await,
        }
    }
}

/// Read a migration file and parse it, reporting failures against its path.
///
/// Errors are wrapped, not flattened: a parse failure keeps the typed error as
/// its source, so the `file:line:col: message` the library produces survives to
/// whatever prints the cause chain.
pub(crate) fn parse_migration(path: &Path) -> Result<Migration, MigrationError> {
    let source = read_migration_file(path)?;
    let registry = DirectiveRegistry::with_builtins().map_err(|e| {
        MigrationError::new_from("built-in directive keys collided; this is a bug in db", e)
    })?;
    StandardMigrationParser::new(path.to_path_buf())
        .parse(&source, &registry)
        .map_err(|e| {
            let context = format!("failed to parse {}", path.display());
            MigrationError::new_from(&context, e)
        })
}

/// Run a group's statements plus its registry row. See the module docs for what
/// "plus" costs when the group contains a `skipTx` statement.
pub(crate) async fn apply_group(
    conn: &PgPool,
    group: &StatementGroup,
    bookkeeping: Bookkeeping<'_>,
) -> Result<(), MigrationError> {
    let (inside, outside) = split_by_tx(group);

    if !outside.is_empty() {
        return apply_split(conn, &inside, &outside, bookkeeping).await;
    }

    let mut tx = conn
        .begin()
        .await
        .map_err(|e| MigrationError::new_from("failed to begin tx", e))?;
    if let Err(e) = run_all(&mut tx, &inside).await {
        let _ = tx.rollback().await;
        return Err(e);
    }
    if let Err(e) = bookkeeping.write(&mut tx).await {
        let _ = tx.rollback().await;
        return Err(e);
    }
    tx.commit()
        .await
        .map_err(|e| MigrationError::new_from("failed to commit migration", e))
}

/// Split a group into the statements that run in the transaction and the ones
/// that must run outside it, each keeping its place in file order.
fn split_by_tx(group: &StatementGroup) -> (Vec<&Statement>, Vec<&Statement>) {
    group
        .statements
        .iter()
        .partition(|s| !s.has_option(StatementOption::OptSkipTx))
}

/// Statements in a caller-owned transaction, in order.
async fn run_all<'a>(
    tx: &mut Transaction<'a, sqlx::Postgres>,
    statements: &[&Statement],
) -> Result<(), MigrationError> {
    for statement in statements {
        log::trace!("executing: {}", one_line(&statement.sql));
        if let Err(e) = tx.execute(AssertSqlSafe(statement.sql.clone())).await {
            log::error!("statement failed: {}", one_line(&statement.sql));
            return Err(MigrationError::new_from("failed to execute statement", e));
        }
    }
    Ok(())
}

/// The three-commit path a `skipTx` statement forces, described in the module
/// docs: transactional statements, then the statements PostgreSQL will only run
/// on its own, then the registry row.
async fn apply_split(
    conn: &PgPool,
    inside: &[&Statement],
    outside: &[&Statement],
    bookkeeping: Bookkeeping<'_>,
) -> Result<(), MigrationError> {
    log::warn!(
        "{} statement(s) marked skipTx: this migration cannot run in one transaction and will \
         not be atomic",
        outside.len()
    );

    let mut tx = conn
        .begin()
        .await
        .map_err(|e| MigrationError::new_from("failed to begin tx", e))?;
    if let Err(e) = run_all(&mut tx, inside).await {
        let _ = tx.rollback().await;
        return Err(e);
    }
    tx.commit()
        .await
        .map_err(|e| MigrationError::new_from("failed to commit the transactional part", e))?;

    for statement in outside {
        log::trace!(
            "executing outside a transaction: {}",
            one_line(&statement.sql)
        );
        if let Err(e) = conn.execute(AssertSqlSafe(statement.sql.clone())).await {
            log::error!(
                "statement failed outside a transaction: {}",
                one_line(&statement.sql)
            );
            return Err(MigrationError::new_from(
                "failed to execute statement outside the transaction; the transactional \
                 statements above it are already committed and the registry row was not \
                 written, so this migration will be retried",
                e,
            ));
        }
    }

    let mut tx = conn
        .begin()
        .await
        .map_err(|e| MigrationError::new_from("failed to begin tx", e))?;
    if let Err(e) = bookkeeping.write(&mut tx).await {
        let _ = tx.rollback().await;
        return Err(e);
    }
    tx.commit()
        .await
        .map_err(|e| MigrationError::new_from("failed to commit the registry row", e))
}

/// A statement on one log line, so a failure stays readable.
fn one_line(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use db::{DirectiveRegistry, MigrationParseError, Span};
    use std::path::PathBuf;

    fn parse(source: &str) -> Result<Migration, MigrationParseError> {
        let registry = DirectiveRegistry::with_builtins().expect("built-ins are distinct");
        StandardMigrationParser::in_memory().parse(source, &registry)
    }

    fn stmt(sql: &str, options: Vec<StatementOption>) -> Statement {
        Statement {
            sql: sql.to_string(),
            options,
            span: Span::new(PathBuf::from("<test>"), 1, 1),
        }
    }

    /// A file with no directives is entirely `up`, which is what every shipped
    /// migration looked like before revert blocks moved into the file.
    #[test]
    fn a_plain_file_parses_as_up_with_no_revert() {
        let migration = parse("CREATE TABLE t (id INT);\n").expect("plain SQL is a migration");
        assert_eq!(migration.up.len(), 1);
        assert_eq!(migration.down, None);
    }

    #[test]
    fn nothing_is_marked_to_leave_the_transaction_by_default() {
        let migration = parse("--migrate:up.begin\nCREATE TABLE t (id INT);\n--migrate:up.end\n")
            .expect("valid");
        let (inside, outside) = split_by_tx(&migration.up);
        assert_eq!(inside.len(), 1);
        assert!(outside.is_empty());
    }

    /// The order within each phase is the order in the file: a `skipTx` index
    /// belongs on the column the earlier statement added.
    #[test]
    fn skip_tx_statements_are_taken_in_file_order() {
        let migration = parse(
            "--migrate:up.begin\n\
             CREATE TABLE t (id INT);\n\
             --migrate:skipTx\n\
             CREATE INDEX CONCURRENTLY i ON t (id);\n\
             INSERT INTO t VALUES (1);\n\
             --migrate:skipTx\n\
             ANALYZE t;\n\
             --migrate:up.end\n",
        )
        .expect("valid");

        let (inside, outside) = split_by_tx(&migration.up);
        assert_eq!(
            inside.iter().map(|s| s.sql.as_str()).collect::<Vec<_>>(),
            vec!["CREATE TABLE t (id INT)", "INSERT INTO t VALUES (1)"]
        );
        assert_eq!(
            outside.iter().map(|s| s.sql.as_str()).collect::<Vec<_>>(),
            vec!["CREATE INDEX CONCURRENTLY i ON t (id)", "ANALYZE t"]
        );
    }

    /// A `skipTx` in the revert block is honoured the same way as in `up`; the
    /// executor does not care which direction it is running.
    #[test]
    fn the_revert_block_carries_its_own_options() {
        let migration = parse(
            "--migrate:up.begin\nCREATE TABLE t (id INT);\n--migrate:up.end\n\
             --migrate:down.begin\n\
             --migrate:skipTx\n\
             DROP INDEX CONCURRENTLY i;\n\
             DROP TABLE t;\n\
             --migrate:down.end\n",
        )
        .expect("valid");

        let down = migration.down.expect("declared revert");
        let (inside, outside) = split_by_tx(&down);
        assert_eq!(inside.len(), 1);
        assert_eq!(outside.len(), 1);
        assert_eq!(outside[0].sql, "DROP INDEX CONCURRENTLY i");
    }

    /// `one_line` is what makes a failure readable in the log; a multi-line
    /// statement must not scatter across the terminal.
    #[test]
    fn statements_are_logged_on_one_line() {
        assert_eq!(
            one_line("CREATE TABLE t (\n    id INT,\n    name TEXT\n)"),
            "CREATE TABLE t ( id INT, name TEXT )"
        );
    }

    /// A migration that cannot be parsed has to fail with its path in the
    /// message, not as an unnamed syntax complaint.
    #[test]
    fn a_broken_file_names_itself() {
        let path = std::env::temp_dir().join("db_exec_broken_migration.sql");
        std::fs::write(
            &path,
            "--migrate:up.begin\nCREATE TABLE t (id INT);\n--migrate:down.begin\nDROP TABLE t;\n",
        )
        .expect("temp file is writable");

        let error = parse_migration(&path).expect_err("unterminated down block");
        let message = format!("{error:?}");
        assert!(
            message.contains(path.to_str().expect("utf-8 temp path")),
            "the error must name the file: {message}"
        );
        std::fs::remove_file(&path).ok();
    }

    /// Every committed migration has to parse through this function — it is the
    /// only way into the database now.
    #[test]
    fn every_committed_migration_parses() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/migrations");
        let mut names: Vec<String> = Vec::new();

        for entry in std::fs::read_dir(&dir).expect("migrations directory must exist") {
            let path = entry.expect("readable entry").path();
            if path.extension().and_then(|s| s.to_str()) != Some("sql") {
                continue;
            }
            names.push(path.file_name().unwrap().to_string_lossy().into_owned());
            parse_migration(&path)
                .unwrap_or_else(|e| panic!("{} must parse as a migration: {e:?}", path.display()));
        }

        assert!(names.len() > 1, "expected the real corpus, found {names:?}");
    }

    /// A `Statement` built by hand carries its options exactly as
    /// `has_option` reports them — the executor's whole input surface.
    #[test]
    fn hand_built_groups_partition_the_same_way() {
        let group = StatementGroup::from_statements(vec![
            stmt("CREATE TABLE t (id INT)", vec![]),
            stmt(
                "CREATE INDEX CONCURRENTLY i ON t (id)",
                vec![StatementOption::OptSkipTx],
            ),
        ]);
        let (inside, outside) = split_by_tx(&group);
        assert_eq!(inside.len(), 1);
        assert_eq!(outside.len(), 1);
    }
}
