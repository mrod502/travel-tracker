//! The wire contract for the two Postgres types sqlx has no built-in support for.
//!
//! These are not tests of business logic. They pin down, against a real server,
//! the facts every other module in this crate quietly depends on:
//!
//! * `h3index` is a base type, not a domain over `bigint`, and its casts to and
//!   from `bigint` are **explicit** — so a generated column decodes only into
//!   [`H3Index`], and a `WHERE geo_cell_macro = $1` comparison only plans when the
//!   parameter is declared `h3index`.
//! * `geography` crosses the wire as EWKB, not WKT.
//! * the enum labels that the signed payload's code tables resolve to are the labels
//!   the server gives the variants — [`AdvType::as_str`] and [`LocationSource::as_str`]
//!   are not a second mapping somebody keeps in step by hand, they are compared with
//!   `pg_enum` and with the column's own `::text` output. The wire itself carries
//!   integers, so these labels are how a decoded payload is matched back to a row.
//! * an instant carrying sub-microsecond precision lands in `observed_at`
//!   **truncated**, not rounded — which is what allows the signature to name the same
//!   instant the row does. (Postgres itself rounds a text value with more digits; the
//!   driver's wire format has no sub-microsecond unit to round.)
//!
//! The first two were discovered by them failing: an `Option<i64>` field and a WKT
//! parameter each produced an error, and in the `INSERT … RETURNING *` case the
//! error arrived *after* the row had been written. If h3-pg or PostGIS ever
//! changes representation, or someone swaps a model field back to `i64`, this is
//! the test that says so.
//!
//! Run with a server available:
//!
//! ```text
//! DATABASE_URL=postgres://postgres:devpassword@database:5432/travel cargo test -p repo
//! ```

use repo::models::enums::{AdvType, LocationSource};
use repo::models::{Occurrence, SignalType};
use repo::repositories::{NodeRepository, OccurrenceRepository};
use repo::types::H3Index;

/// Statue of Liberty: the coordinate every `geo` fixture starts from.
const LAT: f64 = 40.6892;
const LON: f64 = -74.0445;

fn node_id(seed: u8) -> Vec<u8> {
    vec![seed; 32]
}

/// A registered node, because `occurrences.origin_node_id` is a foreign key.
async fn registered_node(pool: &sqlx::PgPool) -> Vec<u8> {
    let id = node_id(0x0a);
    NodeRepository::register(
        pool,
        &id,
        repo::models::NodeType::Full,
        &node_id(0x0b),
        b"ca-credential",
        Some((LAT, LON)),
        &[repo::geo::macro_cell(LAT, LON).unwrap()],
    )
    .await
    .unwrap();

    id
}

fn occurrence(origin_node_id: &[u8], observed_at: chrono::DateTime<chrono::Utc>) -> Occurrence {
    Occurrence::builder()
        .signal_type(SignalType::Bluetooth)
        .origin_node_id(origin_node_id)
        .observed_at(observed_at)
        .observed_at_node_local(observed_at)
        .device_hash(&node_id(0x0c))
        .rssi(-67)
        .signed_payload(b"canonical bytes")
        .signature(&[7u8; 64])
        .with_location(LAT, LON, Some(10.0), Some(5.0), LocationSource::NodeGps)
        .build()
}

/// `observed_at` has to be inside a partition, and the partitions are created
/// relative to `now()` — so an insert dated years ago would fail for a reason
/// unrelated to what this file is testing.
fn now() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}

#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn generated_h3_columns_decode_into_h3index(pool: sqlx::PgPool) {
    let origin = registered_node(&pool).await;

    let stored = OccurrenceRepository::create(&pool, &occurrence(&origin, now()))
        .await
        .expect("insert with a location should be stored and returned");

    let fine = repo::geo::fine_cell(LAT, LON).unwrap();
    let macro_cell = repo::geo::macro_cell(LAT, LON).unwrap();

    // The point of this test: the server computed these, and Rust agrees with
    // what it computed, byte for byte.
    assert_eq!(stored.geo_cell_fine, Some(H3Index::from(fine)));
    assert_eq!(stored.geo_cell_macro, Some(H3Index::from(macro_cell)));
}

