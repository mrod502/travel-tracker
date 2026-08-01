use thiserror::Error;

/// Repository error type covering all database operations
#[derive(Debug, Error)]
pub enum RepoError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("Record not found: {0}")]
    NotFound(String),

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("UUID error: {0}")]
    Uuid(#[from] uuid::Error),

    #[error("Decimal parsing error: {0}")]
    Decimal(#[from] rust_decimal::Error),

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
