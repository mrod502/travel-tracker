//! CA root key management.

use ed25519_dalek::SigningKey;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::credential::Credential;
use crate::error::{CaError, Result};
use crate::revocation::{RevokedNode, RevocationReason, RevocationStatusList};
use crate::signing::sign_credential;

/// CA root keypair and associated operations.
///
/// The `CaRoot` represents the Certificate Authority's master keypair.
/// It is responsible for:
/// - Generating credentials for nodes
/// - Verifying credentials it has issued
/// - Managing its own key storage
///
/// # Security
///
/// The CA root private key is highly sensitive and should be:
/// - Stored securely (encrypted at rest)
/// - Accessible only to authorized processes
/// - Backed up in secure locations
/// - Potentially stored in an HSM for production use
#[derive(Debug, Clone)]
pub struct CaRoot {
    /// The CA's Ed25519 signing key.
    signing_key: SigningKey,

    /// Cached public key bytes (to avoid temporary value issues).
    public_key_bytes: [u8; 32],

    /// Path to the root key file (if loaded from disk).
    key_path: Option<PathBuf>,
}

impl CaRoot {
    /// Generate a new CA root keypair.
    ///
    /// This should only be done once during CA initialization.
    ///
    /// # Returns
    ///
    /// A new `CaRoot` instance with a randomly generated keypair.
    pub fn generate() -> Self {
        let mut csprng = OsRng;
        let signing_key = SigningKey::generate(&mut csprng);
        let public_key_bytes = *signing_key.verifying_key().as_bytes();

        CaRoot {
            signing_key,
            public_key_bytes,
            key_path: None,
        }
    }

    /// Load the CA root keypair from a file.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the file containing the hex-encoded private key
    ///
    /// # Returns
    ///
    /// A `CaRoot` instance loaded from the file, or an error if the file
    /// cannot be read or the key is invalid.
    pub fn load_from_file(path: &Path) -> Result<Self> {
        let hex_key = fs::read_to_string(path)
            .map_err(|e| CaError::RootKey(format!("Failed to read key file: {}", e)))?;

        let signing_key = Self::decode_signing_key_from_hex(&hex_key)?;
        let public_key_bytes = *signing_key.verifying_key().as_bytes();

        Ok(CaRoot {
            signing_key,
            public_key_bytes,
            key_path: Some(path.to_path_buf()),
        })
    }

    /// Save the CA root private key to a file.
    ///
    /// # Arguments
    ///
    /// * `path` - Path where the key should be saved
    /// * `permissions` - File permissions (default: 0o600 on Unix)
    ///
    /// # Security
    ///
    /// This writes the private key to disk. Ensure:
    /// - The directory is secure
    /// - File permissions are restrictive (0o600)
    /// - The file is encrypted if possible
    pub fn save_to_file(&self, path: &Path, permissions: Option<u32>) -> Result<()> {
        let hex_key = self.encode_signing_key_to_hex();

        // Write the key
        fs::write(path, &hex_key)
            .map_err(|e| CaError::RootKey(format!("Failed to write key file: {}", e)))?;

        // Set permissions (Unix only)
        #[cfg(unix)]
        if let Some(perm) = permissions {
            use std::os::unix::fs::PermissionsExt;
            let perms = fs::Permissions::from_mode(perm);
            fs::set_permissions(path, perms)
                .map_err(|e| CaError::RootKey(format!("Failed to set permissions: {}", e)))?;
        }

        Ok(())
    }

    /// Get the CA's public key.
    pub fn public_key(&self) -> Vec<u8> {
        self.public_key_bytes.to_vec()
    }

    /// Get the CA's public key as a slice.
    pub fn public_key_slice(&self) -> &[u8; 32] {
        &self.public_key_bytes
    }

    /// Get the CA's public key as a Vec.
    pub fn public_key_vec(&self) -> Vec<u8> {
        self.public_key()
    }

