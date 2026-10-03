//! Signature verification with revocation checking.
//!
//! This module provides functions for verifying occurrence signatures
//! while also checking the origin node's revocation status.
//!
//! # Example
//!
//! ```ignore
//! use app::provenance::verify_with_revocation::{
//!     verify_occurrence_with_revocation,
//!     VerificationContext,
//! };
//! use ca::{RevocationStatus, InMemoryRslChecker, DataRecordingPolicy, CheckContext, CheckLocation, Decision};
//!
//! // Setup revocation checker from the CA's latest published list. A checker
//! // built with `InMemoryRslChecker::new` holds no list at all, answers
//! // `Unknown` for every node, and DataRecordingPolicy defers rather than
//! // accepts — which is the difference between "not revoked" and "never asked".
//! let rsl_checker = rsl_manager
//!     .get_checker(&ca_id, chrono::Duration::hours(24))
//!     .await?;
//! let policy = DataRecordingPolicy::default();
//!
//! // Verify occurrence with revocation check
//! let context = VerificationContext {
//!     origin_node_id: &occurrence.origin_node_id,
//!     signing_public_key: &node.signing_public_key,
//!     signature: &occurrence.signature,
//!     signed_payload: &occurrence.signed_payload,
//! };
//!
//! match verify_occurrence_with_revocation(&context, &rsl_checker, &policy).await {
//!     Ok(()) => println!("Occurrence is authentic and node is valid"),
//!     Err(e) => println!("Verification failed: {}", e),
//! }
//! ```

use ca::{
    CheckContext, CheckLocation, DataRecordingPolicy, Decision, InMemoryRslChecker,
    RevocationChecker, RevocationPolicy,
};
use ed25519_dalek::{Signature, VerifyingKey};
use log::{debug, warn};

use super::verify::{verify_signature, VerifyError};

/// Error type for verification with revocation.
#[derive(Debug, Clone, PartialEq)]
pub enum RevocationVerificationError {
    /// Signature verification failed.
    SignatureFailed(String),

    /// Node has been revoked.
    NodeRevoked(String),

    /// Revocation status unknown (cache stale or error).
    RevocationUnknown(String),

    /// Invalid public key format.
    InvalidPublicKey(String),

    /// Policy rejection.
    PolicyRejected(String),
}

impl std::fmt::Display for RevocationVerificationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RevocationVerificationError::SignatureFailed(msg) => {
                write!(f, "Signature verification failed: {}", msg)
            }
            RevocationVerificationError::NodeRevoked(msg) => {
                write!(f, "Node has been revoked: {}", msg)
            }
            RevocationVerificationError::RevocationUnknown(msg) => {
                write!(f, "Revocation status unknown: {}", msg)
            }
            RevocationVerificationError::InvalidPublicKey(msg) => {
                write!(f, "Invalid public key: {}", msg)
            }
            RevocationVerificationError::PolicyRejected(msg) => {
                write!(f, "Policy rejection: {}", msg)
            }
        }
    }
}

impl std::error::Error for RevocationVerificationError {}

impl From<VerifyError> for RevocationVerificationError {
    fn from(err: VerifyError) -> Self {
        RevocationVerificationError::SignatureFailed(err.to_string())
    }
}

/// Result type for verification with revocation.
pub type RevResult<T> = std::result::Result<T, RevocationVerificationError>;

/// Context for occurrence verification.
pub struct VerificationContext<'a> {
    /// The origin node ID (SHA-256 of signing public key).
    pub origin_node_id: &'a [u8],

    /// The node's signing public key.
    pub signing_public_key: &'a [u8],

    /// The signature to verify.
    pub signature: &'a [u8],

    /// The signed payload bytes.
    pub signed_payload: &'a [u8],
}

