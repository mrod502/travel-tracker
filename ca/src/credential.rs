//! CA credential types and structures.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{CaError, Result};

/// A CA-issued credential that attests to a node's signing key.
///
/// This credential is the CA's signature over a node's public key,
/// proving that the CA has vetted and authorized this node.
///
/// # Structure
///
/// The credential contains:
/// - `node_id`: SHA-256 hash of the signing public key (self-certifying)
/// - `signing_public_key`: The node's Ed25519 public key (32 bytes)
/// - `ca_signature`: CA's Ed25519 signature over the above fields
/// - `issued_at`: When the credential was issued
/// - `expires_at`: When the credential expires (optional)
///
/// # Verification
///
/// To verify a credential:
/// 1. Recompute `node_id` from `signing_public_key` (SHA-256)
/// 2. Verify `ca_signature` over (`signing_public_key`, `issued_at`, `expires_at`)
/// 3. Check expiration (if set)
/// 4. Check revocation status (separate lookup)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Credential {
    /// Node ID (SHA-256 of signing_public_key).
    /// This is self-certifying - anyone can recompute it.
    pub node_id: Vec<u8>,

    /// Node's Ed25519 public key (32 bytes).
    pub signing_public_key: Vec<u8>,

    /// CA's Ed25519 signature over the credential payload.
    pub ca_signature: Vec<u8>,

    /// When the credential was issued (UTC).
    pub issued_at: DateTime<Utc>,

    /// When the credential expires (UTC). None = never expires.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,

    /// Optional: Issuer identifier (for multi-CA setups).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issuer_id: Option<String>,
}

impl Credential {
    /// Create a new credential.
    ///
    /// The `node_id` is automatically computed as SHA-256(signing_public_key).
    ///
    /// # Arguments
    ///
    /// * `signing_public_key` - The node's Ed25519 public key (must be 32 bytes)
    /// * `ca_signature` - The CA's signature over the credential payload
    /// * `issued_at` - When the credential was issued
    /// * `expires_at` - Optional expiration time
    /// * `issuer_id` - Optional issuer identifier
    ///
    /// # Returns
    ///
    /// A new `Credential` instance, or an error if the public key is invalid.
    pub fn new(
        signing_public_key: Vec<u8>,
        ca_signature: Vec<u8>,
        issued_at: DateTime<Utc>,
        expires_at: Option<DateTime<Utc>>,
        issuer_id: Option<String>,
    ) -> Result<Self> {
        // Validate signing public key length (Ed25519 public keys are 32 bytes)
        if signing_public_key.len() != 32 {
            return Err(CaError::InvalidKey(format!(
                "Signing public key must be 32 bytes, got {}",
                signing_public_key.len()
            )));
        }

        // Compute node_id as SHA-256(signing_public_key)
        let node_id = Sha256::digest(&signing_public_key).to_vec();

        Ok(Credential {
            node_id,
            signing_public_key,
            ca_signature,
            issued_at,
            expires_at,
            issuer_id,
        })
    }

    /// Create a credential for testing purposes (without CA signature).
    ///
    /// # Warning
    ///
    /// This is for testing only. Production credentials MUST be signed by the CA.
    #[cfg(test)]
    pub fn for_testing(
        signing_public_key: Vec<u8>,
        node_id: Option<Vec<u8>>,
    ) -> Self {
        let node_id = node_id.unwrap_or_else(|| {
            Sha256::digest(&signing_public_key).to_vec()
        });

        Credential {
            node_id,
            signing_public_key,
            ca_signature: vec![0u8; 64], // Dummy signature for testing
            issued_at: Utc::now(),
            expires_at: None,
            issuer_id: None,
        }
    }

    /// Verify that the credential's node_id matches the signing_public_key.
    ///
    /// This is a basic integrity check - the node_id should always be
    /// SHA-256(signing_public_key).
    pub fn verify_node_id_integrity(&self) -> Result<()> {
        let computed_node_id = Sha256::digest(&self.signing_public_key).to_vec();

        if computed_node_id != self.node_id {
            return Err(CaError::InvalidCredential(
                "node_id does not match SHA-256(signing_public_key)".to_string(),
            ));
        }

        Ok(())
    }

