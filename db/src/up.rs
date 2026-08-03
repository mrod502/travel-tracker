use crate::{MIGRATION_TIME_FMT, file_attrs::FileAttrs, runner::Runner};
use async_trait::async_trait;
use chrono::{DateTime, Local, NaiveDateTime, TimeZone};
use clap::Args;
use sqlparser::parser::Parser;
use sqlparser::dialect::PostgreSqlDialect;
use sqlx::{AssertSqlSafe, Executor, PgPool, Pool, Postgres, Row, Transaction};
use std::{error::Error, fmt::Display, fs::File, io::Read, path::PathBuf, usize};
use walkdir::WalkDir;

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
    #[arg(long, short, default_value_t = 0)]
    pub number: usize,
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
        let latest_migration = self.get_latest_applied_migration(conn).await?;
        log::trace!("latest migration:{}", latest_migration.to_rfc3339());

        // Resolve migrations path: if empty, auto-detect based on current directory
        let migrations_path = if self.migrations_path.is_empty() {
            let detected = self.detect_migrations_path()?;
            log::info!("auto-detected migrations path: {}", detected);
            detected
        } else {
            log::info!("using explicit migrations path: {}", self.migrations_path);
            self.migrations_path.clone()
        };

        let mut migrations_to_run: Vec<FileAttrs> = WalkDir::new(&migrations_path)
            .into_iter()
            .filter_map(|v| -> Option<FileAttrs> {
                let de = match v {
                    Ok(de) => de,
                    Err(_) => return None,
                };
                let pth = de.clone().into_path();
                let Some(ext) = pth.extension() else {
                    return None;
                };
                let Some(ext_str) = ext.to_str() else {
                    return None;
                };
                if ext_str != "sql" {
                    return None;
                }

                let Ok(file_attrs) = Self::parse_file_name(&pth) else {
                    return None;
                };
                if file_attrs.created_at <= latest_migration {
                    return None;
                }

                Some(file_attrs)
            })
            .collect();
        migrations_to_run.sort();
        for mig in migrations_to_run {
            self.apply_migration(mig, conn).await?;
        }

        Ok("".into())
    }
}

impl UpArgs {
    /// Auto-detect the migrations directory by checking common locations
    fn detect_migrations_path(&self) -> Result<String, MigrationError> {
        let cwd = std::env::current_dir()
            .map_err(|e| MigrationError::new_from("failed to get current directory", e))?;
        log::info!("current directory: {:?}", cwd);

        // Common locations to check (in order of preference)
        let candidates = vec![
            "src/migrations".to_string(),    // Running from crate root (db/)
            "db/src/migrations".to_string(), // Running from workspace root
        ];

        for candidate in &candidates {
            let path = cwd.join(candidate);
            log::info!(
                "checking candidate: {:?} (exists: {}, is_dir: {})",
                path,
                path.exists(),
                path.is_dir()
            );
            if path.exists() && path.is_dir() {
                log::info!("found migrations at: {}", candidate);
                return Ok(candidate.clone());
            }
        }

        Err(MigrationError::new(
            "could not find migrations directory. Tried: src/migrations, db/src/migrations. \
             Run with --migrations-path to specify explicitly.",
        ))
    }

