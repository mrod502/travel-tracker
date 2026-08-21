//! Ed25519 signing for canonical payloads.
//!
//! This module implements Ed25519 digital signature generation using the
//! `ed25519-dalek` crate. The signing key is derived from the node's identity.
//!
//! # Example
//!
//! ```ignore
//! use app::provenance::{
//!     sign::sign_payload,
//!     verify::verify_signature,
//! };
//! use ed25519_dalek::{SigningKey, VerifyingKey};
//!
//! // Generate or load signing key
//! let signing_key = SigningKey::generate(&mut rand::thread_rng());
//!
//! // Sign the payload
//! let payload_bytes = b"canonical payload bytes";
//! let signature = sign_payload(&signing_key, payload_bytes)?;
//!
//! // Verify
//! let verifying_key = signing_key.verifying_key();
//! verify_signature(&verifying_key, payload_bytes, &signature)?;
//! ```

use ed25519_dalek::{ed25519::signature::Signer, SigningKey, Signature};

/// Error type for signing operations.
#[derive(Debug, Clone, PartialEq)]
pub enum SignError {
    /// Signing failed.
    SigningFailed(String),
}

impl std::fmt::Display for SignError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SignError::SigningFailed(msg) => write!(f, "Signing failed: {}", msg),
        }
    }
}

impl std::error::Error for SignError {}

/// Result type for signing operations.
pub type Result<T> = std::result::Result<T, SignError>;

/// Sign a payload using Ed25519.
///
/// This function produces a 64-byte Ed25519 signature over the provided payload.
///
/// # Arguments
///
/// * `signing_key` - The Ed25519 signing key (private key)
/// * `payload` - The bytes to sign (typically the CBOR-encoded canonical payload)
///
/// # Returns
///
/// * `Ok(Signature)` - The 64-byte Ed25519 signature
/// * `Err(SignError)` - If signing fails
///
/// # Example
///
/// ```ignore
/// use app::provenance::sign::sign_payload;
/// use ed25519_dalek::SigningKey;
///
/// let signing_key = /* ... */;
/// let payload = b"canonical payload bytes";
/// let signature = sign_payload(&signing_key, payload)?;
/// assert_eq!(signature.as_ref().len(), 64);
/// ```
pub fn sign_payload(signing_key: &SigningKey, payload: &[u8]) -> Result<Signature> {
    let signature = signing_key.sign(payload);
    Ok(signature)
}

/// Compute the node ID from a public key.
///
/// The node ID is the SHA-256 hash of the Ed25519 public key bytes.
///
/// # Arguments
///
/// * `public_key` - The Ed25519 verifying/public key
///
/// # Returns
///
/// A 32-byte SHA-256 hash of the public key
///
/// # Example
///
/// ```ignore
/// use app::provenance::sign::compute_node_id;
/// use ed25519_dalek::SigningKey;
///
/// let signing_key = SigningKey::generate(&mut rand::thread_rng());
/// let node_id = compute_node_id(&signing_key.verifying_key());
/// assert_eq!(node_id.len(), 32);
/// ```
pub fn compute_node_id(public_key: &ed25519_dalek::VerifyingKey) -> Vec<u8> {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(public_key.as_ref());
    hasher.finalize().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use rand::thread_rng;

    #[test]
    fn test_sign_payload() {
        let signing_key = SigningKey::generate(&mut thread_rng());
        let payload = b"test payload to sign";

        let signature = sign_payload(&signing_key, payload).unwrap();

        assert_eq!(signature.to_bytes().len(), 64);
    }

    #[test]
    fn test_compute_node_id() {
        let signing_key = SigningKey::generate(&mut thread_rng());
        let verifying_key = signing_key.verifying_key();
        let node_id = compute_node_id(&verifying_key);

        assert_eq!(node_id.len(), 32);
    }

    #[test]
    fn test_node_id_is_deterministic_for_same_key() {
        let signing_key = SigningKey::generate(&mut thread_rng());
        let verifying_key = signing_key.verifying_key();
        let node_id_1 = compute_node_id(&verifying_key);
        let node_id_2 = compute_node_id(&verifying_key);

        assert_eq!(node_id_1, node_id_2);
    }

    #[test]
    fn test_different_keys_produce_different_node_ids() {
        let signing_key1 = SigningKey::generate(&mut thread_rng());
        let signing_key2 = SigningKey::generate(&mut thread_rng());
        let verifying_key1 = signing_key1.verifying_key();
        let verifying_key2 = signing_key2.verifying_key();

        let node_id1 = compute_node_id(&verifying_key1);
        let node_id2 = compute_node_id(&verifying_key2);

        assert_ne!(node_id1, node_id2);
    }

    #[test]
    fn test_sign_different_payloads_produce_different_signatures() {
        let signing_key = SigningKey::generate(&mut thread_rng());

        let sig1 = sign_payload(&signing_key, b"payload 1").unwrap();
        let sig2 = sign_payload(&signing_key, b"payload 2").unwrap();

        assert_ne!(sig1.to_bytes(), sig2.to_bytes());
    }

    #[test]
    fn test_sign_empty_payload() {
        let signing_key = SigningKey::generate(&mut thread_rng());

        let signature = sign_payload(&signing_key, b"").unwrap();

        assert_eq!(signature.to_bytes().len(), 64);
    }

    #[test]
    fn test_sign_large_payload() {
        let signing_key = SigningKey::generate(&mut thread_rng());
        let large_payload = vec![0u8; 1024 * 1024]; // 1MB

        let signature = sign_payload(&signing_key, &large_payload).unwrap();

        assert_eq!(signature.to_bytes().len(), 64);
    }
}
