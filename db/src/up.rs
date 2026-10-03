use crate::{MIGRATION_TIME_FMT, exec, file_attrs::FileAttrs, registry, runner::Runner};
use async_trait::async_trait;
use chrono::{Local, NaiveDateTime};
use clap::Args;
use sqlx::PgPool;
use std::{
    error::Error,
    fmt::Display,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};
use walkdir::WalkDir;

/// Months of `occurrences` partitions `db up` keeps provisioned beyond the
/// current month. Matches the horizon the partition migration itself opens with.
const PARTITION_HORIZON_MONTHS: i32 = 15;

/// Suffix of the files that used to hold a migration's revert.
///
/// A revert now lives in the migration file's own `--migrate:down` block, so a
/// file with this suffix is not a migration and not a revert — it is a second
/// source of truth. See [`legacy_revert_scripts`].
pub(crate) const DOWN_SUFFIX: &str = ".down.sql";

/// Is this a revert script rather than a migration to apply?
pub(crate) fn is_down_script(path: &Path) -> bool {
    path.to_str().is_some_and(|s| s.ends_with(DOWN_SUFFIX))
}

/// Every `*.down.sql` under `migrations_path`.
///
/// Reverting a migration used to mean a sidecar file beside it. The revert is a
/// `--migrate:down` block inside the migration now, so a leftover sidecar is SQL
/// nothing runs: either its statements already live in the file, in which case
/// it is dead weight that will silently rot, or they do not, in which case the
/// migration is irreversible and looks reversible. Neither is worth discovering
/// during an incident, so both `db up` and `db down` refuse until it is dealt
/// with.
pub(crate) fn legacy_revert_scripts(migrations_path: &str) -> Vec<PathBuf> {
    WalkDir::new(migrations_path)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.into_path())
        .filter(|path| is_down_script(path))
        .collect()
}

/// The refusal both directions share, naming every file that has to go.
pub(crate) fn legacy_revert_scripts_message(found: &[PathBuf]) -> String {
    let list: Vec<String> = found
        .iter()
        .map(|path| path.display().to_string())
        .collect();
    format!(
        "found legacy revert script(s): {}. A revert is now a --migrate:down block inside the \
         migration itself; move these statements into that block and delete the sidecar \
         (see db/src/migrations/README.md).",
        list.join(", ")
    )
}

/// Locations `db` tries when `--migrations-path` is not given, relative to the
/// process working directory: crate root, then workspace root.
const MIGRATION_PATH_CANDIDATES: [&str; 2] = ["src/migrations", "db/src/migrations"];

/// Resolve the directory that holds the migration files, either the operator's
/// `--migrations-path` or the first candidate that exists.
pub(crate) fn resolve_migrations_path(explicit: &str) -> Result<String, MigrationError> {
    if !explicit.is_empty() {
        log::info!("using explicit migrations path: {explicit}");
        return Ok(explicit.to_string());
    }

    let cwd = std::env::current_dir()
        .map_err(|e| MigrationError::new_from("failed to get current directory", e))?;
    for candidate in MIGRATION_PATH_CANDIDATES {
        let path = cwd.join(candidate);
        log::debug!("checking candidate: {path:?}");
        if path.is_dir() {
            log::info!("found migrations at: {candidate}");
            return Ok(candidate.to_string());
        }
    }

    Err(MigrationError::new(format!(
        "could not find migrations directory. Tried: {}. \
         Run with --migrations-path to specify explicitly.",
        MIGRATION_PATH_CANDIDATES.join(", ")
    )))
}

/// Read a migration or revert script off disk.
pub(crate) fn read_migration_file(path: &Path) -> Result<String, MigrationError> {
    let mut f = File::open(path)
        .map_err(|e| MigrationError::new_from(&format!("failed to open {}", path.display()), e))?;
    let mut out = String::new();
    f.read_to_string(&mut out)
        .map_err(|e| MigrationError::new_from(&format!("failed to read {}", path.display()), e))?;
    Ok(out)
}

