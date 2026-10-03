//! The derived device-identity tables, and the invariants the reprocessing batch leans on.
//!
//! These four tables are written by a batch that can be run again at any time, so almost
//! everything pinned here is about *convergence*: a second pass over the same data must
//! find the identity the first pass created rather than mint a new one, a rotating
//! identifier must be able to gain a second identity (a split) without losing the first,
//! and an unordered pair must be stored once no matter which way round a caller names it.
//!
//! One test ([`the_declaration_order_of_the_method_enum_is_the_reliability_order`]) looks
//! unusual — it reads `pg_enum`. It exists because `link_address` promotes a link with
//! `LEAST(method)`, which is only "keep the more reliable justification" because the enum
//! is declared strongest-first. That is a fact about a catalog, and no Rust test can see
//! it drift.
//!
//! Run with a server available:
//!
//! ```text
//! DATABASE_URL=postgres://postgres:devpassword@database:5432/travel cargo test -p repo --test device_identity
//! ```

use chrono::{DateTime, TimeZone, Utc};
use repo::models::enums::IdentityResolutionMethod;
use repo::models::{
    canonical_pair, BleAddressType, CoOccurrenceEvent, DeviceAddressLink, DeviceIdentity, NodeType,
};
use repo::repositories::association_repo::strength_for;
use repo::repositories::{
    AssociationRepository, CoOccurrenceRepository, DeviceIdentityRepository, NodeRepository,
};
use repo::{H3Index, RepoError};
use uuid::Uuid;

/// Statue of Liberty: the coordinate every `geo` fixture starts from.
const LAT: f64 = 40.6892;
const LON: f64 = -74.0445;

fn ts(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(secs, 0).unwrap()
}

fn node_id(seed: u8) -> Vec<u8> {
    vec![seed; 32]
}

fn fingerprint(name: &str) -> serde_json::Value {
    serde_json::json!({
        "manufacturer_id": 76,
        "service_uuids_hash": "5feceb66",
        "field_layout_hash": "d4735e3a",
        "name_pattern": name,
    })
}

/// A registered node, because both link tables carry a foreign key to `nodes`.
async fn registered_node(pool: &sqlx::PgPool, seed: u8) -> Vec<u8> {
    let id = node_id(seed);
    NodeRepository::register(
        pool,
        &id,
        NodeType::Full,
        &node_id(seed ^ 0xff),
        b"ca-credential",
        Some((LAT, LON)),
        &[repo::geo::macro_cell(LAT, LON).unwrap()],
    )
    .await
    .expect("node should register");
    id
}

/// A fingerprint hash distinguishable by its first byte.
fn hash(seed: u8) -> Vec<u8> {
    let mut bytes = vec![0u8; 32];
    bytes[0] = seed;
    bytes
}

fn identity_for(seed: u8, method: IdentityResolutionMethod, from: i64, to: i64) -> DeviceIdentity {
    DeviceIdentity::builder()
        .fingerprint(fingerprint("Beacon %"))
        .fingerprint_hash(hash(seed))
        .confidence(0.4)
        .resolution_method(method)
        .seen_window(ts(from), ts(to))
        .observation_count(3)
        .resolver_version("test-resolver-1")
        .build()
}

// ── device_identities ───────────────────────────────────────────────────────

