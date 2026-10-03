//! Writing a pass out: the four derived tables, and nothing else.
//!
//! The resolver's ids are numbers in a process; the database's are rows keyed by a
//! fingerprint. This module is the translation, and the whole point of it is that a second
//! pass over the same data lands on the same rows:
//! [`repo::DeviceIdentityRepository::upsert_by_fingerprint`] recognises the fingerprint
//! the last pass wrote, the link upsert widens its window instead of duplicating, and the
//! co-presence rows are keyed by the day they were seen on, so re-recording them is
//! idempotent rather than additive.
//!
//! Two things are deliberately *not* written here. Nothing touches `occurrences`: those
//! rows are signed assertions by a node, and a derived table has no business editing them.
//! And nothing is deleted — a pair whose events were cleared keeps its edge until a
//! deliberate pass says otherwise, because silently dropping history in a batch that runs
//! unattended is how data goes missing without anyone noticing.

use std::collections::HashMap;

use chrono::Utc;
use repo::models::enums::IdentityResolutionMethod;
use repo::models::{CoOccurrenceEvent, DeviceAddressLink, DeviceIdentity};
use repo::repositories::{AssociationRepository, CoOccurrenceRepository, DeviceIdentityRepository};
use repo::{types::H3Index, RepoError};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::replay::{CoPresence, IdentityRecord, ReplayReport};

/// The build of resolver whose decisions these are.
///
/// Stored on every identity row, so that a merge a reader disagrees with can be traced
/// back to the model that made it — and so that a reprocess under a different build is
/// visible as a change in this column rather than as inexplicable drift.
pub fn resolver_version() -> String {
    format!("bt_iden-{}", env!("CARGO_PKG_VERSION"))
}

/// What a write pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WriteOutcome {
    /// Identity rows written.
    pub identities: u64,
    /// Identifier-to-identity links written.
    pub links: u64,
    /// Co-presence events recorded.
    pub co_presences: u64,
    /// Association edges recomputed.
    pub edges: u64,
}

impl WriteOutcome {
    /// The outcome as one line, for the CLI.
    pub fn summary(&self) -> String {
        format!(
            "{} identities, {} links, {} co-presence events, {} association edges",
            self.identities, self.links, self.co_presences, self.edges
        )
    }
}

/// Persists a pass.
///
/// `pool`, not an `Executor`: this spans several statements per identity and the
/// aggregate at the end, and an executor cannot be reused across them.
pub async fn write_report(
    pool: &sqlx::PgPool,
    report: &ReplayReport,
    resolver_version: &str,
) -> Result<WriteOutcome, RepoError> {
    let mut outcome = WriteOutcome::default();
    let mut written: HashMap<u64, Uuid> = HashMap::new();

    for record in &report.identities {
        let identity = DeviceIdentityRepository::upsert_by_fingerprint(
            pool,
            &identity_row(record, resolver_version),
        )
        .await?;
        written.insert(record.identity, identity.identity_id);
        outcome.identities += 1;

        for identifier in &record.identifiers {
            DeviceIdentityRepository::link_address(
                pool,
                &link_row(record, identifier, identity.identity_id),
            )
            .await?;
            outcome.links += 1;
        }
    }

    for presence in &report.co_presences {
        // A co-presence between two identities that did not both survive the pass is not
        // recordable: the foreign key is the point, not an obstacle. A pair where one side
        // was dropped has no edge, and inventing an id for the missing half would let the
        // association table refer to a device that does not exist.
        let (Some(&identity_a), Some(&identity_b)) = (
            written.get(&presence.identity_a),
            written.get(&presence.identity_b),
        ) else {
            continue;
        };

        CoOccurrenceRepository::record(pool, &co_presence_row(presence, identity_a, identity_b))
            .await?;
        outcome.co_presences += 1;
    }

    outcome.edges = AssociationRepository::recompute(pool).await?;
    Ok(outcome)
}

/// The identity row a pass produced.
fn identity_row(record: &IdentityRecord, resolver_version: &str) -> DeviceIdentity {
    let fingerprint = record.fingerprint.canonical_json();

    DeviceIdentity::builder()
        .fingerprint(fingerprint)
        .fingerprint_hash(record.fingerprint.hash())
        .confidence(confidence_of(record.confidence))
        .resolution_method(method_for(record))
        .seen_window(record.first_seen, record.last_seen)
        .observation_count(i32::try_from(record.observation_count).unwrap_or(i32::MAX))
        .resolver_version(resolver_version)
        .build()
}

