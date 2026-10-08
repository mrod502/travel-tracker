//! What the CA signs, and how to check it with only the CA's public key.
//!
//! The two `verify_*` functions here are the consumer-facing half: each takes the
//! CA's 32-byte public key rather than a [`crate::CaRoot`], which is what makes a
//! [`crate::TrustAnchor`] possible. Both preimage builders are shared with the
//! signing side, so a verifier rebuilds exactly what was signed instead of
//! reimplementing the layout.

use chrono::{DateTime, Utc};
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use sha2::{Digest, Sha256};

use crate::credential::Credential;
use crate::error::{CaError, Result};
use crate::revocation::{RevocationStatusList, RevokedNode};

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
pub(crate) fn build_credential_payload(
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
pub fn verify_credential_signature(ca_public_key: &[u8], credential: &Credential) -> Result<bool> {
    use ed25519_dalek::Verifier;

    // Verify node_id integrity first
    credential.verify_node_id_integrity()?;

    // Build the payload that was signed
    let payload = build_credential_payload(
        &credential.signing_public_key,
        credential.issued_at,
        credential.expires_at,
    );

    let verifying_key = parse_ca_public_key(ca_public_key)?;

    // Parse the signature
    let signature = ed25519_dalek::Signature::try_from(credential.ca_signature.as_slice())
        .map_err(|_| CaError::InvalidCredential("Signature must be 64 bytes".to_string()))?;

    // Verify
    Ok(verifying_key.verify(&payload, &signature).is_ok())
}

/// Parse a CA public key into an Ed25519 verifying key.
///
/// # Errors
///
/// [`CaError::Verification`] if the input is not 32 bytes, or is 32 bytes that do
/// not describe a point on the curve.
pub(crate) fn parse_ca_public_key(ca_public_key: &[u8]) -> Result<VerifyingKey> {
    if ca_public_key.len() != 32 {
        return Err(CaError::Verification(
            "Invalid CA public key length".to_string(),
        ));
    }

    let mut pk_bytes = [0u8; 32];
    pk_bytes.copy_from_slice(ca_public_key);
    VerifyingKey::from_bytes(&pk_bytes)
        .map_err(|e| CaError::Verification(format!("Invalid CA public key: {}", e)))
}

/// The CA identifier a public key speaks for: `SHA-256(public key)`, raw bytes.
///
/// This is the same derivation [`crate::CaRoot::ca_id`] uses, and it is the whole
/// reason a CA public key is enough to verify its output: the identifier inside
/// the signed preimage is computable from the public half alone.
///
/// Hex is how a human reads this, not how it is stored or signed; see
/// [`crate::jsonbytes`] for the one place an encoding is applied.
pub(crate) fn issuer_id_of(verifying_key: &VerifyingKey) -> [u8; 32] {
    Sha256::digest(verifying_key.as_bytes()).into()
}

/// Build the RSL preimage: the exact bytes a CA signs over a revocation list.
///
/// Layout, all big-endian: a length-prefixed issuer id, `issued_at` and
/// `expires_at` as Unix seconds, the sequence number, an entry count, then per
/// revocation a length-prefixed node id, `revoked_at` in Unix seconds, the reason
/// code, and a length-prefixed signing public key. Every length-prefixed field is
/// the bytes of the thing itself — identifiers and keys go in as their raw 32
/// bytes, not as text.
///
/// *Changed 2026-10-07:* the issuer used to go in as 64 ASCII hex characters of
/// the same digest. Nothing had published a list under the old layout, so it
/// changed rather than being carried forward; what a signature covers should be
/// the identifier, not one tool's spelling of it. `ca/tests/fixtures/README.md`
/// records both layouts and how the current one was pinned.
///
/// The issuer arrives as bytes rather than from a key so a verifier can rebuild
/// it; see [`verify_rsl_signature`]. `notes` is absent on purpose — it is audit
/// prose, and signing it would let an edit to a comment invalidate a published
/// list without changing a single revocation.
pub(crate) fn build_rsl_payload(
    ca_id: &[u8],
    revocations: &[RevokedNode],
    issued_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    sequence_number: u64,
) -> Vec<u8> {
    let mut payload = Vec::new();

    // Issuer ID (length-prefixed)
    payload.extend_from_slice(&(ca_id.len() as u32).to_be_bytes());
    payload.extend_from_slice(ca_id);

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

    payload
}

/// Verify a Revocation Status List's signature with only the CA's public key.
///
/// [`crate::CaRoot::verify_rsl`] could only ever be called by the CA itself, since
/// it reached through the signing key for its verifying half. This is the same
/// check with the key a consumer is actually allowed to hold.
///
/// The issuer in the preimage comes from `ca_public_key`, never from
/// `rsl.issuer_id`: a list naming some other CA is refused rather than validated
/// against whichever key the caller happened to look up. That is the forgery half
/// of M14 — the honest-mislabelling half is already closed by
/// [`crate::CaRoot::sign_rsl`] refusing to sign for another id.
///
/// # Returns
///
/// * `Ok(false)` — the list names a different CA, or the signature does not match
///   these bytes
/// * `Err` — the key is not a usable 32-byte public key, or the signature is not
///   64 bytes
pub fn verify_rsl_signature(ca_public_key: &[u8], rsl: &RevocationStatusList) -> Result<bool> {
    use ed25519_dalek::Verifier;

    let verifying_key = parse_ca_public_key(ca_public_key)?;
    let ca_id = issuer_id_of(&verifying_key);

    if rsl.issuer_id.as_slice() != ca_id.as_slice() {
        return Ok(false);
    }

    let payload = build_rsl_payload(
        &ca_id,
        &rsl.revocations,
        rsl.issued_at,
        rsl.expires_at,
        rsl.sequence_number,
    );

    let signature = ed25519_dalek::Signature::try_from(rsl.signature.as_slice())
        .map_err(|_| CaError::InvalidCredential("RSL signature must be 64 bytes".to_string()))?;

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
        let is_valid =
            verify_credential_signature(ca_signing_key.verifying_key().as_bytes(), &credential)
                .unwrap();
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
        let signature =
            sign_credential(&ca_signing_key, &node_public_key, Utc::now(), None).unwrap();

        // Create credential with signature
        let mut credential =
            Credential::new(node_public_key.clone(), signature, Utc::now(), None, None).unwrap();

        // Tamper with the signature instead (to test signature verification failure)
        credential.ca_signature[0] ^= 1;

        // Verification should fail
        let is_valid =
            verify_credential_signature(ca_signing_key.verifying_key().as_bytes(), &credential)
                .unwrap();
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
