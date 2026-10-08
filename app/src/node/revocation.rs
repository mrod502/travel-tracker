//! Revocation data as a running node holds it.
//!
//! A CA's revocation status list is the only thing that turns "this node id has
//! never been seen before" into "this node id has not been revoked", and it is
//! perishable: the list is a statement about a validity window, and outside that
//! window its silence about a node means nothing. So a node needs two things, and
//! B11 was the absence of both seen from either side — it had a checker that
//! nothing ever loaded a list into, and it had no question to ask it. The two
//! halves here are [`RevocationWatch::refresh`] (get the CA's current list, verify
//! it, keep it) and [`RevocationWatch::authorize_data`] /
//! [`RevocationWatch::authorize_connection`] (say what that list permits).
//!
//! # When a node cannot check
//!
//! It refuses. `enabled = true` is the operator's decision that an unverifiable
//! list is a reason not to store data, so a node that cannot load a current list
//! from its CA stops recording rather than carrying on as though the list were
//! empty. Startup enforces the same rule with an error instead of a log line: a
//! deployment that comes up healthy and stores nothing is the worst of the
//! outcomes available, because it looks like success.
//!
//! # What is not here
//!
//! The signature check itself. A published list is a JSON document in a column,
//! and [`ca::verified_checker`] refuses one whose signature does not come from the
//! configured anchor's key. This module owns *freshness* and *policy*, never
//! authenticity, and it has no way to read a list that has not already been
//! verified.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use ca::{
    verified_checker, CheckContext, CheckLocation, ConnectionPolicy, DataRecordingPolicy,
    DatabaseRslManager, Decision, InMemoryRslChecker, RevocationChecker, RevocationPolicy,
    RevocationStatus, RslManager, TrustAnchor,
};
use repo::Pool;

use crate::config::RevocationConfig;
use crate::error::{AppError, Result};

/// The CA's latest list, as this node holds it, plus the policies that turn a
/// status into a decision.
pub(crate) struct RevocationWatch {
    /// The CA this node believes. Its id is the issuer every list is looked up
    /// under, so one CA's list can never answer for another.
    anchor: TrustAnchor,

    /// Where published lists are read from. A trait object so the decision logic
    /// here is testable without a database — the verification inside
    /// [`ca::verified_checker`] is exercised either way.
    manager: Arc<dyn RslManager>,

    /// How old a list may be before this node stops trusting what is not in it.
    max_staleness: chrono::Duration,

    /// How often [`refresh`](Self::refresh) is meant to run.
    refresh_interval: Duration,

    /// Built from `max_staleness` rather than defaulted, so the bound the node
    /// loads a list under and the bound it judges decisions by are one number.
    data_policy: DataRecordingPolicy,
    connection_policy: ConnectionPolicy,

    /// The newest list that verified. `None` only between construction and the
    /// first successful [`refresh`](Self::refresh).
    checker: RwLock<Option<InMemoryRslChecker>>,
}

impl RevocationWatch {
    /// A watch that has not loaded anything yet.
    ///
    /// Use [`start`](Self::start) in the node: it reads the anchor from the
    /// configured file and refuses to return a watch that cannot yet answer a
    /// question.
    pub(crate) fn new(
        anchor: TrustAnchor,
        manager: Arc<dyn RslManager>,
        max_staleness: chrono::Duration,
        refresh_interval: Duration,
    ) -> Self {
        Self {
            anchor,
            manager,
            data_policy: DataRecordingPolicy {
                max_cache_age: max_staleness,
                ..Default::default()
            },
            connection_policy: ConnectionPolicy::default(),
            max_staleness,
            refresh_interval,
            checker: RwLock::new(None),
        }
    }

