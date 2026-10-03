//! The migration corpus, projected onto sqlx's own runner.
//!
//! [`Migrator`]'s unit is a whole file: it hands the file's text to PostgreSQL
//! and records a row in `_sqlx_migrations`. It has no idea what `--migrate:`
//! means, and a directive line is just a comment to it — so pointed at this
//! crate's migrations directory it would run a migration *and its revert* on the
//! way up, since the `--migrate:down` block is ordinary SQL between two comment
//! lines. The failure is not hypothetical: `DROP TYPE node_type` in the revert of
//! one file drops the type the next file's column depends on.
//!
//! This module is the adapter. It reads each file with the real parser and gives
//! sqlx only what [`crate::Migration::up`] holds — statements split by the
//! library lexer, in file order, re-joined with `;`, with the revert left where
//! it belongs: with `db down`.
//!
//! ```
//! use db::SQLX_FORWARD_MIGRATOR;
//!
//! // The committed corpus, forward-only, ready for sqlx's runner.
//! assert!(!SQLX_FORWARD_MIGRATOR.migrations.is_empty());
//! ```
//!
//! [`Migrator`]: sqlx::migrate::Migrator

use std::borrow::Cow;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use sqlx::migrate::{Migration as SqlxMigration, MigrationType, Migrator};
use sqlx::{AssertSqlSafe, SqlSafeStr};

use crate::{DirectiveRegistry, Migration, MigrationParser, StandardMigrationParser};

/// The directory this crate ships its migrations in, absolute at compile time.
///
/// A path-dependency crate directory is fixed for a given build, which is what
/// makes it usable from a `static`: consumers reach the corpus through
/// [`SQLX_FORWARD_MIGRATOR`] without any of them having to name a path — and
/// `#[sqlx::test(migrations = "...")]` could not name it anyway, because that
/// attribute resolves relative to the *consuming* crate and rejects absolute
/// paths.
pub const MIGRATIONS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/migrations");

/// The committed corpus, forward-only.
///
/// Parsed from [`MIGRATIONS_DIR`] on first use, then reused. It exists for
/// consumers that run migrations through sqlx rather than through the `db`
/// binary — most often the `#[sqlx::test]` suites in sibling crates:
///
/// ```ignore
/// #[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
/// async fn inserts_a_node(pool: sqlx::PgPool) { /* ... */ }
/// ```
///
/// # Panics
///
/// Panics if the shipped corpus cannot be read or parsed — see
/// [`ForwardMigratorError`]. A corpus that fails to load is a broken checkout or
/// a broken build, not a runtime condition a caller can recover from, and every
/// consumer of this static is going to fail anyway; `#[sqlx::test]` in
/// particular has no way to supply a `Migrator` that returns a `Result`.
/// `cargo test -p db` covers the corpus on every commit, so in practice the panic
/// arrives as a parse failure in the test output rather than at an unrelated
/// consumer's first run.
pub static SQLX_FORWARD_MIGRATOR: LazyLock<Migrator> = LazyLock::new(|| {
    forward_migrator(MIGRATIONS_DIR.as_ref())
        .unwrap_or_else(|e| panic!("cannot build the forward migrator from {MIGRATIONS_DIR}: {e}"))
});

/// Everything that can stop a directory from becoming a [`Migrator`].
#[derive(Debug)]
pub enum ForwardMigratorError {
    /// The directory or a file in it could not be read.
    Io {
        /// What was being read.
        path: PathBuf,
        /// The OS failure.
        source: std::io::Error,
    },
    /// A file is not a well-formed migration.
    Parse {
        /// The offending file.
        path: PathBuf,
        /// What the parser said.
        source: crate::MigrationParseError,
    },
    /// The file name does not start with the `<YYYYMMDDHHMM>_` prefix that
    /// carries the migration's version.
    MissingVersion {
        /// The file with the unrecognisable name.
        path: PathBuf,
    },
    /// Two files claim the same version, so neither has a defined order.
    DuplicateVersion {
        /// The contested version.
        version: i64,
        /// The file that repeated it.
        path: PathBuf,
        /// The file that got there first.
        other: PathBuf,
    },
    /// A `*.down.sql` sidecar is present. Its statements belong in the
    /// migration file's `--migrate:down` block; `db up` and `db down` refuse the
    /// same directory for the same reason.
    LegacyRevertScript {
        /// The sidecar.
        path: PathBuf,
    },
}

