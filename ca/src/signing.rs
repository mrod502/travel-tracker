//! CA credential signing operations.

use chrono::{DateTime, Utc};
use ed25519_dalek::{Signer, SigningKey};

use crate::credential::Credential;
use crate::error::{CaError, Result};

/// Sign a credential payload with the CA's private key.
///
/// This creates the CA's signature over a node's public key,
/// attesting that the CA has vetted and authorized this node.
///
/// # Arguments
///
/// * `ca_signing_key` - The CA's Ed25519 signing key
/// * `signing_public_key` - The node's Ed25519 public key (32 bytes)
/// * `issued_at` - When the credential is being issued
/// * `expires_at` - Optional expiration timestamp
///
/// # Returns
///
/// A 64-byte Ed25519 signature.
pub fn sign_credential(
    ca_signing_key: &SigningKey,
    signing_public_key: &[u8],
    issued_at: DateTime<Utc>,
    expires_at: Option<DateTime<Utc>>,
) -> Result<Vec<u8>> {
    // Build the payload to sign
    let payload = build_credential_payload(signing_public_key, issued_at, expires_at);

    // Sign with Ed25519
    let signature = ca_signing_key.sign(&payload);

    Ok(signature.to_bytes().to_vec())
}

/// Build the credential payload for signing.
///
/// The payload format is deterministic and includes:
/// 1. Signing public key (32 bytes)
/// 2. Issued at timestamp (8 bytes, big-endian Unix timestamp)
/// 3. Expiration flag (1 byte: 0 = no expiration, 1 = has expiration)
/// 4. Expiration timestamp (8 bytes, if flag is 1)
fn build_credential_payload(
    signing_public_key: &[u8],
    issued_at: DateTime<Utc>,
    expires_at: Option<DateTime<Utc>>,
) -> Vec<u8> {
    let mut payload = Vec::new();

    // 1. Signing public key (32 bytes)
    payload.extend_from_slice(signing_public_key);

    // 2. Issued at timestamp (8 bytes, big-endian)
    payload.extend_from_slice(&issued_at.timestamp().to_be_bytes());

    // 3. Expiration flag
    if let Some(expires) = expires_at {
        payload.push(1); // Has expiration
        // 4. Expiration timestamp (8 bytes, big-endian)
        payload.extend_from_slice(&expires.timestamp().to_be_bytes());
    } else {
        payload.push(0); // No expiration
    }

    payload
}

/// Verify a credential's CA signature.
///
/// # Arguments
///
/// * `ca_public_key` - The CA's Ed25519 public key
/// * `credential` - The credential to verify
///
/// # Returns
///
/// `Ok(true)` if the signature is valid, `Ok(false)` otherwise.
pub fn verify_credential_signature(
    ca_public_key: &[u8],
    credential: &Credential,
) -> Result<bool> {
    use ed25519_dalek::{VerifyingKey, Verifier};

    // Verify node_id integrity first
    credential.verify_node_id_integrity()?;

    // Build the payload that was signed
    let payload = build_credential_payload(
        &credential.signing_public_key,
        credential.issued_at,
        credential.expires_at,
    );

    // Parse the CA's public key
    let mut pk_bytes = [0u8; 32];
    if ca_public_key.len() != 32 {
        return Err(CaError::Verification("Invalid CA public key length".to_string()));
    }
    pk_bytes.copy_from_slice(ca_public_key);
    let verifying_key = VerifyingKey::from_bytes(&pk_bytes)
        .map_err(|e| CaError::Verification(format!("Invalid CA public key: {}", e)))?;

    // Parse the signature
    let signature = ed25519_dalek::Signature::try_from(
        credential.ca_signature.as_slice()
    ).map_err(|_| CaError::InvalidCredential("Signature must be 64 bytes".to_string()))?;

    // Verify
    Ok(verifying_key.verify(&payload, &signature).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;

    #[test]
    fn test_sign_and_verify_credential() {
        // Generate CA keypair
        let mut csprng = OsRng;
        let ca_signing_key = SigningKey::generate(&mut csprng);

        // Node's public key
        let node_public_key = vec![1u8; 32];

        // Sign the credential
        let signature = sign_credential(
            &ca_signing_key,
            &node_public_key,
            Utc::now(),
            Some(Utc::now() + chrono::Duration::days(90)),
        )
        .unwrap();

        // Create credential with signature
        let credential = Credential::new(
            node_public_key.clone(),
            signature,
            Utc::now(),
            Some(Utc::now() + chrono::Duration::days(90)),
            None,
        )
        .unwrap();

        // Verify the signature
        let is_valid = verify_credential_signature(ca_signing_key.verifying_key().as_bytes(), &credential).unwrap();
        assert!(is_valid);
    }

    #[test]
    fn test_tampered_credential_fails_verification() {
        // Generate CA keypair
        let mut csprng = OsRng;
        let ca_signing_key = SigningKey::generate(&mut csprng);

        // Node's public key
        let node_public_key = vec![1u8; 32];

        // Sign the credential
        let signature = sign_credential(
            &ca_signing_key,
            &node_public_key,
            Utc::now(),
            None,
        )
        .unwrap();

        // Create credential with signature
        let mut credential = Credential::new(
            node_public_key.clone(),
            signature,
            Utc::now(),
            None,
            None,
        )
        .unwrap();

        // Tamper with the signature instead (to test signature verification failure)
        credential.ca_signature[0] ^= 1;

        // Verification should fail
        let is_valid = verify_credential_signature(ca_signing_key.verifying_key().as_bytes(), &credential).unwrap();
        assert!(!is_valid);
    }

    #[test]
    fn test_payload_determinism() {
        let node_public_key = vec![1u8; 32];
        let issued_at = Utc::now();
        let expires_at = Some(issued_at + chrono::Duration::days(90));

        // Build payload twice
        let payload1 = build_credential_payload(&node_public_key, issued_at, expires_at);
        let payload2 = build_credential_payload(&node_public_key, issued_at, expires_at);

        assert_eq!(payload1, payload2);
    }

    #[test]
    fn test_payload_with_and_without_expiration() {
        let node_public_key = vec![1u8; 32];
        let issued_at = Utc::now();

        // Build payload with expiration
        let expires_at = Some(issued_at + chrono::Duration::days(90));
        let payload_with = build_credential_payload(&node_public_key, issued_at, expires_at);

        // Build payload without expiration
        let payload_without = build_credential_payload(&node_public_key, issued_at, None);

        // They should differ (different expiration flag)
        assert_ne!(payload_with, payload_without);

        // With expiration should be longer (8 extra bytes for expires_at)
        assert_eq!(payload_with.len(), payload_without.len() + 8);
    }
}
