use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::PgPool;
use std::str::FromStr;
use std::time::Duration;

use crate::error::RepoError;

/// Connection pool wrapper for PostgreSQL
///
/// Provides a convenient interface for creating and managing database connections.
/// The pool is cloneable and thread-safe, designed to be shared across tasks.
#[derive(Clone)]
pub struct Pool {
    inner: PgPool,
}

impl Pool {
    /// Create a new connection pool with default settings
    ///
    /// # Arguments
    /// * `dsn` - Database connection string (e.g., "postgres://user:pass@localhost:5432/dbname")
    ///
    /// # Example
    /// ```no_run
    /// use repo::Pool;
    ///
    /// # tokio_test::block_on(async {
    /// let pool = Pool::connect("postgres://user:pass@localhost:5432/dbname").await?;
    /// # Ok::<(), repo::RepoError>(())
    /// # })
    /// ```
    pub async fn connect(dsn: &str) -> Result<Self, RepoError> {
        let options = PgConnectOptions::from_str(dsn)?;
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;
        Ok(Self { inner: pool })
    }

    /// Create a new connection pool with custom settings
    ///
    /// # Arguments
    /// * `dsn` - Database connection string
    /// * `max_connections` - Maximum number of connections in the pool
    /// * `acquire_timeout` - Timeout for acquiring a connection from the pool
    ///
    /// # Example
    /// ```no_run
    /// use repo::Pool;
    /// use std::time::Duration;
    ///
    /// # tokio_test::block_on(async {
    /// let pool = Pool::connect_with_options(
    ///     "postgres://user:pass@localhost:5432/dbname",
    ///     10,
    ///     Duration::from_secs(30),
    /// ).await?;
    /// # Ok::<(), repo::RepoError>(())
    /// # })
    /// ```
    pub async fn connect_with_options(
        dsn: &str,
        max_connections: u32,
        acquire_timeout: Duration,
    ) -> Result<Self, RepoError> {
        let options = PgConnectOptions::from_str(dsn)?;
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .acquire_timeout(acquire_timeout)
            .connect_with(options)
            .await?;
        Ok(Self { inner: pool })
    }

    /// Get a reference to the underlying PgPool
    ///
    /// This allows direct use of sqlx APIs when needed, while still benefiting
    /// from the centralized pool management.
    pub fn as_pool(&self) -> &PgPool {
        &self.inner
    }

    /// Gracefully close the pool
    ///
    /// This waits for all connections to be returned to the pool and closes them
    /// gracefully. Call this during application shutdown.
    pub async fn close(self) {
        self.inner.close().await;
    }

    /// Check if the pool is closed
    pub fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }
}