impl fmt::Display for ForwardMigratorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(f, "failed to read {}: {source}", path.display())
            }
            Self::Parse { path, source } => {
                write!(f, "failed to parse {}: {source}", path.display())
            }
            Self::MissingVersion { path } => write!(
                f,
                "{}: migration file names must start with <YYYYMMDDHHMM>_ so the version and \
                 the order are unambiguous",
                path.display()
            ),
            Self::DuplicateVersion {
                version,
                path,
                other,
            } => write!(
                f,
                "{} and {} are both version {version}; the order between them is undefined",
                path.display(),
                other.display()
            ),
            Self::LegacyRevertScript { path } => write!(
                f,
                "found legacy revert script {}: a revert is now a --migrate:down block inside \
                 the migration itself (see db/src/migrations/README.md)",
                path.display()
            ),
        }
    }
}

impl std::error::Error for ForwardMigratorError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Parse { source, .. } => Some(source),
            Self::MissingVersion { .. }
            | Self::DuplicateVersion { .. }
            | Self::LegacyRevertScript { .. } => None,
        }
    }
}

/// Build a forward-only [`Migrator`] over the `*.sql` files under `dir`.
///
/// One sqlx migration per file, ordered by the version in its name, each holding
/// only that file's [`crate::Migration::up`] statements. Nothing else about the
/// corpus reaches sqlx: no directive lines, no `down` blocks.
///
/// [`crate::StatementOption::OptSkipTx`] is the one directive the projection
/// cannot express per-statement, because sqlx's unit of transactionality is the
/// whole migration. A file that asks for it is marked
/// [`no_tx`](SqlxMigration::no_tx), which moves the entire migration outside the
/// transaction rather than one statement — coarser, and the only honest option:
/// silently keeping the rest inside would run a statement PostgreSQL rejects and
/// fail the migration instead. No migration in the committed corpus needs it.
///
/// Two files sharing a version is refused here rather than left to sqlx: `db up`
/// would apply both in name order and record two rows, while sqlx keys
/// `_sqlx_migrations` on the version and fails on the second with a primary-key
/// violation that names neither file.
pub fn forward_migrator(dir: &Path) -> Result<Migrator, ForwardMigratorError> {
    let mut migrations = Vec::new();
    let mut claimed: Vec<(i64, PathBuf)> = Vec::new();

    for path in sql_files(dir)? {
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".down.sql"))
        {
            return Err(ForwardMigratorError::LegacyRevertScript { path });
        }

        let version = version_of(&path)?;
        if let Some((_, first)) = claimed.iter().find(|(v, _)| *v == version) {
            return Err(ForwardMigratorError::DuplicateVersion {
                version,
                other: first.clone(),
                path: path.clone(),
            });
        }
        claimed.push((version, path.clone()));

        let migration = parse_file(&path)?;
        let no_tx = !migration.skip_tx_statements().is_empty();

        migrations.push(SqlxMigration::new(
            version,
            Cow::Owned(description_of(&path, version)),
            MigrationType::Simple,
            AssertSqlSafe(up_sql(&migration)).into_sql_str(),
            no_tx,
        ));
    }

    migrations.sort();

    Ok(Migrator {
        migrations: Cow::Owned(migrations),
        // The remaining fields are sqlx's own defaults. They are `#[doc(hidden)]
        // pub` — structurally public so `migrate!()` can initialise a `const`,
        // and exempt from semver — so they are taken wholesale from
        // `Migrator::DEFAULT` rather than repeated here field by field.
        ..Migrator::DEFAULT
    })
}

/// Every `*.sql` under `dir`, in path order, so a walk whose order the OS
/// chooses cannot decide what `version_of` tie-breaks on.
fn sql_files(dir: &Path) -> Result<Vec<PathBuf>, ForwardMigratorError> {
    let mut paths: Vec<PathBuf> = walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.into_path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "sql"))
        .collect();
    paths.sort();
    Ok(paths)
}

