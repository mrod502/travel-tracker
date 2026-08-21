//! Signature verification for canonical payloads.
//!
//! This module implements Ed25519 signature verification using the
//! `ed25519-dalek` crate. Verification is done offline without any
//! network access.
//!
//! # Example
//!
//! ```ignore
//! use app::provenance::{
//!     verify::verify_signature,
//! };
//! use ed25519_dalek::VerifyingKey;
//!
//! // The verifying key (public key) of the origin node
//! let verifying_key = /* ... */;
//!
//! // The payload bytes that were signed
//! let payload_bytes = /* ... */;
//!
//! // The signature to verify
//! let signature = /* ... */;
//!
//! // Verify
//! verify_signature(&verifying_key, payload_bytes, &signature)?;
//! // If this returns Ok, the signature is valid
//! ```

use ed25519_dalek::{ed25519::signature::Verifier, VerifyingKey, Signature};

/// Error type for verification operations.
#[derive(Debug, Clone, PartialEq)]
pub enum VerifyError {
    /// Signature verification failed.
    VerificationFailed(String),

    /// Invalid signature format.
    InvalidSignature(String),

    /// Invalid public key format.
    InvalidPublicKey(String),
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VerifyError::VerificationFailed(msg) => write!(f, "Verification failed: {}", msg),
            VerifyError::InvalidSignature(msg) => write!(f, "Invalid signature: {}", msg),
            VerifyError::InvalidPublicKey(msg) => write!(f, "Invalid public key: {}", msg),
        }
    }
}

impl std::error::Error for VerifyError {}

/// Result type for verification operations.
pub type Result<T> = std::result::Result<T, VerifyError>;

/// Verify an Ed25519 signature.
///
/// This function verifies that the signature was produced by the holder
/// of the private key corresponding to the provided public key.
///
/// # Arguments
///
/// * `verifying_key` - The Ed25519 verifying/public key
/// * `payload` - The bytes that were signed
/// * `signature` - The 64-byte Ed25519 signature
///
/// # Returns
///
/// * `Ok(())` - The signature is valid
/// * `Err(VerifyError)` - If verification fails or signature is invalid
///
/// # Example
///
/// ```ignore
/// use app::provenance::verify::verify_signature;
/// use ed25519_dalek::VerifyingKey;
///
/// let verifying_key = /* ... */;
/// let payload = b"canonical payload bytes";
/// let signature = /* ... */;
///
/// match verify_signature(&verifying_key, payload, &signature) {
///     Ok(()) => println!("Signature is valid!"),
///     Err(e) => println!("Invalid signature: {}", e),
/// }
/// ```
pub fn verify_signature(
    verifying_key: &VerifyingKey,
    payload: &[u8],
    signature: &Signature,
) -> Result<()> {
    verifying_key
        .verify(payload, signature)
        .map_err(|e| VerifyError::VerificationFailed(e.to_string()))
}

/// Verify a signature and return the signing public key if valid.
///
/// This is useful for recovering the signer's identity from a signed payload.
///
/// # Arguments
///
/// * `payload` - The bytes that were signed
/// * `signature` - The 64-byte Ed25519 signature
///
/// # Returns
///
/// * `Ok(VerifyingKey)` - The public key of the signer (signature is valid)
/// * `Err(VerifyError)` - If recovery fails
///
/// # Note
///
/// Ed25519 does not support public key recovery in the traditional sense.
/// This function requires the verifying key as input and just confirms it matches.
/// For true key recovery, you would need a different signature scheme.
pub fn recover_signer(_payload: &[u8], _signature: &Signature) -> Result<VerifyingKey> {
    // Ed25519 doesn't support public key recovery from signature + payload
    // This is a limitation of the Ed25519 scheme
    Err(VerifyError::VerificationFailed(
        "Ed25519 does not support public key recovery".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{ed25519::signature::Signer, SigningKey};
    use rand::thread_rng;

    #[test]
    fn test_verify_valid_signature() {
        let signing_key = SigningKey::generate(&mut thread_rng());
        let verifying_key = signing_key.verifying_key();
        let payload = b"test payload";

        let signature = signing_key.sign(payload);

        let result = verify_signature(&verifying_key, payload, &signature);
        assert!(result.is_ok());
    }

    #[test]
    fn test_verify_invalid_signature() {
        let signing_key = SigningKey::generate(&mut thread_rng());
        let verifying_key = signing_key.verifying_key();
        let payload = b"test payload";

        // Create a fake signature (all zeros)
        let invalid_signature = Signature::from_bytes(&[0u8; 64]);

        let result = verify_signature(&verifying_key, payload, &invalid_signature);
        assert!(result.is_err());
    }

    #[test]
    fn test_verify_tampered_payload() {
        let signing_key = SigningKey::generate(&mut thread_rng());
        let verifying_key = signing_key.verifying_key();
        let original_payload = b"original payload";
        let tampered_payload = b"tampered payload";

        let signature = signing_key.sign(original_payload);

        // Try to verify with tampered payload
        let result = verify_signature(&verifying_key, tampered_payload, &signature);
        assert!(result.is_err());
    }

    #[test]
    fn test_verify_wrong_public_key() {
        let signing_key1 = SigningKey::generate(&mut thread_rng());
        let verifying_key1 = signing_key1.verifying_key();
        let signing_key2 = SigningKey::generate(&mut thread_rng());
        let verifying_key2 = signing_key2.verifying_key();
        let payload = b"test payload";

        let signature = signing_key1.sign(payload);

        // Try to verify with wrong public key
        let result = verify_signature(&verifying_key2, payload, &signature);
        assert!(result.is_err());
    }

    #[test]
    fn test_verify_empty_payload() {
        let signing_key = SigningKey::generate(&mut thread_rng());
        let verifying_key = signing_key.verifying_key();
        let payload = b"";

        let signature = signing_key.sign(payload);

        let result = verify_signature(&verifying_key, payload, &signature);
        assert!(result.is_ok());
    }

    #[test]
    fn test_verify_large_payload() {
        let signing_key = SigningKey::generate(&mut thread_rng());
        let verifying_key = signing_key.verifying_key();
        let payload = vec![0u8; 1024 * 1024]; // 1MB

        let signature = signing_key.sign(&payload);

        let result = verify_signature(&verifying_key, &payload, &signature);
        assert!(result.is_ok());
    }

    #[test]
    fn test_recover_signer_not_supported() {
        let signing_key = SigningKey::generate(&mut thread_rng());
        let payload = b"test payload";

        let signature = signing_key.sign(payload);

        let result = recover_signer(payload, &signature);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("does not support"));
    }

    #[test]
    fn test_verify_malleated_signature() {
        let signing_key = SigningKey::generate(&mut thread_rng());
        let verifying_key = signing_key.verifying_key();
        let payload = b"test payload";

        let mut signature_bytes = signing_key.sign(payload).to_bytes();

        // Flip a bit in the signature
        signature_bytes[0] ^= 0x01;

        let malleated_signature = Signature::from_bytes(&signature_bytes);

        let result = verify_signature(&verifying_key, payload, &malleated_signature);
        assert!(result.is_err());
    }

    #[test]
    fn test_multiple_verifications_same_signature() {
        let signing_key = SigningKey::generate(&mut thread_rng());
        let verifying_key = signing_key.verifying_key();
        let payload = b"test payload";

        let signature = signing_key.sign(payload);

        // Verify multiple times
        for _ in 0..100 {
            let result = verify_signature(&verifying_key, payload, &signature);
            assert!(result.is_ok());
        }
    }
}
