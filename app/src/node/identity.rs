//! Node identity management.
//!
//! This module provides the `NodeIdentity` struct which encapsulates the
//! Ed25519 keypair used for signing occurrences, along with the derived
//! node ID (SHA-256 hash of the public key).
//!
//! # Key Generation
//!
//! Node identities can be generated randomly on first run or loaded from
//! persistent storage. The same identity should be used across node restarts
//! to maintain a consistent node ID.
//!
//! # Persistence
//!
//! The signing key is a PKCS#8 **v1** PEM document — `-----BEGIN PRIVATE KEY-----`
//! — at `$DATA_DIR/node_identity.pem`, mode `0600`, read and written by
//! [`ca::pemkeys`]. That is the same module the CA root key goes through, so both
//! halves of this network keep their private keys in files `openssl pkey` can open,
//! and the version choice (and the reason for it) lives in one place.
//!
//! The public half is published beside it as SPKI PEM at
//! `$DATA_DIR/node_identity.pub.pem`. That file is what an operator hands to the CA
//! to get a credential, and being public it is not a secret — but it is kept in sync
//! with the private key on every start so it can never be an old key's.
//!
//! Hex is not used for key material anywhere here, and a file that is not PKCS#8
//! PEM is an error rather than a guess: a secret read through the wrong decoder
//! yields a confident, entirely different identity.
//!
//! # Example
//!
//! ```ignore
//! use app::node::identity::NodeIdentity;
//! use std::path::PathBuf;
//!
//! // Generate new identity
//! let identity = NodeIdentity::generate();
//!
//! // Or load from file
//! let data_dir = PathBuf::from("/var/lib/btmon");
//! let identity = NodeIdentity::load_or_create(&data_dir)?;
//!
//! // Use for signing
//! let signature = identity.sign(payload_bytes);
//!
//! // Get node ID
//! let node_id = identity.node_id();
//! ```

use ed25519_dalek::{SigningKey, VerifyingKey};
use rand::thread_rng;
use std::fs;
use std::path::Path;

use crate::error::{AppError, Result};
use crate::provenance::sign::{compute_node_id, sign_payload as sign_raw_payload};
use crate::provenance::verify::verify_signature as verify_raw_signature;

/// Node identity encapsulating the Ed25519 keypair and derived node ID.
///
/// A node identity is used to:
/// 1. Sign occurrences with the private key
/// 2. Verify signatures with the public key
/// 3. Identify the node via the derived node ID (SHA-256 of public key)
///
/// # Security Considerations
///
/// - The private key should be protected at rest (file permissions)
/// - The private key should never be logged or exposed
/// - Backups of the identity file should be encrypted
#[derive(Debug)]
pub struct NodeIdentity {
    /// The Ed25519 signing key (private key)
    signing_key: SigningKey,

    /// The Ed25519 verifying key (public key)
    verifying_key: VerifyingKey,

    /// The derived node ID (SHA-256 hash of public key, 32 bytes)
    node_id: Vec<u8>,
}

impl NodeIdentity {
    /// The signing key: PKCS#8 PEM, `0600`.
    pub const IDENTITY_FILENAME: &'static str = "node_identity.pem";

    /// The published public key: SPKI PEM.
    pub const PUBLIC_KEY_FILENAME: &'static str = "node_identity.pub.pem";

    /// Generate a new random node identity.
    ///
    /// This creates a new Ed25519 keypair and derives the node ID from it.
    ///
    /// # Returns
    ///
    /// A new `NodeIdentity` with randomly generated keys.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use app::node::identity::NodeIdentity;
    ///
    /// let identity = NodeIdentity::generate();
    /// println!("Node ID: {}", hex::encode(identity.node_id()));
    /// ```
    pub fn generate() -> Self {
        Self::from_signing_key(SigningKey::generate(&mut thread_rng()))
    }

    fn from_signing_key(signing_key: SigningKey) -> Self {
        let verifying_key = signing_key.verifying_key();
        let node_id = compute_node_id(&verifying_key);

        Self {
            signing_key,
            verifying_key,
            node_id,
        }
    }

    /// Load a node identity from a PKCS#8 PEM file.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the identity file
    ///
    /// # Returns
    ///
    /// * `Ok(NodeIdentity)` - If the file exists and holds one Ed25519 PKCS#8 key
    /// * `Err(AppError)` - Otherwise; the message names the file
    ///
    /// # Example
    ///
    /// ```ignore
    /// use app::node::identity::NodeIdentity;
    /// use std::path::PathBuf;
    ///
    /// let path = PathBuf::from("/var/lib/btmon/node_identity.pem");
    /// let identity = NodeIdentity::load(&path)?;
    /// ```
    pub fn load(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .map_err(|e| AppError::Io(format!("Failed to read identity file: {}", e)))?;

        let signing_key = ca::pemkeys::read_signing_key_pem(&content, &path.display().to_string())
            .map_err(|e| AppError::Io(e.to_string()))?;

        Ok(Self::from_signing_key(signing_key))
    }