    /// Build the watch from `[revocation]`, load the anchor, and take the CA's
    /// current list.
    ///
    /// # Errors
    ///
    /// A configuration that cannot do its job is an error, not a warning:
    ///
    /// * no anchor path, or a file that is not a trust anchor;
    /// * a CA that has published no list;
    /// * a list that does not verify under the anchor — tampered with, or the
    ///   wrong CA's key configured;
    /// * a list whose window has closed, or which is older than
    ///   `[revocation].max_staleness_secs`.
    ///
    /// Each of those leaves the node unable to tell a revoked node from a valid
    /// one. It could still store what it hears, and every row would carry an
    /// attestation it has no way to support — so it does not start.
    pub(crate) async fn start(config: &RevocationConfig, pool: &Pool) -> Result<Self> {
        let path = config.anchor_path.as_deref().ok_or_else(|| {
            AppError::Config(
                "revocation checking is enabled with no trust anchor: [revocation].anchor_path \
                 must name the CA's SPKI PEM file, written by `app ca ca-export-anchor`"
                    .to_string(),
            )
        })?;

        let anchor = TrustAnchor::load_from_file(std::path::Path::new(path)).map_err(|e| {
            AppError::Config(format!(
                "[revocation].anchor_path {} could not be read as a trust anchor: {e}. It is the \
                 CA's public key in SubjectPublicKeyInfo PEM — `app ca ca-export-anchor` writes \
                 it beside the root key.",
                path
            ))
        })?;

        let watch = Self::new(
            anchor,
            Arc::new(DatabaseRslManager::new(pool.as_pool().clone())),
            chrono_seconds(config.max_staleness_secs),
            Duration::from_secs(config.refresh_secs.max(1)),
        );

        watch.refresh().await?;

        // A list that verifies but is past the staleness bound answers `Unknown`
        // for every node its CA has not revoked, and the data policy turns that
        // into a refusal — permanently, since nothing about a stored list becomes
        // fresher on its own. Say so at startup rather than running a node that
        // answers `no` to everything.
        if !watch.is_fresh() {
            return Err(unusable(format!(
                "CA {}'s newest list is {} seconds old, past this node's bound of {} seconds",
                watch.ca_id_hex(),
                watch.cache_age().num_seconds(),
                watch.max_staleness.num_seconds()
            )));
        }

        Ok(watch)
    }

    /// Read the CA's newest published list and, if it verifies, make it the node's
    /// answer to every revocation question.
    ///
    /// A refresh that fails leaves the previous list in place: it is still the
    /// most recent thing the CA actually signed, and its own age — not this call's
    /// failure — is what decides how much it can be trusted. Dropping to "no data"
    /// because one read failed would be a self-inflicted outage.
    pub(crate) async fn refresh(&self) -> Result<()> {
        let checker = verified_checker(self.manager.as_ref(), &self.anchor, self.max_staleness)
            .await
            .map_err(|e| {
                AppError::Provenance(format!(
                    "could not load a usable revocation list for CA {}: {e}",
                    self.anchor.ca_id_hex()
                ))
            })?;

        let sequence = checker.sequence_number().unwrap_or(0);
        let age = checker.cache_age().num_seconds();

        *self.write() = Some(checker);

        log::info!(
            "Loaded revocation list #{sequence} from CA {} ({} seconds old, trusted up to {} \
             seconds)",
            self.anchor.ca_id_hex(),
            age,
            self.max_staleness.num_seconds()
        );

        Ok(())
    }

