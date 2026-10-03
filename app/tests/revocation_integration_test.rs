//! Integration tests for revocation checking workflow.
//!
//! These tests verify the complete revocation workflow using only the public API.

use ca::{
    CaRoot, CheckContext, CheckLocation, ConnectionPolicy, DataRecordingPolicy, InMemoryRslChecker,
    InMemoryRslManager, RevocationChecker, RevocationPolicy, RevocationReason, RslManager,
};
use ed25519_dalek::{Signature, SigningKey, VerifyingKey};
use rand::thread_rng;
use sha2::{Digest, Sha256};

/// Compute node ID from verifying key (SHA-256 hash).
fn compute_node_id(verifying_key: &VerifyingKey) -> Vec<u8> {
    Sha256::digest(verifying_key.as_bytes()).to_vec()
}

#[tokio::test]
async fn test_revocation_workflow_full() {
    // Step 1: Setup CA root
    let ca_root = CaRoot::generate();
    let ca_id = ca_root.ca_id();

    // Step 2: Generate node identity
    let node_signing_key = SigningKey::generate(&mut thread_rng());
    let node_verifying_key = node_signing_key.verifying_key();
    let node_id = compute_node_id(&node_verifying_key);

    // Step 3: Issue credential for node
    let credential = ca_root
        .issue_credential(node_verifying_key.as_bytes(), Some(90))
        .expect("Should issue credential");

    assert!(ca_root.verify_credential(&credential).is_ok());

    // Step 4: Create test payload and sign it
    use ed25519_dalek::Signer;
    let payload_bytes = format!("occurrence:{}", hex::encode(&node_id)).into_bytes();
    let signature = node_signing_key.sign(&payload_bytes);

    // Verify signature works
    use ed25519_dalek::Verifier;
    let sig = Signature::from_bytes(&signature.to_bytes());
    assert!(node_verifying_key.verify(&payload_bytes, &sig).is_ok());

    // Step 5: Create in-memory RSL manager
    let rsl_manager = InMemoryRslManager::new();

    // Step 6: Generate RSL (no revocations yet)
    let rsl = rsl_manager.generate_rsl(&ca_id, 1).await.unwrap();
    assert_eq!(rsl.revocation_count(), 0);

    // Step 7: Verify node is not revoked
    assert!(!rsl.is_node_revoked(&node_id));

    // Step 8: Revoke the node
    let ca_credential_bytes = vec![0u8; 100];
    let revoked = rsl_manager
        .revoke_node(
            &node_id,
            RevocationReason::KeyCompromise as u8,
            node_verifying_key.as_bytes(),
            &ca_credential_bytes,
            Some("Test revocation for integration"),
        )
        .await
        .unwrap();

    assert_eq!(revoked.node_id, node_id);

    // Step 9: Generate new RSL with revocation
    let rsl = rsl_manager.generate_rsl(&ca_id, 1).await.unwrap();
    assert_eq!(rsl.revocation_count(), 1);
    assert!(rsl.is_node_revoked(&node_id));

    // Step 10: Sign the generated list. Signing covers the sequence number the
    // manager assigned — rebuilding a list from just the revocations would put
    // the anti-replay counter back to a value nothing published.
    let signed_rsl = ca_root
        .sign_rsl(rsl)
        .expect("Should sign the list the manager produced");
    assert_eq!(signed_rsl.sequence_number, 1);
    assert!(ca_root.verify_rsl(&signed_rsl).is_ok());

    // Step 11: Create checker from the signed list
    let checker = InMemoryRslChecker::from_rsl(signed_rsl.clone(), chrono::Duration::hours(24));
    let status = checker.is_revoked(&node_id).unwrap();
    assert_eq!(status, ca::RevocationStatus::Revoked);

    // Step 12: Store RSL, which is what spends sequence number 1
    rsl_manager.store_rsl(&signed_rsl).await.unwrap();
    assert_eq!(rsl_manager.next_sequence_number(&ca_id).await.unwrap(), 2);
}