/// Verify an occurrence's signature and check revocation status.
///
/// This function performs two checks:
/// 1. Verifies the Ed25519 signature on the occurrence
/// 2. Checks if the origin node has been revoked
///
/// # Arguments
///
/// * `context` - Verification context with payload and keys
/// * `revocation_checker` - Checker for node revocation status
/// * `policy` - Policy for handling uncertain revocation status
///
/// # Returns
///
/// * `Ok(())` - Signature is valid and node is not revoked
/// * `Err(RevocationVerificationError)` - If verification or revocation check failed
///
/// # Example
///
/// ```ignore
/// let context = VerificationContext {
///     origin_node_id: &occurrence.origin_node_id,
///     signing_public_key: &node.signing_public_key,
///     signature: &occurrence.signature,
///     signed_payload: &occurrence.signed_payload,
/// };
///
/// let checker = rsl_manager
///     .get_checker(&ca_id, chrono::Duration::hours(24))
///     .await?;
/// let policy = DataRecordingPolicy::default();
///
/// verify_occurrence_with_revocation(&context, &checker, &policy).await?;
/// ```
pub async fn verify_occurrence_with_revocation<P>(
    context: &VerificationContext<'_>,
    revocation_checker: &InMemoryRslChecker,
    policy: &P,
) -> RevResult<()>
where
    P: RevocationPolicy,
{
    // Step 1: Verify the signature
    let verifying_key_bytes: &[u8; 32] = context.signing_public_key.try_into().map_err(|_| {
        RevocationVerificationError::InvalidPublicKey(format!(
            "Expected 32-byte public key, got {}",
            context.signing_public_key.len()
        ))
    })?;
    let verifying_key = VerifyingKey::from_bytes(verifying_key_bytes)
        .map_err(|e| RevocationVerificationError::InvalidPublicKey(e.to_string()))?;

    let sig_bytes: &[u8; 64] = context.signature.try_into().map_err(|_| {
        RevocationVerificationError::InvalidPublicKey(format!(
            "Expected 64-byte signature, got {}",
            context.signature.len()
        ))
    })?;
    let signature = Signature::from_bytes(sig_bytes);

    verify_signature(&verifying_key, context.signed_payload, &signature)?;

    debug!(
        "Signature verified for node {}",
        hex::encode(context.origin_node_id)
    );

    // Step 2: Check revocation status
    let cache_age = revocation_checker.cache_age();
    // Use a default max staleness - the policy's evaluate method handles freshness
    let max_staleness = chrono::Duration::hours(24);
    let check_context = CheckContext::new(
        CheckLocation::OccurrenceVerification,
        cache_age,
        max_staleness,
    );

    let revocation_status = revocation_checker
        .is_revoked(context.origin_node_id)
        .map_err(|e| RevocationVerificationError::RevocationUnknown(e.to_string()))?;

    // Step 3: Apply policy
    let decision = policy.evaluate(context.origin_node_id, revocation_status, &check_context);

    match decision {
        Decision::Accept => {
            debug!(
                "Node {} accepted for data recording",
                hex::encode(context.origin_node_id)
            );
            Ok(())
        }
        Decision::AcceptWithWarning => {
            warn!(
                "Node {} accepted with warning (revocation status: {:?})",
                hex::encode(context.origin_node_id),
                revocation_status
            );
            Ok(())
        }
        Decision::Reject => {
            warn!(
                "Node {} rejected by policy (revocation status: {:?})",
                hex::encode(context.origin_node_id),
                revocation_status
            );
            Err(RevocationVerificationError::PolicyRejected(format!(
                "Node {:?} rejected",
                hex::encode(context.origin_node_id)
            )))
        }
        Decision::Defer => Err(RevocationVerificationError::RevocationUnknown(format!(
            "Need fresh revocation data for node {}",
            hex::encode(context.origin_node_id)
        ))),
    }
}

/// Verify an occurrence's signature only (without revocation check).
///
/// This is useful for debugging or when revocation checking is not needed.
///
/// # Arguments
///
/// * `signing_public_key` - The node's signing public key
/// * `signed_payload` - The bytes that were signed
/// * `signature` - The signature to verify
///
/// # Returns
///
/// * `Ok(())` - Signature is valid
/// * `Err(RevocationVerificationError)` - If verification failed
pub fn verify_signature_only(
    signing_public_key: &[u8],
    signed_payload: &[u8],
    signature: &[u8],
) -> RevResult<()> {
    let verifying_key_bytes: &[u8; 32] = signing_public_key.try_into().map_err(|_| {
        RevocationVerificationError::InvalidPublicKey(format!(
            "Expected 32-byte public key, got {}",
            signing_public_key.len()
        ))
    })?;
    let verifying_key = VerifyingKey::from_bytes(verifying_key_bytes)
        .map_err(|e| RevocationVerificationError::InvalidPublicKey(e.to_string()))?;

    let sig_bytes: &[u8; 64] = signature.try_into().map_err(|_| {
        RevocationVerificationError::InvalidPublicKey(format!(
            "Expected 64-byte signature, got {}",
            signature.len()
        ))
    })?;
    let signature = Signature::from_bytes(sig_bytes);

    verify_signature(&verifying_key, signed_payload, &signature)
        .map_err(|e| RevocationVerificationError::SignatureFailed(e.to_string()))
}