#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn an_occurrence_without_a_location_has_no_cells(pool: sqlx::PgPool) {
    let origin = registered_node(&pool).await;

    let mut record = occurrence(&origin, now());
    record.location = None;
    record.location_source = LocationSource::NodeFixed;

    let stored = OccurrenceRepository::create(&pool, &record)
        .await
        .expect("a location-less occurrence is still a valid occurrence");

    assert_eq!(stored.geo_cell_fine, None);
    assert_eq!(stored.geo_cell_macro, None);
}

#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn geography_survives_the_round_trip_as_a_point(pool: sqlx::PgPool) {
    let origin = registered_node(&pool).await;

    let stored = OccurrenceRepository::create(&pool, &occurrence(&origin, now()))
        .await
        .unwrap();

    let location = stored
        .location
        .expect("the occurrence was built with a location");
    assert!(
        (location.0.x() - LON).abs() < 1e-9,
        "longitude came back as {}",
        location.0.x()
    );
    assert!(
        (location.0.y() - LAT).abs() < 1e-9,
        "latitude came back as {}",
        location.0.y()
    );
}

#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn the_stored_fine_cell_actually_contains_the_location(pool: sqlx::PgPool) {
    let origin = registered_node(&pool).await;
    OccurrenceRepository::create(&pool, &occurrence(&origin, now()))
        .await
        .unwrap();

    // The check that does not depend on anyone's coordinate convention: the cell
    // the server derived has to contain the point it was derived from. PostGIS'
    // built-in `point <@ polygon` is plain planar geometry over the (x = lng,
    // y = lat) pair the geography already holds, so this fails the moment the
    // derivation transposes the coordinates — which is exactly what
    // 202609041200_fix_geo_cell_coordinate_order.sql exists to fix.
    let contains: bool = sqlx::query_scalar(
        "SELECT point(ST_X(location::geometry), ST_Y(location::geometry))
                  <@ h3_cell_to_boundary(geo_cell_fine)
         FROM occurrences LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    assert!(
        contains,
        "geo_cell_fine does not contain the location it was derived from — the \
         generated column's coordinate order has drifted again"
    );
}

#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn a_bare_integer_parameter_would_not_match_an_h3index_column(pool: sqlx::PgPool) {
    let origin = registered_node(&pool).await;
    let macro_cell = repo::geo::macro_cell(LAT, LON).unwrap();
    OccurrenceRepository::create(&pool, &occurrence(&origin, now()))
        .await
        .unwrap();

    // The shape the product used before `H3Index` existed: `h3index = bigint`.
    // The conversion back to an integer is deliberate — this parameter has to be
    // the wrong type on purpose, which is precisely what no other call site may do.
    let without_a_cast =
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM occurrences WHERE geo_cell_macro = $1")
            .bind(i64::try_from(u64::from(macro_cell)).expect("a valid cell fits an i64"))
            .fetch_one(&pool)
            .await;

    assert!(
        without_a_cast.is_err(),
        "a bigint parameter was accepted against an h3index column — either the \
         schema changed or h3-pg added an implicit cast, and the newtype's \
         justification needs reviewing"
    );

    // What the repository does now: no cast in the SQL, so the operator resolves
    // and the index on geo_cell_macro stays usable.
    let found = OccurrenceRepository::find_by_geo_cell(&pool, macro_cell, 10)
        .await
        .unwrap();

    assert_eq!(found.len(), 1);
    assert_eq!(found[0].geo_cell_macro, Some(H3Index::from(macro_cell)));
}

#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn text_and_binary_forms_name_the_same_cell(pool: sqlx::PgPool) {
    let origin = registered_node(&pool).await;
    let macro_cell = repo::geo::macro_cell(LAT, LON).unwrap();
    OccurrenceRepository::create(&pool, &occurrence(&origin, now()))
        .await
        .unwrap();

    let printed: String =
        sqlx::query_scalar("SELECT geo_cell_macro::text FROM occurrences LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap();

    // h3index_out prints lowercase hex with no leading zeros, which is exactly
    // what a `CellIndex` prints — the two spellings are interchangeable.
    assert_eq!(printed, macro_cell.to_string());
    assert_eq!(repo::geo::parse_cell(&printed).unwrap(), macro_cell);
}

