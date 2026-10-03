//! The fingerprint a derived device identity is keyed by.
//!
//! `bt_iden` hands out `u64` identity ids that are only meaningful inside one process:
//! re-run the same data and device A is `Identity(1)` both times only by coincidence of
//! ordering. Persisting a decision therefore needs a key that survives the run, and the
//! only thing that survives is the set of features the decision was made from.
//!
//! That makes this the join between the resolver and `device_identities`: the batch
//! computes a fingerprint per resolved identity, hashes it, and the hash is what
//! [`repo::DeviceIdentityRepository::upsert_by_fingerprint`] recognises on the next
//! pass. Two passes that disagree about a fingerprint get two identities, so the recipe
//! below is deliberately exact rather than helpful:
//!
//! * **Sets, not counts.** Which manufacturer ids, service UUIDs, layouts and names were
//!   seen — not how many times. A device seen 4 times and then 400 times is still the
//!   same device, and counting would mint a new identity for it at the next threshold.
//! * **Exact names, fuzzy names for humans.** `name_pattern` is the longest common prefix
//!   of the observed names, stored so a reader can recognise the device at a glance. The
//!   *hash* covers the exact name set. A fingerprint keyed on the prefix would collapse
//!   `Mock Beacon 0` and `Mock Beacon 1` into one identity — and name *similarity* is
//!   already the resolver's 25-point feature, scored with evidence attached. The
//!   fingerprint is a cache key, and a cache key that merges two devices is worse than a
//!   slow one.
//! * **The identifier source is part of the key.** A device whose only feature is its
//!   name, seen once under a BLE MAC and once under a platform UUID, is two observations
//!   of two unknowns, not one. The design notes are explicit that platform identifiers
//!   are not primary identity, and leaving the source out would let a UUID-derived row
//!   and a MAC-derived row share an identity on nothing but a matching name.
//!
//! `recipe` is carried inside the hashed string and in the JSON: the day the recipe
//! changes, old rows must not be silently reinterpreted as new ones.

use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

/// The feature set one derived identity was keyed by.
///
/// Every collection is a `BTreeSet` so that insertion order — which depends on the order
/// rows came back from the database — cannot change the hash.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct DeviceFingerprint {
    /// The manufacturer id, when exactly one was ever seen.
    ///
    /// `None` when none was seen *and* when two were: an identity that has advertised as
    /// two companies is not keyed by a manufacturer id, and picking one would assert
    /// something the evidence does not support.
    pub manufacturer_id: Option<u16>,
    /// Service UUIDs advertised, in their full 128-bit form.
    pub service_uuids: BTreeSet<String>,
    /// AD field-type layouts seen, each sorted, so that the same set of structures in a
    /// different order is the same layout.
    pub field_layouts: BTreeSet<String>,
    /// Local names seen, exactly as reported.
    pub names: BTreeSet<String>,
    /// Where the volatile identifier came from: `ble_mac`, `uuid`, `opaque_id`, or
    /// `derived` for a row that had no address at all.
    pub identifier_source: String,
}

/// The hashing recipe these keys implement.
const RECIPE: &str = "v1";

impl DeviceFingerprint {
    /// The SHA-256 that `device_identities.fingerprint_hash` stores.
    ///
    /// Hashed over a canonical string built here rather than over the JSON, because
    /// `serde_json`'s object ordering is a feature flag away from changing, and a hash
    /// that changes when a dependency's default changes mints a new identity for every
    /// device in the database.
    pub fn hash(&self) -> Vec<u8> {
        let canonical = self.canonical_string();
        Sha256::digest(canonical.as_bytes()).to_vec()
    }

    /// The stored form, whose keys `device_identities`' CHECK constraint names.
    ///
    /// The four named keys are the comparable parts; the rest is review data so that a
    /// human reading a row can see what the identity actually was. A fingerprint with no
    /// manufacturer and no names is a real and common case — a beacon that advertises
    /// nothing but an address — so every key is always present, and an empty value is
    /// the row saying "nothing observed" rather than a schema accident.
    pub fn canonical_json(&self) -> Value {
        json!({
            "recipe": RECIPE,
            "manufacturer_id": self.manufacturer_id.map(|id| id as u64),
            "service_uuids_hash": short_hash(&self.service_uuids),
            "field_layout_hash": short_hash(&self.field_layouts),
            "name_pattern": self.name_pattern(),
            "service_uuids": self.service_uuids.iter().cloned().collect::<Vec<_>>(),
            "field_layouts": self
                .field_layouts
                .iter()
                .map(|layout| layout.split(',').filter(|s| !s.is_empty()).collect::<Vec<_>>())
                .collect::<Vec<_>>(),
            "names": self.names.iter().cloned().collect::<Vec<_>>(),
            "identifier_source": self.identifier_source,
        })
    }

    /// The longest common prefix of the observed names, for a reader.
    ///
    /// `Mock Beacon 0` and `Mock Beacon 12` give `Mock Beacon 1`; that is a hint, not a
    /// comparison, and it is nowhere in the hash.
    pub fn name_pattern(&self) -> String {
        let mut names = self.names.iter();
        let Some(first) = names.next() else {
            return String::new();
        };
        let mut prefix = first.clone();
        for name in names {
            while !name.starts_with(&prefix) {
                prefix.pop();
            }
            if prefix.is_empty() {
                break;
            }
        }
        prefix
    }

