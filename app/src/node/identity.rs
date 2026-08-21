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
//! Node identities are stored as JSON files at `$DATA_DIR/node_identity.json`.
//! The file contains the private and public keys in hexadecimal format.
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
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

use crate::error::{AppError, Result};
use crate::provenance::sign::{compute_node_id, sign_payload as sign_raw_payload};
use crate::provenance::verify::verify_signature as verify_raw_signature;

/// Serialized representation of a node identity.
///
/// This struct is used for JSON serialization/deserialization of node identities.
#[derive(Serialize, Deserialize, Debug)]
struct SerializedIdentity {
    /// Private key in hexadecimal format (64 hex chars = 32 bytes)
    private_key_hex: String,

    /// Public key in hexadecimal format (64 hex chars = 32 bytes)
    public_key_hex: String,
}

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
    /// The default filename for storing node identity.
    pub const IDENTITY_FILENAME: &'static str = "node_identity.json";

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
        let signing_key = SigningKey::generate(&mut thread_rng());
        let verifying_key = signing_key.verifying_key();
        let node_id = compute_node_id(&verifying_key);

        Self {
            signing_key,
            verifying_key,
            node_id,
        }
    }

    /// Load a node identity from a file.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the identity file
    ///
    /// # Returns
    ///
    /// * `Ok(NodeIdentity)` - If the file exists and contains valid data
    /// * `Err(AppError)` - If the file doesn't exist or contains invalid data
    ///
    /// # Example
    ///
    /// ```ignore
    /// use app::node::identity::NodeIdentity;
    /// use std::path::PathBuf;
    ///
    /// let path = PathBuf::from("/var/lib/btmon/node_identity.json");
    /// let identity = NodeIdentity::load(&path)?;
    /// ```
    pub fn load(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .map_err(|e| AppError::Io(format!("Failed to read identity file: {}", e)))?;

        let serialized: SerializedIdentity = serde_json::from_str(&content)
            .map_err(|e| AppError::Io(format!("Failed to parse identity file: {}", e)))?;

        // Decode hex strings to bytes
        let private_key_bytes = hex::decode(&serialized.private_key_hex)
            .map_err(|e| AppError::Io(format!("Invalid private key hex: {}", e)))?;

        let public_key_bytes = hex::decode(&serialized.public_key_hex)
            .map_err(|e| AppError::Io(format!("Invalid public key hex: {}", e)))?;

        // Convert to Ed25519 keys
        let signing_key = SigningKey::from_bytes(
            private_key_bytes
                .as_slice()
                .try_into()
                .map_err(|_| AppError::Io("Invalid private key length".to_string()))?,
        );

        let verifying_key = VerifyingKey::from_bytes(
            public_key_bytes
                .as_slice()
                .try_into()
                .map_err(|_| AppError::Io("Invalid public key length".to_string()))?,
        )
        .map_err(|e| AppError::Io(format!("Invalid public key: {}", e)))?;

        let node_id = compute_node_id(&verifying_key);

        Ok(Self {
            signing_key,
            verifying_key,
            node_id,
        })
    }

    /// Save a node identity to a file.
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
    /// let path = PathBuf::from("/var/lib/btmon/node_identity.json");
    /// identity.save(&path)?;
    /// ```
    pub fn save(&self, path: &Path) -> Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| AppError::Io(format!("Failed to create directory: {}", e)))?;
        }

        let serialized = SerializedIdentity {
            private_key_hex: hex::encode(self.signing_key.as_bytes()),
            public_key_hex: hex::encode(self.verifying_key.as_bytes()),
        };

        let content = serde_json::to_string_pretty(&serialized)
            .map_err(|e| AppError::Io(format!("Failed to serialize identity: {}", e)))?;

        fs::write(path, &content)
            .map_err(|e| AppError::Io(format!("Failed to write identity file: {}", e)))?;

        // Set restrictive file permissions (owner read/write only)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))
                .map_err(|e| AppError::Io(format!("Failed to set file permissions: {}", e)))?;
        }

        Ok(())
    }

    /// Load a node identity from a directory, generating a new one if it doesn't exist.
    ///
    /// This is the recommended way to get a node identity for production use.
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

        if identity_path.exists() {
            Self::load(&identity_path)
        } else {
            let identity = Self::generate();
            identity.save(&identity_path)?;
            Ok(identity)
        }
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
        sign_raw_payload(&self.signing_key, payload)
            .expect("Signing should never fail")
    }

    /// Verify a signature.
    ///
    /// # Arguments
    ///
    /// * `payload` - The bytes that were signed
    /// * `signature` - The signature to verify
    ///
    /// # Returns
    ///
    /// * `Ok(())` - If the signature is valid
    /// * `Err(VerifyError)` - If verification fails
    pub fn verify(&self, payload: &[u8], signature: &ed25519_dalek::Signature) -> crate::provenance::verify::Result<()> {
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
        let identity_path = temp_dir.path().join("node_identity.json");

        // Generate and save
        let identity1 = NodeIdentity::generate();
        identity1.save(&identity_path).unwrap();

        // Load
        let identity2 = NodeIdentity::load(&identity_path).unwrap();

        // Verify they're the same
        assert_eq!(identity1.node_id(), identity2.node_id());
        assert_eq!(identity1.verifying_key().as_bytes(), identity2.verifying_key().as_bytes());
    }

    #[test]
    fn test_load_or_create_new() {
        let temp_dir = TempDir::new().unwrap();

        // Should create new identity
        let identity = NodeIdentity::load_or_create(temp_dir.path()).unwrap();

        assert_eq!(identity.node_id().len(), 32);
        assert!(temp_dir.path().join("node_identity.json").exists());
    }

    #[test]
    fn test_load_or_create_existing() {
        let temp_dir = TempDir::new().unwrap();

        // Create identity
        let identity1 = NodeIdentity::generate();
        identity1.save(&temp_dir.path().join("node_identity.json")).unwrap();

        // Load existing
        let identity2 = NodeIdentity::load_or_create(temp_dir.path()).unwrap();

        // Should be the same identity
        assert_eq!(identity1.node_id(), identity2.node_id());
    }

    #[test]
    fn test_file_permissions() {
        let temp_dir = TempDir::new().unwrap();
        let identity_path = temp_dir.path().join("node_identity.json");

        let identity = NodeIdentity::generate();
        identity.save(&identity_path).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = fs::metadata(&identity_path).unwrap();
            let mode = metadata.permissions().mode() & 0o777;

            // Should be 0o600 (owner read/write only)
            assert_eq!(mode, 0o600, "Identity file should have restrictive permissions");
        }
    }
}
