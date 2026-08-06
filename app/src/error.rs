//! Application error types.

use thiserror::Error;

/// Application-level errors.
#[derive(Error, Debug)]
pub enum AppError {
    /// CLI argument parsing error.
    #[error("CLI error: {0}")]
    Cli(#[from] clap::Error),

    /// Database error.
    #[error("Database error: {0}")]
    Database(#[from] repo::RepoError),

    /// Bluetooth monitoring error.
    #[error("Bluetooth error: {0}")]
    Bluetooth(#[from] bt_mon::Error),

    /// Invalid UUID.
    #[error("Invalid UUID: {0}")]
    InvalidUuid(#[from] uuid::Error),

    /// Invalid MAC address.
    #[error("Invalid MAC address: {0}")]
    InvalidMacAddress(String),

    /// Configuration error.
    #[error("Configuration error: {0}")]
    Config(String),

    /// Node ID not configured.
    #[error("NODE_ID environment variable or --node-id required")]
    NodeIdMissing,

    /// Validation error.
    #[error("Validation error: {0}")]
    Validation(String),
}

impl From<String> for AppError {
    fn from(s: String) -> Self {
        AppError::Validation(s)
    }
}

impl<'a> From<&'a str> for AppError {
    fn from(s: &'a str) -> Self {
        AppError::Validation(s.to_string())
    }
}

/// Result type using AppError.
pub type Result<T> = std::result::Result<T, AppError>;
