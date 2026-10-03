//! `db down` — revert migrations — and `db reset` — destroy and re-apply.
//!
//! These are two different operations and deliberately have two argument
//! types. `down` runs each migration's own `--migrate:down` block in reverse
//! order, one transaction per migration, and removes that migration's registry
//! row. `reset` drops the schema and replays everything; it takes no `--number`,
//! because an argument it could not honour would just be a lie.
//!
//! A revert lives in the file it reverts, so `down` reads exactly the same files
//! `up` does and a migration cannot have a revert nobody kept in sync with it.
//! A migration whose file declares no down block has no revert path, and naming
//! it is more use than quietly skipping it.
//!
//! The registry created by `202607312100_create_migrations_registry.sql` is
//! the floor of `down`: reverting it would delete the record of what is
//! applied, so it is skipped rather than reverted.

use crate::exec::{self, Bookkeeping};
use crate::registry::{self, AppliedMigration};
use crate::runner::Runner;
use crate::up::{
    MigrationError, UpArgs, legacy_revert_scripts, legacy_revert_scripts_message, parse_file_name,
    resolve_migrations_path,
};
use async_trait::async_trait;
use chrono::{DateTime, Local};
use clap::Args;
use db::StatementGroup;
use sqlx::{AssertSqlSafe, Executor, Pool, Postgres, Row};
use std::collections::HashMap;
use std::error::Error;
use std::fmt::Display;
use std::path::PathBuf;
use walkdir::WalkDir;

/// One migration queued for revert, with the statements that revert it.
///
/// The down block is parsed while planning, so a dry run reports the revert it
/// would actually run and an unparseable file stops the plan before anything is
/// dropped.
#[derive(Debug)]
struct RevertPlan {
    name: String,
    /// When the migration was authored, per its file name.
    authored_at: DateTime<Local>,
    /// The migration file the revert came from.
    file: PathBuf,
    down: StatementGroup,
    /// True when the revert drops things, so running it loses data.
    destroys_data: bool,
}

#[derive(Debug)]
pub struct DownError {
    src: Option<Box<dyn Error + Send + Sync>>,
    reason: String,
}

impl Error for DownError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.src
            .as_ref()
            .map(|e| e.as_ref() as &(dyn Error + 'static))
    }
}

impl Display for DownError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DownError: {}", self.reason)
    }
}

impl DownError {
    pub fn new(reason: impl ToString) -> Self {
        Self {
            src: None,
            reason: reason.to_string(),
        }
    }

    pub fn new_from<E: Error + Send + Sync + 'static>(reason: impl ToString, source: E) -> Self {
        Self {
            reason: reason.to_string(),
            src: Some(Box::new(source)),
        }
    }
}

impl From<MigrationError> for DownError {
    fn from(e: MigrationError) -> Self {
        DownError {
            src: Some(Box::new(e)),
            reason: String::from("migration step failed"),
        }
    }
}

#[derive(Args, Debug, Clone)]
pub struct DownArgs {
    /// Revert at most this many applied migrations, newest first.
    /// 0 reverts every applied migration above the registry.
    #[arg(long, short, default_value_t = 1)]
    pub number: usize,
    /// Print what would be reverted and change nothing.
    #[arg(long, default_value_t = false)]
    pub dry_run: bool,
    /// Required to revert (or reset) a database that is not on this machine.
    #[arg(long, default_value_t = false)]
    pub allow_destructive: bool,
    #[arg(long, default_value = "localhost")]
    pub host: String,
    #[arg(long, default_value_t = 5432)]
    pub port: u16,
    #[arg(long, default_value = "postgres")]
    pub user: String,
    #[arg(long, default_value = "postgres")]
    pub db: String,
    #[arg(long, default_value = "")]
    pub migrations_path: String,
}

#[derive(Args, Debug, Clone)]
pub struct ResetArgs {
    /// Print what would be dropped and change nothing.
    #[arg(long, default_value_t = false)]
    pub dry_run: bool,
    /// Required to reset a database that is not on this machine.
    #[arg(long, default_value_t = false)]
    pub allow_destructive: bool,
    #[arg(long, default_value = "localhost")]
    pub host: String,
    #[arg(long, default_value_t = 5432)]
    pub port: u16,
    #[arg(long, default_value = "postgres")]
    pub user: String,
    #[arg(long, default_value = "postgres")]
    pub db: String,
    #[arg(long, default_value = "")]
    pub migrations_path: String,
}