#[tokio::test]
async fn test_revocation_workflow_multiple_nodes() {
    let rsl_manager = InMemoryRslManager::new();
    let ca_id = "test-multiple-nodes";

    // Create multiple nodes
    let mut nodes = vec![];
    for i in 0..10 {
        let signing_key = SigningKey::generate(&mut thread_rng());
        let verifying_key = signing_key.verifying_key();
        let node_id = compute_node_id(&verifying_key);

        nodes.push((signing_key, verifying_key, node_id));
    }

    // Revoke 5 of them
    for (i, (_, verifying_key, node_id)) in nodes.iter().enumerate() {
        if i < 5 {
            let _ = rsl_manager
                .revoke_node(
                    node_id,
                    RevocationReason::KeyCompromise as u8,
                    verifying_key.as_bytes(),
                    &vec![0u8; 100],
                    None,
                )
                .await;
        }
    }

    // Generate RSL
    let rsl = rsl_manager.generate_rsl(&ca_id, 1).await.unwrap();
    assert_eq!(rsl.revocation_count(), 5);

    // Verify each node status
    for (i, (_, verifying_key, node_id)) in nodes.iter().enumerate() {
        let checker = InMemoryRslChecker::from_rsl(rsl.clone(), chrono::Duration::hours(24));
        let status = checker.is_revoked(node_id).unwrap();

        if i < 5 {
            assert_eq!(
                status,
                ca::RevocationStatus::Revoked,
                "Node {} should be revoked",
                i
            );
        } else {
            assert_eq!(
                status,
                ca::RevocationStatus::Valid,
                "Node {} should be valid",
                i
            );
        }

        // Also verify using RSL directly
        assert_eq!(
            rsl.is_node_revoked(node_id),
            i < 5,
            "RSL should match expected revocation status for node {}",
            i
        );
    }
}

#[tokio::test]
async fn test_connection_policy_stricter_than_data_policy() {
    // Generate node identity
    let _node_verifying_key = SigningKey::generate(&mut thread_rng()).verifying_key();
    let node_id = compute_node_id(&_node_verifying_key);

    // A checker that was constructed and never given a list has no evidence
    // either way about this node, which is a different fact from "the CA says
    // it is not revoked" and must not be reported as one.
    let checker = InMemoryRslChecker::new(chrono::Duration::hours(24));

    let data_policy = DataRecordingPolicy::default();
    let connection_policy = ConnectionPolicy::default();

    let ctx = CheckContext::new(
        CheckLocation::OccurrenceVerification,
        checker.cache_age(),
        chrono::Duration::hours(24),
    );

    let status = checker.is_revoked(&node_id).unwrap();
    assert_eq!(status, ca::RevocationStatus::Unknown);

    let data_decision = data_policy.evaluate(&node_id, status, &ctx);
    let connection_decision = connection_policy.evaluate(&node_id, status, &ctx);

    // With no list at all there is no cache age within any bound, so even the
    // availability-leaning data policy declines to record rather than inventing
    // a status; the handshake policy refuses outright.
    assert_eq!(data_decision, ca::Decision::Defer);
    assert_eq!(connection_decision, ca::Decision::Reject);

    // The same node against a current list from the CA is genuinely Valid, and
    // both policies then accept it.
    let rsl_manager = InMemoryRslManager::new();
    let ca_id = "test-policy";
    let clean_rsl = rsl_manager.generate_rsl(&ca_id, 1).await.unwrap();
    let clean_checker = InMemoryRslChecker::from_rsl(clean_rsl, chrono::Duration::hours(24));

    let clean_ctx = CheckContext::new(
        CheckLocation::Handshake,
        clean_checker.cache_age(),
        chrono::Duration::hours(24),
    );
    let clean_status = clean_checker.is_revoked(&node_id).unwrap();
    assert_eq!(clean_status, ca::RevocationStatus::Valid);
    assert_eq!(
        data_policy.evaluate(&node_id, clean_status, &clean_ctx),
        ca::Decision::Accept
    );
    assert_eq!(
        connection_policy.evaluate(&node_id, clean_status, &clean_ctx),
        ca::Decision::Accept
    );

    // Now test with a revoked node
    let revoked_node_id = vec![99u8; 32];
    let revoked_signing_key = vec![100u8; 32];
    let rsl_manager = InMemoryRslManager::new();
    let ca_id = "test-policy";

    rsl_manager
        .revoke_node(
            &revoked_node_id,
            RevocationReason::KeyCompromise as u8,
            &revoked_signing_key,
            &vec![0u8; 100],
            None,
        )
        .await
        .unwrap();

    let rsl = rsl_manager.generate_rsl(&ca_id, 1).await.unwrap();
    let mut revoked_checker = InMemoryRslChecker::new(chrono::Duration::hours(24));
    revoked_checker.update_rsl(rsl).unwrap();

    let revoked_status = revoked_checker.is_revoked(&revoked_node_id).unwrap();
    let data_decision_revoked = data_policy.evaluate(&revoked_node_id, revoked_status, &ctx);
    let connection_decision_revoked =
        connection_policy.evaluate(&revoked_node_id, revoked_status, &ctx);

    // Both policies should reject revoked nodes
    assert_eq!(data_decision_revoked, ca::Decision::Reject);
    assert_eq!(connection_decision_revoked, ca::Decision::Reject);
}