    /// Save the signing key as PKCS#8 v1 PEM, mode `0600`.
    ///
    /// The public half is not stored here — it is derived on load and published by
    /// [`Self::save_public_key`] — so there is no second copy of the key material to
    /// drift out of step with the secret.
    ///
    /// # Arguments
    ///
    /// * `path` - Path where to save the identity file
    ///
    /// # Returns
    ///
    /// * `Ok(())` - If the file was saved successfully
    /// * `Err(AppError)` - If saving failed
    ///
    /// # Example
    ///
    /// ```ignore
    /// use app::node::identity::NodeIdentity;
    /// use std::path::PathBuf;
    ///
    /// let identity = NodeIdentity::generate();
    /// let path = PathBuf::from("/var/lib/btmon/node_identity.pem");
    /// identity.save(&path)?;
    /// ```
    pub fn save(&self, path: &Path) -> Result<()> {
        ca::pemkeys::write_key_file(
            path,
            &ca::pemkeys::write_signing_key_pem(&self.signing_key),
            Some(0o600),
        )
        .map_err(|e| AppError::Io(e.to_string()))
    }

    /// The public half as an SPKI PEM document.
    pub fn public_key_pem(&self) -> String {
        ca::pemkeys::write_public_key_pem(&self.verifying_key)
    }

    /// Publish the public half as SPKI PEM, mode `0644`.
    ///
    /// World-readable on purpose, for the same reason as a trust anchor: this is
    /// public material whose entire use is being handed to someone else — the CA at
    /// enrollment, an operator debugging a signature.
    pub fn save_public_key(&self, path: &Path) -> Result<()> {
        ca::pemkeys::write_key_file(path, &self.public_key_pem(), Some(0o644))
            .map_err(|e| AppError::Io(e.to_string()))
    }

    /// Load a node identity from a directory, generating a new one if it doesn't exist.
    ///
    /// This is the recommended way to get a node identity for production use.
    ///
    /// The public half is written beside the private key on every start, so the file
    /// an operator hands to the CA can never belong to an older key.
    ///
    /// # Arguments
    ///
    /// * `data_dir` - Directory where the identity file should be stored
    ///
    /// # Returns
    ///
    /// * `Ok(NodeIdentity)` - The loaded or newly generated identity
    /// * `Err(AppError)` - If loading/generating failed
    ///
    /// # Example
    ///
    /// ```ignore
    /// use app::node::identity::NodeIdentity;
    /// use std::path::PathBuf;
    ///
    /// let data_dir = PathBuf::from("/var/lib/btmon");
    /// let identity = NodeIdentity::load_or_create(&data_dir)?;
    /// ```
    pub fn load_or_create(data_dir: &Path) -> Result<Self> {
        let identity_path = data_dir.join(Self::IDENTITY_FILENAME);
        let public_path = data_dir.join(Self::PUBLIC_KEY_FILENAME);

        if identity_path.exists() {
            let identity = Self::load(&identity_path)?;
            // Rewritten every start: a published key that outlives the secret it
            // belongs to is worse than no published key.
            identity.save_public_key(&public_path)?;
            return Ok(identity);
        }

        let identity = Self::generate();
        identity.save(&identity_path)?;
        identity.save_public_key(&public_path)?;
        Ok(identity)
    }

    /// Get the node ID (SHA-256 hash of the public key).
    ///
    /// # Returns
    ///
    /// A 32-byte vector containing the SHA-256 hash.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use app::node::identity::NodeIdentity;
    ///
    /// let identity = NodeIdentity::generate();
    /// println!("Node ID: {}", hex::encode(identity.node_id()));
    /// ```
    pub fn node_id(&self) -> &[u8] {
        &self.node_id
    }

    /// Get the signing public key.
    ///
    /// # Returns
    ///
    /// A reference to the Ed25519 verifying key (public key).
    pub fn verifying_key(&self) -> &VerifyingKey {
        &self.verifying_key
    }

    /// Sign a payload.
    ///
    /// # Arguments
    ///
    /// * `payload` - The bytes to sign
    ///
    /// # Returns
    ///
    /// A 64-byte Ed25519 signature.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use app::node::identity::NodeIdentity;
    ///
    /// let identity = NodeIdentity::generate();
    /// let payload = b"canonical payload bytes";
    /// let signature = identity.sign(payload);
    /// ```
    pub fn sign(&self, payload: &[u8]) -> ed25519_dalek::Signature {
        sign_raw_payload(&self.signing_key, payload).expect("Signing should never fail")
    }

    /// Verify a signature.
    ///
    /// # Arguments
    ///
    /// * `payload` - The bytes that were signed
    /// * `signature` - The signature to verify
    ///
    /// * `Ok(())` - If the signature is valid
    /// * `Err(VerifyError)` - If verification fails
    pub fn verify(
        &self,
        payload: &[u8],
        signature: &ed25519_dalek::Signature,
    ) -> crate::provenance::verify::Result<()> {
        verify_raw_signature(&self.verifying_key, payload, signature)
    }
}

