//! Error types for CA operations.

use thiserror::Error;

/// Error types for CA operations.
#[derive(Debug, Error, Clone, PartialEq)]
pub enum CaError {
    /// Failed to generate or load the CA root key.
    #[error("Root key error: {0}")]
    RootKey(String),

    /// Failed to issue a credential.
    #[error("Credential issuance failed: {0}")]
    Issuance(String),

    /// Credential verification failed.
    #[error("Credential verification failed: {0}")]
    Verification(String),

    /// Invalid credential format or structure.
    #[error("Invalid credential: {0}")]
    InvalidCredential(String),

    /// Credential has expired.
    #[error("Credential expired at {0}")]
    ExpiredCredential(chrono::DateTime<chrono::Utc>),

    /// Credential has not yet become valid.
    #[error("Credential not yet valid, valid from {0}")]
    NotYetValid(chrono::DateTime<chrono::Utc>),

    /// Node has been revoked.
    #[error("Node {0} has been revoked")]
    NodeRevoked(String),

    /// Invalid key format or length.
    #[error("Invalid key: {0}")]
    InvalidKey(String),

    /// Serialization/deserialization error.
    #[error("Serialization error: {0}")]
    Serialization(String),

    /// File I/O error.
    #[error("I/O error: {0}")]
    Io(String),

    /// CA root key not initialized.
    #[error("CA root key not initialized. Run 'ca-init' first.")]
    RootKeyNotInitialized,

    /// Database error.
    #[error("Database error: {0}")]
    Database(String),
}

/// Result type for CA operations.
pub type Result<T> = std::result::Result<T, CaError>;

impl From<ed25519_dalek::SignatureError> for CaError {
    fn from(err: ed25519_dalek::SignatureError) -> Self {
        CaError::Verification(format!("Signature error: {}", err))
    }
}

impl From<std::io::Error> for CaError {
    fn from(err: std::io::Error) -> Self {
        CaError::Io(err.to_string())
    }
}

impl From<hex::FromHexError> for CaError {
    fn from(err: hex::FromHexError) -> Self {
        CaError::InvalidKey(format!("Hex decode error: {}", err))
    }
}