#[tokio::test]
async fn test_rsl_sequence_number_increment() {
    let ca_root = CaRoot::generate();
    let rsl_manager = InMemoryRslManager::new();
    let ca_id = ca_root.ca_id();

    // 0 means "nothing published yet", so the first list is numbered 1.
    assert_eq!(rsl_manager.next_sequence_number(&ca_id).await.unwrap(), 1);

    let mut sequences = vec![];
    for _ in 0..3 {
        // The full publication: generate, sign the manager's own list, store it.
        let signed = ca_root
            .sign_rsl(rsl_manager.generate_rsl(&ca_id, 1).await.unwrap())
            .expect("signing should work");
        rsl_manager.store_rsl(&signed).await.unwrap();
        sequences.push(signed.sequence_number);
    }

    assert_eq!(sequences, vec![1, 2, 3]);
    for pair in sequences.windows(2) {
        assert!(
            pair[1] > pair[0],
            "sequence numbers must strictly increase: {:?} then {:?}",
            pair[0],
            pair[1]
        );
    }
}

#[tokio::test]
async fn test_revocation_reason_codes() {
    let rsl_manager = InMemoryRslManager::new();
    let ca_id = "test-reasons";

    // Revoke nodes with different reasons
    let reasons = vec![
        (RevocationReason::Unspecified as u8, "unspecified"),
        (RevocationReason::KeyCompromise as u8, "key_compromise"),
        (RevocationReason::CaCompromise as u8, "ca_compromise"),
        (RevocationReason::CeasedOperation as u8, "ceased_operation"),
        (RevocationReason::PolicyViolation as u8, "policy_violation"),
        (RevocationReason::Superseded as u8, "superseded"),
        (RevocationReason::Hold as u8, "hold"),
    ];

    for (reason, name) in &reasons {
        let node_id = vec![*reason; 32];
        let signing_key = vec![*reason + 10; 32];

        let _revoked = rsl_manager
            .revoke_node(
                &node_id,
                *reason,
                &signing_key,
                &vec![0u8; 100],
                Some(*name),
            )
            .await
            .unwrap();
    }

    // Generate RSL and verify all nodes are included
    let rsl = rsl_manager.generate_rsl(&ca_id, 1).await.unwrap();
    assert_eq!(rsl.revocation_count(), reasons.len());

    // Verify each node is marked as revoked
    for (reason, _name) in &reasons {
        let node_id = vec![*reason; 32];
        assert!(rsl.is_node_revoked(&node_id));
    }
}