impl Default for NodeIdentity {
    fn default() -> Self {
        Self::generate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_generate_creates_valid_identity() {
        let identity = NodeIdentity::generate();

        assert_eq!(identity.node_id().len(), 32);
        assert_eq!(identity.verifying_key().as_bytes().len(), 32);
    }

    #[test]
    fn test_generate_produces_different_ids() {
        let id1 = NodeIdentity::generate();
        let id2 = NodeIdentity::generate();

        assert_ne!(id1.node_id(), id2.node_id());
    }

    #[test]
    fn test_sign_and_verify() {
        let identity = NodeIdentity::generate();
        let payload = b"test payload";

        let signature = identity.sign(payload);
        let result = identity.verify(payload, &signature);

        assert!(result.is_ok());
    }

    #[test]
    fn test_verify_fails_on_tampered_payload() {
        let identity = NodeIdentity::generate();
        let payload = b"original payload";
        let tampered_payload = b"tampered payload";

        let signature = identity.sign(payload);
        let result = identity.verify(tampered_payload, &signature);

        assert!(result.is_err());
    }

    #[test]
    fn test_save_and_load() {
        let temp_dir = TempDir::new().unwrap();
        let identity_path = temp_dir.path().join(NodeIdentity::IDENTITY_FILENAME);

        // Generate and save
        let identity1 = NodeIdentity::generate();
        identity1.save(&identity_path).unwrap();

        // Load
        let identity2 = NodeIdentity::load(&identity_path).unwrap();

        // Verify they're the same
        assert_eq!(identity1.node_id(), identity2.node_id());
        assert_eq!(
            identity1.verifying_key().as_bytes(),
            identity2.verifying_key().as_bytes()
        );
    }

    /// What the file actually is, not just what round-trips: a private key on disk
    /// has to be the format every other tool reads.
    #[test]
    fn a_saved_identity_is_a_pkcs8_pem_document() {
        let temp_dir = TempDir::new().unwrap();
        let identity_path = temp_dir.path().join(NodeIdentity::IDENTITY_FILENAME);

        NodeIdentity::generate().save(&identity_path).unwrap();

        let written = fs::read_to_string(&identity_path).unwrap();
        assert!(
            written.starts_with("-----BEGIN PRIVATE KEY-----"),
            "the node's signing key must be a standard PEM private key, not a bespoke document: \
             {written}"
        );
    }

    #[test]
    fn the_public_half_is_published_as_spki_pem() {
        let temp_dir = TempDir::new().unwrap();
        let identity = NodeIdentity::generate();
        let public_path = temp_dir.path().join(NodeIdentity::PUBLIC_KEY_FILENAME);

        identity.save_public_key(&public_path).unwrap();

        let written = fs::read_to_string(&public_path).unwrap();
        assert!(
            written.starts_with("-----BEGIN PUBLIC KEY-----"),
            "{written}"
        );
        // It has to be *this* key, or enrollment would hand the CA the wrong one.
        let loaded = ca::TrustAnchor::from_pem(&identity.public_key_pem())
            .expect("the published key parses as a trust anchor");
        assert_eq!(*loaded.public_key(), *identity.verifying_key().as_bytes());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&public_path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o644, "a published key is not a secret");
        }
    }

    #[test]
    fn test_load_or_create_new() {
        let temp_dir = TempDir::new().unwrap();

        // Should create new identity
        let identity = NodeIdentity::load_or_create(temp_dir.path()).unwrap();

        assert_eq!(identity.node_id().len(), 32);
        assert!(temp_dir
            .path()
            .join(NodeIdentity::IDENTITY_FILENAME)
            .exists());
        // A node that has a private key but no published public one cannot be
        // enrolled without a second, separate step.
        assert!(temp_dir
            .path()
            .join(NodeIdentity::PUBLIC_KEY_FILENAME)
            .exists());
    }

    #[test]
    fn test_load_or_create_existing() {
        let temp_dir = TempDir::new().unwrap();

        // Create identity
        let identity1 = NodeIdentity::generate();
        identity1
            .save(&temp_dir.path().join(NodeIdentity::IDENTITY_FILENAME))
            .unwrap();

        // Load existing
        let identity2 = NodeIdentity::load_or_create(temp_dir.path()).unwrap();

        // Should be the same identity
        assert_eq!(identity1.node_id(), identity2.node_id());
    }

    #[test]
    fn test_file_permissions() {
        let temp_dir = TempDir::new().unwrap();
        let identity_path = temp_dir.path().join(NodeIdentity::IDENTITY_FILENAME);

        let identity = NodeIdentity::generate();
        identity.save(&identity_path).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = fs::metadata(&identity_path).unwrap();
            let mode = metadata.permissions().mode() & 0o777;

            // Should be 0o600 (owner read/write only)
            assert_eq!(
                mode, 0o600,
                "Identity file should have restrictive permissions"
            );
        }
    }
}