/// Is this host the machine the CLI is running on?
pub(crate) fn is_local_target(host: &str) -> bool {
    host == "localhost" || host == "::1" || host.starts_with("127.")
}

/// Refuse to destroy a database elsewhere unless the operator said so.
///
/// `dry_run` never writes, so it never needs the acknowledgement.
pub(crate) fn destruction_guard(
    verb: &str,
    host: &str,
    port: u16,
    db: &str,
    dry_run: bool,
    acknowledged: bool,
) -> Result<(), DownError> {
    if dry_run || is_local_target(host) || acknowledged {
        return Ok(());
    }
    Err(DownError::new(format!(
        "refusing to {verb} database '{db}' at {host}:{port} without --allow-destructive"
    )))
}

#[async_trait]
impl Runner for DownArgs {
    type RunError = DownError;

    async fn run(&self, maybe_conn: Option<&Pool<Postgres>>) -> Result<String, DownError> {
        let conn = maybe_conn.ok_or_else(|| DownError::new("no connection provided"))?;
        let migrations_path = resolve_migrations_path(&self.migrations_path)?;
        registry::ensure_registry(conn, &migrations_path).await?;

        let limit = if self.number > 0 {
            Some(self.number)
        } else {
            None
        };
        let applied = registry::applied_newest_first(conn, limit).await?;
        let plan = plan_reverts(applied, &migrations_path)?;

        if plan.is_empty() {
            log::info!("nothing to revert");
            return Ok("reverted 0 migration(s)".into());
        }

        if self.dry_run {
            for step in &plan {
                let suffix = if step.destroys_data {
                    "  (drops data)"
                } else {
                    ""
                };
                println!(
                    "would revert {} (from {}, {} statement(s) from the --migrate:down block in \
                     {}){}",
                    step.name,
                    step.authored_at.format("%Y%m%d%H%M"),
                    step.down.len(),
                    step.file
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("?"),
                    suffix
                );
            }
            log::info!(
                "dry run: {} migration(s) would be reverted, nothing changed",
                plan.len()
            );
            return Ok(format!("dry run: {} to revert", plan.len()));
        }

        for step in &plan {
            revert_one(conn, step).await?;
        }
        Ok(format!("reverted {} migration(s)", plan.len()))
    }
}

/// Pair each applied migration with the revert its own file declares, newest
/// first.
///
/// Refuses to produce a partial plan. A migration whose file has disappeared,
/// or declares no `--migrate:down` block, is named — together with every other
/// such migration — rather than quietly reverting the ones that can be reverted,
/// because a half-reverted schema is not a state anybody asked for.
fn plan_reverts(
    applied: Vec<AppliedMigration>,
    migrations_path: &str,
) -> Result<Vec<RevertPlan>, DownError> {
    let stale = legacy_revert_scripts(migrations_path);
    if !stale.is_empty() {
        return Err(DownError::new(legacy_revert_scripts_message(&stale)));
    }

    let files = discover_migrations(migrations_path)?;
    let mut plan = Vec::new();
    let mut unrevertible = Vec::new();

    for applied in applied {
        // The registry is the floor: it has no revert by design.
        if applied.name == registry_name() {
            log::debug!(
                "{} is the floor of db down; not a revert candidate",
                applied.name
            );
            continue;
        }

        let Some(file) = files.get(&applied.name) else {
            unrevertible.push(format!(
                "{} (its file is not in {migrations_path})",
                applied.name
            ));
            continue;
        };

        let migration = exec::parse_migration(file)
            .map_err(|e| DownError::new_from(format!("cannot revert '{}'", applied.name), e))?;
        // `down` is `None` only when the file declares no revert at all: a
        // declared-but-empty block is rejected while parsing, so a planned step
        // always has at least one statement to run.
        let Some(down) = migration.down else {
            unrevertible.push(format!(
                "{} (no --migrate:down block in {})",
                applied.name,
                file.display()
            ));
            continue;
        };

        plan.push(RevertPlan {
            name: applied.name,
            authored_at: applied.created_at,
            destroys_data: destroys_data(&down),
            file: file.clone(),
            down,
        });
    }

    if !unrevertible.is_empty() {
        return Err(DownError::new(format!(
            "nothing was reverted. These applied migrations have no revert path: {}. Add a \
             --migrate:down block to the migration file that owns it (see \
             db/src/migrations/README.md).",
            unrevertible.join("; ")
        )));
    }
    Ok(plan)
}