#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn owns_geo_cells_round_trips_and_absence_means_no_claim(pool: sqlx::PgPool) {
    let id = node_id(0x1a);
    let claimed = [repo::geo::macro_cell(LAT, LON).unwrap()];

    NodeRepository::register(
        &pool,
        &id,
        repo::models::NodeType::Full,
        &node_id(0x1b),
        b"ca-credential",
        None,
        &claimed,
    )
    .await
    .unwrap();

    assert_eq!(
        NodeRepository::owns_geo_cells(&pool, &id).await.unwrap(),
        claimed
    );

    // Re-enrolling a credential without repeating the claim must not erase it.
    NodeRepository::register(
        &pool,
        &id,
        repo::models::NodeType::Full,
        &node_id(0x1b),
        b"a-renewed-credential",
        None,
        &[],
    )
    .await
    .unwrap();

    assert_eq!(
        NodeRepository::owns_geo_cells(&pool, &id).await.unwrap(),
        claimed,
        "an empty cell list means the caller said nothing about ownership"
    );
}

#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn a_node_row_loads_with_the_declared_field_types(pool: sqlx::PgPool) {
    let id = node_id(0x2a);
    let claimed = repo::geo::macro_cell(LAT, LON).unwrap();
    NodeRepository::register(
        &pool,
        &id,
        repo::models::NodeType::Full,
        &node_id(0x2b),
        b"ca-credential",
        Some((LAT, LON)),
        &[claimed],
    )
    .await
    .unwrap();

    let node = NodeRepository::find_by_id(&pool, &id)
        .await
        .expect("Node must be loadable from the columns it declares")
        .unwrap();

    assert_eq!(node.owns_geo_cells, Some(vec![H3Index::from(claimed)]));
    assert!(node.is_active());
    assert_eq!(node.fixed_lat, Some(LAT));
}

#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn a_node_with_no_claim_loads_as_none(pool: sqlx::PgPool) {
    let id = node_id(0x3a);
    NodeRepository::register(
        &pool,
        &id,
        repo::models::NodeType::Signal,
        &node_id(0x3b),
        b"ca-credential",
        None,
        &[],
    )
    .await
    .unwrap();

    let node = NodeRepository::find_by_id(&pool, &id)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(node.owns_geo_cells, None);
}

#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn an_h3index_column_refuses_to_decode_as_an_ordinary_integer(pool: sqlx::PgPool) {
    let origin = registered_node(&pool).await;
    OccurrenceRepository::create(&pool, &occurrence(&origin, now()))
        .await
        .unwrap();

    // This is the failure that motivated `H3Index`, and the one a model field
    // quietly swapped back to `Option<i64>` would walk straight into.
    let as_integer =
        sqlx::query_scalar::<_, Option<i64>>("SELECT geo_cell_macro FROM occurrences LIMIT 1")
            .fetch_one(&pool)
            .await;

    assert!(
        as_integer.is_err(),
        "an h3index column decoded into Option<i64> — if h3-pg made h3index a \
         domain over bigint this newtype could go away, but not before then"
    );

    let as_cell: Option<H3Index> =
        sqlx::query_scalar("SELECT geo_cell_macro FROM occurrences LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(as_cell.is_some());

    let null: Option<H3Index> = sqlx::query_scalar("SELECT NULL::h3index")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(null, None);
}

/// The labels the server uses, in declaration order, for one enum type.
async fn pg_enum_labels(pool: &sqlx::PgPool, typname: &str) -> Vec<String> {
    sqlx::query_scalar::<_, String>(
        "SELECT e.enumlabel
           FROM pg_enum e
           JOIN pg_type t ON t.oid = e.enumtypid
          WHERE t.typname = $1
          ORDER BY e.enumsortorder",
    )
    .bind(typname)
    .fetch_all(pool)
    .await
    .unwrap()
}