#[tokio::test]
async fn test_ca_signs_and_verifies_rsl() {
    // Setup
    let ca_root = CaRoot::generate();
    let rsl_manager = InMemoryRslManager::new();
    let ca_id = ca_root.ca_id();

    // Create and revoke a node
    let node_signing_key = SigningKey::generate(&mut thread_rng());
    let node_verifying_key = node_signing_key.verifying_key();
    let node_id = compute_node_id(&node_verifying_key);

    rsl_manager
        .revoke_node(
            &node_id,
            RevocationReason::KeyCompromise as u8,
            node_verifying_key.as_bytes(),
            &vec![0u8; 100],
            None,
        )
        .await
        .unwrap();

    // Generate unsigned RSL
    let unsigned_rsl = rsl_manager.generate_rsl(&ca_id, 1).await.unwrap();
    let (issued_at, expires_at, sequence_number) = (
        unsigned_rsl.issued_at,
        unsigned_rsl.expires_at,
        unsigned_rsl.sequence_number,
    );

    // Sign RSL with CA
    let signed_rsl = ca_root.sign_rsl(unsigned_rsl).expect("Should sign RSL");

    // Verify RSL signature
    assert!(ca_root.verify_rsl(&signed_rsl).is_ok());

    // Verify RSL metadata
    assert_eq!(signed_rsl.issuer_id, ca_id);
    assert_eq!(signed_rsl.revocation_count(), 1);
    assert!(signed_rsl.is_valid_now());
    assert!(!signed_rsl.is_expired());

    // Signing must not quietly re-issue the list: the window and the sequence
    // the manager chose are what the signature covers.
    assert_eq!(signed_rsl.sequence_number, sequence_number);
    assert_eq!(signed_rsl.issued_at, issued_at);
    assert_eq!(signed_rsl.expires_at, expires_at);

    // Verify tampered RSL fails
    let mut tampered_rsl = signed_rsl.clone();
    tampered_rsl.issuer_id = "tampered".to_string();
    assert!(
        ca_root.verify_rsl(&tampered_rsl).is_err() || !ca_root.verify_rsl(&tampered_rsl).unwrap()
    );
}

#[tokio::test]
async fn test_in_memory_checker_cache_age() {
    // A checker with nothing loaded has no revocation data at all, so there is
    // no age to quote and nothing is fresh.
    let checker = InMemoryRslChecker::new(chrono::Duration::hours(24));
    assert!(!checker.is_fresh());

    // Create RSL and load it
    let node_id = vec![1u8; 32];
    let rsl_manager = InMemoryRslManager::new();
    let ca_id = "test-cache";

    rsl_manager
        .revoke_node(&node_id, 1, &vec![2u8; 32], &vec![0u8; 100], None)
        .await
        .unwrap();

    let rsl = rsl_manager.generate_rsl(&ca_id, 1).await.unwrap();
    let checker = InMemoryRslChecker::from_rsl(rsl, chrono::Duration::hours(24));

    // Check revocation
    assert_eq!(
        checker.is_revoked(&node_id).unwrap(),
        ca::RevocationStatus::Revoked
    );
    assert!(checker.is_fresh());
    assert!(checker.cache_age() < chrono::Duration::seconds(5));

    // Age is the age of the data, not of the load: a list the CA signed three
    // days ago is three days stale the instant somebody reads it into a checker,
    // which is the only reading under which a staleness bound can bite.
    let mut aged = rsl_manager.generate_rsl(&ca_id, 1).await.unwrap();
    aged.issued_at -= chrono::Duration::days(3);
    aged.expires_at = aged.issued_at + chrono::Duration::days(4);
    let aged_checker = InMemoryRslChecker::from_rsl(aged, chrono::Duration::hours(24));

    assert!(!aged_checker.is_fresh());
    assert!(aged_checker.cache_age() > chrono::Duration::days(2));
    // The revoked node is still revoked; only the absence of a name stops
    // meaning anything.
    assert_eq!(
        aged_checker.is_revoked(&node_id).unwrap(),
        ca::RevocationStatus::Revoked
    );
    assert_eq!(
        aged_checker.is_revoked(&[2u8; 32]).unwrap(),
        ca::RevocationStatus::Unknown
    );
}