#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn a_repeated_pass_finds_the_identity_the_first_pass_made(pool: sqlx::PgPool) {
    let first = DeviceIdentityRepository::upsert_by_fingerprint(
        &pool,
        &identity_for(1, IdentityResolutionMethod::Fingerprint, 200, 400),
    )
    .await
    .expect("first pass should write");

    // A second pass, later, over data that still produces the same fingerprint.
    let mut again = identity_for(1, IdentityResolutionMethod::Fingerprint, 500, 600);
    again.confidence_score = 0.7;
    again.observation_count = 9;
    let second = DeviceIdentityRepository::upsert_by_fingerprint(&pool, &again)
        .await
        .expect("second pass should write");

    assert_eq!(
        first.identity_id, second.identity_id,
        "the same fingerprint produced a second identity, so every reprocess invents a \
         new device and no history survives a rerun"
    );
    assert_eq!(
        DeviceIdentityRepository::count(&pool)
            .await
            .expect("count should work"),
        1
    );
    assert_eq!(
        (second.first_seen, second.last_seen),
        (ts(200), ts(600)),
        "the window has to widen rather than jump to the newest pass"
    );
    assert_eq!(
        second.confidence_score, 0.7,
        "confidence is whatever the latest pass concluded"
    );
    assert_eq!(
        second.observation_count, 9,
        "counts are the latest pass's recomputation, not a running total, or a rerun \
         inflates how often a device was seen"
    );

    let found = DeviceIdentityRepository::find_by_fingerprint_hash(&pool, &hash(1))
        .await
        .expect("lookup should work")
        .expect("the natural key should be findable");
    assert_eq!(found.identity_id, first.identity_id);
}

#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn a_different_fingerprint_is_a_different_identity(pool: sqlx::PgPool) {
    let a = DeviceIdentityRepository::upsert_by_fingerprint(
        &pool,
        &identity_for(2, IdentityResolutionMethod::Fingerprint, 100, 200),
    )
    .await
    .expect("write should work");
    let b = DeviceIdentityRepository::upsert_by_fingerprint(
        &pool,
        &identity_for(3, IdentityResolutionMethod::Fingerprint, 100, 200),
    )
    .await
    .expect("write should work");

    assert_ne!(a.identity_id, b.identity_id);
    assert_eq!(
        DeviceIdentityRepository::count(&pool)
            .await
            .expect("count should work"),
        2
    );
}

#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn a_fingerprint_must_carry_every_feature_it_claims(pool: sqlx::PgPool) {
    let mut row = identity_for(4, IdentityResolutionMethod::Name, 100, 200);
    // A fingerprint that names no manufacturer at all: comparing it against a full one
    // would silently disagree on a feature that was never there.
    row.fingerprint = serde_json::json!({ "name_pattern": "Beacon %" });

    let error = DeviceIdentityRepository::upsert_by_fingerprint(&pool, &row)
        .await
        .expect_err("a fingerprint missing its keys must not be storable");
    assert!(
        matches!(
            error,
            RepoError::Database(sqlx::Error::Database(ref e))
                if e.message().contains("valid_fingerprint_shape")
        ),
        "expected the shape CHECK to refuse it, got {error:?}"
    );
}

#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn the_declaration_order_of_the_method_enum_is_the_reliability_order(pool: sqlx::PgPool) {
    // `LEAST(method)` in device_address_links is only correct because Postgres orders an
    // enum by declaration and the declaration is strongest-first. Inserting a variant in
    // the middle of the CREATE TYPE would break that silently, in the dark, in production.
    let labels: Vec<String> = sqlx::query_scalar(
        r#"
        SELECT e.enumlabel
        FROM pg_enum e
        JOIN pg_type t ON t.oid = e.enumtypid
        WHERE t.typname = 'identity_resolution_method'
        ORDER BY e.enumsortorder
        "#,
    )
    .fetch_all(&pool)
    .await
    .expect("catalog should be readable");

    let expected: Vec<String> = IdentityResolutionMethod::all()
        .iter()
        .map(|method| method.as_str().to_string())
        .collect();

    assert_eq!(
        labels, expected,
        "the column orders justification differently from the code, so LEAST(method) \
         keeps the weaker one"
    );
}

// ── device_address_links ────────────────────────────────────────────────────