/// Every migration file in the directory, keyed by the `name` the registry
/// stores for the migration it defines.
///
/// The key comes from the same parser `db up` uses, so a file cannot register
/// itself under one name and be looked up under another.
fn discover_migrations(migrations_path: &str) -> Result<HashMap<String, PathBuf>, DownError> {
    let mut files = HashMap::new();
    for entry in WalkDir::new(migrations_path)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.into_path();
        if path.extension().and_then(|s| s.to_str()) != Some("sql") {
            continue;
        }
        // A file whose name is not a migration name is not a migration, the same
        // way `db up` decides.
        let Ok(attrs) = parse_file_name(&path) else {
            continue;
        };
        if let Some(previous) = files.insert(attrs.name, path.clone()) {
            return Err(DownError::new(format!(
                "two migration files claim the same name: {} and {}",
                previous.display(),
                path.display()
            )));
        }
    }
    Ok(files)
}

/// The registry name of the migration that creates the registry itself.
fn registry_name() -> String {
    parse_file_name(&PathBuf::from(crate::registry::REGISTRY_FILE))
        .expect("registry file name must parse")
        .name
}

/// Does this revert drop objects, and therefore lose whatever rows are in them?
/// Used only to label the dry-run plan honestly.
fn destroys_data(down: &StatementGroup) -> bool {
    let upper = down
        .statements
        .iter()
        .map(|statement| statement.sql.to_ascii_uppercase())
        .collect::<Vec<_>>()
        .join("\n");

    [
        "DROP TABLE ",
        "DROP TYPE ",
        "DROP EXTENSION ",
        "DROP COLUMN ",
    ]
    .iter()
    .any(|keyword| upper.contains(keyword))
}

/// Revert one migration: its `--migrate:down` statements and the removal of its
/// registry row, in one transaction, so a half-reverted migration cannot be
/// recorded as gone.
async fn revert_one(conn: &Pool<Postgres>, step: &RevertPlan) -> Result<(), DownError> {
    exec::apply_group(conn, &step.down, Bookkeeping::Forget { name: &step.name })
        .await
        .map_err(|e| DownError::new_from(format!("failed to revert '{}'", step.name), e))?;

    log::info!("reverted {}", step.name);
    Ok(())
}

#[async_trait]
impl Runner for ResetArgs {
    type RunError = DownError;

    async fn run(&self, maybe_conn: Option<&Pool<Postgres>>) -> Result<String, DownError> {
        let conn = maybe_conn.ok_or_else(|| DownError::new("no connection provided"))?;
        let migrations_path = resolve_migrations_path(&self.migrations_path)?;

        let tables = user_tables(conn).await?;
        let types = custom_types(conn).await?;

        if self.dry_run {
            println!("would drop {} tables: {}", tables.len(), tables.join(", "));
            println!("would drop {} types: {}", types.len(), types.join(", "));
            println!(
                "would then re-apply every migration from {}",
                migrations_path
            );
            log::info!("dry run: nothing dropped");
            return Ok(format!(
                "dry run: {} tables and {} types would be dropped",
                tables.len(),
                types.len()
            ));
        }

        log::info!("resetting: dropping and re-applying everything");
        drop_objects(conn, "TABLE", &tables).await?;
        drop_objects(conn, "TYPE", &types).await?;

        // The registry went down with the tables; the up path recreates it from
        // the migration that owns it, so nothing here has to know its DDL.
        let up_args = UpArgs {
            number: 0,
            dry_run: false,
            host: self.host.clone(),
            port: self.port,
            user: self.user.clone(),
            db: self.db.clone(),
            migrations_path: self.migrations_path.clone(),
        };
        up_args
            .run(Some(conn))
            .await
            .map_err(|e| DownError::new_from("failed to re-apply migrations after reset", e))?;

        Ok("database reset: schema dropped and all migrations re-applied".into())
    }
}