    /// May this node store an occurrence attributed to `node_id`?
    ///
    /// `Err` means the revocation data does not support the claim, and the message
    /// says which half is missing: the node is on the CA's list, or the list is too
    /// old to say anything about it. Those are different operational problems, and
    /// nobody reading a log line should have to guess which one they have.
    pub(crate) fn authorize_data(&self, node_id: &[u8]) -> Result<()> {
        let status = self.status_of(node_id)?;
        let age = self.cache_age();

        let context = CheckContext::new(
            CheckLocation::OccurrenceVerification,
            age,
            self.max_staleness,
        );

        match self.data_policy.evaluate(node_id, status, &context) {
            Decision::Accept => Ok(()),
            Decision::AcceptWithWarning => {
                // The policy chose availability: record it, but not without a note
                // that this node's status was never actually established.
                log::warn!(
                    "Recording data for {} with unestablished revocation status (CA {}'s list \
                     neither names it nor covers it)",
                    hex::encode(node_id),
                    self.anchor.ca_id_hex()
                );
                Ok(())
            }
            Decision::Reject => {
                log::warn!(
                    "Refusing to store data from revoked node {}",
                    hex::encode(node_id)
                );
                Err(AppError::Provenance(format!(
                    "node {} is revoked according to CA {}'s list #{}",
                    hex::encode(node_id),
                    self.anchor.ca_id_hex(),
                    self.sequence_number()
                )))
            }
            Decision::Defer => {
                log::warn!(
                    "Refusing to store data for {}: CA {}'s list is {} seconds old, past the {} \
                     second bound",
                    hex::encode(node_id),
                    self.anchor.ca_id_hex(),
                    age.num_seconds(),
                    self.max_staleness.num_seconds()
                );
                Err(AppError::Provenance(format!(
                    "the revocation list from CA {} is {} seconds old, beyond this node's {} \
                     second bound, so {} cannot be shown not to be revoked",
                    self.anchor.ca_id_hex(),
                    age.num_seconds(),
                    self.max_staleness.num_seconds(),
                    hex::encode(node_id)
                )))
            }
        }
    }

    /// Should a peer with this node id be allowed through a handshake?
    ///
    /// Stricter than [`authorize_data`](Self::authorize_data) by design: recording
    /// one more occurrence from a node of unknown status costs a row, while
    /// admitting it to a session costs everything that session touches. So any
    /// uncertainty — revoked, unknown, or no data at all — is a refusal.
    pub(crate) fn authorize_connection(&self, peer_node_id: &[u8]) -> bool {
        let status = match self.status_of(peer_node_id) {
            Ok(status) => status,
            Err(e) => {
                log::warn!(
                    "Rejecting handshake from {}: {e}",
                    hex::encode(peer_node_id)
                );
                return false;
            }
        };

        let context = CheckContext::new(
            CheckLocation::Handshake,
            self.cache_age(),
            self.max_staleness,
        );

        match self
            .connection_policy
            .evaluate(peer_node_id, status, &context)
        {
            Decision::Accept | Decision::AcceptWithWarning => true,
            Decision::Reject | Decision::Defer => {
                log::warn!(
                    "Rejecting handshake from {} ({}): CA {} says {}",
                    hex::encode(peer_node_id),
                    status_label(status),
                    self.anchor.ca_id_hex(),
                    match status {
                        RevocationStatus::Revoked => "it is revoked",
                        _ => "nothing either way, which is not the same as it being valid",
                    }
                );
                false
            }
        }
    }

    /// The CA's current statement about one node id.
    pub(crate) fn status_of(&self, node_id: &[u8]) -> Result<RevocationStatus> {
        let checker = self.checker();
        let Some(checker) = checker.as_ref() else {
            return Err(AppError::Provenance(format!(
                "no revocation list from CA {} has been loaded, so nothing can be said about {}",
                self.anchor.ca_id_hex(),
                hex::encode(node_id)
            )));
        };

        checker.is_revoked(node_id).map_err(|e| {
            AppError::Provenance(format!(
                "revocation check for {} failed: {e}",
                hex::encode(node_id)
            ))
        })
    }

    /// Age of the loaded list; [`chrono::Duration::MAX`] when there is none, which
    /// is the only age that cannot be mistaken for a recently refreshed cache.
    pub(crate) fn cache_age(&self) -> chrono::Duration {
        match self.checker().as_ref() {
            Some(checker) => checker.cache_age(),
            None => chrono::Duration::MAX,
        }
    }