fn link_for(
    device: &[u8],
    observer: &[u8],
    identity_id: Uuid,
    method: IdentityResolutionMethod,
    confidence: f32,
    from: i64,
    to: i64,
) -> DeviceAddressLink {
    DeviceAddressLink {
        device_hash: device.to_vec(),
        observer_node_id: observer.to_vec(),
        identity_id,
        address: None,
        address_type: None,
        method,
        confidence,
        first_seen: ts(from),
        last_seen: ts(to),
        observation_count: 1,
        computed_at: Utc::now(),
    }
}

#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn a_link_is_promoted_when_a_later_pass_names_the_address(pool: sqlx::PgPool) {
    let observer = registered_node(&pool, 0x11).await;
    let identity = DeviceIdentityRepository::upsert_by_fingerprint(
        &pool,
        &identity_for(5, IdentityResolutionMethod::Fingerprint, 100, 200),
    )
    .await
    .expect("identity should exist");

    let device = node_id(0xa1);
    DeviceIdentityRepository::link_address(
        &pool,
        &link_for(
            &device,
            &observer,
            identity.identity_id,
            IdentityResolutionMethod::TemporalAdjacency,
            0.3,
            100,
            200,
        ),
    )
    .await
    .expect("weak link should record");

    let mut strong = link_for(
        &device,
        &observer,
        identity.identity_id,
        IdentityResolutionMethod::ExactAddress,
        0.9,
        50,
        300,
    );
    strong.address = Some(vec![0xf0, 0xee, 0, 0, 0, 1]);
    strong.address_type = Some(BleAddressType::Public);
    DeviceIdentityRepository::link_address(&pool, &strong)
        .await
        .expect("strong link should record");

    let links = DeviceIdentityRepository::links_for_hash(&pool, &device, &observer)
        .await
        .expect("lookup should work");
    assert_eq!(
        links.len(),
        1,
        "one justification per identifier and observer"
    );
    assert_eq!(
        links[0].method,
        IdentityResolutionMethod::ExactAddress,
        "an exact address match should replace a temporal guess, not be dropped as \
         weaker-looking bookkeeping"
    );
    assert_eq!(links[0].confidence, 0.9);
    assert_eq!(links[0].address, Some(vec![0xf0, 0xee, 0, 0, 0, 1]));
    assert_eq!(
        (links[0].first_seen, links[0].last_seen),
        (ts(50), ts(300)),
        "both passes' windows describe the same identifier"
    );
}

#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn a_link_is_not_demoted_by_a_later_weaker_pass(pool: sqlx::PgPool) {
    let observer = registered_node(&pool, 0x12).await;
    let identity = DeviceIdentityRepository::upsert_by_fingerprint(
        &pool,
        &identity_for(6, IdentityResolutionMethod::Fingerprint, 100, 200),
    )
    .await
    .expect("identity should exist");
    let device = node_id(0xa2);

    let mut named = link_for(
        &device,
        &observer,
        identity.identity_id,
        IdentityResolutionMethod::ExactAddress,
        0.9,
        100,
        200,
    );
    named.address = Some(vec![0xf0, 0xee, 0, 0, 0, 2]);
    DeviceIdentityRepository::link_address(&pool, &named)
        .await
        .expect("named link should record");

    DeviceIdentityRepository::link_address(
        &pool,
        &link_for(
            &device,
            &observer,
            identity.identity_id,
            IdentityResolutionMethod::Name,
            0.1,
            300,
            400,
        ),
    )
    .await
    .expect("weak link should record");

    let links = DeviceIdentityRepository::links_for_hash(&pool, &device, &observer)
        .await
        .expect("lookup should work");
    assert_eq!(links.len(), 1);
    assert_eq!(
        links[0].method,
        IdentityResolutionMethod::ExactAddress,
        "a later, weaker inference must not erase a stronger justification"
    );
    assert_eq!(
        links[0].address,
        Some(vec![0xf0, 0xee, 0, 0, 0, 2]),
        "a pass that saw no address must not blank the address that was known"
    );
    assert_eq!(links[0].confidence, 0.9);
}