/// Parse `YYYYMMDDHHMM_<name>.sql` into its name, timestamp, and path.
///
/// The timestamp is the migration's identity for ordering and for the
/// registry, so both `db up` and `db down` derive it the same way.
pub(crate) fn parse_file_name(pth: &Path) -> Result<FileAttrs, MigrationError> {
    let Some(os_name) = pth.file_name() else {
        return Err(MigrationError::new("no filename"));
    };
    let Some(full_name) = os_name.to_str() else {
        return Err(MigrationError::new("failed conversion to str"));
    };
    let Some((date_str, rest)) = full_name.split_once("_") else {
        return Err(MigrationError::new(full_name));
    };

    let created_at = match NaiveDateTime::parse_from_str(date_str, MIGRATION_TIME_FMT) {
        Ok(f) => f,
        Err(e) => {
            return Err(MigrationError::new_from("failed to parse timestamp", e));
        }
    }
    .and_local_timezone(Local)
    .unwrap();

    let Some((name, sql)) = rest.split_once(".") else {
        return Err(MigrationError::new("no extension"));
    };
    if sql != "sql" {
        return Err(MigrationError::new(format!(
            "invalid file extension: {}",
            sql
        )));
    }
    let attrs: FileAttrs = FileAttrs {
        name: name.into(),
        created_at,
        full_path: pth.to_path_buf(),
    };
    Ok(attrs)
}

/// The pending migrations to apply, oldest first, capped at `number` — `0`
/// means "all of them", which is what an uncapped `db up` has always done.
///
/// Sorting happens here rather than at the call site: applying a migration
/// out of timestamp order means applying it against a schema it assumes
/// already exists, and a truncated list taken from an unsorted walk would do
/// exactly that.
fn select_pending(mut pending: Vec<FileAttrs>, number: usize) -> Vec<FileAttrs> {
    pending.sort();
    if number > 0 {
        pending.truncate(number);
    }
    pending
}

#[derive(Debug, Default)]
pub struct MigrationError {
    src: Option<Box<dyn Error + Send + Sync>>,
    #[allow(dead_code)]
    reason: String,
}

impl Error for MigrationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.src
            .as_ref()
            .map(|e| e.as_ref() as &(dyn Error + 'static))
    }

    fn cause(&self) -> Option<&dyn Error> {
        self.source()
    }
}

impl MigrationError {
    pub fn new(reason: impl ToString) -> MigrationError {
        MigrationError {
            src: None,
            reason: reason.to_string(),
        }
    }

    pub fn new_from<E: Error + Send + Sync + 'static>(message: &str, source: E) -> Self {
        Self {
            reason: String::from(message),
            src: Some(Box::new(source)),
        }
    }
}

impl Display for MigrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[derive(Args, Debug, Clone)]
pub struct UpArgs {
    /// Apply at most this many pending migrations. 0 means all of them.
    #[arg(long, short, default_value_t = 0)]
    pub number: usize,
    /// Print what would be applied and change nothing.
    #[arg(long, default_value_t = false)]
    pub dry_run: bool,
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

#[async_trait]
impl Runner for UpArgs {
    type RunError = MigrationError;
    async fn run(&self, maybe_conn: Option<&PgPool>) -> Result<String, MigrationError> {
        let conn = match maybe_conn {
            Some(c) => c,
            None => return Err(MigrationError::new("no conn provided")),
        };
        log::info!("running:{:?}", self);

        let migrations_path = resolve_migrations_path(&self.migrations_path)?;
        let stale = legacy_revert_scripts(&migrations_path);
        if !stale.is_empty() {
            return Err(MigrationError::new(legacy_revert_scripts_message(&stale)));
        }
        registry::ensure_registry(conn, &migrations_path).await?;

        let latest_migration = registry::latest_applied(conn).await?;
        log::trace!("latest migration:{}", latest_migration.to_rfc3339());

        let pending: Vec<FileAttrs> = WalkDir::new(&migrations_path)
            .into_iter()
            .filter_map(|v| -> Option<FileAttrs> {
                let de = match v {
                    Ok(de) => de,
                    Err(_) => return None,
                };
                let pth = de.clone().into_path();
                let ext = pth.extension()?;
                let ext_str = ext.to_str()?;
                if ext_str != "sql" || is_down_script(&pth) {
                    return None;
                }

                let Ok(file_attrs) = parse_file_name(&pth) else {
                    return None;
                };
                if file_attrs.created_at <= latest_migration {
                    return None;
                }

                Some(file_attrs)
            })
            .collect();

        let migrations_to_run = select_pending(pending, self.number);
        if migrations_to_run.is_empty() {
            log::info!("no pending migrations within --number {}", self.number);
        }
        if self.dry_run {
            for mig in &migrations_to_run {
                // Parsed here rather than printed blind: a dry run that cannot
                // describe the statements it would run has nothing to report.
                let migration = exec::parse_migration(&mig.full_path)?;
                let outside = migration.skip_tx_statements().len();
                let outside_note = if outside == 0 {
                    String::new()
                } else {
                    format!(", {outside} outside the transaction")
                };
                println!(
                    "would apply {} ({} statement(s){outside_note})",
                    mig.full_path.display(),
                    migration.up.len()
                );
            }
            log::info!(
                "dry run: {} migration(s) would be applied, nothing changed",
                migrations_to_run.len()
            );
            return Ok(format!("dry run: {} pending", migrations_to_run.len()));
        }

        for mig in &migrations_to_run {
            self.apply_migration(mig.clone(), conn).await?;
        }

        self.ensure_occurrence_partitions(conn).await?;

        Ok(format!("applied {} migration(s)", migrations_to_run.len()))
    }
}

impl UpArgs {
    /// Extend the `occurrences` partition coverage on every `db up`.
    ///
    /// The function is created by
    /// `202609021200_partition_occurrences_forward.sql`; it is looked up rather
    /// than assumed so a database that predates that migration (or a run that
    /// is about to apply it, where this call happens after) still behaves.
    ///
    /// This call is the reason partitions do not silently run out again. With
    /// no caller at all, the horizon set by the migration expires and every
    /// insert fails at the partition boundary — which is precisely how the
    /// write path died in September 2026. An install that goes longer than the
    /// horizon without a redeploy still needs a scheduler; see the function's
    /// own COMMENT.
    async fn ensure_occurrence_partitions(&self, conn: &PgPool) -> Result<(), MigrationError> {
        let present: bool = sqlx::query_scalar(
            "SELECT to_regprocedure('ensure_occurrence_partitions(integer)') IS NOT NULL",
        )
        .fetch_one(conn)
        .await
        .map_err(|e| MigrationError::new_from("failed to look up partition maintenance", e))?;

        if !present {
            log::info!(
                "ensure_occurrence_partitions() not present yet; skipping partition extension"
            );
            return Ok(());
        }

        let created: i32 = sqlx::query_scalar("SELECT ensure_occurrence_partitions($1)")
            .bind(PARTITION_HORIZON_MONTHS)
            .fetch_one(conn)
            .await
            .map_err(|e| MigrationError::new_from("failed to extend occurrence partitions", e))?;

        log::info!(
            "occurrence partitions ensured {} months ahead ({} created)",
            PARTITION_HORIZON_MONTHS,
            created
        );
        Ok(())
    }

