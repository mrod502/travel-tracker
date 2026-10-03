//! What happens to a write when its month has no partition.
//!
//! `occurrences` is `PARTITION BY RANGE (observed_at)`. Partitions are
//! provisioned a horizon ahead of now — `db up` keeps fifteen months open — so
//! a row outside that horizon is a good row meeting a schema fact it knows
//! nothing about. This file pins the two behaviours that keep the write path
//! alive:
//!
//! * [`OccurrenceRepository::create`] notices
//!   `no partition of relation "occurrences" found for row`, creates exactly
//!   the month that row needs, and retries. One month, not a sweep.
//! * it does not do that for a timestamp the schema refuses to open. A row
//!   dated sixty years out is bad data, and quietly giving it a home would let
//!   whoever wrote the row fill this database's catalog.
//!
//! Also pinned, because they are the contracts the healing sits on: the row
//! really lands in the partition named after its month, a duplicate id still
//! reports `Duplicate` rather than being retried into confusion, concurrent
//! writers of one missing month get one partition between them, and a
//! transaction — which cannot heal, because the failed insert aborts it — works
//! when the partition is ensured first.
//!
//! Run with a server available:
//!
//! ```text
//! DATABASE_URL=postgres://postgres:devpassword@database:5432/travel cargo test -p repo --test partition_healing
//! ```

use chrono::{DateTime, Datelike, TimeZone, Utc};
use repo::models::enums::LocationSource;
use repo::models::{Occurrence, SignalType};
use repo::repositories::{NodeRepository, OccurrenceRepository};
use repo::RepoError;
use uuid::Uuid;

/// Statue of Liberty: the coordinate every `geo` fixture starts from.
const LAT: f64 = 40.6892;
const LON: f64 = -74.0445;

/// Months `db up` keeps provisioned, from `PARTITION_HORIZON_MONTHS` in the `db`
/// crate. A month past this has no partition until something creates one.
const PROVISIONED_HORIZON_MONTHS: i32 = 15;

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

fn occurrence(origin_node_id: &[u8], observed_at: DateTime<Utc>, seed: u8) -> Occurrence {
    Occurrence::builder()
        .signal_type(SignalType::Bluetooth)
        .origin_node_id(origin_node_id)
        .observed_at(observed_at)
        .observed_at_node_local(observed_at)
        .device_hash(&node_id(seed))
        .rssi(-67)
        .signed_payload(b"canonical bytes")
        .signature(&[7u8; 64])
        .with_location(LAT, LON, Some(10.0), Some(5.0), LocationSource::NodeGps)
        .build()
}

/// The 15th of the month `months` away from the current one, so the day of
/// month never lands a fixture on a boundary it did not mean to cross.
fn month_offset(months: i32) -> DateTime<Utc> {
    let now = Utc::now().date_naive();
    let ordinal = now.year() * 12 + now.month() as i32 - 1 + months;

    Utc.with_ymd_and_hms(
        ordinal.div_euclid(12),
        (ordinal.rem_euclid(12) + 1) as u32,
        15,
        0,
        0,
        0,
    )
    .unwrap()
}

/// The name the schema gives the month `ts` falls in — the same UTC-based name
/// `occurrence_partition_name()` builds in SQL.
fn expected_partition(ts: DateTime<Utc>) -> String {
    format!("occurrences_{}", ts.format("%Y_%m"))
}

async fn partition_exists(pool: &sqlx::PgPool, name: &str) -> bool {
    sqlx::query_scalar::<_, bool>("SELECT to_regclass($1) IS NOT NULL")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// The leaf partition a stored row actually landed in, via its `tableoid`.
async fn stored_in(pool: &sqlx::PgPool, occurrence_id: Uuid) -> String {
    sqlx::query_scalar::<_, String>(
        "SELECT tableoid::regclass::text FROM occurrences WHERE occurrence_id = $1",
    )
    .bind(occurrence_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn partition_count(pool: &sqlx::PgPool) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM pg_inherits WHERE inhparent = 'occurrences'::regclass",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

/// The insert that has no healing in front of it — which is what every write in
/// this project was before the repair existed.
fn expect_partition_error(error: RepoError) {
    let message = error.to_string();
    assert!(
        message.contains("no partition of relation"),
        "expected a partition-routing failure, got: {message}"
    );
}

#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn create_creates_the_month_a_row_needs(pool: sqlx::PgPool) {
    let origin = registered_node(&pool).await;
    // Past the provisioned horizon, inside the window the schema will open.
    let observed_at = month_offset(PROVISIONED_HORIZON_MONTHS + 3);
    let partition = expected_partition(observed_at);
    let record = occurrence(&origin, observed_at, 0x0c);

    assert!(
        !partition_exists(&pool, &partition).await,
        "{partition} should not exist before the write that needs it"
    );

    // The same row through the non-healing entry point: this is the failure
    // `create` exists to absorb, and asserting it here is what proves the rest
    // of this test is about the repair rather than about a partition that was
    // already there.
    let error = OccurrenceRepository::insert_once(&pool, &record)
        .await
        .expect_err("a row past the horizon must fail without healing");
    expect_partition_error(error);

    let stored = OccurrenceRepository::create(&pool, &record)
        .await
        .expect("create should create the partition and store the row");
    assert_eq!(stored.occurrence_id, record.occurrence_id);
    assert_eq!(stored.observed_at, observed_at);

    assert!(
        partition_exists(&pool, &partition).await,
        "{partition} should exist after the write that needed it"
    );
    assert_eq!(
        stored_in(&pool, record.occurrence_id).await,
        partition,
        "the row belongs in the partition named after its month"
    );
}

#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn healing_creates_only_the_month_that_was_asked_for(pool: sqlx::PgPool) {
    let origin = registered_node(&pool).await;
    let before = partition_count(&pool).await;

    let observed_at = month_offset(PROVISIONED_HORIZON_MONTHS + 4);
    OccurrenceRepository::create(&pool, &occurrence(&origin, observed_at, 0x0c))
        .await
        .expect("heal should succeed");

    assert_eq!(
        partition_count(&pool).await,
        before + 1,
        "one row needing one month must create one partition, not the rest of the window"
    );
}

#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn a_healed_month_is_reused_not_repeated(pool: sqlx::PgPool) {
    let origin = registered_node(&pool).await;
    let observed_at = month_offset(PROVISIONED_HORIZON_MONTHS + 5);
    let partition = expected_partition(observed_at);
    let before = partition_count(&pool).await;

    for seed in 0x0c..0x12 {
        OccurrenceRepository::create(&pool, &occurrence(&origin, observed_at, seed))
            .await
            .unwrap_or_else(|e| panic!("write for seed {seed:#04x} failed: {e}"));
    }

    assert_eq!(partition_count(&pool).await, before + 1);
    assert!(partition_exists(&pool, &partition).await);
}

