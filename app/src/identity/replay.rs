//! The offline identity pass: occurrences in, reviewable device identities out.
//!
//! This is the stage the gap analysis asked for before any live wiring — `bt_iden` run as
//! a batch whose merge decisions are written down next to the data that produced them.
//! Three things follow from that framing, and they shape everything below.
//!
//! **The resolver's id is not the stored identity.** `bt_iden` hands out `Identity(1)`,
//! `Identity(2)`, … in the order it first saw them, which means nothing to someone
//! reading a database, and not even the same thing across two runs of the same data. What
//! gets persisted is the [`DeviceFingerprint`] of everything folded into that id: a key
//! that survives the run, recognises the same device next time, and lets a reprocess
//! converge on the row the last pass wrote.
//!
//! **A decision is its evidence, not its label.** `Merged` says nothing about whether to
//! trust it. Every line here carries the score, the coverage, the features that carried
//! the decision, and — for what the feed could not see — what that cost. A 65-point merge
//! resting on name and RSSI and a 65-point merge resting on manufacturer, layout and
//! payload are different claims; before evidence accounting they printed identically.
//!
//! **Nothing here is wired to the live path.** `FullNode` does not call this, so a wrong
//! merge cannot affect anything a node stores. The pass reads `occurrences` and writes
//! only the four derived tables, every one of which is documented as rebuildable.
//!
//! # What a pass is
//!
//! Observations are replayed in ascending wall-clock order through one resolver, with
//! [`expire`](IdentityResolver::expire) driven by the observations rather than by the
//! clock on the wall: replaying last week's rows against *now* would expire every identity
//! before it was considered and report a silent world. So the resolver groups a session.
//! Two bursts a week apart are two ids even for one device, which is not a limitation
//! being worked around — the durable identity is the fingerprint's job, and keeping the
//! two apart is what lets the resolver's time thresholds stay tight while a device seen
//! once in August is still recognisable in September.

use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use bt_iden::config::ResolverConfig;
use bt_iden::models::MatchEvidence;
use bt_iden::resolver::{HeuristicIdentityResolver, IdentityResolver, Outcome};
use chrono::{DateTime, Utc};
use repo::types::H3Index;
use serde::Serialize;
use uuid::Uuid;

use super::adapt::FeedObservation;
use super::fingerprint::DeviceFingerprint;

/// How far apart two observations by one node can be and still count as being together.
///
/// Generous on purpose: co-presence is two devices being somewhere in the same period,
/// and a two-minute gap inside a macro cell is still the same place. Tightening it is
/// calibration work for when association strengths are measured against known pairs.
pub const DEFAULT_CO_PRESENCE_WINDOW: Duration = Duration::from_secs(120);

/// Tuning for one pass.
#[derive(Debug, Clone)]
pub struct ReplayOptions {
    /// The resolver's thresholds, weights and evidence floor.
    pub config: ResolverConfig,
    /// See [`DEFAULT_CO_PRESENCE_WINDOW`].
    pub co_presence_window: Duration,
}

impl Default for ReplayOptions {
    fn default() -> Self {
        Self {
            config: ResolverConfig::default(),
            co_presence_window: DEFAULT_CO_PRESENCE_WINDOW,
        }
    }
}

/// What the resolver decided about one observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    /// Opened an identity.
    New,
    /// Joined an identity that already existed.
    Merged,
    /// Scored near enough to report, not enough to merge.
    BelowThreshold,
    /// Cleared the score on too little of the model to be trusted.
    InsufficientEvidence,
    /// Scored high enough, but a directly-observed feature disagreed.
    Contradicted,
}

impl DecisionKind {
    /// Stable label, for the report and for `co_occurrence_events`' reason columns.
    pub fn as_str(self) -> &'static str {
        match self {
            DecisionKind::New => "new",
            DecisionKind::Merged => "merged",
            DecisionKind::BelowThreshold => "below_threshold",
            DecisionKind::InsufficientEvidence => "insufficient_evidence",
            DecisionKind::Contradicted => "contradicted",
        }
    }

    fn from_outcome(outcome: &Outcome) -> Self {
        match outcome {
            Outcome::New => DecisionKind::New,
            Outcome::Merged(_) => DecisionKind::Merged,
            Outcome::BelowThreshold(_) => DecisionKind::BelowThreshold,
            Outcome::InsufficientEvidence(_) => DecisionKind::InsufficientEvidence,
            Outcome::Contradicted(_) => DecisionKind::Contradicted,
        }
    }
}