/// How this identity was established, of the five ways the column admits.
///
/// `temporal_adjacency` and `irk` never appear here: this pass never merges on a gap in
/// time, and nothing in this project has an IRK. An identity that was bridged across an
/// address change is keyed by its fingerprint by definition, and one that never changed
/// address is still keyed by its fingerprint — the address is what makes the *link*
/// certain, not what the row is keyed on, so `exact_address` is reserved for the case
/// where the address is the only evidence there was.
fn method_for(record: &IdentityRecord) -> IdentityResolutionMethod {
    let keyed_on_features = record.fingerprint.manufacturer_id.is_some()
        || !record.fingerprint.service_uuids.is_empty()
        || !record.fingerprint.field_layouts.is_empty();
    if keyed_on_features {
        return IdentityResolutionMethod::Fingerprint;
    }
    if !record.fingerprint.names.is_empty() {
        return IdentityResolutionMethod::Name;
    }
    IdentityResolutionMethod::ExactAddress
}

/// The link between one identifier and the identity that now explains it.
fn link_row(
    record: &IdentityRecord,
    identifier: &super::replay::IdentifierRecord,
    identity_id: Uuid,
) -> DeviceAddressLink {
    // The table stores the hash that `occurrences.device_hash` uses, and CHECKs it at 32
    // bytes. A live observation has no row behind it, and a caller that has not hashed yet
    // may hand over anything; hashing whatever arrived keeps the invariant here instead of
    // reporting a constraint name that points nowhere near the mistake.
    let device_hash = match identifier.device_hash.clone() {
        Some(hash) if hash.len() == 32 => hash,
        other => Sha256::digest(other.unwrap_or_else(|| identifier.identifier.as_bytes().to_vec()))
            .to_vec(),
    };

    DeviceAddressLink {
        device_hash,
        observer_node_id: identifier.observer_node_id.clone().unwrap_or_default(),
        identity_id,
        address: hex::decode(&identifier.identifier).ok(),
        address_type: None,
        // A MAC the radio reported identifies the identifier exactly; anything else is a
        // stand-in, and the link's method should say so rather than look decisive.
        method: if identifier.source == "ble_mac" {
            IdentityResolutionMethod::ExactAddress
        } else {
            IdentityResolutionMethod::Fingerprint
        },
        confidence: confidence_of(record.confidence),
        first_seen: identifier.first_seen,
        last_seen: identifier.last_seen,
        observation_count: i32::try_from(identifier.observation_count).unwrap_or(i32::MAX),
        computed_at: Utc::now(),
    }
}

/// One co-presence, in the shape `co_occurrence_events` wants.
fn co_presence_row(presence: &CoPresence, identity_a: Uuid, identity_b: Uuid) -> CoOccurrenceEvent {
    let (a, b) = repo::canonical_pair(identity_a, identity_b)
        .expect("two different identities cannot resolve to one device");

    CoOccurrenceEvent {
        identity_a: a,
        identity_b: b,
        node_id: presence.observer_node_id.clone(),
        geo_cell_macro: H3Index(presence.geo_cell_macro.0),
        window_start: presence.window_start,
        window_end: presence.window_end,
        sample_count: i32::try_from(presence.sample_count).unwrap_or(i32::MAX),
        // No distance: a co-presence inside one macro cell is a fact about the cell, and
        // two locations several kilometres apart can share one. Computing a distance here
        // would report a precision the aggregation does not have.
        distance_m: None,
        generated_at: Utc::now(),
    }
}