/// Check if a node should be accepted for data recording.
///
/// This function checks only the revocation status without verifying signatures.
///
/// # Arguments
///
/// * `node_id` - The node ID to check
/// * `revocation_checker` - Checker for node revocation status
/// * `policy` - Policy for handling uncertain revocation status
///
/// # Returns
///
/// * `Ok(true)` - Node should be accepted
/// * `Ok(false)` - Node should be rejected
/// * `Err(RevocationVerificationError)` - If check failed
pub fn should_record_data<P>(
    node_id: &[u8],
    revocation_checker: &InMemoryRslChecker,
    policy: &P,
) -> RevResult<bool>
where
    P: RevocationPolicy,
{
    let cache_age = revocation_checker.cache_age();
    let max_staleness = chrono::Duration::hours(24);
    let check_context = CheckContext::new(
        CheckLocation::OccurrenceVerification,
        cache_age,
        max_staleness,
    );

    let revocation_status = revocation_checker
        .is_revoked(node_id)
        .map_err(|e| RevocationVerificationError::RevocationUnknown(e.to_string()))?;

    let decision = policy.evaluate(node_id, revocation_status, &check_context);

    match decision {
        Decision::Accept | Decision::AcceptWithWarning => Ok(true),
        Decision::Reject | Decision::Defer => Ok(false),
    }
}