/// One line of the replay: an observation, the decision, and what it was worth.
#[derive(Debug, Clone, Serialize)]
pub struct Decision {
    /// Position in the pass, ascending by time.
    pub seq: usize,
    /// The row it came from, when it came from one.
    pub occurrence_id: Option<Uuid>,
    /// Wall-clock time of the observation.
    pub observed_at: DateTime<Utc>,
    /// The identifier it arrived under.
    pub identifier: String,
    /// `ble_mac`, `uuid`, `opaque_id`, or `derived`.
    pub identifier_source: &'static str,
    /// The resolver's in-process id. Not a database key — see [`IdentityRecord`].
    pub identity: u64,
    /// What was decided.
    pub decision: DecisionKind,
    /// Candidate score, when a candidate was considered.
    pub score: Option<f64>,
    /// Fraction of the designed model that had something to say.
    pub coverage: Option<f64>,
    /// The directly-observed feature that vetoed the merge, when one did.
    pub contradicted_by: Option<&'static str>,
    /// Features that carried the decision, largest first.
    pub top_features: Vec<(&'static str, f64)>,
    /// Features with no source, and the weight each one costs.
    pub missing: Vec<(&'static str, f64)>,
    /// Other identifiers already under this identity — the part a reader can check
    /// against the real world.
    pub with_identifiers: Vec<String>,
}

impl Decision {
    /// Whether this line records a merge.
    pub fn is_merge(&self) -> bool {
        self.decision == DecisionKind::Merged
    }
}

/// One identifier seen under an identity.
#[derive(Debug, Clone, Serialize)]
pub struct IdentifierRecord {
    /// `occurrences.device_hash`, when the observation came from a row.
    pub device_hash: Option<Vec<u8>>,
    /// The node that saw it.
    pub observer_node_id: Option<Vec<u8>>,
    /// The identifier, hex.
    pub identifier: String,
    /// Where the identifier came from.
    pub source: &'static str,
    /// How many observations arrived under it.
    pub observation_count: usize,
    /// First observation under it.
    pub first_seen: DateTime<Utc>,
    /// Last observation under it.
    pub last_seen: DateTime<Utc>,
}

/// Everything the pass learned about one identity.
#[derive(Debug, Clone, Serialize)]
pub struct IdentityRecord {
    /// The resolver's in-process id, for lining up against the decision lines.
    pub identity: u64,
    /// The feature set this identity is keyed by — what actually gets persisted.
    pub fingerprint: DeviceFingerprint,
    /// The resolver's own confidence in this identity.
    pub confidence: f64,
    /// Observations folded in.
    pub observation_count: usize,
    /// First observation.
    pub first_seen: DateTime<Utc>,
    /// Last observation.
    pub last_seen: DateTime<Utc>,
    /// The identifiers that resolved to this identity.
    pub identifiers: Vec<IdentifierRecord>,
}

/// Two identities seen together by one node in one cell.
#[derive(Debug, Clone, Serialize)]
pub struct CoPresence {
    /// Lower identity first — the same canonical ordering `co_occurrence_events` CHECKs.
    pub identity_a: u64,
    /// Higher identity second.
    pub identity_b: u64,
    /// The node that saw both.
    pub observer_node_id: Vec<u8>,
    /// The macro cell they overlapped in.
    pub geo_cell_macro: H3Index,
    /// The UTC day the co-presence falls on, which is what makes this row converge with
    /// the same pass run again.
    pub day: chrono::NaiveDate,
    /// First and last overlapping observation that day.
    pub window_start: DateTime<Utc>,
    /// See [`Self::window_start`].
    pub window_end: DateTime<Utc>,
    /// Overlapping pairs counted.
    pub sample_count: usize,
}

/// A whole pass: the decisions, and what they added up to.
#[derive(Debug, Clone, Serialize)]
pub struct ReplayReport {
    /// One line per observation, in time order.
    pub decisions: Vec<Decision>,
    /// One line per identity the pass produced.
    pub identities: Vec<IdentityRecord>,
    /// Co-presence found between identities.
    pub co_presences: Vec<CoPresence>,
}

impl ReplayReport {
    /// Identities produced.
    pub fn identity_count(&self) -> usize {
        self.identities.len()
    }

    /// Merges performed.
    pub fn merge_count(&self) -> usize {
        self.decisions.iter().filter(|d| d.is_merge()).count()
    }

    /// Refusals, by reason.
    pub fn refusals(&self) -> Vec<(&'static str, usize)> {
        let mut counts: Vec<(&'static str, usize)> = self
            .decisions
            .iter()
            .filter(|d| {
                matches!(
                    d.decision,
                    DecisionKind::BelowThreshold
                        | DecisionKind::InsufficientEvidence
                        | DecisionKind::Contradicted
                )
            })
            .fold(HashMap::new(), |mut counts, d| {
                *counts.entry(d.decision.as_str()).or_default() += 1;
                counts
            })
            .into_iter()
            .collect();
        counts.sort();
        counts
    }

    /// Mean coverage across every decision that considered a candidate.
    ///
    /// This is the number that says how much of the identity model the data being
    /// replayed can actually support, and it is usually far lower than the thresholds
    /// were written for.
    pub fn mean_coverage(&self) -> f64 {
        let scored: Vec<f64> = self.decisions.iter().filter_map(|d| d.coverage).collect();
        if scored.is_empty() {
            return 0.0;
        }
        scored.iter().sum::<f64>() / scored.len() as f64
    }

    /// The pass as text, for a terminal or a log file.
    pub fn render_text(&self) -> String {
        use std::fmt::Write;

        let mut out = String::new();
        let _ = writeln!(
            out,
            "== device identity replay: {} observations, {} identities, {} merges, mean \
             coverage {:.2} ==",
            self.decisions.len(),
            self.identity_count(),
            self.merge_count(),
            self.mean_coverage(),
        );

        for decision in &self.decisions {
            let score = decision
                .score
                .map_or_else(|| "     -".to_string(), |value| format!("{value:6.1}"));
            let coverage = decision
                .coverage
                .map_or_else(|| "  -  ".to_string(), |value| format!("{value:4.2}"));
            let _ = writeln!(
                out,
                "{:>4} {} {:<12} {:<8} -> #{:<3} {:<21} cov {coverage} score {score}",
                decision.seq,
                decision.observed_at.format("%Y-%m-%dT%H:%M:%S"),
                decision.identifier,
                decision.identifier_source,
                decision.identity,
                decision.decision.as_str(),
            );

            if !decision.top_features.is_empty() {
                let _ = writeln!(out, "       carried: {}", carried(&decision.top_features));
            }
            if let Some(feature) = decision.contradicted_by {
                let _ = writeln!(out, "       vetoed by a directly-observed {feature}");
            }
            if decision.is_merge() && !decision.with_identifiers.is_empty() {
                let _ = writeln!(
                    out,
                    "       joined to: {}",
                    decision.with_identifiers.join(", ")
                );
            }
            if !decision.missing.is_empty() {
                let _ = writeln!(out, "       not observed: {}", carried(&decision.missing));
            }
        }

        let refusals = self.refusals();
        if !refusals.is_empty() {
            let _ = writeln!(
                out,
                "-- refusals: {} --",
                refusals
                    .iter()
                    .map(|(reason, count)| format!("{reason} {count}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }

        let _ = writeln!(out, "-- identities --");
        for identity in &self.identities {
            let _ = writeln!(
                out,
                "#{:<3} fingerprint {} obs {:>3} conf {:.2} {}",
                identity.identity,
                hex_prefix(&identity.fingerprint.hash()),
                identity.observation_count,
                identity.confidence,
                identity.first_seen.format("%Y-%m-%dT%H:%M:%S"),
            );
            let _ = writeln!(
                out,
                "       identifiers: {}",
                identity
                    .identifiers
                    .iter()
                    .map(|id| format!("{}({})", id.identifier, id.observation_count))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            if !identity.fingerprint.names.is_empty() {
                let _ = writeln!(
                    out,
                    "       names: {}",
                    identity
                        .fingerprint
                        .names
                        .iter()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" | ")
                );
            }
        }

        if !self.co_presences.is_empty() {
            let _ = writeln!(out, "-- co-presence --");
            for presence in &self.co_presences {
                let _ = writeln!(
                    out,
                    "#{} + #{} in {} on {}: {} samples, {} .. {}",
                    presence.identity_a,
                    presence.identity_b,
                    presence.geo_cell_macro.0,
                    presence.day,
                    presence.sample_count,
                    presence.window_start.format("%H:%M:%S"),
                    presence.window_end.format("%H:%M:%S"),
                );
            }
        }

        out
    }
}

/// Features and the points or weight each carried, for a report line.
fn carried(features: &[(&'static str, f64)]) -> String {
    features
        .iter()
        .map(|(feature, value)| format!("{feature} {value:.0}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A readable prefix of a hash, for a report line.
fn hex_prefix(bytes: &[u8]) -> String {
    hex::encode(bytes).chars().take(12).collect()
}

/// Runs the pass.
///
/// The input does not have to be sorted — a pass that replayed out of order would compute
/// time continuity backwards and report merges no live run could have made, so the sort is
/// part of what this function guarantees rather than a courtesy to the caller.
pub fn replay(observations: &[FeedObservation], options: &ReplayOptions) -> ReplayReport {
    let mut ordered: Vec<&FeedObservation> = observations.iter().collect();
    ordered.sort_by_key(|feed| {
        (
            feed.occurred_at.timestamp(),
            feed.occurred_at.timestamp_subsec_nanos(),
        )
    });

    let mut resolver = HeuristicIdentityResolver::new(options.config.clone());
    let mut tracked: HashMap<u64, Tracked> = HashMap::new();
    let mut recent: Vec<Recent> = Vec::new();
    let mut presences: HashMap<PresenceKey, CoPresence> = HashMap::new();
    let mut decisions = Vec::with_capacity(ordered.len());

    for (index, feed) in ordered.iter().enumerate() {
        resolver.expire(feed.observation.timestamp);

        let resolution = resolver.resolve(feed.observation.clone());
        let identity = resolution.identity.id();

        // Captured before this observation is folded in: the question a reviewer asks is
        // "what was already in that identity", and after the absorb the answer includes
        // the observation being reviewed.
        let with_identifiers = tracked
            .get(&identity)
            .map(|tracked| tracked.identifier_names())
            .unwrap_or_default();

        tracked
            .entry(identity)
            .or_insert_with(|| Tracked::new(identity))
            .absorb(feed);

        let evidence = resolution.outcome.evidence();
        decisions.push(Decision {
            seq: index + 1,
            occurrence_id: feed.occurrence_id,
            observed_at: feed.occurred_at,
            identifier: feed.identifier.hex(),
            identifier_source: feed.identifier.source,
            identity,
            decision: DecisionKind::from_outcome(&resolution.outcome),
            score: evidence.map(|evidence| evidence.total_score),
            coverage: evidence.map(MatchEvidence::coverage),
            contradicted_by: resolution.outcome.contradicted_feature(),
            top_features: evidence
                .map(|evidence| evidence.top_features(4))
                .unwrap_or_default(),
            missing: feed
                .observation
                .sources
                .missing(&options.config.weights)
                .into_iter()
                .map(|(feature, weight)| (feature.label(), weight))
                .collect(),
            with_identifiers,
        });

        if let (Some(node), Some(cell)) = (feed.observer_node_id.clone(), feed.geo_cell_macro) {
            note_co_presence(
                &mut recent,
                &mut presences,
                Recent {
                    at: feed.occurred_at,
                    node,
                    cell,
                    identity,
                },
                options.co_presence_window,
            );
        }
    }

    let mut identities: Vec<IdentityRecord> = tracked
        .values()
        .map(|tracked| tracked.record(&resolver))
        .collect();
    identities.sort_by_key(|record| record.identity);

    let mut co_presences: Vec<CoPresence> = presences.into_values().collect();
    co_presences.sort_by_key(|presence| (presence.identity_a, presence.identity_b, presence.day));

    ReplayReport {
        decisions,
        identities,
        co_presences,
    }
}

/// What one node saw recently, for co-presence.
struct Recent {
    at: DateTime<Utc>,
    node: Vec<u8>,
    cell: H3Index,
    identity: u64,
}

/// What makes one co-presence row: a pair, the node, the cell, and the UTC day.
///
/// The day is in the key so that re-running the pass over the same data re-records the
/// same rows instead of inventing a new window each time, and so that a pair seen on
/// three days produces three events — which is what `distinct_days` on the association
/// edge is then counting.
type PresenceKey = (u64, u64, Vec<u8>, H3Index, chrono::NaiveDate);

/// Notes the new observation against everything still inside the window.
///
/// Co-presence requires the same node, the same macro cell, and overlapping time. Two
/// devices seen by two different nodes are not known to be together, however close their
/// cells: the nodes might be the only thing they have in common.
fn note_co_presence(
    recent: &mut Vec<Recent>,
    found: &mut HashMap<PresenceKey, CoPresence>,
    new: Recent,
    window: Duration,
) {
    let window =
        chrono::Duration::from_std(window).unwrap_or_else(|_| chrono::Duration::seconds(120));
    recent.retain(|seen| new.at - seen.at <= window);

    for seen in recent.iter() {
        if seen.node != new.node || seen.cell != new.cell || seen.identity == new.identity {
            continue;
        }

        let (a, b) = if seen.identity < new.identity {
            (seen.identity, new.identity)
        } else {
            (new.identity, seen.identity)
        };
        let day = new.at.date_naive();
        let presence = found
            .entry((a, b, new.node.clone(), new.cell, day))
            .or_insert_with(|| CoPresence {
                identity_a: a,
                identity_b: b,
                observer_node_id: new.node.clone(),
                geo_cell_macro: new.cell,
                day,
                window_start: new.at,
                window_end: new.at,
                sample_count: 0,
            });
        presence.window_start = presence.window_start.min(seen.at).min(new.at);
        presence.window_end = presence.window_end.max(seen.at).max(new.at);
        presence.sample_count += 1;
    }

    recent.push(new);
}

/// The pass's running picture of one identity.
///
/// `bt_iden` keeps learned features of its own, but they are internal state, and they do
/// not include what only the row knows — which node saw it, under which identifier, how
/// many times. The fingerprint needs all of it, so the pass accumulates alongside, from
/// the observations it fed in.
struct Tracked {
    identity: u64,
    observation_count: usize,
    first_seen: DateTime<Utc>,
    last_seen: DateTime<Utc>,
    manufacturer_ids: BTreeSet<u16>,
    service_uuids: BTreeSet<String>,
    field_layouts: BTreeSet<String>,
    names: BTreeSet<String>,
    identifier_sources: BTreeSet<String>,
    identifiers: HashMap<IdentifierKey, IdentifierRecord>,
}

/// One identifier under one identity, keyed by everything that distinguishes it.
type IdentifierKey = (Option<Vec<u8>>, Option<Vec<u8>>, String);

impl Tracked {
    fn new(identity: u64) -> Self {
        Self {
            identity,
            observation_count: 0,
            first_seen: DateTime::<Utc>::MIN_UTC,
            last_seen: DateTime::<Utc>::MIN_UTC,
            manufacturer_ids: BTreeSet::new(),
            service_uuids: BTreeSet::new(),
            field_layouts: BTreeSet::new(),
            names: BTreeSet::new(),
            identifier_sources: BTreeSet::new(),
            identifiers: HashMap::new(),
        }
    }

    fn absorb(&mut self, feed: &FeedObservation) {
        if self.observation_count == 0 {
            self.first_seen = feed.occurred_at;
            self.last_seen = feed.occurred_at;
        }
        self.observation_count += 1;
        self.first_seen = self.first_seen.min(feed.occurred_at);
        self.last_seen = self.last_seen.max(feed.occurred_at);

        let observation = &feed.observation;
        if let Some(manufacturer) = observation.manufacturer_id {
            self.manufacturer_ids.insert(manufacturer);
        }
        self.service_uuids.extend(
            observation
                .service_uuids
                .iter()
                .map(|uuid| uuid.simple().to_string()),
        );
        if !observation.field_layout.is_empty() {
            // Sorted, so the same set of structures in a different order in the packet is
            // the same layout. `bt_iden` scores the order; the key does not, because an
            // identity is not supposed to change just because a device reordered its
            // advertisement.
            let mut layout = observation.field_layout.clone();
            layout.sort_unstable();
            self.field_layouts.insert(hex::encode(layout));
        }
        if let Some(name) = &observation.local_name {
            self.names.insert(name.clone());
        }
        self.identifier_sources
            .insert(feed.identifier.source.to_string());

        let key = (
            feed.device_hash.clone(),
            feed.observer_node_id.clone(),
            feed.identifier.hex(),
        );
        let identifier = self
            .identifiers
            .entry(key)
            .or_insert_with(|| IdentifierRecord {
                device_hash: feed.device_hash.clone(),
                observer_node_id: feed.observer_node_id.clone(),
                identifier: feed.identifier.hex(),
                source: feed.identifier.source,
                observation_count: 0,
                first_seen: feed.occurred_at,
                last_seen: feed.occurred_at,
            });
        identifier.observation_count += 1;
        identifier.first_seen = identifier.first_seen.min(feed.occurred_at);
        identifier.last_seen = identifier.last_seen.max(feed.occurred_at);
    }

    fn identifier_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .identifiers
            .keys()
            .map(|(_, _, name)| name.clone())
            .collect();
        names.sort();
        names
    }

    fn record(&self, resolver: &HeuristicIdentityResolver) -> IdentityRecord {
        let mut identifiers: Vec<IdentifierRecord> = self.identifiers.values().cloned().collect();
        identifiers.sort_by(|a, b| a.identifier.cmp(&b.identifier));

        IdentityRecord {
            identity: self.identity,
            fingerprint: DeviceFingerprint {
                // One manufacturer id identifies. Two means the identity has absorbed
                // advertisements from two companies, and is not keyed by either.
                manufacturer_id: if self.manufacturer_ids.len() == 1 {
                    self.manufacturer_ids.iter().next().copied()
                } else {
                    None
                },
                service_uuids: self.service_uuids.clone(),
                field_layouts: self.field_layouts.clone(),
                names: self.names.clone(),
                identifier_source: if self.identifier_sources.len() == 1 {
                    self.identifier_sources
                        .iter()
                        .next()
                        .cloned()
                        .unwrap_or_default()
                } else {
                    "mixed".to_string()
                },
            },
            confidence: resolver.confidence_of(self.identity),
            observation_count: self.observation_count,
            first_seen: self.first_seen,
            last_seen: self.last_seen,
            identifiers,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bt_iden::models::{AddressType, AdvertisementObservation, BluetoothAddress};
    use bt_iden::time::ObservationTime;
    use std::time::{Duration, UNIX_EPOCH};

    const LAT: f64 = 40.6892;
    const LON: f64 = -74.0445;

    fn cell(lat: f64) -> H3Index {
        H3Index::from(repo::geo::macro_cell(lat, LON).unwrap())
    }

    fn wall(secs: i64) -> ObservationTime {
        ObservationTime::from_wall(UNIX_EPOCH + Duration::from_secs(secs as u64))
    }

    fn at(secs: i64) -> DateTime<Utc> {
        chrono::DateTime::from_timestamp(secs, 0).unwrap()
    }

    /// A device that says who it is: manufacturer id, name, and advertisement layout.
    ///
    /// This is the evidence the resolver was designed around, so a merge here should
    /// happen; the thin-feed tests below take it away again.
    fn talkative(
        secs: i64,
        address: [u8; 6],
        name: &str,
        manufacturer: u16,
    ) -> AdvertisementObservation {
        AdvertisementObservation::new(
            wall(secs),
            BluetoothAddress::new(address),
            AddressType::Public,
        )
        .with_local_name_datum(name.to_string(), bt_iden::Datum::Direct)
        .with_manufacturer_data(manufacturer, vec![0x02, 0x15, 0x01])
        .with_field_layout(vec![0x01, 0x03, 0x09, 0xff])
    }

    /// The same, with nothing but a name — the shape of most rows actually stored.
    fn name_only(secs: i64, address: [u8; 6], name: &str) -> AdvertisementObservation {
        AdvertisementObservation::new(
            wall(secs),
            BluetoothAddress::new(address),
            AddressType::Public,
        )
        .with_local_name_datum(name.to_string(), bt_iden::Datum::Direct)
    }

    fn feed(observation: AdvertisementObservation, node: &[u8], lat: f64) -> FeedObservation {
        let identifier_hex = hex::encode(observation.address.as_bytes());
        FeedObservation {
            identifier: super::super::adapt::ObservedIdentifier {
                bytes: *observation.address.as_bytes(),
                source: "ble_mac",
                reported: true,
            },
            occurred_at: at(observation.timestamp.micros_since_epoch().unwrap() as i64 / 1_000_000),
            observer_node_id: Some(node.to_vec()),
            geo_cell_macro: Some(cell(lat)),
            device_hash: Some(hex::decode(&identifier_hex).unwrap()),
            occurrence_id: None,
            observation,
        }
    }

    fn node(seed: u8) -> Vec<u8> {
        vec![seed; 32]
    }

    fn options() -> ReplayOptions {
        ReplayOptions {
            config: ResolverConfig::new(),
            co_presence_window: Duration::from_secs(120),
        }
    }

    #[test]
    fn a_device_that_changed_its_address_is_still_one_device() {
        // The whole point of the feature: the address rotates, the device does not, and
        // the identity the pass hands back is one record whose two identifiers are both on
        // file. If this regressed, every rotation would look like a new device arriving.
        let observations = vec![
            feed(
                talkative(1_000, [0xf0, 0xee, 0, 0, 0, 1], "Sensor A", 0x004c),
                &node(1),
                LAT,
            ),
            feed(
                talkative(1_010, [0xf0, 0xee, 0, 0, 9, 9], "Sensor A", 0x004c),
                &node(1),
                LAT,
            ),
        ];

        let report = replay(&observations, &options());

        assert_eq!(
            report.identity_count(),
            1,
            "one device became two identities"
        );
        assert_eq!(report.merge_count(), 1);
        assert_eq!(report.decisions[1].with_identifiers, vec!["f0ee00000001"]);
        assert_eq!(report.identities[0].identifiers.len(), 2);
        assert_eq!(report.identities[0].observation_count, 2);
    }

    #[test]
    fn a_different_manufacturer_is_not_the_same_device() {
        // Same name, same layout, different company id: a directly-observed feature says
        // these are two products, and a merge that ignored it would be a wrong answer with
        // a confident score attached.
        let observations = vec![
            feed(
                talkative(1_000, [0xf0, 0xee, 0, 0, 0, 1], "Sensor", 0x004c),
                &node(1),
                LAT,
            ),
            feed(
                talkative(1_010, [0xf0, 0xee, 0, 0, 9, 9], "Sensor", 0x004d),
                &node(1),
                LAT,
            ),
        ];

        let report = replay(&observations, &options());

        assert_eq!(
            report.merge_count(),
            0,
            "a direct contradiction was merged anyway"
        );
        assert_eq!(report.identity_count(), 2);
    }

    #[test]
    fn a_name_alone_is_not_enough_to_call_it_the_same_device() {
        // 25 of 185 points, and 25 is below the merge threshold. It also used to be the
        // only evidence the stored rows could offer at all, which is exactly why this is
        // pinned rather than assumed.
        let observations = vec![
            feed(
                name_only(1_000, [0xf0, 0xee, 0, 0, 0, 1], "Beacon"),
                &node(1),
                LAT,
            ),
            feed(
                name_only(1_010, [0xf0, 0xee, 0, 0, 9, 9], "Beacon"),
                &node(1),
                LAT,
            ),
        ];

        let report = replay(&observations, &options());

        assert_eq!(
            report.merge_count(),
            0,
            "two devices with the same name became one"
        );
        let refusal = report.decisions[1].decision;
        assert!(
            matches!(
                refusal,
                DecisionKind::BelowThreshold | DecisionKind::InsufficientEvidence
            ),
            "expected a refusal that says why, got {refusal:?}"
        );
        assert!(
            report.decisions[1]
                .missing
                .iter()
                .any(|(feature, _)| *feature == "manufacturer_id"),
            "the report should name what the feed cost it"
        );
    }

    #[test]
    fn replaying_the_same_data_twice_says_the_same_thing() {
        // A batch that is not deterministic cannot be reviewed: a reader comparing two
        // runs could not tell a changed rule from a different row order.
        let observations = vec![
            feed(
                talkative(1_000, [0xf0, 0xee, 0, 0, 0, 1], "A", 0x004c),
                &node(1),
                LAT,
            ),
            feed(
                name_only(1_010, [0xf0, 0xee, 0, 0, 0, 2], "B"),
                &node(1),
                LAT,
            ),
            feed(
                talkative(1_020, [0xf0, 0xee, 0, 0, 9, 9], "A", 0x004c),
                &node(1),
                LAT,
            ),
        ];

        assert_eq!(
            replay(&observations, &options()).render_text(),
            replay(&observations, &options()).render_text()
        );
    }

    #[test]
    fn out_of_order_rows_are_replayed_in_order() {
        // Time continuity is a scored feature, so a pass fed newest-first would score the
        // past as if it came later. The sort is inside the pass for that reason.
        let a = feed(
            talkative(1_000, [0xf0, 0xee, 0, 0, 0, 1], "A", 0x004c),
            &node(1),
            LAT,
        );
        let b = feed(
            talkative(1_010, [0xf0, 0xee, 0, 0, 9, 9], "A", 0x004c),
            &node(1),
            LAT,
        );

        let report = replay(&[b.clone(), a], &options());

        assert_eq!(report.decisions[0].observed_at, at(1_000));
        assert_eq!(report.decisions[1].observed_at, at(1_010));
        assert_eq!(report.merge_count(), 1);
    }

    #[test]
    fn two_devices_seen_together_by_one_node_are_an_association() {
        let phone = |secs: i64| {
            feed(
                talkative(secs, [0x11, 0, 0, 0, 0, 1], "Phone", 0x004c),
                &node(1),
                LAT,
            )
        };
        let beacon = |secs: i64| {
            feed(
                talkative(secs, [0x22, 0, 0, 0, 0, 1], "Beacon", 0x0123),
                &node(1),
                LAT,
            )
        };

        let report = replay(
            &[phone(1_000), beacon(1_005), phone(1_010), beacon(1_015)],
            &options(),
        );

        assert_eq!(report.identity_count(), 2);
        assert_eq!(report.co_presences.len(), 1, "one pair, one cell, one day");
        let presence = &report.co_presences[0];
        assert!(
            presence.identity_a < presence.identity_b,
            "the pair must be canonical"
        );
        assert_eq!(
            presence.sample_count, 4,
            "every overlapping pair should count"
        );
        assert_eq!(presence.window_start, at(1_000));
        assert_eq!(presence.window_end, at(1_015));
    }

    #[test]
    fn two_nodes_seeing_two_devices_is_not_co_presence() {
        // The nodes might be the only thing these sightings have in common. Recording it
        // anyway would turn network topology into device association.
        let phone = |secs: i64| {
            feed(
                talkative(secs, [0x11, 0, 0, 0, 0, 1], "Phone", 0x004c),
                &node(1),
                LAT,
            )
        };
        let beacon = |secs: i64| {
            feed(
                talkative(secs, [0x22, 0, 0, 0, 0, 1], "Beacon", 0x0123),
                &node(2),
                LAT,
            )
        };

        // Interleaved in time and in the same macro cell; the only thing separating them
        // is which radio heard them.
        let report = replay(
            &[phone(1_000), beacon(1_005), phone(1_010), beacon(1_015)],
            &options(),
        );

        assert_eq!(report.identity_count(), 2);
        assert!(
            report.co_presences.is_empty(),
            "co-presence was recorded across observers: {:?}",
            report.co_presences
        );
    }

    #[test]
    fn a_pair_seen_on_two_days_produces_two_events() {
        // `distinct_days` on the association edge counts events per day, so the day has to
        // be part of the co-presence key or a pair seen all week looks like one long day.
        let phone = |secs: i64| {
            feed(
                talkative(secs, [0x11, 0, 0, 0, 0, 1], "Phone", 0x004c),
                &node(1),
                LAT,
            )
        };
        let beacon = |secs: i64| {
            feed(
                talkative(secs, [0x22, 0, 0, 0, 0, 1], "Beacon", 0x0123),
                &node(1),
                LAT,
            )
        };
        let day = 86_400;

        let report = replay(
            &[
                phone(1_000),
                beacon(1_005),
                phone(1_000 + day),
                beacon(1_005 + day),
            ],
            &options(),
        );

        assert_eq!(report.co_presences.len(), 2);
        assert_ne!(report.co_presences[0].day, report.co_presences[1].day);
    }

    #[test]
    fn co_presence_outside_the_window_is_not_together() {
        let phone = |secs: i64| {
            feed(
                talkative(secs, [0x11, 0, 0, 0, 0, 1], "Phone", 0x004c),
                &node(1),
                LAT,
            )
        };
        let beacon = |secs: i64| {
            feed(
                talkative(secs, [0x22, 0, 0, 0, 0, 1], "Beacon", 0x0123),
                &node(1),
                LAT,
            )
        };

        let report = replay(&[phone(1_000), beacon(1_000 + 600)], &options());

        assert!(
            report.co_presences.is_empty(),
            "ten minutes apart is not together"
        );
    }

    #[test]
    fn the_report_shows_the_evidence_and_the_cost() {
        let observations = vec![
            feed(
                talkative(1_000, [0xf0, 0xee, 0, 0, 0, 1], "A", 0x004c),
                &node(1),
                LAT,
            ),
            feed(
                name_only(1_010, [0xf0, 0xee, 0, 0, 9, 9], "A"),
                &node(1),
                LAT,
            ),
        ];

        let text = replay(&observations, &options()).render_text();

        assert!(text.contains("not observed:"), "{text}");
        assert!(text.contains("manufacturer_id"), "{text}");
        assert!(text.contains("identities"), "{text}");
    }

    #[test]
    fn mean_coverage_says_how_thin_the_data_is() {
        // The headline number of the whole exercise: a database of name-and-address rows
        // supports a small fraction of the model, and a threshold written for the full
        // model is not the same threshold here.
        let thin = vec![
            feed(
                name_only(1_000, [0xf0, 0xee, 0, 0, 0, 1], "A"),
                &node(1),
                LAT,
            ),
            feed(
                name_only(1_010, [0xf0, 0xee, 0, 0, 0, 2], "B"),
                &node(1),
                LAT,
            ),
        ];
        let full = vec![
            feed(
                talkative(1_000, [0xf0, 0xee, 0, 0, 0, 1], "A", 0x004c),
                &node(1),
                LAT,
            ),
            feed(
                talkative(1_010, [0xf0, 0xee, 0, 0, 9, 9], "A", 0x004c),
                &node(1),
                LAT,
            ),
        ];

        let thin_coverage = replay(&thin, &options()).mean_coverage();
        let full_coverage = replay(&full, &options()).mean_coverage();

        assert!(
            thin_coverage < full_coverage,
            "{thin_coverage} vs {full_coverage}"
        );
        assert!(
            thin_coverage < 0.25,
            "a name-only feed should not look well-covered"
        );
    }
}