    /// Get the CA's private key.
    ///
    /// # Warning
    ///
    /// This exposes the raw private key bytes. Handle with extreme care.
    pub fn private_key(&self) -> &[u8] {
        self.signing_key.as_bytes()
    }

    /// Get the keypair path (if loaded from file).
    pub fn key_path(&self) -> Option<&Path> {
        self.key_path.as_deref()
    }

    /// Get the CA's identifier (hash of public key).
    pub fn ca_id(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(&self.public_key_bytes);
        hex::encode(hasher.finalize())
    }

    /// Issue a credential for a node's public key.
    ///
    /// # Arguments
    ///
    /// * `signing_public_key` - The node's Ed25519 public key (32 bytes)
    /// * `validity_days` - How many days the credential should be valid (None = default)
    ///
    /// # Returns
    ///
    /// A signed `Credential` that attests to the node's key.
    pub fn issue_credential(
        &self,
        signing_public_key: &[u8],
        validity_days: Option<u64>,
    ) -> Result<Credential> {
        let issued_at = chrono::Utc::now();
        let expires_at = validity_days.map(|days| {
            issued_at + chrono::Duration::days(days as i64)
        });

        // Sign the credential
        let ca_signature = sign_credential(
            &self.signing_key,
            signing_public_key,
            issued_at,
            expires_at,
        )?;

        Credential::new(
            signing_public_key.to_vec(),
            ca_signature,
            issued_at,
            expires_at,
            None,
        )
    }

    /// Verify a credential was issued by this CA.
    ///
    /// # Arguments
    ///
    /// * `credential` - The credential to verify
    ///
    /// # Returns
    ///
    /// `Ok(true)` if the credential is valid and was issued by this CA.
    /// `Ok(false)` or `Err(...)` if verification fails.
    pub fn verify_credential(&self, credential: &Credential) -> Result<()> {
        // First, verify node_id integrity
        credential.verify_node_id_integrity()?;

        // Check validity period
        credential.is_valid_now()?;

        // Verify the CA signature
        let payload = self.credential_payload_bytes(
            &credential.signing_public_key,
            credential.issued_at,
            credential.expires_at,
        )?;

        let signature = ed25519_dalek::Signature::try_from(credential.ca_signature.as_slice())
            .map_err(|_| CaError::InvalidCredential("Signature must be 64 bytes".to_string()))?;

        use ed25519_dalek::Verifier;
        self.signing_key
            .verifying_key()
            .verify(&payload, &signature)?;

        Ok(())
    }

    /// Verify a credential's signature is valid (without checking expiration).
    ///
    /// This is useful for historical verification where expiration doesn't matter.
    pub fn verify_credential_signature(&self, credential: &Credential) -> Result<()> {
        // Verify node_id integrity
        credential.verify_node_id_integrity()?;

        // Verify the CA signature
        let payload = self.credential_payload_bytes(
            &credential.signing_public_key,
            credential.issued_at,
            credential.expires_at,
        )?;

        let signature = ed25519_dalek::Signature::try_from(credential.ca_signature.as_slice())
            .map_err(|_| CaError::InvalidCredential("Signature must be 64 bytes".to_string()))?;

        use ed25519_dalek::Verifier;
        self.signing_key
            .verifying_key()
            .verify(&payload, &signature)?;

        Ok(())
    }