fn rust_labels<T>(variants: &[T], as_str: fn(&T) -> &'static str) -> Vec<String> {
    variants.iter().map(|v| as_str(v).to_string()).collect()
}

#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn the_enum_labels_the_payload_codes_map_to_are_the_servers_own(pool: sqlx::PgPool) {
    // Signed payload v2 carries `adv_type` and `location_source` as integer codes, and
    // the table that turns a code back into a label names these strings. So a verifier
    // comparing a decoded payload with the row is comparing `as_str()` against whatever
    // the server calls the variant, with one mapping in between — and if the two
    // spellings part company, every such comparison fails in the direction that looks
    // like tampering. This is the test that keeps them one mapping.
    let origin = registered_node(&pool).await;

    for adv_type in AdvType::all() {
        let mut record = occurrence(&origin, now());
        record.adv_type = Some(*adv_type);

        let stored = OccurrenceRepository::create(&pool, &record)
            .await
            .expect("every advertisement type is a storable row");
        assert_eq!(stored.adv_type, Some(*adv_type), "adv_type round trip");
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT adv_type::text FROM occurrences WHERE occurrence_id = $1",
            )
            .bind(stored.occurrence_id)
            .fetch_one(&pool)
            .await
            .unwrap(),
            adv_type.as_str(),
            "AdvType::as_str() no longer names the label the column holds"
        );
    }

    for source in LocationSource::all() {
        let mut record = occurrence(&origin, now());
        record.location_source = *source;

        let stored = OccurrenceRepository::create(&pool, &record)
            .await
            .expect("every location source is a storable row");
        assert_eq!(stored.location_source, *source);
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT location_source::text FROM occurrences WHERE occurrence_id = $1",
            )
            .bind(stored.occurrence_id)
            .fetch_one(&pool)
            .await
            .unwrap(),
            source.as_str(),
            "LocationSource::as_str() no longer names the label the column holds"
        );
    }

    // In the other direction: the server must not hold a label Rust does not name,
    // and Rust must not name one the server has never heard of — either way a payload
    // string would reach a verifier that no row could produce.
    assert_eq!(
        pg_enum_labels(&pool, "adv_type").await,
        rust_labels(AdvType::all(), AdvType::as_str)
    );
    assert_eq!(
        pg_enum_labels(&pool, "location_source").await,
        rust_labels(LocationSource::all(), LocationSource::as_str)
    );
}

#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn an_instant_finer_than_a_microsecond_is_truncated_not_rounded(pool: sqlx::PgPool) {
    // The signature spells `observed_at` with six fractional digits. That is only a
    // description of the row if the column holds the same digits, so the writer
    // truncates before it stores anything — the alternative, letting the value be
    // adjusted on the way in, moves the instant after it has been signed.
    let origin = registered_node(&pool).await;

    // Whole microseconds plus 700 ns: rounding would step to the next microsecond,
    // truncation stays where it was.
    let micros = chrono::Utc::now().timestamp_micros();
    let with_nanos = chrono::DateTime::from_timestamp(
        micros.div_euclid(1_000_000),
        (micros.rem_euclid(1_000_000) as u32) * 1_000 + 700,
    )
    .expect("a real instant");

    let stored = OccurrenceRepository::create(&pool, &occurrence(&origin, with_nanos))
        .await
        .unwrap();

    assert_eq!(
        stored.observed_at.timestamp_micros(),
        micros,
        "observed_at came back at a different microsecond than the one that was signed"
    );

    // And the server's own spelling of the column matches the six-digit form digit
    // for digit, which is what a reader needs to re-derive the signed string from the
    // row it already has.
    let spelled: String = sqlx::query_scalar(
        "SELECT to_char(observed_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US+00:00')
           FROM occurrences WHERE occurrence_id = $1",
    )
    .bind(stored.occurrence_id)
    .fetch_one(&pool)
    .await
    .unwrap();

    let rederived = stored
        .observed_at
        .format("%Y-%m-%dT%H:%M:%S%.6f+00:00")
        .to_string();
    assert_eq!(spelled, rederived);

    // Worth pinning why the truncation is the writer's job: handed a text value with
    // more than six digits, Postgres does round rather than truncate.
    let server_rounds: String = sqlx::query_scalar(
        "SELECT to_char('2026-09-15T12:00:00.1234567Z'::timestamptz AT TIME ZONE 'UTC',
                        'HH24:MI:SS.US')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(server_rounds, "12:00:00.123457");
}