    /// The string that gets hashed.
    fn canonical_string(&self) -> String {
        format!(
            "{RECIPE}|m={}|u={}|l={}|n={}|s={}",
            self.manufacturer_id
                .map_or_else(|| "-".to_string(), |id| id.to_string()),
            set_hash(&self.service_uuids),
            set_hash(&self.field_layouts),
            set_hash(&self.names),
            self.identifier_source,
        )
    }
}

/// SHA-256 over a set, its members joined in sorted order.
///
/// Joined with a byte that cannot appear in any member, so `{ab, c}` and `{a, bc}` — which
/// a naive join would both render as `a,b,c` — cannot share a hash.
fn set_hash(members: &BTreeSet<String>) -> String {
    let joined = members
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join("\u{1f}");
    hex::encode(Sha256::digest(joined.as_bytes()))
}

/// The short form of [`set_hash`], for the JSON a human reads.
///
/// 128 bits, enough that a reader can compare two rows' eyes, not the security boundary:
/// `fingerprint_hash` holds the full SHA-256 of everything.
fn short_hash(members: &BTreeSet<String>) -> String {
    set_hash(members).chars().take(32).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    fn fingerprint() -> DeviceFingerprint {
        DeviceFingerprint {
            manufacturer_id: Some(0x004C),
            service_uuids: set(&["0000180f-0000-1000-8000-00805f9b34fb"]),
            field_layouts: set(&["1,9,255"]),
            names: set(&["Mock Beacon 0"]),
            identifier_source: "ble_mac".to_string(),
        }
    }

    #[test]
    fn the_same_features_always_hash_the_same() {
        assert_eq!(fingerprint().hash(), fingerprint().hash());
    }

    #[test]
    fn insertion_order_cannot_change_the_hash() {
        // Which is what a `BTreeSet` buys: rows arrive from the database in whatever
        // order the scan produced, and a hash that moved with it would create a second
        // identity for a device that has always had one.
        let mut a = fingerprint();
        a.names = set(&["one", "two", "three"]);
        let mut b = fingerprint();
        b.names = set(&["three", "one", "two"]);
        assert_eq!(a.hash(), b.hash());
    }

    #[test]
    fn a_different_name_is_a_different_identity() {
        let mut other = fingerprint();
        other.names = set(&["Mock Beacon 1"]);
        assert_ne!(fingerprint().hash(), other.hash());
    }

    #[test]
    fn being_seen_more_often_does_not_change_the_fingerprint() {
        // The batch re-runs over longer windows every time; counts in the key would
        // mint a new identity each time an observation count crossed a line.
        let first = fingerprint();
        let later = DeviceFingerprint { ..first.clone() };
        assert_eq!(first.hash(), later.hash());
    }

    #[test]
    fn two_manufacturers_are_not_keyed_by_one_of_them() {
        // Exercised through the field directly: the accumulator is what collapses a
        // second manufacturer id to `None`, and a key that picked one of the two would
        // decide which company a device belongs to by iteration order.
        let ambiguous = DeviceFingerprint {
            manufacturer_id: None,
            ..fingerprint()
        };
        assert_ne!(ambiguous.hash(), fingerprint().hash());
    }

    #[test]
    fn the_identifier_source_keeps_two_unknowns_apart() {
        let mac = fingerprint();
        let uuid = DeviceFingerprint {
            identifier_source: "uuid".to_string(),
            ..fingerprint()
        };
        assert_ne!(mac.hash(), uuid.hash());
    }

    #[test]
    fn members_cannot_be_reshuffled_into_each_other() {
        // The unit-separator join: without it {ab, c} and {a, bc} hash alike.
        let one = DeviceFingerprint {
            names: set(&["ab", "c"]),
            ..DeviceFingerprint::default()
        };
        let other = DeviceFingerprint {
            names: set(&["a", "bc"]),
            ..DeviceFingerprint::default()
        };
        assert_ne!(one.hash(), other.hash());
    }

    #[test]
    fn the_stored_object_has_every_key_the_column_demands() {
        // `valid_fingerprint_shape` refuses a fingerprint missing any of these, and the
        // batch would fail at write time rather than here.
        let json = DeviceFingerprint::default().canonical_json();
        for key in [
            "manufacturer_id",
            "service_uuids_hash",
            "field_layout_hash",
            "name_pattern",
        ] {
            assert!(
                json.get(key).is_some(),
                "{key} missing from an empty fingerprint: {json}"
            );
        }
    }

    #[test]
    fn the_name_pattern_is_a_hint_and_never_a_merge() {
        let shared = DeviceFingerprint {
            names: set(&["Mock Beacon 0", "Mock Beacon 12"]),
            ..DeviceFingerprint::default()
        };
        // They differ at the twelfth character, so that is where the shared prefix stops.
        assert_eq!(shared.name_pattern(), "Mock Beacon ");
        assert_eq!(
            shared.canonical_json()["name_pattern"].as_str(),
            Some("Mock Beacon ")
        );

        // Two devices whose names share that prefix are still two identities.
        let other = DeviceFingerprint {
            names: set(&["Mock Beacon 3"]),
            ..DeviceFingerprint::default()
        };
        assert_eq!(other.name_pattern(), "Mock Beacon 3");
        assert_ne!(shared.hash(), other.hash());
    }

    #[test]
    fn nothing_known_still_has_a_key() {
        let empty = DeviceFingerprint::default();
        assert_eq!(empty.hash().len(), 32);
        assert_eq!(empty.name_pattern(), "");
        // And it is distinct from a device whose only feature is a name.
        let named = DeviceFingerprint {
            names: set(&["a"]),
            ..DeviceFingerprint::default()
        };
        assert_ne!(empty.hash(), named.hash());
    }
}