    /// Apply one migration: the file's `up` statements and its registry row,
    /// committed together.
    ///
    /// The file is parsed here, as it is applied, rather than at discovery —
    /// the statements that run are the ones in the file on disk, and a file
    /// that cannot be parsed stops before it touches the database.
    async fn apply_migration(&self, attrs: FileAttrs, conn: &PgPool) -> Result<(), MigrationError> {
        log::info!("applying {} as {}", attrs.full_path.display(), attrs.name);
        let migration = exec::parse_migration(&attrs.full_path)?;
        exec::apply_group(
            conn,
            &migration.up,
            exec::Bookkeeping::Record {
                name: &attrs.name,
                created_at: attrs.created_at,
            },
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Local, TimeZone};
    use std::path::PathBuf;

    // ========================================================================
    // Filename parsing tests
    // ========================================================================

    #[test]
    fn test_parse_file_name_valid() {
        let mut pb = PathBuf::new();
        pb.push("202603081008_create_users.sql");

        let result = parse_file_name(&pb);
        assert!(result.is_ok(), "Expected Ok, got {:?}", result);

        let attrs = result.unwrap();
        assert_eq!(attrs.name, "create_users");
        assert_eq!(
            attrs.created_at,
            Local.with_ymd_and_hms(2026, 3, 8, 10, 8, 0).unwrap()
        );
    }

    #[test]
    fn test_parse_file_name_with_timestamp() {
        let mut pb = PathBuf::new();
        pb.push("202607312147_create_bluetooth_occurrences.sql");

        let result = parse_file_name(&pb);
        assert!(result.is_ok(), "Expected Ok, got {:?}", result);

        let attrs = result.unwrap();
        assert_eq!(attrs.name, "create_bluetooth_occurrences");
        assert_eq!(
            attrs.created_at,
            Local.with_ymd_and_hms(2026, 7, 31, 21, 47, 0).unwrap()
        );
    }

    #[test]
    fn test_parse_file_name_no_extension() {
        let pb = PathBuf::from("202603081008_no_extension");

        let result = parse_file_name(&pb);
        assert!(result.is_err(), "Expected error for file without extension");
    }

    #[test]
    fn test_parse_file_name_invalid_extension() {
        let pb = PathBuf::from("202603081008_wrong_extension.txt");

        let result = parse_file_name(&pb);
        assert!(result.is_err(), "Expected error for non-SQL extension");
    }

    #[test]
    fn test_parse_file_name_invalid_timestamp() {
        let pb = PathBuf::from("invalid_timestamp_create_users.sql");

        let result = parse_file_name(&pb);
        assert!(
            result.is_err(),
            "Expected error for invalid timestamp format"
        );
    }

    #[test]
    fn test_parse_file_name_timestamp_too_short() {
        let pb = PathBuf::from("20260308_create_users.sql"); // 8 digits instead of 12

        let result = parse_file_name(&pb);
        assert!(result.is_err(), "Expected error for incomplete timestamp");
    }

    #[test]
    fn test_parse_file_name_timestamp_too_long() {
        let pb = PathBuf::from("2026030810081234_extra.sql");

        let result = parse_file_name(&pb);
        // The parser expects exactly 14 digits for timestamp
        // Any extra digits cause parse failure since "0810081234_extra" isn't valid
        assert!(result.is_err(), "Should fail due to malformed timestamp");
    }

    // ========================================================================
    // Statement splitting
    // ========================================================================
    //
    // `db` used to split each file itself: `sqlparser` first, a hand-written
    // scanner where that gave up (plpgsql bodies, `DROP EXTENSION`, `uuidv7()`
    // generated columns). Both are gone. `db::Lexer` is the only splitter now and
    // it knows no dialect, so the behaviour these tests pinned down is covered
    // where it now lives: `db/src/lex/mod.rs` for strings, comments, dollar
    // quotes and `$1`; `db/tests/parse_migration_file.rs` for every committed
    // migration file; `db/src/exec.rs` for which of those statements run inside
    // the transaction.

    // ========================================================================
    // File reading tests
    // Note: These tests use test-specific file paths that don't require temp dirs
    // ========================================================================

    #[test]
    fn test_read_file_not_found() {
        let file_path = PathBuf::from("/nonexistent/path/file.sql");

        let result = read_migration_file(&file_path);

        assert!(result.is_err());
    }

    // ========================================================================
    // --number selection tests
    // ========================================================================

    fn pending_migrations(names: &[&str]) -> Vec<FileAttrs> {
        let base = Local.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        names
            .iter()
            .enumerate()
            .map(|(i, name)| FileAttrs {
                name: name.to_string(),
                full_path: PathBuf::from(format!("{name}.sql")),
                created_at: base + chrono::Duration::days(i as i64),
            })
            .collect()
    }

    /// `--number 0` is "everything pending", which is what `db up` has always
    /// done, so an unchanged invocation keeps its behaviour.
    #[test]
    fn number_zero_applies_every_pending_migration() {
        let selected = select_pending(pending_migrations(&["a", "b", "c"]), 0);
        assert_eq!(
            selected.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );
    }

    /// The defect this closes: `--number N` used to be parsed, printed at
    /// startup, and then ignored, so an operator limiting the run to one
    /// migration actually applied all of them.
    #[test]
    fn number_limits_the_pending_migrations_in_order() {
        let pending = pending_migrations(&["a", "b", "c"]);

        let one = select_pending(pending.clone(), 1);
        assert_eq!(one.len(), 1, "must not apply every pending migration");
        assert_eq!(one[0].name, "a", "oldest pending goes first");

        let two = select_pending(pending.clone(), 2);
        assert_eq!(
            two.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
            vec!["a", "b"]
        );
    }

    /// Asking for more than is pending is not an error — there is simply
    /// nothing more to do.
    #[test]
    fn number_larger_than_the_pending_set_applies_what_exists() {
        let selected = select_pending(pending_migrations(&["a"]), 5);
        assert_eq!(selected.len(), 1);
    }

    /// The cap is only meaningful if the list was sorted first: taking the
    /// first N of a raw directory walk would apply migrations in whatever
    /// order the filesystem returned them, against a schema that assumes
    /// otherwise.
    #[test]
    fn number_selects_the_oldest_migrations_however_they_arrive() {
        let base = Local.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let at = |name: &str, days: i64| FileAttrs {
            name: name.to_string(),
            full_path: PathBuf::from(format!("{name}.sql")),
            created_at: base + chrono::Duration::days(days),
        };
        let walk_order = vec![at("latest", 30), at("middle", 10), at("first", 1)];

        let selected = select_pending(walk_order, 2);
        assert_eq!(
            selected.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
            vec!["first", "middle"],
            "must apply oldest-first regardless of discovery order"
        );
    }

    #[test]
    fn empty_pending_set_stays_empty_whatever_number_says() {
        assert!(select_pending(vec![], 3).is_empty());
        assert!(select_pending(vec![], 0).is_empty());
    }

    // ========================================================================
    // Revert-script discovery tests
    // ========================================================================

    #[test]
    fn down_scripts_are_not_migrations() {
        assert!(is_down_script(Path::new(
            "202607312146_create_nodes.down.sql"
        )));
        assert!(!is_down_script(Path::new("202607312146_create_nodes.sql")));
    }

    /// A `*.down.sql` left behind is SQL nothing runs: the revert it holds is
    /// either duplicated inside the migration or lost entirely. Finding one has
    /// to be loud.
    #[test]
    fn legacy_revert_scripts_are_found_and_named() {
        let dir = std::env::temp_dir().join("db_up_legacy_sidecars");
        std::fs::create_dir_all(&dir).expect("temp dir is writable");
        std::fs::write(dir.join("202601010000_things.sql"), "SELECT 1;").unwrap();
        std::fs::write(
            dir.join("202601010000_things.down.sql"),
            "DROP TABLE things;",
        )
        .unwrap();

        let found = legacy_revert_scripts(dir.to_str().unwrap());
        assert_eq!(found.len(), 1, "the migration itself is not one: {found:?}");
        assert!(found[0].ends_with("202601010000_things.down.sql"));

        let message = legacy_revert_scripts_message(&found);
        assert!(message.contains("202601010000_things.down.sql"));
        assert!(
            message.contains("--migrate:down"),
            "the refusal has to say what to do instead: {message}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The shape the runner expects — one file per migration, revert inside it —
    /// must not be flagged.
    #[test]
    fn a_directory_of_migrations_alone_is_not_flagged() {
        let dir = std::env::temp_dir().join("db_up_no_sidecars");
        std::fs::create_dir_all(&dir).expect("temp dir is writable");
        std::fs::write(dir.join("202601010000_things.sql"), "SELECT 1;").unwrap();

        assert!(legacy_revert_scripts(dir.to_str().unwrap()).is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    // ========================================================================
    // FileAttrs ordering tests
    // ========================================================================

    #[test]
    fn test_file_attrs_ordering_by_time() {
        let earlier = FileAttrs {
            name: "earlier".to_string(),
            full_path: PathBuf::from("earlier.sql"),
            created_at: Local.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
        };

        let later = FileAttrs {
            name: "later".to_string(),
            full_path: PathBuf::from("later.sql"),
            created_at: Local.with_ymd_and_hms(2026, 12, 31, 23, 59, 59).unwrap(),
        };

        assert!(earlier < later);
        assert!(later > earlier);
    }

    #[test]
    fn test_file_attrs_ordering_by_path_when_times_equal() {
        let ts = Local.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();

        let path_a = FileAttrs {
            name: "a".to_string(),
            full_path: PathBuf::from("01_a.sql"),
            created_at: ts,
        };

        let path_b = FileAttrs {
            name: "b".to_string(),
            full_path: PathBuf::from("02_b.sql"),
            created_at: ts,
        };

        assert!(path_a < path_b);
    }

    #[test]
    fn test_file_attrs_ordering_by_name_when_both_equal() {
        let ts = Local.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();

        let name_a = FileAttrs {
            name: "aaa".to_string(),
            full_path: PathBuf::from("same.sql"),
            created_at: ts,
        };

        let name_b = FileAttrs {
            name: "bbb".to_string(),
            full_path: PathBuf::from("same.sql"),
            created_at: ts,
        };

        assert!(name_a < name_b);
    }

    // ========================================================================
    // Migration atomicity
    // ========================================================================
    //
    // What used to sit here tested the splitter against the PostgreSQL
    // constructs that defeated it: `uuidv7()` defaults, `GENERATED ALWAYS AS
    // (fn(x)) STORED`, `H3INDEX`, `USING GIN`, an extension's own function
    // names. The lexer has no parser to defeat, so those inputs are covered as
    // ordinary statement text in db/tests/parse_migration_file.rs, and what
    // "atomic" actually means — the group and its registry row committing
    // together, and `skipTx` being the one way out — is tested in db/src/exec.rs.

    // ========================================================================
    // Error handling tests
    // ========================================================================

    #[test]
    fn test_migration_error_display() {
        let error = MigrationError::new("test error");
        let display = format!("{}", error);

        // Should display as Debug since Display delegates to Debug
        assert!(display.contains("MigrationError"));
    }

    #[test]
    fn test_migration_error_with_source() {
        let source = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let error = MigrationError::new_from("wrapped error", source);

        assert!(error.source().is_some());
    }

    #[test]
    fn test_migration_error_no_source() {
        let error = MigrationError::new("simple error");

        assert!(error.source().is_none());
    }
}