#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn one_identifier_can_be_linked_to_two_identities(pool: sqlx::PgPool) {
    let observer = registered_node(&pool, 0x13).await;
    let device = node_id(0xa3);

    let before = DeviceIdentityRepository::upsert_by_fingerprint(
        &pool,
        &identity_for(7, IdentityResolutionMethod::Fingerprint, 100, 200),
    )
    .await
    .expect("identity A should exist");
    let after = DeviceIdentityRepository::upsert_by_fingerprint(
        &pool,
        &identity_for(8, IdentityResolutionMethod::Fingerprint, 900, 1_000),
    )
    .await
    .expect("identity B should exist");

    // The address was seen, then went away, then came back on a different device.
    for identity in [&before, &after] {
        DeviceIdentityRepository::link_address(
            &pool,
            &link_for(
                &device,
                &observer,
                identity.identity_id,
                IdentityResolutionMethod::ExactAddress,
                0.8,
                identity.first_seen.timestamp(),
                identity.last_seen.timestamp(),
            ),
        )
        .await
        .expect("link should record");
    }

    let links = DeviceIdentityRepository::links_for_hash(&pool, &device, &observer)
        .await
        .expect("lookup should work");
    assert_eq!(
        links.len(),
        2,
        "a re-used address has two owners; collapsing them is the exact bug this table \
         exists to prevent"
    );

    let for_before = DeviceIdentityRepository::links_for_identity(&pool, before.identity_id)
        .await
        .expect("lookup should work");
    assert_eq!(
        for_before.len(),
        1,
        "the reverse query must not pull in the other owner"
    );
}

#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn links_are_scoped_to_the_observer_that_saw_them(pool: sqlx::PgPool) {
    let one = registered_node(&pool, 0x14).await;
    let two = registered_node(&pool, 0x15).await;
    let identity = DeviceIdentityRepository::upsert_by_fingerprint(
        &pool,
        &identity_for(9, IdentityResolutionMethod::Fingerprint, 100, 200),
    )
    .await
    .expect("identity should exist");
    let device = node_id(0xa4);

    for observer in [&one, &two] {
        DeviceIdentityRepository::link_address(
            &pool,
            &link_for(
                &device,
                observer,
                identity.identity_id,
                IdentityResolutionMethod::Fingerprint,
                0.5,
                100,
                200,
            ),
        )
        .await
        .expect("link should record");
    }

    let for_one = DeviceIdentityRepository::links_for_hash(&pool, &device, &one)
        .await
        .expect("lookup should work");
    assert_eq!(
        for_one.len(),
        1,
        "one node's view must not include another node's justification"
    );
    assert_eq!(for_one[0].observer_node_id, one);

    assert_eq!(
        DeviceIdentityRepository::links_for_identity(&pool, identity.identity_id)
            .await
            .expect("lookup should work")
            .len(),
        2,
        "the identity's own view spans observers"
    );
}

// ── co_occurrence_events ────────────────────────────────────────────────────

/// A real identity, because both columns of `co_occurrence_events` are foreign keys: a
/// pair of invented ids is a co-presence between two devices that do not exist.
async fn seeded_identity(pool: &sqlx::PgPool, seed: u8) -> Uuid {
    let identity = DeviceIdentityRepository::upsert_by_fingerprint(
        pool,
        &identity_for(seed, IdentityResolutionMethod::Fingerprint, 100, 200),
    )
    .await
    .expect("identity should exist");
    identity.identity_id
}

#[allow(clippy::too_many_arguments)] // test builder: one parameter per column the fixture varies
fn event(
    a: Uuid,
    b: Uuid,
    node: &[u8],
    cell: H3Index,
    from: i64,
    to: i64,
    samples: i32,
    distance: Option<f32>,
) -> CoOccurrenceEvent {
    let (identity_a, identity_b) = canonical_pair(a, b).expect("two different identities");
    CoOccurrenceEvent {
        identity_a,
        identity_b,
        node_id: node.to_vec(),
        geo_cell_macro: cell,
        window_start: ts(from),
        window_end: ts(to),
        sample_count: samples,
        distance_m: distance,
        generated_at: Utc::now(),
    }
}