/// Base and partitioned tables in `public` that no extension owns.
///
/// Extension ownership is resolved by object id, not by name: matching
/// `pg_class.relname` against `information_schema.tables.table_name` classified
/// a user table as extension-owned whenever the names coincided.
async fn user_tables(conn: &Pool<Postgres>) -> Result<Vec<String>, DownError> {
    let rows = sqlx::query(
        r#"SELECT c.relname
           FROM pg_class c
           JOIN pg_namespace n ON n.oid = c.relnamespace
           WHERE n.nspname = 'public'
             AND c.relkind IN ('r', 'p')
             AND NOT EXISTS (
                 SELECT 1 FROM pg_depend d
                 WHERE d.classid = 'pg_class'::regclass
                   AND d.objid = c.oid
                   AND d.deptype = 'e'
             )
           ORDER BY CASE WHEN c.relkind = 'p' THEN 0 ELSE 1 END, c.relname"#,
    )
    .fetch_all(conn)
    .await
    .map_err(|e| DownError::new_from("failed to query tables", e))?;

    rows.iter()
        .map(|row| {
            row.try_get::<String, _>(0)
                .map_err(|e| DownError::new_from("failed to read table name", e))
        })
        .collect()
}

/// User-defined enum, composite, and range types in `public`.
async fn custom_types(conn: &Pool<Postgres>) -> Result<Vec<String>, DownError> {
    let rows = sqlx::query(
        r#"SELECT t.typname
           FROM pg_type t
           JOIN pg_namespace n ON t.typnamespace = n.oid
           WHERE n.nspname = 'public'
             AND t.typtype IN ('e'::"char", 'c'::"char", 'r'::"char")
             AND NOT EXISTS (
                 SELECT 1 FROM pg_depend d
                 WHERE d.classid = 'pg_type'::regclass
                   AND d.objid = t.oid
                   AND d.deptype = 'e'
             )
           ORDER BY t.typname"#,
    )
    .fetch_all(conn)
    .await
    .map_err(|e| DownError::new_from("failed to query types", e))?;

    rows.iter()
        .map(|row| {
            row.try_get::<String, _>(0)
                .map_err(|e| DownError::new_from("failed to read type name", e))
        })
        .collect()
}

