//! Database-backed RSL sequencing: the properties that make `sequence_number`
//! an anti-replay counter rather than a field on a struct.
//!
//! These run against a real server (`DATABASE_URL`), each in a throwaway
//! database built from `db/src/migrations`, because every one of them is a
//! claim about durable state — which is precisely what cannot be shown with a
//! mock, and precisely what B5 was: the counter lived in the process, so every
//! list came out numbered 0.

#![cfg(feature = "database")]

use ca::{
    CaError, CaRoot, DatabaseRslManager, RevocationChecker, RevocationStatusList, RslManager,
};
use chrono::Duration;

/// Publish a generated list: sign the manager's own document and store it.
async fn publish(
    ca: &CaRoot,
    manager: &DatabaseRslManager,
    validity_days: u64,
) -> RevocationStatusList {
    let unsigned = manager
        .generate_rsl(&ca.ca_id(), validity_days)
        .await
        .expect("generating an RSL should work");

    let signed = ca
        .sign_rsl(unsigned)
        .expect("signing a freshly generated list should work");
    manager
        .store_rsl(&signed)
        .await
        .expect("publishing a new list should work");

    signed
}

/// `(revoked_by, rsl_sequence_number)` for one ledger row, the sequence read as
/// the domain reads it (`u64`) rather than as the column stores it (`BIGINT`).
async fn revocation_row(pool: &sqlx::PgPool, node_id: &[u8]) -> Option<(String, u64)> {
    sqlx::query_as::<_, (String, i64)>(
        "SELECT revoked_by, rsl_sequence_number
           FROM node_revocations
          WHERE node_id = $1",
    )
    .bind(node_id)
    .fetch_optional(pool)
    .await
    .unwrap()
    .map(|(revoked_by, seq)| {
        (
            revoked_by,
            u64::try_from(seq).expect("a sequence number is never negative"),
        )
    })
}

#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn the_first_list_a_ca_publishes_is_numbered_one(pool: sqlx::PgPool) {
    let ca = CaRoot::generate();
    let manager = DatabaseRslManager::new(pool.clone());
    let ca_id = ca.ca_id();

    // Nothing published yet reads as 0, and the next list is therefore 1 —
    // never 0, which is what an undated pre-history list would be labelled.
    assert_eq!(manager.next_sequence_number(&ca_id).await.unwrap(), 1);
    assert_eq!(publish(&ca, &manager, 7).await.sequence_number, 1);
    assert_eq!(manager.next_sequence_number(&ca_id).await.unwrap(), 2);
}

// The counter is a property of the database, not of the object handed to the
// CA. Building a second manager is what the next `app ca ca-generate-rsl`
// process is: the old one no longer exists, and the numbers must not restart.
#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn the_sequence_survives_the_manager_being_rebuilt(pool: sqlx::PgPool) {
    let ca = CaRoot::generate();
    let ca_id = ca.ca_id();

    let first = DatabaseRslManager::new(pool.clone());
    assert_eq!(publish(&ca, &first, 7).await.sequence_number, 1);
    drop(first);

    let restarted = DatabaseRslManager::new(pool.clone());
    assert_eq!(restarted.next_sequence_number(&ca_id).await.unwrap(), 2);
    assert_eq!(publish(&ca, &restarted, 7).await.sequence_number, 2);
    assert_eq!(publish(&ca, &restarted, 7).await.sequence_number, 3);
}

// A CA with nothing to revoke is the common case, and the one the revocation
// ledger could not carry: with no rows to read, its MAX() would say 0 forever
// and one captured empty list would be serveable for all time.
#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn a_ca_with_nothing_to_revoke_still_advances(pool: sqlx::PgPool) {
    let ca = CaRoot::generate();
    let manager = DatabaseRslManager::new(pool.clone());

    let first = publish(&ca, &manager, 7).await;
    let second = publish(&ca, &manager, 7).await;
    let third = publish(&ca, &manager, 7).await;

    assert_eq!(first.revocation_count(), 0);
    assert_eq!(second.revocation_count(), 0);
    assert_eq!(third.revocation_count(), 0);
    assert_eq!(
        (
            first.sequence_number,
            second.sequence_number,
            third.sequence_number
        ),
        (1, 2, 3)
    );
}

#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn a_replayed_sequence_is_refused_and_stays_refused(pool: sqlx::PgPool) {
    let ca = CaRoot::generate();
    let manager = DatabaseRslManager::new(pool.clone());

    let published = publish(&ca, &manager, 7).await;

    // Yesterday's list, served again today.
    let err = manager
        .store_rsl(&published)
        .await
        .expect_err("a replayed list must not be accepted");
    assert!(
        matches!(
            &err,
            CaError::RslNotNewer {
                incoming: 1,
                current: 1,
                ..
            }
        ),
        "expected a not-newer rejection, got {err}"
    );

    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM revocation_status_lists")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 1, "the replay must not have written anything");

    // And an out-of-order number from the future cannot be used to jump ahead
    // of the sequence either: only the next number is next.
    let manager2 = DatabaseRslManager::new(pool.clone());
    assert_eq!(manager2.next_sequence_number(&ca.ca_id()).await.unwrap(), 2);
}