fn cells() -> (H3Index, H3Index) {
    let near = repo::geo::macro_cell(LAT, LON).unwrap();
    // Several kilometres away: far enough to be a different macro cell.
    let far = repo::geo::macro_cell(LAT + 0.05, LON).unwrap();
    assert_ne!(near, far, "fixture needs two distinct macro cells");
    (H3Index::from(near), H3Index::from(far))
}

#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn a_pair_is_stored_once_whichever_order_it_arrives_in(pool: sqlx::PgPool) {
    let node = registered_node(&pool, 0x21).await;
    let a = seeded_identity(&pool, 1).await;
    let b = seeded_identity(&pool, 2).await;
    let (cell, _) = cells();

    CoOccurrenceRepository::record(&pool, &event(a, b, &node, cell, 100, 200, 2, Some(10.0)))
        .await
        .expect("first direction should record");
    CoOccurrenceRepository::record(&pool, &event(b, a, &node, cell, 100, 200, 5, Some(4.0)))
        .await
        .expect("reverse direction should record");

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM co_occurrence_events")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        count, 1,
        "the same co-presence recorded from both directions became two rows, doubling \
         its weight in every aggregate"
    );

    let stored = CoOccurrenceRepository::for_identity(&pool, a, 10)
        .await
        .expect("query should work");
    assert_eq!(stored[0].sample_count, 5, "the better sample count wins");
    assert_eq!(
        stored[0].distance_m,
        Some(4.0),
        "two devices reported 10 m and then 4 m apart were 4 m apart"
    );

    // Both sides must find it: the canonical ordering is exactly why filtering on
    // identity_a alone would be wrong.
    let from_b = CoOccurrenceRepository::for_identity(&pool, b, 10)
        .await
        .expect("query should work");
    assert_eq!(from_b.len(), 1);
}

#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn a_reversed_pair_written_directly_is_refused(pool: sqlx::PgPool) {
    let node = registered_node(&pool, 0x22).await;
    let (a, b) = {
        let (low, high) = canonical_pair(Uuid::nil(), Uuid::max()).unwrap();
        (high, low) // deliberately backwards
    };
    let (cell, _) = cells();

    let error = sqlx::query(
        "INSERT INTO co_occurrence_events (identity_a, identity_b, node_id, geo_cell_macro, \
         window_start, window_end, sample_count) VALUES ($1, $2, $3, $4, $5, $6, 1)",
    )
    .bind(a)
    .bind(b)
    .bind(&node)
    .bind(cell)
    .bind(ts(100))
    .bind(ts(200))
    .execute(&pool)
    .await
    .expect_err("identity_a > identity_b must not be storable");

    assert!(
        error.to_string().contains("canonical_co_occurrence_order"),
        "expected the canonical-order CHECK, got {error}"
    );
}

#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn a_window_widens_instead_of_splitting(pool: sqlx::PgPool) {
    let node = registered_node(&pool, 0x23).await;
    let a = seeded_identity(&pool, 1).await;
    let b = seeded_identity(&pool, 2).await;
    let (cell, _) = cells();

    CoOccurrenceRepository::record(&pool, &event(a, b, &node, cell, 100, 200, 1, None))
        .await
        .expect("first window");
    CoOccurrenceRepository::record(&pool, &event(a, b, &node, cell, 100, 260, 3, None))
        .await
        .expect("extended window");

    let stored = CoOccurrenceRepository::for_identity(&pool, a, 10)
        .await
        .expect("query should work");
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].window_end, ts(260));
    assert_eq!(stored[0].sample_count, 3);
}

// ── association_edges ───────────────────────────────────────────────────────