    /// Check if the credential is currently valid (not expired, not yet valid).
    pub fn is_valid_now(&self) -> Result<()> {
        let now = Utc::now();

        if now < self.issued_at {
            return Err(CaError::NotYetValid(self.issued_at));
        }

        if let Some(expires_at) = self.expires_at {
            if now > expires_at {
                return Err(CaError::ExpiredCredential(expires_at));
            }
        }

        Ok(())
    }

    /// Get the credential's validity period in days.
    pub fn validity_days(&self) -> Option<u64> {
        self.expires_at.map(|expires_at| {
            (expires_at - self.issued_at).num_days() as u64
        })
    }

    /// Encode the credential as hex for storage/display.
    pub fn to_hex(&self) -> String {
        hex::encode(&self.ca_signature)
            + &hex::encode(&self.signing_public_key)
            + &hex::encode(&self.issued_at.to_rfc3339())
    }

    /// Decode a credential from hex encoding.
    pub fn from_hex(hex_str: &str) -> Result<Self> {
        let bytes = hex::decode(hex_str)
            .map_err(|e| CaError::Serialization(format!("Hex decode failed: {}", e)))?;

        // Minimum length: 64 (signature) + 32 (public key) + variable (timestamp)
        if bytes.len() < 96 {
            return Err(CaError::InvalidCredential(
                "Hex string too short for valid credential".to_string(),
            ));
        }

        let ca_signature = bytes[0..64].to_vec();
        let signing_public_key = bytes[64..96].to_vec();
        let timestamp_bytes = &bytes[96..];

        let timestamp_str = String::from_utf8(timestamp_bytes.to_vec())
            .map_err(|e| CaError::Serialization(format!("Invalid timestamp: {}", e)))?;

        let issued_at = DateTime::parse_from_rfc3339(&timestamp_str)
            .map(|dt| dt.with_timezone(&Utc))
            .map_err(|e| CaError::Serialization(format!("Invalid timestamp format: {}", e)))?;

        let node_id = Sha256::digest(&signing_public_key).to_vec();

        Ok(Credential {
            node_id,
            signing_public_key,
            ca_signature,
            issued_at,
            expires_at: None,
            issuer_id: None,
        })
    }
}

/// Credential request from a node to the CA.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialRequest {
    /// Node's Ed25519 public key (32 bytes).
    pub signing_public_key: Vec<u8>,

    /// Optional: Node's display name for logging.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,

    /// Optional: Requested validity period in days.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validity_days: Option<u64>,
}

/// Credential response from the CA.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialResponse {
    /// The issued credential.
    pub credential: Credential,

    /// The CA's public key (for verification).
    pub ca_public_key: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_credential_node_id_computation() {
        let public_key = vec![1u8; 32];
        let expected_node_id = Sha256::digest(&public_key).to_vec();

        let credential = Credential::new(
            public_key.clone(),
            vec![0u8; 64],
            Utc::now(),
            None,
            None,
        )
        .unwrap();

        assert_eq!(credential.node_id, expected_node_id);
    }

    #[test]
    fn test_credential_invalid_key_length() {
        let result = Credential::new(vec![1u8; 16], vec![0u8; 64], Utc::now(), None, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_credential_node_id_integrity() {
        let public_key = vec![1u8; 32];
        let credential = Credential::new(
            public_key.clone(),
            vec![0u8; 64],
            Utc::now(),
            None,
            None,
        )
        .unwrap();

        assert!(credential.verify_node_id_integrity().is_ok());
    }

    #[test]
    fn test_credential_expired() {
        let public_key = vec![1u8; 32];
        let issued_at = Utc::now() - chrono::Duration::days(100);
        let expires_at = Utc::now() - chrono::Duration::days(50);

        let credential = Credential::new(
            public_key,
            vec![0u8; 64],
            issued_at,
            Some(expires_at),
            None,
        )
        .unwrap();

        assert!(credential.is_valid_now().is_err());
    }

    #[test]
    fn test_credential_hex_encoding() {
        let public_key = vec![1u8; 32];
        let credential = Credential::new(
            public_key,
            vec![2u8; 64],
            Utc::now(),
            None,
            None,
        )
        .unwrap();

        let hex = credential.to_hex();
        let decoded = Credential::from_hex(&hex).unwrap();

        assert_eq!(credential.signing_public_key, decoded.signing_public_key);
        assert_eq!(credential.ca_signature, decoded.ca_signature);
    }
}