/// The `<YYYYMMDDHHMM>` prefix of a file name, as sqlx's integer version.
///
/// Twelve digits fit an `i64` with room to spare, and the value sorts the same
/// numerically as the name does lexically, which is the order `db up` uses.
fn version_of(path: &Path) -> Result<i64, ForwardMigratorError> {
    let stem = path.file_stem().and_then(|stem| stem.to_str());
    let digits = stem.and_then(|stem| stem.split('_').next());
    let version = digits.and_then(|digits| digits.parse::<i64>().ok());
    version.ok_or_else(|| ForwardMigratorError::MissingVersion {
        path: path.to_path_buf(),
    })
}

/// The part of the file name after the version, which is what the `db` registry
/// calls a migration and what `_sqlx_migrations.description` should show.
fn description_of(path: &Path, version: i64) -> String {
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("");
    match stem.strip_prefix(&format!("{version}_")) {
        Some(rest) if !rest.is_empty() => rest.to_string(),
        _ => stem.to_string(),
    }
}

/// The `up` statements, re-joined the way PostgreSQL wants to receive them.
///
/// The lexer already resolved every `;` — including the ones inside function
/// bodies, dollar-quoted strings, and trigger definitions — so joining with `;`
/// reproduces exactly the statements the `db` binary would run, as one script.
fn up_sql(migration: &Migration) -> String {
    let mut sql = String::new();
    for statement in &migration.up.statements {
        sql.push_str(statement.sql.trim());
        sql.push_str(";\n");
    }
    sql
}

