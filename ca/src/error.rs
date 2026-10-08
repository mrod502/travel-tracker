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

    /// An RSL was offered that does not advance the sequence already held for
    /// its CA. This is the replay rejection: an RSL captured yesterday and
    /// served again today (or a second list minted with an already-used
    /// number) carries no new information, and accepting it would let the
    /// receiver believe it has current revocation data.
    #[error("RSL sequence {incoming} does not advance {current} already held for CA {issuer_id}")]
    RslNotNewer {
        /// CA the list claims to come from.
        issuer_id: String,
        /// Sequence number the offered list carries.
        incoming: u64,
        /// Sequence number of the newest list already accepted for that CA.
        current: u64,
    },

    /// A list was handed to something that only accepts signed RSLs.
    #[error("RSL from {0} is not signed")]
    UnsignedRsl(String),

    /// The CA in question has published no revocation list at all.
    ///
    /// Separate from "the list says this node is fine" because it is the absence
    /// of evidence, and a consumer that cannot tell the two apart turns an outage
    /// at the CA into a clean bill of health for every node in the federation.
    #[error("CA {0} has published no revocation status list")]
    RslNotFound(String),

    /// The newest list a CA published is past its `expires_at`.
    ///
    /// An expired list is no longer evidence about anything: it is a statement
    /// about a window that has closed, so the absence of a node from it cannot be
    /// read as "not revoked". Loaders refuse one rather than answering
    /// `Valid` from stale data.
    #[error("RSL expired at {0}, so it can no longer say anything about a node's status")]
    RslExpired(chrono::DateTime<chrono::Utc>),

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