    /// Encode the credential payload for signing.
    fn credential_payload_bytes(
        &self,
        signing_public_key: &[u8],
        issued_at: chrono::DateTime<chrono::Utc>,
        expires_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<Vec<u8>> {
        let mut payload = Vec::new();

        // Signing public key (32 bytes)
        payload.extend_from_slice(signing_public_key);

        // Issued at timestamp
        payload.extend_from_slice(&issued_at.timestamp().to_be_bytes());

        // Expiration timestamp (0 if None)
        if let Some(expires) = expires_at {
            payload.extend_from_slice(&1u8.to_be_bytes()); // Has expiration flag
            payload.extend_from_slice(&expires.timestamp().to_be_bytes());
        } else {
            payload.extend_from_slice(&0u8.to_be_bytes()); // No expiration flag
        }

        Ok(payload)
    }

    /// Revoke a node by adding it to a revocation list.
    ///
    /// # Arguments
    ///
    /// * `node_id` - The node identifier to revoke (SHA-256 of signing public key)
    /// * `signing_public_key` - The node's public key (for verification)
    /// * `reason` - Reason for revocation
    /// * `notes` - Optional notes about the revocation
    ///
    /// # Returns
    ///
    /// A `RevokedNode` entry that can be added to an RSL.
    pub fn revoke_node(
        &self,
        node_id: Vec<u8>,
        signing_public_key: Vec<u8>,
        reason: RevocationReason,
        notes: Option<String>,
    ) -> RevokedNode {
        let mut revocation = RevokedNode::new(
            node_id,
            chrono::Utc::now(),
            reason,
            signing_public_key,
        );
        revocation.notes = notes;
        revocation
    }

    /// Create a signed Revocation Status List (RSL).
    ///
    /// # Arguments
    ///
    /// * `revocations` - List of revoked nodes
    /// * `validity_days` - How many days the RSL should be valid
    ///
    /// # Returns
    ///
    /// A signed `RevocationStatusList` ready for distribution.
    pub fn create_rsl(
        &self,
        revocations: Vec<RevokedNode>,
        validity_days: u64,
    ) -> Result<RevocationStatusList> {
        use ed25519_dalek::Signer;
        
        let issued_at = chrono::Utc::now();
        let expires_at = issued_at + chrono::Duration::days(validity_days as i64);
        let sequence_number = 0; // TODO: Implement sequence tracking

        // Build the payload to sign
        let payload = self.rsl_payload_bytes(&revocations, issued_at, expires_at, sequence_number)?;

        // Sign the payload
        let signature = self.signing_key.sign(&payload);

        Ok(RevocationStatusList {
            issuer_id: self.ca_id(),
            issued_at,
            expires_at,
            sequence_number,
            revocations,
            signature: signature.to_bytes().to_vec(),
        })
    }

    /// Verify a Revocation Status List's signature.
    ///
    /// # Arguments
    ///
    /// * `rsl` - The RSL to verify
    ///
    /// # Returns
    ///
    /// `Ok(true)` if the signature is valid and from this CA.
    pub fn verify_rsl(&self, rsl: &RevocationStatusList) -> Result<bool> {
        use ed25519_dalek::Verifier;

        // Verify the issuer matches
        if rsl.issuer_id != self.ca_id() {
            return Ok(false);
        }

        // Build the payload that was signed
        let payload = self.rsl_payload_bytes(
            &rsl.revocations,
            rsl.issued_at,
            rsl.expires_at,
            rsl.sequence_number,
        )?;

        // Parse the signature
        let signature = ed25519_dalek::Signature::try_from(
            rsl.signature.as_slice()
        ).map_err(|_| CaError::InvalidCredential("RSL signature must be 64 bytes".to_string()))?;

        // Verify
        Ok(self.signing_key.verifying_key().verify(&payload, &signature).is_ok())
    }

    /// Encode the RSL payload for signing.
    fn rsl_payload_bytes(
        &self,
        revocations: &[RevokedNode],
        issued_at: chrono::DateTime<chrono::Utc>,
        expires_at: chrono::DateTime<chrono::Utc>,
        sequence_number: u64,
    ) -> Result<Vec<u8>> {
        let mut payload = Vec::new();

        // Issuer ID (length-prefixed)
        let ca_id = self.ca_id();
        let issuer_bytes = ca_id.as_bytes();
        payload.extend_from_slice(&(issuer_bytes.len() as u32).to_be_bytes());
        payload.extend_from_slice(issuer_bytes);

        // Timestamps
        payload.extend_from_slice(&issued_at.timestamp().to_be_bytes());
        payload.extend_from_slice(&expires_at.timestamp().to_be_bytes());

        // Sequence number
        payload.extend_from_slice(&sequence_number.to_be_bytes());

        // Revocation count
        payload.extend_from_slice(&(revocations.len() as u32).to_be_bytes());

        // Each revocation
        for revocation in revocations {
            // Node ID
            payload.extend_from_slice(&(revocation.node_id.len() as u32).to_be_bytes());
            payload.extend_from_slice(&revocation.node_id);

            // Revoked at
            payload.extend_from_slice(&revocation.revoked_at.timestamp().to_be_bytes());

            // Reason
            payload.push(revocation.reason.as_u8());

            // Signing public key
            payload.extend_from_slice(&(revocation.signing_public_key.len() as u32).to_be_bytes());
            payload.extend_from_slice(&revocation.signing_public_key);
        }

        Ok(payload)
    }

    /// Encode the signing key to hex format for storage.
    fn encode_signing_key_to_hex(&self) -> String {
        hex::encode(self.signing_key.as_bytes())
    }

    /// Decode a signing key from hex format.
    fn decode_signing_key_from_hex(hex_str: &str) -> Result<SigningKey> {
        let secret_bytes = hex::decode(hex_str)
            .map_err(|e| CaError::RootKey(format!("Invalid hex encoding: {}", e)))?;

        if secret_bytes.len() != 32 {
            return Err(CaError::RootKey(format!(
                "Secret key must be 32 bytes, got {}",
                secret_bytes.len()
            )));
        }

        let mut secret_bytes_array = [0u8; 32];
        secret_bytes_array.copy_from_slice(&secret_bytes);

        Ok(SigningKey::from_bytes(&secret_bytes_array))
    }
}

/// Configuration for CA root key storage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaRootConfig {
    /// Path to the CA root key file.
    pub key_path: PathBuf,

