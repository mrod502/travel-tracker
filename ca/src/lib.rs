//! Certificate Authority library for node enrollment and credential verification.
//!
//! This crate provides the core CA functionality for issuing and verifying
//! node credentials in the travel tracking network.
//!
//! # Overview
//!
//! The CA (Certificate Authority) is responsible for:
//! 1. **Root key management** - Generating and storing the CA's master signing key
//! 2. **Credential issuance** - Signing node public keys to create verifiable credentials
//! 3. **Credential verification** - Validating that a credential was issued by this CA
//! 4. **Revocation management** - Tracking which nodes have been revoked
//!
//! # Architecture
//!
//! ```text
//! CA Root Key (Ed25519)
//!         │
//!         ▼
//! +------------------+
//! │ Issue Credential │ --(signs node's public key)--> CA Credential
//! +------------------+
//!         │
//!         ▼
//! +------------------+
//! │ Verify Credential│ <--(checks signature)----- CA Credential
//! +------------------+
//! ```
//!
//! # Example
//!
//! ```ignore
//! use ca::{CaRoot, Credential};
//!
//! // Generate or load CA root key
//! let ca = CaRoot::generate();
//!
//! // Issue a credential for a node
//! let node_public_key = /* ... */;
//! let credential = ca.issue_credential(&node_public_key, node_id)?;
//!
//! // Verify a credential
//! let is_valid = ca.verify_credential(&credential)?;
//! ```
//!
//! # Security Considerations
//!
//! - The CA root private key should be stored securely (encrypted, restricted permissions)
//! - The root key should be backed up securely
//! - Consider using HSM for production deployments
//! - Rotate CA root key periodically (see key rotation policy)

pub mod credential;
pub mod error;
pub mod revocation;
pub mod root;
pub mod rsl_manager;
pub mod signing;

pub use credential::Credential;
pub use error::CaError;
pub use revocation::{
    CheckContext, CheckLocation, ConnectionPolicy, DataRecordingPolicy, Decision,
    InMemoryRslChecker, RevocationChecker, RevocationPolicy, RevocationReason, RevocationStatus,
    RevocationStatusList, RevokedNode, RslBuilder,
};
pub use root::CaRoot;
#[cfg(feature = "database")]
pub use rsl_manager::DatabaseRslManager;
pub use rsl_manager::InMemoryRslManager;
pub use rsl_manager::RslManager;
pub use signing::sign_credential;

/// Default validity period for credentials (90 days).
pub const DEFAULT_CREDENTIAL_VALIDITY_DAYS: u64 = 90;

/// File permissions for CA root key file (owner read/write only).
#[cfg(unix)]
pub const CA_ROOT_KEY_PERMISSIONS: u32 = 0o600;