    fn parse_file_name<'b>(pth: &'b PathBuf) -> Result<FileAttrs, MigrationError> {
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
            full_path: pth.clone(),
        };
        Ok(attrs)
    }
    async fn apply_migration(
        &self,
        attrs: FileAttrs,
        conn: &Pool<Postgres>,
    ) -> Result<(), MigrationError> {
        log::info!("attrs:{:?}", attrs);
        let mut tx = match conn.begin().await {
            Ok(t) => t,
            Err(e) => return Err(MigrationError::new_from("failed to begin tx", e)),
        };

        // Read file outside of transaction (file I/O doesn't need transaction)
        let migration = self.read_file(&attrs.full_path)?;

        // Register migration in the transaction
        let reg_result = self.add_migration_to_registry(&mut tx, &attrs).await;
        if reg_result.is_err() {
            let _ = tx.rollback().await;
            return Err(MigrationError::new("failed to register migration"));
        }

        let statements = Self::split_query(&migration);

        for statement in statements {
            if statement.trim().is_empty() {
                continue;
            }
            let exec_result = sqlx::query(AssertSqlSafe(statement.clone()))
                .execute(&mut *tx)
                .await;

            if let Err(e) = exec_result {
                let _ = tx.rollback().await;
                log::error!("migration failed: {}", statement);
                return Err(MigrationError::new_from(
                    "failed to execute migration statement",
                    e,
                ));
            }
        }

        tx.commit()
            .await
            .map_err(|e| MigrationError::new_from("failed to commit migration", e))
    }
    async fn add_migration_to_registry(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        attrs: &FileAttrs,
    ) -> Result<(), MigrationError> {
        let q = sqlx::query("INSERT INTO migrations (name, created_at) VALUES ($1,$2)")
            .bind(attrs.name.clone())
            .bind(attrs.created_at);

        match tx.execute(q).await {
            Ok(_) => Ok(()),
            Err(e) => Err(MigrationError::new_from("failed to register migration", e)),
        }
    }

    fn split_query(q: &str) -> Vec<String> {
        let dialect = PostgreSqlDialect {};
        match Parser::parse_sql(&dialect, q) {
            Ok(statements) => statements
                .into_iter()
                .map(|stmt| stmt.to_string())
                .collect(),
            Err(e) => {
                log::error!("Failed to parse SQL: {}", e);
                // Fallback to simple split for compatibility with PostgreSQL extensions
                // that may not be fully supported by sqlparser
                q.split(';').map(|v| v.to_string()).collect()
            }
        }
    }

    fn read_file<'a, 'b>(&self, p: &'a PathBuf) -> Result<String, MigrationError> {
        let mut f = match File::open(p) {
            Ok(v) => v,
            Err(e) => return Err(MigrationError::new_from("failed to open file", e)),
        };
        let mut out = String::new();
        let _ = match f.read_to_string(&mut out) {
            Ok(v) => v,
            Err(e) => return Err(MigrationError::new_from("failed to read file", e)),
        };
        Ok(out)
    }

    async fn get_latest_applied_migration(
        &self,
        conn: &Pool<Postgres>,
    ) -> Result<DateTime<Local>, MigrationError> {
        let v: Result<Option<DateTime<Local>>, sqlx::Error> = match conn
            .fetch_one("SELECT MAX(created_at) FROM migrations")
            .await
        {
            Ok(v) => v.try_get(0),
            Err(e) => {
                return Err(MigrationError::new_from(
                    "failed to fetch latest migration",
                    e,
                ));
            }
        };

        let maybe_dt = match v {
            Ok(maybe_dt) => maybe_dt,
            Err(e) => {
                return Err(MigrationError::new_from(
                    "failed to get created_at from result",
                    e,
                ));
            }
        };
        match maybe_dt {
            Some(dt) => Ok(dt.into()),
            None => Ok(Local.with_ymd_and_hms(0, 1, 1, 0, 0, 0).unwrap()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Local, TimeZone};

    // ========================================================================
    // Filename parsing tests
    // ========================================================================

    #[test]
    fn test_parse_file_name_valid() {
        let mut pb = PathBuf::new();
        pb.push("202603081008_create_users.sql");

        let result = UpArgs::parse_file_name(&pb);
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

        let result = UpArgs::parse_file_name(&pb);
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

        let result = UpArgs::parse_file_name(&pb);
        assert!(result.is_err(), "Expected error for file without extension");
    }

    #[test]
    fn test_parse_file_name_invalid_extension() {
        let pb = PathBuf::from("202603081008_wrong_extension.txt");

        let result = UpArgs::parse_file_name(&pb);
        assert!(result.is_err(), "Expected error for non-SQL extension");
    }

    #[test]
    fn test_parse_file_name_invalid_timestamp() {
        let pb = PathBuf::from("invalid_timestamp_create_users.sql");

        let result = UpArgs::parse_file_name(&pb);
        assert!(
            result.is_err(),
            "Expected error for invalid timestamp format"
        );
    }

    #[test]
    fn test_parse_file_name_timestamp_too_short() {
        let pb = PathBuf::from("20260308_create_users.sql"); // 8 digits instead of 12

        let result = UpArgs::parse_file_name(&pb);
        assert!(result.is_err(), "Expected error for incomplete timestamp");
    }

    #[test]
    fn test_parse_file_name_timestamp_too_long() {
        let pb = PathBuf::from("2026030810081234_extra.sql");

        let result = UpArgs::parse_file_name(&pb);
        // The parser expects exactly 14 digits for timestamp
        // Any extra digits cause parse failure since "0810081234_extra" isn't valid
        assert!(result.is_err(), "Should fail due to malformed timestamp");
    }

    // ========================================================================
    // Query splitting tests
    // ========================================================================

    #[test]
    fn test_split_query_simple() {
        let query = "SELECT 1; SELECT 2;";
        let result = UpArgs::split_query(query);

        // sqlparser correctly parses two statements
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], "SELECT 1");
        assert_eq!(result[1], "SELECT 2");
    }

    #[test]
    fn test_split_query_with_empty_statements() {
        let query = "SELECT 1;; SELECT 2;";
        let result = UpArgs::split_query(query);

        // sqlparser correctly ignores empty statements (consecutive semicolons)
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], "SELECT 1");
        assert_eq!(result[1], "SELECT 2");
    }

    #[test]
    fn test_split_query_empty_input() {
        let query = "";
        let result = UpArgs::split_query(query);

        // sqlparser returns empty vector for empty input
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn test_split_query_single_statement_no_semicolon() {
        let query = "CREATE TABLE test (id INT)";
        let result = UpArgs::split_query(query);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0], "CREATE TABLE test (id INT)");
    }

    #[test]
    fn test_split_query_with_comments() {
        let query = "-- comment\nSELECT 1; -- another comment";
        let result = UpArgs::split_query(query);

        // sqlparser correctly parses comments and ignores semicolons within them
        assert_eq!(result.len(), 1);
        assert!(result[0].contains("SELECT 1"));
    }

    #[test]
    fn test_split_query_with_semicolon_in_string() {
        let query = "INSERT INTO test VALUES ('a; b');";
        let result = UpArgs::split_query(query);

        // With sqlparser, semicolons inside strings are handled correctly
        assert_eq!(result.len(), 1, "Should handle semicolons in string literals");
        assert!(result[0].contains("INSERT INTO test VALUES"));
        assert!(result[0].contains("a; b"));
    }

    #[test]
    fn test_split_query_with_semicolon_in_comment() {
        let query = "-- this is; a comment\nSELECT 1;";
        let result = UpArgs::split_query(query);

        // With sqlparser, semicolons inside comments are handled correctly
        assert_eq!(result.len(), 1, "Should handle semicolons in line comments");
        assert!(result[0].contains("SELECT 1"));
    }

    #[test]
    fn test_split_query_with_semicolon_in_block_comment() {
        let query = "/* this; is; a; comment */ SELECT 1;";
        let result = UpArgs::split_query(query);

        // With sqlparser, semicolons inside block comments are handled correctly
        assert_eq!(result.len(), 1, "Should handle semicolons in block comments");
        assert!(result[0].contains("SELECT 1"));
    }

    #[test]
    fn test_split_query_dollar_quoted_strings() {
        let query = r#"CREATE FUNCTION test() AS $$ SELECT 'a; b'; $$ LANGUAGE sql;"#;
        let result = UpArgs::split_query(query);

        // With sqlparser, dollar-quoted strings are handled correctly
        assert_eq!(result.len(), 1, "Should handle dollar-quoted strings");
        assert!(result[0].contains("CREATE FUNCTION"));
    }

    #[test]
    fn test_split_query_filters_empty_statements() {
        // Verify that the migration runner correctly handles empty statements
        // With sqlparser, empty statements are already filtered out
        let query = "SELECT 1;; SELECT 2;";
        let statements = UpArgs::split_query(query);

        let non_empty: Vec<&String> = statements.iter().filter(|s| !s.trim().is_empty()).collect();

        assert_eq!(non_empty.len(), 2);
        assert_eq!(non_empty[0], "SELECT 1");
        assert_eq!(non_empty[1], "SELECT 2"); // sqlparser trims whitespace
    }

    // ========================================================================
    // File reading tests
    // Note: These tests use test-specific file paths that don't require temp dirs
    // ========================================================================

    fn create_test_up_args() -> UpArgs {
        UpArgs {
            number: 0,
            host: "localhost".to_string(),
            port: 5432,
            user: "postgres".to_string(),
            db: "postgres".to_string(),
            migrations_path: "".to_string(), // Empty means auto-detect
        }
    }

    #[test]
    fn test_read_file_not_found() {
        let up_args = create_test_up_args();
        let file_path = PathBuf::from("/nonexistent/path/file.sql");

        let result = up_args.read_file(&file_path);

        assert!(result.is_err());
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
    // Migration atomicity tests (unit-level, not integration)
    // ========================================================================

    #[test]
    fn test_split_query_preserves_statement_boundaries() {
        // Test that statements are properly delimited
        let multi_statement = r#"
CREATE TABLE users (id INT);
CREATE INDEX idx_users_id ON users(id);
INSERT INTO users VALUES (1);
        "#;

        let statements = UpArgs::split_query(multi_statement);

        // Filter out empty statements
        let non_empty: Vec<&String> = statements.iter().filter(|s| !s.trim().is_empty()).collect();

        assert_eq!(non_empty.len(), 3);
        assert!(non_empty[0].contains("CREATE TABLE"));
        assert!(non_empty[1].contains("CREATE INDEX"));
        assert!(non_empty[2].contains("INSERT"));
    }

    #[test]
    fn test_split_query_fallback_on_parse_error() {
        // Test that fallback to simple split works when sqlparser fails
        // This can happen with PostgreSQL extensions or non-standard syntax
        let query = "SELECT * FROM h3_to_string(123);"; // H3 function - may not parse
        
        let result = UpArgs::split_query(query);
        
        // Should return at least something (either parsed or fallback)
        assert!(!result.is_empty(), "Should always return some statements");
        assert!(result.iter().any(|s| s.contains("SELECT")));
    }

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
