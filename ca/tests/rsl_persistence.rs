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
    verified_checker, CaError, CaRoot, DatabaseRslManager, RevocationChecker, RevocationStatusList,
    RslManager, TrustAnchor,
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
///
/// `revoked_by` is nullable: NULL is "recorded, but no signed list carrying it
/// has been published yet", which is a fact these tests assert on.
async fn revocation_row(pool: &sqlx::PgPool, node_id: &[u8]) -> Option<(Option<Vec<u8>>, u64)> {
    sqlx::query_as::<_, (Option<Vec<u8>>, i64)>(
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
        matches!(err, CaError::UnsignedRsl(ref issuer) if *issuer == ca.ca_id_hex()),
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
    assert_eq!(revoked_by, None, "an unpublished revocation has no issuer");
    assert_eq!(seq, 0);

    let first_list = publish(&ca, &manager, 7).await;
    let (revoked_by, seq) = revocation_row(&pool, &first_node).await.unwrap();
    assert_eq!(revoked_by, Some(ca.ca_id().to_vec()));
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

/// A CA that has published nothing has no revocation data to hand out, and the
/// loader says so instead of returning an empty checker. "Nothing published" and
/// "nobody is revoked" are different facts; conflating them is how an outage at
/// the CA becomes an all-clear for every node in the federation.
#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn a_checker_needs_a_list_to_exist_first(pool: sqlx::PgPool) {
    let ca = CaRoot::generate();
    let manager = DatabaseRslManager::new(pool.clone());
    let anchor = TrustAnchor::from_public_key(ca.public_key_slice()).unwrap();

    let err = verified_checker(&manager, &anchor, Duration::hours(24))
        .await
        .expect_err("there is no list to load");

    assert!(
        matches!(err, CaError::RslNotFound(ref id) if *id == ca.ca_id_hex()),
        "{err}"
    );
}

#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn a_checker_reads_the_published_list(pool: sqlx::PgPool) {
    let ca = CaRoot::generate();
    let manager = DatabaseRslManager::new(pool.clone());
    let anchor = TrustAnchor::from_public_key(ca.public_key_slice()).unwrap();

    let revoked = vec![0x11u8; 32];
    manager
        .revoke_node(&revoked, 1, &[0x22u8; 32], &[], None)
        .await
        .unwrap();
    publish(&ca, &manager, 7).await;

    let checker = verified_checker(&manager, &anchor, Duration::hours(24))
        .await
        .expect("a list this CA signed itself verifies");

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

/// M11. The published list is a JSON document in a column — the same row an
/// operator can `UPDATE`, a replica can replay, a bad migration can rewrite. The
/// signature is the only thing that makes it the CA's statement, so a reader that
/// skipped the check would answer `Valid` for precisely the nodes an edit deleted.
#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn a_list_edited_after_publication_never_becomes_a_checker(pool: sqlx::PgPool) {
    let ca = CaRoot::generate();
    let manager = DatabaseRslManager::new(pool.clone());
    let anchor = TrustAnchor::from_public_key(ca.public_key_slice()).unwrap();

    let revoked = vec![0x11u8; 32];
    manager
        .revoke_node(&revoked, 1, &[0x22u8; 32], &[], None)
        .await
        .unwrap();
    let published = publish(&ca, &manager, 7).await;
    assert_eq!(published.revocation_count(), 1);

    // The edit worth defending against: drop the entries, keep the row and its
    // signature. `revocation_count` has to move with them, because the table's
    // CHECK ties the column to the array.
    sqlx::query(
        "UPDATE revocation_status_lists
            SET rsl = jsonb_set(rsl, '{revocations}', '[]'::jsonb),
                revocation_count = 0
          WHERE issuer_id = $1 AND sequence_number = $2",
    )
    .bind(ca.ca_id())
    .bind(i64::try_from(published.sequence_number).unwrap())
    .execute(&pool)
    .await
    .unwrap();

    // Proof that the tamper took, and that an unverified read would have swallowed
    // it: the document now says nobody is revoked.
    let edited = manager
        .get_latest_rsl(&ca.ca_id())
        .await
        .unwrap()
        .expect("the row is still there");
    assert_eq!(edited.revocation_count(), 0);
    assert!(!edited.is_node_revoked(&revoked));

    let err = verified_checker(&manager, &anchor, Duration::hours(24))
        .await
        .expect_err("the signature no longer covers what the document claims");
    assert!(matches!(err, CaError::Verification(_)), "{err}");
}

/// The row can also hold somebody *else's* honest document — copied in, or the
/// wrong anchor configured. `issuer_id` in the column is a lookup key, not proof,
/// so the issuer inside the document is compared against the key, and a list
/// signed by CA B never passes under CA A's anchor.
#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn a_list_signed_by_another_ca_is_refused_under_this_anchor(pool: sqlx::PgPool) {
    let ca_a = CaRoot::generate();
    let ca_b = CaRoot::generate();
    let manager = DatabaseRslManager::new(pool.clone());
    let anchor_a = TrustAnchor::from_public_key(ca_a.public_key_slice()).unwrap();

    publish(&ca_a, &manager, 7).await;
    let b_list = publish(&ca_b, &manager, 7).await;

    sqlx::query(
        "UPDATE revocation_status_lists
            SET rsl = $1
          WHERE issuer_id = $2 AND sequence_number = 1",
    )
    .bind(sqlx::types::Json(&b_list))
    .bind(ca_a.ca_id())
    .execute(&pool)
    .await
    .unwrap();

    let err = verified_checker(&manager, &anchor_a, Duration::hours(24))
        .await
        .expect_err("B's signature is not A's");
    assert!(matches!(err, CaError::Verification(_)), "{err}");
}

/// An expired list is a statement about a window that has closed. Its silence
/// about a node is not "not revoked", so the loader refuses it instead of
/// answering `Valid` from data that has stopped meaning anything.
#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn an_expired_list_is_refused_not_read_as_an_all_clear(pool: sqlx::PgPool) {
    let ca = CaRoot::generate();
    let manager = DatabaseRslManager::new(pool.clone());
    let anchor = TrustAnchor::from_public_key(ca.public_key_slice()).unwrap();

    let revoked = vec![0x11u8; 32];
    manager
        .revoke_node(&revoked, 1, &[0x22u8; 32], &[], None)
        .await
        .unwrap();

    // Backdate before signing, so this is a genuinely expired list the CA really
    // published — not a forged one, which the tests above already cover.
    let mut stale = manager.generate_rsl(&ca.ca_id(), 7).await.unwrap();
    stale.issued_at -= Duration::days(30);
    stale.expires_at = stale.issued_at + Duration::days(7);
    let signed = ca.sign_rsl(stale).expect("signing the backdated window");
    manager.store_rsl(&signed).await.unwrap();

    let err = verified_checker(&manager, &anchor, Duration::hours(24))
        .await
        .expect_err("the window closed three weeks ago");
    assert!(matches!(err, CaError::RslExpired(_)), "{err}");
}