    /// Whether the loaded list is within the staleness bound.
    pub(crate) fn is_fresh(&self) -> bool {
        self.checker()
            .as_ref()
            .is_some_and(InMemoryRslChecker::is_cache_fresh)
    }

    /// Sequence number of the newest list adopted — the anti-replay watermark, and
    /// what an operator asks about when a revocation has not taken effect.
    pub(crate) fn sequence_number(&self) -> u64 {
        self.checker()
            .as_ref()
            .and_then(InMemoryRslChecker::sequence_number)
            .unwrap_or(0)
    }

    pub(crate) fn refresh_interval(&self) -> Duration {
        self.refresh_interval
    }

    /// The CA this node believes, for the startup log and for error messages.
    pub(crate) fn ca_id_hex(&self) -> String {
        self.anchor.ca_id_hex()
    }

    /// One line for the startup log: which CA, which list, how old, how long it is
    /// trusted.
    pub(crate) fn describe(&self) -> String {
        format!(
            "CA {}, list #{}, {} seconds old (bound {} seconds)",
            self.anchor.ca_id_hex(),
            self.sequence_number(),
            self.cache_age().num_seconds(),
            self.max_staleness.num_seconds()
        )
    }

    fn checker(&self) -> std::sync::RwLockReadGuard<'_, Option<InMemoryRslChecker>> {
        // A panic elsewhere in the node must not wedge revocation checking: what
        // this guards is a list that verified, and reading it is safe whatever was
        // in flight when the panic happened.
        self.checker
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Option<InMemoryRslChecker>> {
        self.checker
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// `RevocationStatus` as an operator words it, for lines that have to say which of
/// the two failures happened.
fn status_label(status: RevocationStatus) -> &'static str {
    match status {
        RevocationStatus::Revoked => "revoked",
        RevocationStatus::Valid => "valid",
        RevocationStatus::Unknown => "unknown",
    }
}

/// The startup message for a `[revocation]` section that cannot work, with both
/// ways out named.
fn unusable(cause: impl Into<String>) -> AppError {
    let cause = cause.into();
    AppError::Config(format!(
        "revocation checking is enabled but unusable: {cause}. A node with checking on refuses to \
         start rather than store occurrences it cannot attest to. Publish a current list with \
         `app ca ca-generate-rsl` (and check that [revocation].anchor_path is the anchor that CA \
         exported), or set [revocation].enabled = false to run without checking."
    ))
}

/// `u64` seconds as a `chrono::Duration`, saturating rather than panicking on an
/// absurd configuration value.
fn chrono_seconds(secs: u64) -> chrono::Duration {
    let secs = i64::try_from(secs).unwrap_or(i64::MAX / 1_000);
    chrono::Duration::seconds(secs.clamp(1, i64::MAX / 1_000))
}

/// A signed list revoking one node id, issued `age_days` ago and valid for
/// `valid_for_days` after that.
///
/// Shared with `full`'s database tests so the node-level exit tests and these unit
/// tests are built from the same fixture: a list that means something different in
/// one place than the other would test the fixture, not the node.
#[cfg(test)]
pub(crate) fn list_revoking(
    ca: &ca::CaRoot,
    node_id: &[u8],
    sequence: u64,
    age_days: i64,
    valid_for_days: i64,
) -> ca::RevocationStatusList {
    let mut rsl = ca::RevocationStatusList::builder(ca.ca_id())
        .sequence_number(sequence)
        .add_revocation(ca::RevokedNode::new(
            node_id.to_vec(),
            chrono::Utc::now(),
            ca::RevocationReason::KeyCompromise,
            vec![0x22u8; 32],
        ))
        .build_unsigned();

    // Backdated after building and before signing: the signature has to cover the
    // window the test is about, since an issuer timestamp that does not match what
    // was signed is precisely the tampering `verified_checker` exists to catch.
    rsl.issued_at = chrono::Utc::now() - chrono::Duration::days(age_days);
    rsl.expires_at = rsl.issued_at + chrono::Duration::days(valid_for_days);

    ca.sign_rsl(rsl).expect("signing the list")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ca::{CaRoot, InMemoryRslManager};
    use chrono::{Duration, Utc};

    const DAY_DAYS: i64 = 1;

    fn ca() -> CaRoot {
        CaRoot::generate()
    }

    /// The anchor a node would be configured with, from the CA that signs.
    fn anchor_for(ca: &CaRoot) -> TrustAnchor {
        TrustAnchor::from_public_key(ca.public_key_slice()).expect("a CA key makes an anchor")
    }

    /// A watch reading from an in-memory publisher, with nothing loaded yet.
    fn watch(
        ca: &CaRoot,
        manager: &Arc<InMemoryRslManager>,
        max_staleness_days: i64,
    ) -> RevocationWatch {
        RevocationWatch::new(
            anchor_for(ca),
            manager.clone(),
            Duration::days(max_staleness_days),
            std::time::Duration::from_secs(60),
        )
    }

    #[tokio::test]
    async fn nothing_is_authorised_before_a_list_is_loaded() {
        let ca = ca();
        let manager = Arc::new(InMemoryRslManager::new());
        let watch = watch(&ca, &manager, DAY_DAYS);

        let err = watch
            .authorize_data(&[1u8; 32])
            .expect_err("there is no revocation data yet");
        assert!(err.to_string().contains("no revocation list"), "{err}");
        assert_eq!(watch.sequence_number(), 0);
        assert!(!watch.is_fresh());
        assert!(
            !watch.authorize_connection(&[1u8; 32]),
            "a handshake with no data is refused"
        );
    }

    #[tokio::test]
    async fn a_refresh_loads_the_published_list_and_says_who_is_revoked() {
        let ca = ca();
        let manager = Arc::new(InMemoryRslManager::new());
        let revoked = vec![0x11u8; 32];
        let clean = vec![0x33u8; 32];

        manager
            .store_rsl(&list_revoking(&ca, &revoked, 1, 0, 7))
            .await
            .unwrap();

        let watch = watch(&ca, &manager, DAY_DAYS);
        watch.refresh().await.expect("a signed list verifies");
        assert_eq!(watch.sequence_number(), 1);
        assert!(watch.is_fresh());
        assert!(watch.describe().contains("list #1"), "{}", watch.describe());

        assert!(watch
            .authorize_data(&revoked)
            .expect_err("a revoked node's data is refused")
            .to_string()
            .contains("revoked"));
        assert!(watch.authorize_data(&clean).is_ok());
        assert!(!watch.authorize_connection(&revoked));
        assert!(watch.authorize_connection(&clean));
    }

    /// The half B11 was: a list older than the node's own bound says nothing about
    /// a node it has not revoked, and "record it anyway" is not the alternative.
    #[tokio::test]
    async fn a_stale_list_refuses_data_for_a_node_it_does_not_name() {
        let ca = ca();
        let manager = Arc::new(InMemoryRslManager::new());
        let revoked = vec![0x11u8; 32];
        let clean = vec![0x33u8; 32];

        // Issued three days ago and still inside its validity window: it verifies
        // and loads, and the staleness bound is all that stands between this node
        // and reading an absence as an all-clear.
        manager
            .store_rsl(&list_revoking(&ca, &revoked, 1, 3, 7))
            .await
            .unwrap();

        let watch = watch(&ca, &manager, DAY_DAYS);
        watch.refresh().await.expect("unexpired, so it loads");
        assert!(!watch.is_fresh());

        let err = watch
            .authorize_data(&clean)
            .expect_err("silence from a stale list is not evidence");
        let message = err.to_string();
        assert!(message.contains("beyond this node's"), "{message}");
        assert!(
            !message.contains("is revoked"),
            "a stale list must not be reported as a revocation: {message}"
        );

        // A node the list does name is still revoked: staleness makes the absences
        // meaningless, not the entries.
        assert!(watch.authorize_data(&revoked).is_err());
        assert!(!watch.authorize_connection(&clean));
    }

    /// Revocation is only as good as the refresh, so a failed one must not cost
    /// the node the knowledge it already has — the previous list is still the most
    /// recent thing the CA actually signed.
    #[tokio::test]
    async fn a_failed_refresh_keeps_the_last_list() {
        let ca = ca();
        let manager = Arc::new(InMemoryRslManager::new());
        let revoked = vec![0x11u8; 32];

        manager
            .store_rsl(&list_revoking(&ca, &revoked, 1, 0, 7))
            .await
            .unwrap();

        let watch = watch(&ca, &manager, DAY_DAYS);
        watch.refresh().await.unwrap();

        // The CA's next publication is expired on arrival — its bug, not this
        // node's, and the node's revocation knowledge must not be wiped by it.
        manager
            .store_rsl(&list_revoking(&ca, &[0x44u8; 32], 2, 30, 1))
            .await
            .unwrap();

        let err = watch.refresh().await.expect_err("list #2 expired");
        assert!(err.to_string().contains("expired"), "{err}");

        assert_eq!(watch.sequence_number(), 1, "list #1 is still held");
        assert!(
            watch.authorize_data(&revoked).is_err(),
            "the revocation learned from list #1 still holds"
        );
    }

    /// A list that does not verify is never installed, whatever a reader of the
    /// stored document would have made of it.
    #[tokio::test]
    async fn a_list_that_does_not_verify_is_not_installed() {
        let honest = ca();
        let impostor = ca();
        let manager = Arc::new(InMemoryRslManager::new());
        let revoked = vec![0x11u8; 32];

        manager
            .store_rsl(&list_revoking(&honest, &revoked, 1, 0, 7))
            .await
            .unwrap();

        let watch = watch(&honest, &manager, DAY_DAYS);
        watch.refresh().await.unwrap();

        // Somebody else's signature over a document stamped with this CA's id,
        // which is what an edit to the published list looks like from here.
        let mut mislabelled = list_revoking(&impostor, &[0x99u8; 32], 2, 0, 7);
        mislabelled.issuer_id = honest.ca_id().to_vec();
        manager
            .store_rsl(&mislabelled)
            .await
            .expect("the manager stores what it is handed; verification is the reader's job");

        let err = watch.refresh().await.expect_err("impostor's signature");
        assert!(err.to_string().contains("does not verify"), "{err}");
        assert_eq!(watch.sequence_number(), 1);
        assert!(watch.authorize_data(&revoked).is_err());
    }

    #[tokio::test]
    async fn the_anchor_names_the_ca_whose_lists_are_read() {
        let ca_a = ca();
        let ca_b = ca();
        let manager = Arc::new(InMemoryRslManager::new());

        manager
            .store_rsl(&list_revoking(&ca_a, &[0x11u8; 32], 1, 0, 7))
            .await
            .unwrap();

        // Configured with B's anchor while only A has published: this is not a node
        // with an empty list, it is a node pointed at the wrong CA, and the error
        // names the CA it was told to believe.
        let watch = watch(&ca_b, &manager, DAY_DAYS);
        let err = watch.refresh().await.expect_err("B has published nothing");
        assert!(err.to_string().contains(&ca_b.ca_id_hex()), "{err}");
    }

    #[test]
    fn an_absurd_staleness_value_saturates_instead_of_panicking() {
        assert_eq!(
            chrono_seconds(0),
            Duration::seconds(1),
            "zero is refused by configuration validation, but must not panic here"
        );
        assert_eq!(chrono_seconds(3600), Duration::hours(1));
        assert!(chrono_seconds(u64::MAX) > Duration::days(365));
    }
}
