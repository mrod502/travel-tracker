use chrono::{DateTime, Utc};
use thiserror::Error;
use uuid::Uuid;

/// Repository error type covering all database operations
#[derive(Debug, Error)]
pub enum RepoError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    /// A row's month had no `occurrences` partition and creating one failed.
    ///
    /// Both errors are kept because the pair is the diagnosis. `heal` is why
    /// the schema would not open — usually SQLSTATE 22023, a timestamp outside
    /// the window the maintenance function accepts, or SQLSTATE 42883 on a
    /// database that has not had migration `202609141353` applied. The insert
    /// error on its own is what let "the partitions ran out" read as an ordinary
    /// write failure for as long as it did.
    #[error("No occurrences partition for observed_at {observed_at}; creating one failed: {heal} (the insert said: {insert})")]
    PartitionHealFailed {
        observed_at: DateTime<Utc>,
        #[source]
        heal: Box<RepoError>,
        insert: Box<RepoError>,
    },

    #[error("Record not found: {0}")]
    NotFound(String),

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Duplicate record: {0}")]
    Duplicate(Uuid),

    #[error("UUID error: {0}")]
    Uuid(#[from] uuid::Error),

    #[error("Chrono parsing error: {0}")]
    Chrono(#[from] chrono::ParseError),
}

impl RepoError {
    /// Create a new validation error
    pub fn validation(msg: impl Into<String>) -> Self {
        Self::Validation(msg.into())
    }

    /// Create a new not found error
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::NotFound(msg.into())
    }
}