#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn writers_racing_for_one_missing_month_create_one_partition(pool: sqlx::PgPool) {
    let origin = registered_node(&pool).await;
    let observed_at = month_offset(PROVISIONED_HORIZON_MONTHS + 6);

    let writers: Vec<_> = (0x0c..0x14)
        .map(|seed| {
            let pool = pool.clone();
            let origin = origin.clone();
            tokio::spawn(async move {
                OccurrenceRepository::create(&pool, &occurrence(&origin, observed_at, seed)).await
            })
        })
        .collect();

    for writer in writers {
        let stored = writer
            .await
            .expect("writer task should not panic")
            .expect("every concurrent writer should end up stored");
        assert_eq!(stored.observed_at, observed_at);
    }

    let partition = expected_partition(observed_at);
    assert!(partition_exists(&pool, &partition).await);
    // 8 writers, one month: `ensure_occurrence_partition_at` serialises them on
    // an advisory lock, so the DDL runs once instead of eight times.
    let carrying_this_month: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM pg_class WHERE relkind = 'r' AND relname = $1")
            .bind(&partition)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        carrying_this_month, 1,
        "exactly one table should carry this month"
    );
}

#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn a_timestamp_outside_the_window_is_refused_loudly(pool: sqlx::PgPool) {
    let origin = registered_node(&pool).await;
    let observed_at = month_offset(12 * 60); // sixty years out
    let partition = expected_partition(observed_at);

    let error =
        match OccurrenceRepository::create(&pool, &occurrence(&origin, observed_at, 0x0c)).await {
            Err(error @ RepoError::PartitionHealFailed { .. }) => error,
            other => panic!("a sixty-year-old-horizon row must not be stored, got: {other:?}"),
        };

    let message = error.to_string();
    assert!(
        message.contains("refusing to create an occurrences partition"),
        "the error should carry the schema's reason, got: {message}"
    );
    assert!(
        message.contains("no partition of relation"),
        "the error should also carry the insert that triggered it, got: {message}"
    );
    assert!(
        !partition_exists(&pool, &partition).await,
        "a refused row must not leave a partition behind"
    );
}

#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn duplicate_ids_still_report_duplicate(pool: sqlx::PgPool) {
    let origin = registered_node(&pool).await;
    let record = occurrence(&origin, Utc::now(), 0x0c);

    OccurrenceRepository::create(&pool, &record)
        .await
        .expect("first write should succeed");

    let error = OccurrenceRepository::create(&pool, &record)
        .await
        .expect_err("the same id twice is a duplicate, not a partition problem");
    assert!(
        matches!(error, RepoError::Duplicate(id) if id == record.occurrence_id),
        "a unique violation must keep its own error: {error}"
    );
}

#[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
async fn a_transaction_heals_only_when_asked_first(pool: sqlx::PgPool) {
    let origin = registered_node(&pool).await;
    let observed_at = month_offset(PROVISIONED_HORIZON_MONTHS + 7);
    let partition = expected_partition(observed_at);
    let record = occurrence(&origin, observed_at, 0x0c);

    // Mid-transaction there is nothing to heal with: the failed insert aborts
    // the transaction, which is why `create` takes a pool and this path exists.
    let aborted = {
        let mut tx = pool.begin().await.unwrap();
        let error = OccurrenceRepository::insert_once(&mut *tx, &record)
            .await
            .expect_err("no partition, and no repair available inside a transaction");
        // Rollback is implicit when the transaction is dropped.
        error
    };
    expect_partition_error(aborted);
    assert!(
        !partition_exists(&pool, &partition).await,
        "the aborted transaction must not have created anything"
    );

    // Ensuring the partition up front is the supported way to write a month the
    // schema has not opened yet.
    let mut tx = pool.begin().await.unwrap();
    let healed = OccurrenceRepository::ensure_partition(&mut *tx, observed_at)
        .await
        .expect("ensure_partition should create the month");
    assert_eq!(healed, partition);
    OccurrenceRepository::insert_once(&mut *tx, &record)
        .await
        .expect("insert_once should succeed once the month exists");
    tx.commit().await.expect("commit should succeed");

    assert_eq!(stored_in(&pool, record.occurrence_id).await, partition);
}