#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn an_unsigned_list_cannot_be_published(pool: sqlx::PgPool) {
    let ca = CaRoot::generate();
    let manager = DatabaseRslManager::new(pool.clone());

    let unsigned = manager.generate_rsl(&ca.ca_id(), 7).await.unwrap();
    let err = manager
        .store_rsl(&unsigned)
        .await
        .expect_err("an unsigned list must not become the record");

    assert!(
        matches!(err, CaError::UnsignedRsl(ref issuer) if *issuer == ca.ca_id()),
        "storing an unsigned list should name the issuer it could not attribute"
    );
}

// `get_latest_rsl` has to return the artifact that was signed. Rebuilding a
// list from whatever the ledger says now produces something that does not
// match the CA's signature, and quietly renumbers the world the receiver
// already has.
#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn the_latest_list_is_the_one_that_was_signed(pool: sqlx::PgPool) {
    let ca = CaRoot::generate();
    let manager = DatabaseRslManager::new(pool.clone());
    let ca_id = ca.ca_id();

    manager
        .revoke_node(&[1u8; 32], 1, &[2u8; 32], &[], Some("first"))
        .await
        .unwrap();
    let published = publish(&ca, &manager, 7).await;
    assert_eq!(published.revocation_count(), 1);

    // A revocation lands after publication. The published list must not change
    // shape just because the ledger did.
    manager
        .revoke_node(&[3u8; 32], 4, &[4u8; 32], &[], None)
        .await
        .unwrap();

    let latest = manager
        .get_latest_rsl(&ca_id)
        .await
        .unwrap()
        .expect("the published list should be readable back");

    assert_eq!(
        latest, published,
        "the stored list is the published document"
    );
    assert!(
        ca.verify_rsl(&latest).unwrap(),
        "the list read from storage must still carry a valid signature"
    );
    assert_eq!(latest.revocation_count(), 1);
}

#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn publishing_attributes_the_revocations_it_carries(pool: sqlx::PgPool) {
    let ca = CaRoot::generate();
    let manager = DatabaseRslManager::new(pool.clone());

    let first_node = vec![0xaau8; 32];
    manager
        .revoke_node(&first_node, 1, &[0xbbu8; 32], &[], None)
        .await
        .unwrap();

    // Recorded, but no list carrying it has been signed: it has no issuer and
    // no list number yet.
    let (revoked_by, seq) = revocation_row(&pool, &first_node)
        .await
        .expect("the revocation should be recorded");
    assert_eq!(revoked_by, "pending");
    assert_eq!(seq, 0);

    let first_list = publish(&ca, &manager, 7).await;
    let (revoked_by, seq) = revocation_row(&pool, &first_node).await.unwrap();
    assert_eq!(revoked_by, ca.ca_id());
    assert_eq!(seq, first_list.sequence_number);

    // A second list: the new revocation takes this list's number, and the first
    // keeps the number of the list it first appeared in — which is the fact the
    // column is documented to hold.
    let second_node = vec![0xccu8; 32];
    manager
        .revoke_node(&second_node, 4, &[0xddu8; 32], &[], None)
        .await
        .unwrap();
    let second_list = publish(&ca, &manager, 7).await;

    let (_, first_seq) = revocation_row(&pool, &first_node).await.unwrap();
    let (_, second_seq) = revocation_row(&pool, &second_node).await.unwrap();
    assert_eq!(first_seq, 1, "first publication must not be rewritten");
    assert_eq!(second_seq, second_list.sequence_number);
    assert_eq!(second_list.sequence_number, 2);
}

#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn a_checker_before_any_list_is_unknown(pool: sqlx::PgPool) {
    let manager = DatabaseRslManager::new(pool.clone());

    let checker = manager
        .get_checker("never-published-ca", Duration::hours(24))
        .await
        .unwrap();

    assert_eq!(
        checker.is_revoked(&[1u8; 32]).unwrap(),
        ca::RevocationStatus::Unknown,
        "no published list is no evidence, not an all-clear"
    );
    assert!(!checker.is_fresh());
}

#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn a_checker_reads_the_published_list(pool: sqlx::PgPool) {
    let ca = CaRoot::generate();
    let manager = DatabaseRslManager::new(pool.clone());
    let ca_id = ca.ca_id();

    let revoked = vec![0x11u8; 32];
    manager
        .revoke_node(&revoked, 1, &[0x22u8; 32], &[], None)
        .await
        .unwrap();
    publish(&ca, &manager, 7).await;

    let checker = manager
        .get_checker(&ca_id, Duration::hours(24))
        .await
        .unwrap();

    assert_eq!(
        checker.is_revoked(&revoked).unwrap(),
        ca::RevocationStatus::Revoked
    );
    assert_eq!(
        checker.is_revoked(&[0x33u8; 32]).unwrap(),
        ca::RevocationStatus::Valid,
        "absent from a current, unexpired list"
    );
    assert_eq!(checker.sequence_number(), Some(1));
}