    /// Whether to auto-generate a key if not exists.
    pub auto_generate: bool,

    /// File permissions for the key file (Unix only, default: 0o600).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permissions: Option<u32>,
}

impl Default for CaRootConfig {
    fn default() -> Self {
        Self {
            key_path: PathBuf::from("/var/lib/btmon/ca/root_key.hex"),
            auto_generate: true,
            permissions: None,
        }
    }
}

impl CaRootConfig {
    /// Load or create the CA root based on this configuration.
    pub fn load_or_create(&self) -> Result<CaRoot> {
        let path = &self.key_path;

        if path.exists() {
            CaRoot::load_from_file(path)
        } else if self.auto_generate {
            // Ensure parent directory exists
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| CaError::RootKey(format!("Failed to create dir: {}", e)))?;
            }

            let ca = CaRoot::generate();
            ca.save_to_file(
                path,
                self.permissions.or(Some(crate::CA_ROOT_KEY_PERMISSIONS)),
            )?;
            Ok(ca)
        } else {
            Err(CaError::RootKeyNotInitialized)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_generate_and_issue_credential() {
        let ca = CaRoot::generate();

        let node_public_key = vec![1u8; 32];
        let credential = ca.issue_credential(&node_public_key, Some(90)).unwrap();

        // Verify the credential
        ca.verify_credential(&credential).unwrap();

        assert_eq!(credential.signing_public_key, node_public_key);
        assert!(credential.expires_at.is_some());
    }

    #[test]
    fn test_save_and_load_keypair() {
        let temp_dir = TempDir::new().unwrap();
        let key_path = temp_dir.path().join("root_key.hex");

        // Generate and save
        let ca1 = CaRoot::generate();
        ca1.save_to_file(&key_path, Some(0o600)).unwrap();

        // Load and verify
        let ca2 = CaRoot::load_from_file(&key_path).unwrap();

        assert_eq!(ca1.public_key_vec(), ca2.public_key_vec());
    }

    #[test]
    fn test_verify_invalid_credential() {
        let ca = CaRoot::generate();

        let node_public_key = vec![1u8; 32];
        let mut credential = ca.issue_credential(&node_public_key, Some(90)).unwrap();

        // Tamper with the credential
        credential.signing_public_key[0] ^= 1;

        assert!(ca.verify_credential(&credential).is_err());
    }

    #[test]
    fn test_expired_credential_fails_verification() {
        let ca = CaRoot::generate();

        let node_public_key = vec![1u8; 32];
        let credential = ca.issue_credential(&node_public_key, Some(1)).unwrap();

        // Wait for expiration (simulate by modifying credential)
        let mut expired_credential = credential.clone();
        expired_credential.issued_at = chrono::Utc::now() - chrono::Duration::days(100);
        expired_credential.expires_at = Some(chrono::Utc::now() - chrono::Duration::days(50));

        assert!(ca.verify_credential(&expired_credential).is_err());
    }

    #[test]
    fn test_credential_verification_signature_only() {
        let ca = CaRoot::generate();

        let node_public_key = vec![1u8; 32];
        let credential = ca.issue_credential(&node_public_key, Some(90)).unwrap();

        // Signature verification should succeed even for expired creds
        assert!(ca.verify_credential_signature(&credential).is_ok());
    }

    #[test]
    fn test_ca_id_generation() {
        let ca = CaRoot::generate();
        let ca_id = ca.ca_id();
        
        // Should be a valid hex string (64 chars for SHA-256)
        assert_eq!(ca_id.len(), 64);
        assert!(ca_id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_create_and_verify_rsl() {
        let ca = CaRoot::generate();

        // Create some revoked nodes
        let revocation1 = ca.revoke_node(
            vec![1u8; 32],
            vec![2u8; 32],
            RevocationReason::KeyCompromise,
            Some("Test revocation".to_string()),
        );

        let revocation2 = ca.revoke_node(
            vec![3u8; 32],
            vec![4u8; 32],
            RevocationReason::PolicyViolation,
            None,
        );

        // Create RSL
        let rsl = ca.create_rsl(
            vec![revocation1, revocation2],
            7,
        ).unwrap();

        // Verify RSL
        assert!(ca.verify_rsl(&rsl).unwrap());
        assert_eq!(rsl.revocation_count(), 2);
        assert!(rsl.is_valid_now());
    }

    #[test]
    fn test_tampered_rsl_fails_verification() {
        let ca = CaRoot::generate();

        let revocation = ca.revoke_node(
            vec![1u8; 32],
            vec![2u8; 32],
            RevocationReason::KeyCompromise,
            None,
        );

        let mut rsl = ca.create_rsl(vec![revocation], 7).unwrap();

        // Tamper with the signature
        rsl.signature[0] ^= 1;

        // Verification should fail
        assert!(!ca.verify_rsl(&rsl).unwrap());
    }

    #[test]
    fn test_rsl_from_different_ca_fails_verification() {
        let ca1 = CaRoot::generate();
        let ca2 = CaRoot::generate();

        let revocation = ca1.revoke_node(
            vec![1u8; 32],
            vec![2u8; 32],
            RevocationReason::KeyCompromise,
            None,
        );

        let rsl = ca1.create_rsl(vec![revocation], 7).unwrap();

        // Verify with different CA should fail
        assert!(!ca2.verify_rsl(&rsl).unwrap());
    }

    #[test]
    fn test_rsl_node_revocation_check() {
        let ca = CaRoot::generate();

        let revoked_node_id = vec![1u8; 32];
        let active_node_id = vec![2u8; 32];

        let revoked_node = ca.revoke_node(
            revoked_node_id.clone(),
            vec![3u8; 32],
            RevocationReason::KeyCompromise,
            None,
        );

        let rsl = ca.create_rsl(vec![revoked_node], 7).unwrap();

        assert!(rsl.is_node_revoked(&revoked_node_id));
        assert!(!rsl.is_node_revoked(&active_node_id));
    }
}
