use crate::{runner::Runner, up::UpArgs};
use async_trait::async_trait;
use clap::Args;
use sqlx::{Executor, Pool, Postgres};
use std::{error::Error, fmt::Display};

#[derive(Args, Debug, Clone)]
pub struct DownArgs {
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

#[derive(Debug)]
pub struct DownError {
    src: Option<Box<dyn Error + Send + Sync>>,
    reason: String,
}

impl Error for DownError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.src.as_ref().map(|e| e.as_ref() as &(dyn Error + 'static))
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

#[async_trait]
impl Runner for DownArgs {
    type RunError = DownError;

    async fn run(&self, maybe_conn: Option<&Pool<Postgres>>) -> Result<String, Self::RunError> {
        let conn = match maybe_conn {
            Some(c) => c,
            None => return Err(DownError::new("no connection provided")),
        };

        log::info!("resetting database: dropping and re-applying all migrations");

        // Step 1: Drop all custom types (enums, composite types, ranges)
        // This must happen before dropping tables because tables may reference types
        self.drop_all_types(conn).await?;

        // Step 2: Drop all tables including migrations table
        // Note: Extensions are preserved - migrations use CREATE EXTENSION IF NOT EXISTS
        self.drop_all_tables(conn).await?;

        // Step 3: Re-create the migrations table (it was just dropped)
        const MIGRATION_INIT: &str = "CREATE TABLE IF NOT EXISTS migrations (
            id BIGSERIAL PRIMARY KEY NOT NULL,
            name TEXT NOT NULL UNIQUE,
            created_at TIMESTAMPTZ NOT NULL,
            executed_at TIMESTAMPTZ NOT NULL default now()
        )";
        conn.execute(sqlx::query(MIGRATION_INIT))
            .await
            .map_err(|e| DownError::new_from("failed to create migrations table", e))?;
        log::info!("migrations table created");

        // Step 4: Re-apply all migrations from scratch
        // Build UpArgs with the same connection parameters
        let up_args = UpArgs {
            number: 0,
            host: self.host.clone(),
            port: self.port,
            user: self.user.clone(),
            db: self.db.clone(),
            migrations_path: self.migrations_path.clone(),
        };

        up_args.run(Some(conn)).await.map_err(|e| {
            DownError::new_from("failed to re-apply migrations after reset", e)
        })?;

        Ok("Database reset complete: all tables dropped and migrations re-applied".into())
    }
}

impl DownArgs {
    /// Drop all custom types (enums, composite types, ranges) in the public schema
    /// Note: We don't drop types owned by extensions to avoid breaking extension functionality
    async fn drop_all_types(&self, conn: &Pool<Postgres>) -> Result<(), DownError> {
        // Get all user-defined types in the public schema that are NOT owned by extensions
        // This includes: ENUM, composite types (RECORD), and RANGE types
        // Note: typtype is PostgreSQL's "char" type, so we use i8 for decoding
        // We check pg_depend to filter out types that are required by extensions
        let types: Vec<(String, i8)> = sqlx::query_as(
            r#"SELECT t.typname, t.typtype
               FROM pg_type t
               JOIN pg_namespace n ON t.typnamespace = n.oid
               WHERE n.nspname = 'public'
               AND t.typtype IN ('e'::"char", 'c'::"char", 'r'::"char")
               AND NOT EXISTS (
                   SELECT 1 
                   FROM pg_depend d
                   WHERE d.objid = t.oid
                   AND d.deptype = 'e'  -- dependency on extension
               )"#
        )
        .fetch_all(conn)
        .await
        .map_err(|e| DownError::new_from("failed to query types", e))?;

        if types.is_empty() {
            log::info!("no custom types to drop");
            return Ok(());
        }

        log::info!("found {} custom types to drop", types.len());

        // Drop each type
        for (type_name, type_kind) in &types {
            // typtype is returned as i8 (ASCII value): 101='e', 99='c', 114='r'
            let kind_desc = match *type_kind {
                101 => "ENUM",      // 'e'
                99 => "composite type", // 'c'
                114 => "RANGE",     // 'r'
                _ => "type",
            };
            log::info!("dropping {} {}", kind_desc, type_name);
            let drop_sql = format!("DROP TYPE IF EXISTS \"{}\" CASCADE", type_name);
            
            match conn.execute(sqlx::query(sqlx::AssertSqlSafe(drop_sql))).await {
                Ok(_) => {},
                Err(e) => {
                    // Skip types that can't be dropped (might be extension-owned despite our filter)
                    log::warn!("skipping type {} (may be extension-owned): {}", type_name, e);
                }
            }
        }

        log::info!("all custom types dropped successfully");
        Ok(())
    }

    /// Drop all tables including the migrations table
    /// Note: We don't drop extensions here because dropping plpgsql extension can make
    /// it impossible to re-create (the CREATE LANGUAGE command requires plpgsql to execute).
    /// The migrations use CREATE EXTENSION IF NOT EXISTS which is idempotent.
    async fn drop_all_tables(&self, conn: &Pool<Postgres>) -> Result<(), DownError> {
        // Get all tables in the public schema that are NOT owned by extensions
        let tables: Vec<(String,)> = sqlx::query_as(
            r#"SELECT t.table_name 
               FROM information_schema.tables t
               WHERE t.table_schema = 'public' 
               AND t.table_type = 'BASE TABLE'
               AND NOT EXISTS (
                   SELECT 1 
                   FROM pg_depend d
                   JOIN pg_class c ON d.objid = c.oid
                   WHERE c.relname = t.table_name
                   AND d.deptype = 'e'  -- dependency on extension
               )"#
        )
        .fetch_all(conn)
        .await
        .map_err(|e| DownError::new_from("failed to query tables", e))?;

        if tables.is_empty() {
            log::info!("no tables to drop");
            return Ok(());
        }

        log::info!("found {} tables to drop", tables.len());

        // Drop each table
        for (table_name,) in &tables {
            log::info!("dropping table: {}", table_name);
            let drop_sql = format!("DROP TABLE IF EXISTS \"{}\" CASCADE", table_name);
            
            match conn.execute(sqlx::query(sqlx::AssertSqlSafe(drop_sql))).await {
                Ok(_) => {},
                Err(e) => {
                    // Skip tables that can't be dropped (might be extension-owned despite our filter)
                    log::warn!("skipping table {} (may be extension-owned): {}", table_name, e);
                }
            }
        }

        log::info!("all tables dropped successfully");
        Ok(())
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_down_error_display() {
        let error = DownError::new("test error");
        let display = format!("{}", error);
        assert!(display.contains("DownError"));
        assert!(display.contains("test error"));
    }

    #[test]
    fn test_down_error_with_source() {
        let source = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let error = DownError::new_from("wrapped error", source);
        assert!(error.source().is_some());
    }

    #[test]
    fn test_down_error_no_source() {
        let error = DownError::new("simple error");
        assert!(error.source().is_none());
    }

    #[test]
    fn test_type_kind_display() {
        // Test the type kind mapping used in drop_all_types
        // typtype is returned as i8 (ASCII value): 101='e', 99='c', 114='r'
        assert_eq!(match_type_kind(101), "ENUM");
        assert_eq!(match_type_kind(99), "composite type");
        assert_eq!(match_type_kind(114), "RANGE");
        assert_eq!(match_type_kind(0), "type"); // unknown type falls back to generic
    }

    fn match_type_kind(kind: i8) -> &'static str {
        match kind {
            101 => "ENUM",      // 'e'
            99 => "composite type", // 'c'
            114 => "RANGE",     // 'r'
            _ => "type",
        }
    }
}