/// Parse one migration file with the crate's own parser.
fn parse_file(path: &Path) -> Result<Migration, ForwardMigratorError> {
    let source = std::fs::read_to_string(path).map_err(|source| ForwardMigratorError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let registry =
        DirectiveRegistry::with_builtins().expect("built-in directive keys are distinct");
    StandardMigrationParser::new(path.to_path_buf())
        .parse(&source, &registry)
        .map_err(|source| ForwardMigratorError::Parse {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, contents: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, contents).expect("write fixture");
        path
    }

    #[test]
    fn joins_the_up_statements_with_semicolons() {
        let migration = StandardMigrationParser::in_memory()
            .parse(
                "--migrate:up.begin\nCREATE TABLE a (id INT);\nCREATE INDEX i ON a (id);\n--migrate:up.end\n",
                &DirectiveRegistry::with_builtins().expect("registry"),
            )
            .expect("parses");

        assert_eq!(
            up_sql(&migration),
            "CREATE TABLE a (id INT);\nCREATE INDEX i ON a (id);\n"
        );
    }

    #[test]
    fn reads_the_version_and_description_out_of_the_name() {
        let path = Path::new("202607312146_create_nodes.sql");
        assert_eq!(version_of(path).expect("version"), 202607312146);
        assert_eq!(description_of(path, 202607312146), "create_nodes");
    }

    #[test]
    fn a_name_without_a_version_prefix_is_refused() {
        let path = Path::new("create_nodes.sql");
        assert!(matches!(
            version_of(path),
            Err(ForwardMigratorError::MissingVersion { .. })
        ));
    }

    #[test]
    fn a_legacy_sidecar_in_the_directory_is_refused() {
        let dir = std::env::temp_dir().join("db_forward_legacy_sidecar");
        std::fs::create_dir_all(&dir).expect("temp dir is writable");
        write(
            &dir,
            "202601010000_create_things.sql",
            "--migrate:up.begin\nCREATE TABLE things (id INT);\n--migrate:up.end\n",
        );
        write(
            &dir,
            "202601010000_create_things.down.sql",
            "DROP TABLE things;\n",
        );

        let error = forward_migrator(&dir).expect_err("a sidecar is not a migration");
        assert!(
            matches!(error, ForwardMigratorError::LegacyRevertScript { .. }),
            "got {error}"
        );
        assert!(
            error
                .to_string()
                .contains("202601010000_create_things.down.sql"),
            "the message names the file: {error}"
        );
    }

    #[test]
    fn two_files_sharing_a_version_are_refused() {
        let dir = std::env::temp_dir().join("db_forward_duplicate_version");
        std::fs::create_dir_all(&dir).expect("temp dir is writable");
        write(
            &dir,
            "202601010001_a_first.sql",
            "--migrate:up.begin\nCREATE TABLE a (id INT);\n--migrate:up.end\n",
        );
        write(
            &dir,
            "202601010001_b_second.sql",
            "--migrate:up.begin\nCREATE TABLE b (id INT);\n--migrate:up.end\n",
        );

        let error = forward_migrator(&dir).expect_err("one version, two migrations");
        assert!(
            matches!(
                error,
                ForwardMigratorError::DuplicateVersion {
                    version: 202601010001,
                    ..
                }
            ),
            "got {error}"
        );
        assert!(
            error.to_string().contains("b_second"),
            "the message names both files: {error}"
        );
    }

    #[test]
    fn a_skip_tx_statement_moves_the_whole_migration_out_of_the_transaction() {
        let dir = std::env::temp_dir().join("db_forward_skip_tx");
        std::fs::create_dir_all(&dir).expect("temp dir is writable");
        write(
            &dir,
            "202601010002_add_index.sql",
            "--migrate:up.begin\nCREATE TABLE t (id INT);\n--migrate:skipTx\nCREATE INDEX CONCURRENTLY i ON t (id);\n--migrate:up.end\n",
        );

        let migrator = forward_migrator(&dir).expect("projects");
        assert_eq!(migrator.migrations.len(), 1);
        assert!(
            migrator.migrations[0].no_tx,
            "sqlx cannot honour skipTx per statement, so the whole migration leaves the tx"
        );
    }

    #[test]
    fn the_shipped_corpus_projects_without_error() {
        let migrator = forward_migrator(MIGRATIONS_DIR.as_ref()).expect("shipped corpus projects");
        assert!(!migrator.migrations.is_empty());
        assert!(
            migrator
                .migrations
                .windows(2)
                .all(|w| w[0].version < w[1].version),
            "versions must be strictly ascending: {:?}",
            versions(&migrator)
        );
    }

    #[test]
    fn the_static_and_the_directory_agree() {
        let from_dir = forward_migrator(MIGRATIONS_DIR.as_ref()).expect("projects");
        assert_eq!(
            versions(&SQLX_FORWARD_MIGRATOR),
            versions(&from_dir),
            "the static is the directory"
        );
    }

    #[test]
    fn the_projection_holds_no_revert_statements() {
        let migrator = forward_migrator(MIGRATIONS_DIR.as_ref()).expect("projects");

        for shipped in migrator.migrations.iter() {
            let file = PathBuf::from(MIGRATIONS_DIR)
                .join(format!("{}_{}.sql", shipped.version, shipped.description));
            let parsed = parse_file(&file).unwrap_or_else(|e| panic!("{file:?}: {e}"));

            // Statement-for-statement rather than by substring: in
            // 202609041200 the `up` and the `down` share their first statement
            // (both drop the generated columns before re-adding them), so text
            // containment would report a leak where there is none.
            let projected = statements_in(shipped.sql.as_str());
            let expected: Vec<String> = parsed
                .up
                .statements
                .iter()
                .map(|statement| fold(&statement.sql))
                .collect();
            assert_eq!(
                projected,
                expected,
                "{}: the forward script is not the file's `up`, statement for statement",
                file.display()
            );

            if let Some(down) = &parsed.down {
                assert_eq!(
                    projected.len(),
                    parsed.up.len(),
                    "{}: the `down` block's {} statement(s) are in the forward path",
                    file.display(),
                    down.len()
                );
            }
        }
    }

    #[test]
    fn every_up_statement_survives_the_projection() {
        let migrator = forward_migrator(MIGRATIONS_DIR.as_ref()).expect("projects");

        for shipped in migrator.migrations.iter() {
            let file = PathBuf::from(MIGRATIONS_DIR)
                .join(format!("{}_{}.sql", shipped.version, shipped.description));
            let parsed = parse_file(&file).unwrap_or_else(|e| panic!("{file:?}: {e}"));

            assert_eq!(
                up_sql(&parsed),
                shipped.sql.as_str(),
                "{}: the forward projection is not the file's `up`",
                file.display()
            );
        }
    }

    /// Re-lex a forward script into folded statement text.
    fn statements_in(sql: &str) -> Vec<String> {
        crate::Lexer::new(sql, "projection.sql")
            .filter_map(|token| token.ok())
            .filter_map(|token| match token {
                crate::LexToken::Statement(statement) => Some(fold(&statement.sql)),
                crate::LexToken::Directive(_) => None,
            })
            .collect()
    }

    fn fold(text: &str) -> String {
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn versions(migrator: &Migrator) -> Vec<i64> {
        migrator.migrations.iter().map(|m| m.version).collect()
    }
}
