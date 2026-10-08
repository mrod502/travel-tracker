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
//! Hex is not used for key material anywhere here. The JSON-with-hex-file this
//! project wrote before PKCS#8 is converted on load ([`NodeIdentity::load_or_create`])
//! rather than silently read, because a secret read as the wrong format yields a
//! confident, entirely different identity.
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
use serde::Deserialize;
use std::fs;
use std::path::Path;

use crate::error::{AppError, Result};
use crate::provenance::sign::{compute_node_id, sign_payload as sign_raw_payload};
use crate::provenance::verify::verify_signature as verify_raw_signature;

/// The on-disk shape of an identity from before PKCS#8.
///
/// Read-only and only for [`NodeIdentity::load_or_create`]'s conversion: nothing
/// writes this format any more, and a private key in a JSON string field is the
/// thing being retired.
#[derive(Deserialize, Debug)]
struct LegacySerializedIdentity {
    private_key_hex: String,
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
    /// The signing key: PKCS#8 PEM, `0600`.
    pub const IDENTITY_FILENAME: &'static str = "node_identity.pem";

    /// The published public key: SPKI PEM.
    pub const PUBLIC_KEY_FILENAME: &'static str = "node_identity.pub.pem";

    /// The JSON/hex file used before PKCS#8, converted on first start rather than
    /// read, and renamed to `.bak` once the conversion has been written.
    const LEGACY_IDENTITY_FILENAME: &'static str = "node_identity.json";

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
    /// A `node_identity.json` from before PKCS#8 is converted here on first start:
    /// the same secret is written as PEM (so the node ID, and every occurrence this
    /// node has already signed, are unchanged), the public half is published beside
    /// it, and the JSON is renamed to `.bak` rather than deleted — it holds a private
    /// key, and a rename leaves an operator a way back. The `.bak` is then safe to
    /// remove.
    ///
    /// # Arguments
    ///
    /// * `data_dir` - Directory where the identity file should be stored
    ///
    /// # Returns
    ///
    /// * `Ok(NodeIdentity)` - The loaded, converted or newly generated identity
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

        if let Some(converted) = Self::convert_legacy_file(data_dir)? {
            converted.save_public_key(&public_path)?;
            return Ok(converted);
        }

        let identity = Self::generate();
        identity.save(&identity_path)?;
        identity.save_public_key(&public_path)?;
        Ok(identity)
    }

    /// Read a pre-PKCS#8 `node_identity.json`, write it as PEM, and set it aside.
    ///
    /// Returns `Ok(None)` when there is no such file, so the caller can proceed to
    /// generate a fresh identity.
    fn convert_legacy_file(data_dir: &Path) -> Result<Option<Self>> {
        let legacy_path = data_dir.join(Self::LEGACY_IDENTITY_FILENAME);
        if !legacy_path.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(&legacy_path).map_err(|e| {
            AppError::Io(format!(
                "Failed to read legacy identity file {}: {e}",
                legacy_path.display()
            ))
        })?;

        let legacy: LegacySerializedIdentity = serde_json::from_str(&content).map_err(|e| {
            AppError::Io(format!(
                "{} is not a PKCS#8 PEM key and could not be read as the older JSON format \
                     either: {e}",
                legacy_path.display()
            ))
        })?;

        let private_key_bytes = hex::decode(&legacy.private_key_hex).map_err(|e| {
            AppError::Io(format!(
                "Legacy identity file {} has an invalid private key: {e}",
                legacy_path.display()
            ))
        })?;

        let secret: [u8; 32] = private_key_bytes.as_slice().try_into().map_err(|_| {
            AppError::Io(format!(
                "Legacy identity file {} holds a {}-byte private key, not 32",
                legacy_path.display(),
                private_key_bytes.len()
            ))
        })?;

        // Only the secret is taken from the old file. The public half — and with it
        // the node ID — is derived, so a JSON whose two fields disagreed cannot carry
        // that disagreement into the new format.
        let identity = Self::from_signing_key(SigningKey::from_bytes(&secret));
        let pem_path = data_dir.join(Self::IDENTITY_FILENAME);
        identity.save(&pem_path)?;

        let archived = legacy_path.with_extension("json.bak");
        fs::rename(&legacy_path, &archived).map_err(|e| {
            AppError::Io(format!(
                "Failed to move {} aside to {}: {e}",
                legacy_path.display(),
                archived.display()
            ))
        })?;

        log::warn!(
            "converted the node identity from hex JSON to PKCS#8 PEM: {} -> {}; the node ID is \
             unchanged, so occurrences this node already signed still verify. The old copy of \
             the secret is at {} — delete it when you are satisfied",
            legacy_path.display(),
            pem_path.display(),
            archived.display()
        );

        Ok(Some(identity))
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
        assert!(
            !written.contains("private_key_hex"),
            "hex key material is what this format replaced"
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

    /// The migration test, with the identity that migration exists to preserve: a
    /// node that signed occurrences under the old file must still be the same node
    /// afterwards, or the conversion invalidates its whole history.
    #[test]
    fn a_legacy_hex_identity_is_converted_not_dropped() {
        let temp_dir = TempDir::new().unwrap();
        let legacy_path = temp_dir.path().join(NodeIdentity::LEGACY_IDENTITY_FILENAME);

        let original = NodeIdentity::generate();
        let legacy = serde_json::json!({
            "private_key_hex": hex::encode(original.signing_key.as_bytes()),
            "public_key_hex": hex::encode(original.verifying_key.as_bytes()),
        });
        fs::write(&legacy_path, legacy.to_string()).unwrap();

        let loaded = NodeIdentity::load_or_create(temp_dir.path()).unwrap();

        assert_eq!(
            loaded.node_id(),
            original.node_id(),
            "the same secret must yield the same node id after conversion"
        );
        assert!(!legacy_path.exists(), "the old file is set aside");
        assert!(legacy_path.with_extension("json.bak").exists());
        assert!(temp_dir
            .path()
            .join(NodeIdentity::IDENTITY_FILENAME)
            .exists());

        // And the next start reads the PEM, rather than converting again.
        let again = NodeIdentity::load_or_create(temp_dir.path()).unwrap();
        assert_eq!(again.node_id(), original.node_id());
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