#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn a_jsonb_column_serialises_the_same_after_the_round_trip(pool: sqlx::PgPool) {
    // v2 signs `serde_json`'s compact bytes of this column, which is only a promise a
    // reader can keep if JSONB's own normalisation — sorting object keys, collapsing
    // whitespace, reprinting numbers as `numeric` — is invisible to that form.
    // Everything the node puts in the column is here: nested objects, an integer, a
    // boolean, a hex string, and the coordinates and fix details as f64.
    let origin = registered_node(&pool).await;
    let written = serde_json::json!({
        "ble": {
            "name": "Liberty Beacon",
            "rssi": -63,
            "address": "aabbccddeeff",
            "services_resolved": false,
            "service_uuids": ["180d", "180f"],
            "manufacturer_data": { "company_id": 76, "bytes": "0215" },
        },
        "position": {
            "lat": 40.6892,
            "lon": -74.0445,
            "altitude_m": 12.5,
            "accuracy_m": 4.0,
            "origin": "gps",
        },
    });

    let mut record = occurrence(&origin, now());
    record.signal_payload = written.clone();
    let stored = OccurrenceRepository::create(&pool, &record).await.unwrap();

    // Key order and spacing are not part of a JSON value, so re-serialising what came
    // back has to give the bytes that were signed. If this fails, a v2 signature over
    // `signal_payload` can no longer be checked by anyone but the node that wrote it.
    assert_eq!(
        serde_json::to_vec(&stored.signal_payload).unwrap(),
        serde_json::to_vec(&written).unwrap(),
        "JSONB came back spelling a value differently than it went in"
    );

    // What a verifier has to do, stated as the thing that does not work: the column's
    // own text carries a space after every colon and comma, while the signature is over
    // compact bytes. Parse, then re-serialise — never compare stored text to signed
    // bytes.
    let as_text: String =
        sqlx::query_scalar("SELECT signal_payload::text FROM occurrences WHERE occurrence_id = $1")
            .bind(stored.occurrence_id)
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_ne!(
        as_text.as_bytes(),
        serde_json::to_vec(&written).unwrap(),
        "the column's text form became the signed form — this test's premise changed"
    );
    let reparsed: serde_json::Value = serde_json::from_str(&as_text).unwrap();
    assert_eq!(
        serde_json::to_vec(&reparsed).unwrap(),
        serde_json::to_vec(&written).unwrap()
    );

    // Numbers are where a decimal format could plausibly have gone wrong, so they are
    // tested rather than trusted: exponent notation, a whole number past f64's
    // mantissa, a decimal needing 17 digits to round-trip, a trailing zero, and zero.
    // The literal is spelled with whitespace and one key per line on purpose: the
    // bytes the signature covers are the compact, key-sorted form either way.
    let numbers: serde_json::Value = serde_json::from_str(
        r#"{
            "exp": 1e30,
            "huge": 123456789012345678901234567890,
            "exact": 0.30000000000000004,
            "trailing": 1.50,
            "zero": 0.0
        }"#,
    )
    .unwrap();
    let mut numeric = occurrence(&origin, now());
    numeric.signal_payload = numbers.clone();
    let stored = OccurrenceRepository::create(&pool, &numeric).await.unwrap();

    assert_eq!(
        serde_json::to_vec(&stored.signal_payload).unwrap(),
        serde_json::to_vec(&numbers).unwrap(),
        "a number changed spelling through JSONB — signing this column's serialisation \
         is no longer reproducible by a reader"
    );
}