/// Check if a node should be allowed to connect (P2P handshake).
///
/// This uses a stricter policy than data recording - it rejects nodes
/// with unknown or revoked status.
///
/// # Arguments
///
/// * `node_id` - The node ID to check
/// * `revocation_checker` - Checker for node revocation status
///
/// # Returns
///
/// * `Ok(true)` - Node should be allowed to connect
/// * `Ok(false)` - Node should be rejected
/// * `Err(RevocationVerificationError)` - If check failed
pub fn should_allow_connection(
    node_id: &[u8],
    revocation_checker: &InMemoryRslChecker,
) -> RevResult<bool> {
    use ca::ConnectionPolicy;
    let policy = ConnectionPolicy::default();

    let cache_age = revocation_checker.cache_age();
    let check_context = CheckContext::new(
        CheckLocation::Handshake,
        cache_age,
        chrono::Duration::hours(1),
    );

    let revocation_status = revocation_checker
        .is_revoked(node_id)
        .map_err(|e| RevocationVerificationError::RevocationUnknown(e.to_string()))?;

    let decision = policy.evaluate(node_id, revocation_status, &check_context);

    Ok(decision == Decision::Accept)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ca::{
        DataRecordingPolicy, InMemoryRslChecker, RevocationReason, RevocationStatusList,
        RevokedNode,
    };
    use chrono::Utc;
    use sha2::Digest;

    /// A checker holding a current list from the CA that revokes nobody.
    ///
    /// Distinct from `InMemoryRslChecker::new`, which holds no list at all:
    /// "the CA published a list and this node is not on it" is evidence, "we
    /// never loaded one" is not, and the checker reports them differently.
    fn current_empty_checker() -> InMemoryRslChecker {
        let rsl = RevocationStatusList::builder("test-ca")
            .sequence_number(1)
            .validity_days(1)
            .build_unsigned();

        InMemoryRslChecker::from_rsl(rsl, chrono::Duration::hours(24))
    }

    #[tokio::test]
    async fn test_verify_with_valid_node() {
        use ed25519_dalek::{ed25519::signature::Signer, SigningKey};
        use rand::thread_rng;

        // Generate keys
        let signing_key = SigningKey::generate(&mut thread_rng());
        let verifying_key = signing_key.verifying_key();
        let node_id = sha2::Sha256::digest(verifying_key.as_bytes()).to_vec();

        // Create and sign payload
        let payload = b"test payload";
        let signature = signing_key.sign(payload);
        let signature_bytes = signature.to_bytes();

        // Create verification context
        let context = VerificationContext {
            origin_node_id: &node_id,
            signing_public_key: verifying_key.as_bytes(),
            signature: &signature_bytes,
            signed_payload: payload,
        };

        // A current list from the CA that does not name this node
        let checker = current_empty_checker();
        let policy = DataRecordingPolicy::default();

        // Verify
        let result = verify_occurrence_with_revocation(&context, &checker, &policy).await;
        assert!(result.is_ok());
    }

    // No list loaded is not an all-clear. Recording data from a node nobody has
    // checked against the CA's list is what the DataRecordingPolicy's Defer
    // branch exists for, and it used to be unreachable because the checker
    // answered `Valid` whether or not it held anything.
    #[tokio::test]
    async fn test_verify_defers_when_no_list_has_been_loaded() {
        use ed25519_dalek::{ed25519::signature::Signer, SigningKey};
        use rand::thread_rng;

        let signing_key = SigningKey::generate(&mut thread_rng());
        let verifying_key = signing_key.verifying_key();
        let node_id = sha2::Sha256::digest(verifying_key.as_bytes()).to_vec();

        let payload = b"test payload";
        let signature = signing_key.sign(payload);
        let signature_bytes = signature.to_bytes();

        let context = VerificationContext {
            origin_node_id: &node_id,
            signing_public_key: verifying_key.as_bytes(),
            signature: &signature_bytes,
            signed_payload: payload,
        };

        let checker = InMemoryRslChecker::new(chrono::Duration::hours(24));
        let policy = DataRecordingPolicy::default();

        let result = verify_occurrence_with_revocation(&context, &checker, &policy).await;
        assert!(
            matches!(
                result,
                Err(RevocationVerificationError::RevocationUnknown(_))
            ),
            "a signature this node can prove is not a reason to store data from a \
             node the CA has never been asked about: got {result:?}"
        );
    }

    #[tokio::test]
    async fn test_verify_with_revoked_node() {
        use ed25519_dalek::{ed25519::signature::Signer, SigningKey};
        use rand::thread_rng;

        // Generate keys
        let signing_key = SigningKey::generate(&mut thread_rng());
        let verifying_key = signing_key.verifying_key();
        let node_id = sha2::Sha256::digest(verifying_key.as_bytes()).to_vec();

        // Create and sign payload
        let payload = b"test payload";
        let signature = signing_key.sign(payload);
        let signature_bytes = signature.to_bytes();

        // Create verification context
        let context = VerificationContext {
            origin_node_id: &node_id,
            signing_public_key: verifying_key.as_bytes(),
            signature: &signature_bytes,
            signed_payload: payload,
        };

        // Create checker with revoked node
        let revoked_node = RevokedNode::new(
            node_id.clone(),
            Utc::now(),
            RevocationReason::KeyCompromise,
            verifying_key.as_bytes().to_vec(),
        );

        let rsl = RevocationStatusList::builder("test-ca")
            .add_revocation(revoked_node)
            .build_unsigned();

        let checker = InMemoryRslChecker::from_rsl(rsl, chrono::Duration::hours(24));
        let policy = DataRecordingPolicy::default();

        // Verify should fail
        let result = verify_occurrence_with_revocation(&context, &checker, &policy).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            RevocationVerificationError::PolicyRejected(_)
        ));
    }

    #[tokio::test]
    async fn test_verify_with_invalid_signature() {
        use ed25519_dalek::{ed25519::signature::Signer, SigningKey};
        use rand::thread_rng;

        // Generate keys
        let signing_key = SigningKey::generate(&mut thread_rng());
        let verifying_key = signing_key.verifying_key();
        let node_id = sha2::Sha256::digest(verifying_key.as_bytes()).to_vec();

        // Create payload with wrong signature
        let payload = b"test payload";
        let wrong_signing_key = SigningKey::generate(&mut thread_rng());
        let signature = wrong_signing_key.sign(payload);
        let signature_bytes = signature.to_bytes();

        // Create verification context
        let context = VerificationContext {
            origin_node_id: &node_id,
            signing_public_key: verifying_key.as_bytes(),
            signature: &signature_bytes,
            signed_payload: payload,
        };

        // Create checker with no revocations
        let checker = InMemoryRslChecker::new(chrono::Duration::hours(24));
        let policy = DataRecordingPolicy::default();

        // Verify should fail
        let result = verify_occurrence_with_revocation(&context, &checker, &policy).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            RevocationVerificationError::SignatureFailed(_)
        ));
    }

    #[tokio::test]
    async fn test_should_record_data() {
        let policy = DataRecordingPolicy::default();

        // Should record a node absent from the CA's current list
        let result = should_record_data(&[1u8; 32], &current_empty_checker(), &policy).unwrap();
        assert!(result);

        // Should not record one the checker has no list to judge against.
        let unloaded = InMemoryRslChecker::new(chrono::Duration::hours(24));
        assert!(!should_record_data(&[1u8; 32], &unloaded, &policy).unwrap());
    }

    #[tokio::test]
    async fn test_should_allow_connection() {
        // Should allow a node absent from the CA's current list
        let result = should_allow_connection(&[1u8; 32], &current_empty_checker()).unwrap();
        assert!(result);

        // The handshake policy is the strict one: with nothing loaded it refuses.
        let unloaded = InMemoryRslChecker::new(chrono::Duration::hours(24));
        assert!(!should_allow_connection(&[1u8; 32], &unloaded).unwrap());
    }
}