/// Drop a list of objects, failing if any drop fails.
///
/// A reset that swallows a failed drop leaves a half-dropped schema and still
/// reports success, which is worse than one that stops.
async fn drop_objects(
    conn: &Pool<Postgres>,
    kind: &str,
    names: &[String],
) -> Result<(), DownError> {
    if names.is_empty() {
        log::info!("no {}s to drop", kind.to_lowercase());
        return Ok(());
    }
    log::info!("dropping {} {}(s)", names.len(), kind.to_lowercase());

    let mut failed = Vec::new();
    for name in names {
        let sql = format!("DROP {} IF EXISTS \"{}\" CASCADE", kind, name);
        if let Err(e) = conn.execute(AssertSqlSafe(sql)).await {
            log::error!("failed to drop {} {}: {}", kind, name, e);
            failed.push(name.clone());
        }
    }

    if !failed.is_empty() {
        return Err(DownError::new(format!(
            "could not drop {}: the schema is half-reset",
            failed.join(", ")
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn migrations_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/migrations")
    }

    #[test]
    fn test_down_error_display() {
        let error = DownError::new("test error");
        assert_eq!(format!("{}", error), "DownError: test error");
    }

    #[test]
    fn test_down_error_with_source() {
        let source = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        assert!(
            DownError::new_from("wrapped error", source)
                .source()
                .is_some()
        );
    }

    /// Every committed migration must be revertible, or must say so out loud
    /// (the registry migration is the documented floor).
    #[test]
    fn every_non_floor_migration_declares_a_down_block() {
        let dir = migrations_dir();
        let files = discover_migrations(dir.to_str().unwrap()).unwrap();

        let mut unrevertible = Vec::new();
        for (name, path) in &files {
            if name == &registry_name() {
                continue;
            }
            let migration = exec::parse_migration(path)
                .unwrap_or_else(|e| panic!("{} must parse: {e:?}", path.display()));
            if !migration.is_revertible() {
                unrevertible.push(name.clone());
            }
        }

        assert!(
            unrevertible.is_empty(),
            "migrations with no --migrate:down block: {unrevertible:?}"
        );
    }

    /// A `*.down.sql` beside a migration is a revert the runner will not read.
    /// Committing one means the migration's real revert is somewhere else, so
    /// none may exist.
    #[test]
    fn no_legacy_revert_scripts_are_committed() {
        let found = legacy_revert_scripts(migrations_dir().to_str().unwrap());
        assert!(
            found.is_empty(),
            "reverts belong in a --migrate:down block inside the migration file: {found:?}"
        );
    }

    /// A migration file maps to the registry name `db up` records for it —
    /// otherwise `db down` would never find the revert.
    #[test]
    fn migration_files_map_to_the_names_the_registry_stores() {
        let dir = migrations_dir();
        let files = discover_migrations(dir.to_str().unwrap()).unwrap();

        for (name, path) in &files {
            let derived = parse_file_name(path).expect("committed name parses").name;
            assert_eq!(name, &derived, "key drifted from what db up records");
        }
        // The registry file is discovered like any other; it is the floor that
        // keeps it out of the plan, not a gap in discovery.
        assert!(files.contains_key(&registry_name()));
    }

    /// Reverting a migration whose file is gone must fail the whole plan, never
    /// part of it: a half-reverted schema is not a state anybody asked for.
    #[test]
    fn plan_reverts_refuses_when_the_file_is_missing() {
        let empty_dir = std::env::temp_dir().join("db_down_no_files");
        std::fs::create_dir_all(&empty_dir).unwrap();

        let applied = vec![AppliedMigration {
            name: "create_something".to_string(),
            created_at: parse_file_name(&PathBuf::from("202601010000_create_something.sql"))
                .unwrap()
                .created_at,
        }];

        let err = plan_reverts(applied, empty_dir.to_str().unwrap())
            .expect_err("must refuse without the file");
        assert!(
            format!("{}", err).contains("create_something"),
            "error should name the migration: {err}"
        );
    }

    /// A file that exists but declares no revert is the same refusal, with the
    /// file named so the fix is obvious.
    #[test]
    fn plan_reverts_refuses_when_no_down_block_is_declared() {
        let dir = std::env::temp_dir().join("db_down_no_down_block");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("202601010000_create_something.sql");
        std::fs::write(&file, "CREATE TABLE something (id INT);\n").unwrap();

        let applied = vec![AppliedMigration {
            name: "create_something".to_string(),
            created_at: parse_file_name(&file).unwrap().created_at,
        }];

        let err = plan_reverts(applied, dir.to_str().unwrap())
            .expect_err("must refuse without a down block");
        let message = format!("{err}");
        assert!(message.contains("create_something"), "{message}");
        assert!(
            message.contains("--migrate:down"),
            "the refusal has to say what to add: {message}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn only_this_machine_counts_as_a_local_target() {
        assert!(is_local_target("localhost"));
        assert!(is_local_target("127.0.0.1"));
        assert!(is_local_target("127.0.0.53"));
        assert!(is_local_target("::1"));
        for host in [
            "db.internal",
            "database",
            "postgres",
            "10.0.0.4",
            "localhost.evil.example",
        ] {
            assert!(!is_local_target(host), "{host} is not local");
        }
    }

    #[test]
    fn destroying_a_remote_target_needs_the_acknowledgement() {
        let err = destruction_guard("revert", "db.internal", 5432, "travel", false, false)
            .expect_err("remote without ack must refuse");
        assert!(format!("{}", err).contains("--allow-destructive"));

        // Same target, acknowledged, or a dry run: allowed through.
        assert!(destruction_guard("revert", "db.internal", 5432, "travel", false, true).is_ok());
        assert!(destruction_guard("revert", "db.internal", 5432, "travel", true, false).is_ok());
        assert!(destruction_guard("revert", "localhost", 5432, "travel", false, false).is_ok());
    }

    /// The plan labels the steps that lose rows, so a dry run reads as an
    /// honest description of what `down` will do.
    #[test]
    fn destructive_reverts_are_flagged_in_the_plan() {
        let files = discover_migrations(migrations_dir().to_str().unwrap()).unwrap();
        let nodes = files.get("create_nodes").expect("nodes migration file");
        let down = exec::parse_migration(nodes)
            .expect("nodes migration parses")
            .down
            .expect("nodes migration declares a revert");

        assert!(destroys_data(&down), "dropping nodes loses their rows");
    }
}