#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn an_edge_counts_places_and_days_not_only_sightings(pool: sqlx::PgPool) {
    let node = registered_node(&pool, 0x31).await;
    let a = seeded_identity(&pool, 3).await;
    let b = seeded_identity(&pool, 4).await;
    let (near, far) = cells();
    const DAY: i64 = 86_400;

    // Three days, two of them in different cells.
    CoOccurrenceRepository::record(&pool, &event(a, b, &node, near, 100, 200, 2, None))
        .await
        .expect("day 1");
    CoOccurrenceRepository::record(
        &pool,
        &event(a, b, &node, far, 100 + DAY, 200 + DAY, 3, None),
    )
    .await
    .expect("day 2");
    CoOccurrenceRepository::record(
        &pool,
        &event(a, b, &node, near, 100 + 2 * DAY, 200 + 2 * DAY, 4, None),
    )
    .await
    .expect("day 3");

    let aggregates = AssociationRepository::aggregate(&pool)
        .await
        .expect("aggregate should work");
    assert_eq!(aggregates.len(), 1);
    assert_eq!(aggregates[0].co_occurrence_count, 9);
    assert_eq!(aggregates[0].distinct_geo_cells, 2);
    assert_eq!(aggregates[0].distinct_days, 3);

    let written = AssociationRepository::recompute(&pool)
        .await
        .expect("recompute should work");
    assert_eq!(written, 1);

    let edge = AssociationRepository::between(&pool, b, a)
        .await
        .expect("query should work")
        .expect("the edge should exist whichever order it was asked in");
    assert_eq!(edge.co_occurrence_count, 9);
    assert_eq!(edge.distinct_geo_cells, 2);
    assert_eq!(edge.distinct_days, 3);
    assert_eq!(
        edge.association_strength,
        strength_for(9, 2, 3),
        "the stored score must be exactly what the Rust scoring function says, or the \
         formula is not the one under test"
    );
    assert_eq!(edge.computed_through, ts(200 + 2 * DAY));

    let strongest = AssociationRepository::strongest(&pool, 0.5, 10)
        .await
        .expect("query should work");
    assert_eq!(strongest.len(), 1);
    // Below the edge's own strength, it disappears; that threshold is the reader's filter.
    let score = edge.association_strength;
    assert_eq!(
        AssociationRepository::strongest(&pool, score + 0.01, 10)
            .await
            .expect("query should work")
            .len(),
        0
    );
}

#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn recompute_updates_in_place_rather_than_appending(pool: sqlx::PgPool) {
    let node = registered_node(&pool, 0x32).await;
    let a = seeded_identity(&pool, 5).await;
    let b = seeded_identity(&pool, 6).await;
    let (near, _) = cells();

    CoOccurrenceRepository::record(&pool, &event(a, b, &node, near, 100, 200, 1, None))
        .await
        .expect("first event");
    AssociationRepository::recompute(&pool)
        .await
        .expect("first recompute");

    CoOccurrenceRepository::record(&pool, &event(a, b, &node, near, 300, 400, 6, None))
        .await
        .expect("second event");
    AssociationRepository::recompute(&pool)
        .await
        .expect("second recompute");

    assert_eq!(
        AssociationRepository::count(&pool)
            .await
            .expect("count should work"),
        1,
        "every pass added a row for one pair"
    );
    let edge = AssociationRepository::between(&pool, a, b)
        .await
        .expect("query should work")
        .expect("edge");
    assert_eq!(edge.co_occurrence_count, 7);
    assert_eq!(
        edge.computed_through,
        ts(400),
        "computed_through is how a reader tells a stale edge from one the batch has not \
         reached, so it has to track the newest event"
    );
}

#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn an_identity_is_never_associated_with_itself(pool: sqlx::PgPool) {
    let id = Uuid::now_v7();
    assert!(canonical_pair(id, id).is_none());
    assert!(
        AssociationRepository::between(&pool, id, id)
            .await
            .expect("query should work")
            .is_none(),
        "asking about an identity with itself must not return some other pair"
    );
}