/// Resolver confidence, in the `0..=1` the columns CHECK.
fn confidence_of(confidence: f64) -> f32 {
    confidence.clamp(0.0, 1.0) as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::adapt::{FeedObservation, ObservedIdentifier};
    use crate::identity::replay::{replay, ReplayOptions};
    use bt_iden::models::{AddressType, AdvertisementObservation, BluetoothAddress};
    use bt_iden::time::ObservationTime;
    use bt_iden::Datum;
    use repo::models::NodeType;
    use repo::repositories::{NodeRepository, OccurrenceRepository};
    use std::time::{Duration, UNIX_EPOCH};

    const LAT: f64 = 40.6892;
    const LON: f64 = -74.0445;

    fn node_id(seed: u8) -> Vec<u8> {
        vec![seed; 32]
    }

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

    fn observation(
        secs: i64,
        address: [u8; 6],
        name: &str,
        manufacturer: u16,
    ) -> AdvertisementObservation {
        AdvertisementObservation::new(
            ObservationTime::from_wall(UNIX_EPOCH + Duration::from_secs(secs as u64)),
            BluetoothAddress::new(address),
            AddressType::Public,
        )
        .with_local_name_datum(name.to_string(), Datum::Direct)
        .with_manufacturer_data(manufacturer, vec![0x02, 0x15])
        .with_field_layout(vec![0x01, 0x03, 0x09, 0xff])
    }

    /// The 32-byte `occurrences.device_hash` this observation stands in for.
    fn hash_of(address: [u8; 6]) -> Vec<u8> {
        Sha256::digest(address).to_vec()
    }

    fn feed(observation: AdvertisementObservation, node: &[u8]) -> FeedObservation {
        let bytes = *observation.address.as_bytes();
        FeedObservation {
            identifier: ObservedIdentifier {
                bytes,
                source: "ble_mac",
                reported: true,
            },
            occurred_at: chrono::DateTime::from_timestamp(
                observation.timestamp.micros_since_epoch().unwrap() as i64 / 1_000_000,
                0,
            )
            .unwrap(),
            observer_node_id: Some(node.to_vec()),
            geo_cell_macro: Some(H3Index::from(repo::geo::macro_cell(LAT, LON).unwrap())),
            device_hash: Some(hash_of(bytes)),
            occurrence_id: None,
            observation,
        }
    }

    fn options() -> ReplayOptions {
        ReplayOptions {
            config: bt_iden::ResolverConfig::new(),
            co_presence_window: Duration::from_secs(120),
        }
    }

    /// A phone and a beacon, seen together by one node.
    fn two_devices(node: &[u8]) -> Vec<FeedObservation> {
        vec![
            feed(
                observation(1_000, [0x11, 0, 0, 0, 0, 1], "Phone", 0x004c),
                node,
            ),
            feed(
                observation(1_005, [0x22, 0, 0, 0, 0, 1], "Beacon", 0x0123),
                node,
            ),
            // The phone rotates its address mid-pass.
            feed(
                observation(1_010, [0x11, 0, 0, 9, 9, 9], "Phone", 0x004c),
                node,
            ),
        ]
    }

    #[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
    async fn a_second_pass_finds_the_identity_the_first_pass_wrote(pool: sqlx::PgPool) {
        let node = registered_node(&pool, 0x61).await;
        let observations = two_devices(&node);
        let report = replay(&observations, &options());
        assert_eq!(
            report.identity_count(),
            2,
            "fixture should produce two identities"
        );

        let first = write_report(&pool, &report, "test-resolver-1")
            .await
            .expect("first pass should write");
        let second = write_report(&pool, &report, "test-resolver-1")
            .await
            .expect("second pass should write");

        assert_eq!(first.identities, 2);
        assert_eq!(
            DeviceIdentityRepository::count(&pool)
                .await
                .expect("count should work"),
            2,
            "re-running the pass minted new identities, so nothing a reader stored against \
             one survives a reprocess"
        );
        assert_eq!(
            second, first,
            "the second pass should be a no-op, not an addition"
        );
    }

    #[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
    async fn the_stored_row_names_the_features_that_keyed_it(pool: sqlx::PgPool) {
        let node = registered_node(&pool, 0x62).await;
        let report = replay(&two_devices(&node), &options());
        write_report(&pool, &report, "test-resolver-1")
            .await
            .expect("pass should write");

        let record = report
            .identities
            .iter()
            .find(|record| record.identifiers.len() == 2)
            .expect("the phone should have two identifiers on record");
        let stored =
            DeviceIdentityRepository::find_by_fingerprint_hash(&pool, &record.fingerprint.hash())
                .await
                .expect("lookup should work")
                .expect("the identity should be stored under the fingerprint the pass computed");

        assert_eq!(
            stored.resolution_method,
            IdentityResolutionMethod::Fingerprint
        );
        assert_eq!(stored.resolver_version, "test-resolver-1");
        assert_eq!(stored.observation_count, 2);
        // The CHECK on `fingerprint` requires these keys; a fingerprint that could not be
        // written would fail the batch rather than get here.
        for key in [
            "manufacturer_id",
            "service_uuids_hash",
            "field_layout_hash",
            "name_pattern",
        ] {
            assert!(
                stored.fingerprint.get(key).is_some(),
                "{key} missing from {stored:?}"
            );
        }
        assert_eq!(stored.fingerprint["manufacturer_id"], serde_json::json!(76));

        let links = DeviceIdentityRepository::links_for_identity(&pool, stored.identity_id)
            .await
            .expect("query should work");
        assert_eq!(
            links.len(),
            2,
            "both identifiers should be linked to the identity"
        );
        assert!(
            links
                .iter()
                .all(|link| link.method == IdentityResolutionMethod::ExactAddress),
            "a MAC the radio reported is not an inference"
        );
    }

    #[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
    async fn co_presence_becomes_an_association_edge(pool: sqlx::PgPool) {
        let node = registered_node(&pool, 0x63).await;
        let report = replay(&two_devices(&node), &options());
        assert_eq!(
            report.co_presences.len(),
            1,
            "one pair on one day is one event, whatever the clock said about it"
        );
        assert_eq!(
            report.co_presences[0].sample_count, 2,
            "the beacon meeting the phone, and the phone's new address meeting the beacon, \
             are two samples of the same encounter"
        );

        let outcome = write_report(&pool, &report, "test-resolver-1")
            .await
            .expect("pass should write");
        assert_eq!(outcome.co_presences, 1, "one pair, one day, one event");
        assert_eq!(outcome.edges, 1, "the rollup should have been recomputed");

        let edges = AssociationRepository::strongest(&pool, 0.0, 10)
            .await
            .expect("query should work");
        assert_eq!(edges.len(), 1);
        assert!(edges[0].association_strength > 0.0);
        assert_eq!(edges[0].distinct_days, 1);
    }

    #[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
    async fn a_richer_pass_and_a_thinner_one_meet_at_the_device_hash(pool: sqlx::PgPool) {
        let node = registered_node(&pool, 0x65).await;
        let address = [0x33u8, 0, 0, 0, 0, 7];

        let rich = replay(
            &[feed(observation(2_000, address, "Phone", 0x004c), &node)],
            &options(),
        );
        // The same device as a later capture would describe it: no AD structure, so no
        // manufacturer and no layout. Different evidence is a different hypothesis, and the
        // schema should say so rather than overwrite the better one with the worse.
        let thin_observation = AdvertisementObservation::new(
            ObservationTime::from_wall(UNIX_EPOCH + Duration::from_secs(2_600)),
            BluetoothAddress::new(address),
            AddressType::Public,
        )
        .with_local_name("Phone".to_string());
        let thin_feed = FeedObservation {
            observation: thin_observation,
            occurred_at: chrono::DateTime::from_timestamp(2_600, 0).unwrap(),
            ..feed(observation(2_600, address, "Phone", 0x004c), &node)
        };
        let thin = replay(&[thin_feed], &options());

        write_report(&pool, &rich, "test-resolver-1")
            .await
            .expect("the rich pass should write");
        write_report(&pool, &thin, "test-resolver-1")
            .await
            .expect("the thin pass should write");

        assert_eq!(
            DeviceIdentityRepository::count(&pool)
                .await
                .expect("count should work"),
            2,
            "one device observed two ways is two hypotheses, not one row rewritten"
        );

        // What keeps that from being data loss: both rows are reachable from the identifier
        // they disagree about. A reader asking "what do we think this device is" gets both
        // answers, ranked, instead of whichever pass ran last.
        let links = DeviceIdentityRepository::links_for_hash(&pool, &hash_of(address), &node)
            .await
            .expect("query should work");
        assert_eq!(links.len(), 2, "both hypotheses claim this identifier");
        assert_ne!(links[0].identity_id, links[1].identity_id);
    }

    #[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
    async fn the_pass_never_touches_occurrences(pool: sqlx::PgPool) {
        // `occurrences` are signed assertions by a node. A derived table may read them and
        // must not edit them, and the only cheap way to keep that true is to check it.
        let node = registered_node(&pool, 0x64).await;
        let report = replay(&two_devices(&node), &options());

        let before = OccurrenceRepository::count_all(&pool)
            .await
            .expect("count should work");
        write_report(&pool, &report, "test-resolver-1")
            .await
            .expect("pass should write");
        let after = OccurrenceRepository::count_all(&pool)
            .await
            .expect("count should work");

        assert_eq!(before, after);
        assert_eq!(before, 0);
    }
}
