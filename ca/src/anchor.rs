//! A CA's public key, as something a node can actually hold.
//!
//! Every verifier in this crate was a method on [`crate::CaRoot`], which owns the CA's
//! private key. That works for the CA and is impossible for everyone else: a node
//! that wants to check a revocation list must not hold the CA's signing key. This is
//! the gap GAP_ANALYSIS calls M12 — with no anchor, nothing downstream of it has
//! anything to stand on: verifying an RSL when it is loaded (M11) and a node
//! refusing a revoked peer (B11) both need a key to check against, and until now the
//! only key available was one no consumer may have.
//!
//! A verifier does not need the private key, though; it needs the *preimage*. The
//! RSL preimage is the issuer id — `SHA-256(public key)`, see [`crate::CaRoot::ca_id`] —
//! followed by the list's own timestamps, sequence number and entries. No secret
//! goes into it, which is why an anchor can be a public key and nothing more.
//!
//! # File format
//!
//! A trust anchor is a SubjectPublicKeyInfo PEM document (RFC 5280), the
//! `-----BEGIN PUBLIC KEY-----` form that `rustls-pemfile` parses:
//!
//! ```text
//! -----BEGIN PUBLIC KEY-----
//! MCowBQYDK2VwAyEA...
//! -----END PUBLIC KEY-----
//! ```
//!
//! Using the standard envelope settles a mistake that is otherwise easy to make. A
//! CA root key and a bare public key are both opaque bytes on disk, and reading one
//! as the other fails confusingly or, worse, silently: a 32 public bytes decoded as
//! a signing key yields a confident, entirely wrong CA identity. SPKI and PKCS#8
//! carry different labels, so each reader finds nothing in the other's file and says
//! which one it expected — [`TrustAnchor::from_pem`] and [`crate::CaRoot::load_from_file`].

use std::fs;
use std::path::Path;

use ed25519_dalek::VerifyingKey;

use crate::credential::Credential;
use crate::error::{CaError, Result};
use crate::revocation::RevocationStatusList;
use crate::signing;

/// Permissions for an anchor file.
///
/// World-readable on purpose: this is public material whose entire use is being
/// copied onto other machines. The CA *root key* is the one that needs 0o600.
#[cfg(unix)]
pub const TRUST_ANCHOR_PERMISSIONS: u32 = 0o644;

/// The CA's Ed25519 public key, with the identity that key implies.
///
/// The id is stored rather than recomputed so that [`TrustAnchor::ca_id`] and
/// [`TrustAnchor::verify_rsl`] cannot disagree about which CA this is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustAnchor {
    verifying_key: VerifyingKey,
    ca_id: [u8; 32],
}

impl TrustAnchor {
    /// Build an anchor from a raw 32-byte CA public key.
    ///
    /// # Errors
    ///
    /// [`CaError::Verification`] if the key is not 32 bytes or is not a valid
    /// Ed25519 point.
    pub fn from_public_key(ca_public_key: &[u8]) -> Result<Self> {
        let verifying_key = signing::parse_ca_public_key(ca_public_key)?;
        Ok(Self {
            ca_id: signing::issuer_id_of(&verifying_key),
            verifying_key,
        })
    }

    /// Build an anchor from a SubjectPublicKeyInfo PEM document.
    ///
    /// # Errors
    ///
    /// [`CaError::InvalidKey`] if there is no SPKI section. A file holding a
    /// `PRIVATE KEY` block is called out as such rather than reported as
    /// unparseable: an operator who means to publish an anchor and reaches for the
    /// root key has just distributed the CA, and that deserves to be said plainly.
    pub fn from_pem(pem: &str) -> Result<Self> {
        use ed25519_dalek::pkcs8::DecodePublicKey;

        let mut reader = std::io::BufReader::new(pem.as_bytes());
        let mut keys = Vec::new();
        for key in rustls_pemfile::public_keys(&mut reader) {
            keys.push(key.map_err(|e| CaError::InvalidKey(format!("invalid PEM: {e}")))?);
        }

        match keys.len() {
            1 => {
                let key = VerifyingKey::from_public_key_der(keys[0].as_ref())
                    .map_err(|e| CaError::Verification(format!("invalid SPKI public key: {e}")))?;
                Self::from_public_key(key.as_bytes())
            }
            0 => {
                if pem.contains("BEGIN PRIVATE KEY") || pem.contains("BEGIN ENCRYPTED PRIVATE KEY")
                {
                    Err(CaError::InvalidKey(
                        "this is a private key, not a trust anchor. Publish the public half with \
                         `app ca ca-export-anchor`; a CA root key must never leave the machine \
                         that holds it."
                            .to_string(),
                    ))
                } else {
                    Err(CaError::InvalidKey(
                        "no \"BEGIN PUBLIC KEY\" section found; a trust anchor is a \
                         SubjectPublicKeyInfo PEM document"
                            .to_string(),
                    ))
                }
            }
            n => Err(CaError::InvalidKey(format!(
                "found {n} public keys; a trust anchor names exactly one CA"
            ))),
        }
    }

    /// The anchor as a SubjectPublicKeyInfo PEM document, LF line endings.
    pub fn to_pem(&self) -> String {
        use ed25519_dalek::pkcs8::spki::EncodePublicKey;

        // Written by the same PKCS#8/SPKI code that reads it, with a fixed line
        // ending, so re-exporting an anchor reproduces the file byte for byte.
        self.verifying_key
            .to_public_key_pem(pkcs8::LineEnding::LF)
            .expect("encoding a public key has no failure mode")
    }

    /// The id this key speaks for: `SHA-256(public key)`, 32 raw bytes.
    ///
    /// Equal to [`crate::CaRoot::ca_id`] for the matching CA, and equal to nothing else.
    pub fn ca_id(&self) -> &[u8; 32] {
        &self.ca_id
    }

    /// [`ca_id`](Self::ca_id) in lowercase hex, for logs and terminal output.
    pub fn ca_id_hex(&self) -> String {
        hex::encode(self.ca_id)
    }

    /// The raw 32-byte public key.
    pub fn public_key(&self) -> &[u8; 32] {
        self.verifying_key.as_bytes()
    }

    /// Check a revocation list against this key.
    ///
    /// A list naming any other CA is refused here rather than verified against
    /// whichever key the caller happened to look up: the issuer is taken from the
    /// key, never from the document.
    ///
    /// # Errors
    ///
    /// Only for input that cannot be a signature at all (wrong length). A
    /// well-formed signature over different bytes is `Ok(false)`, because "that is
    /// not mine" is an answer, not a failure.
    pub fn verify_rsl(&self, rsl: &RevocationStatusList) -> Result<bool> {
        signing::verify_rsl_signature(self.verifying_key.as_bytes(), rsl)
    }

    /// Check a credential this CA issued, without its private key.
    ///
    /// Two things have to hold, and they are different questions:
    ///
    /// * the signature is this key's — checked over the bytes the CA actually
    ///   signed, which are the node's public key and the validity window;
    /// * the `issuer_id` the credential carries, if it carries one, names *this*
    ///   CA. The issuer is not inside the signed payload, so on its own it is a
    ///   pointer saying which anchor to reach for, nothing more. Checked here, it
    ///   stops a credential being presented to the wrong CA and passing — the
    ///   lookup half of GAP_ANALYSIS M14.
    ///
    /// # Errors
    ///
    /// If the credential's own `node_id` does not match its signing public key, or
    /// its signature is not 64 bytes. A credential that simply is not this CA's is
    /// `Ok(false)`.
    pub fn verify_credential(&self, credential: &Credential) -> Result<bool> {
        if let Some(issuer_id) = &credential.issuer_id {
            if issuer_id.as_slice() != self.ca_id.as_slice() {
                return Ok(false);
            }
        }

        signing::verify_credential_signature(self.verifying_key.as_bytes(), credential)
    }

    /// Read an anchor from `path`.
    pub fn load_from_file(path: &Path) -> Result<Self> {
        let contents = fs::read_to_string(path)
            .map_err(|e| CaError::Io(format!("Failed to read trust anchor: {}", e)))?;
        Self::from_pem(&contents)
            .map_err(|e| CaError::InvalidKey(format!("{}: {}", path.display(), e)))
    }

    /// Write the anchor to `path`, world-readable.
    pub fn save_to_file(&self, path: &Path) -> Result<()> {
        fs::write(path, self.to_pem())
            .map_err(|e| CaError::Io(format!("Failed to write trust anchor: {}", e)))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(TRUST_ANCHOR_PERMISSIONS))
                .map_err(|e| {
                    CaError::Io(format!("Failed to set trust anchor permissions: {}", e))
                })?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::revocation::{RevocationReason, RevokedNode};
    use crate::CaRoot;

    /// Key material lives in fixtures, not in source — see
    /// `tests/fixtures/README.md` for what each file is and how it was made.
    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(FIXTURES).join(name)
    }

    /// A signed list and the anchor that should accept it. The CA is dropped before
    /// the anchor is built, so nothing below can reach a private key by accident.
    fn signed_list_and_anchor() -> (RevocationStatusList, TrustAnchor) {
        let ca = CaRoot::generate();
        let revocation = ca.revoke_node(
            vec![1u8; 32],
            vec![2u8; 32],
            RevocationReason::KeyCompromise,
            None,
        );
        let rsl = ca.create_rsl(vec![revocation], 7, 1).unwrap();

        let published = ca.trust_anchor().to_pem();
        drop(ca);

        let anchor = TrustAnchor::from_pem(&published).unwrap();
        (rsl, anchor)
    }

    #[test]
    fn the_anchor_carries_the_id_the_ca_computes() {
        let ca = CaRoot::generate();
        let anchor = TrustAnchor::from_public_key(ca.public_key_slice()).unwrap();
        assert_eq!(anchor.ca_id(), &ca.ca_id());
    }

    #[test]
    fn a_list_signed_by_the_root_verifies_under_the_anchor() {
        let ca = CaRoot::generate();
        let revocation = ca.revoke_node(
            vec![1u8; 32],
            vec![2u8; 32],
            RevocationReason::KeyCompromise,
            None,
        );
        let rsl = ca.create_rsl(vec![revocation], 7, 1).unwrap();

        let anchor = ca.trust_anchor();
        assert!(anchor.verify_rsl(&rsl).unwrap());
        assert_eq!(
            anchor.verify_rsl(&rsl).unwrap(),
            ca.verify_rsl(&rsl).unwrap(),
            "the CA and a holder of its public key must agree"
        );
    }

    /// The property the design rests on: the exported bytes are the whole story,
    /// with no private key in scope at verification time.
    #[test]
    fn an_anchor_is_buildable_from_exported_pem_alone() {
        let (published, rsl) = {
            let ca = CaRoot::generate();
            let revocation = ca.revoke_node(
                vec![9u8; 32],
                vec![8u8; 32],
                RevocationReason::PolicyViolation,
                None,
            );
            let rsl = ca.create_rsl(vec![revocation], 7, 3).unwrap();
            (ca.trust_anchor().to_pem(), rsl)
        };

        let anchor = TrustAnchor::from_pem(&published).unwrap();
        assert!(anchor.verify_rsl(&rsl).unwrap());
    }

    #[test]
    fn a_list_naming_another_ca_is_refused_by_the_anchor() {
        let signer = CaRoot::generate();
        let other = CaRoot::generate();
        let rsl = signer.create_rsl(Vec::new(), 7, 1).unwrap();

        assert!(!other.trust_anchor().verify_rsl(&rsl).unwrap());
    }

    /// Rewriting `issuer_id` to a key the attacker does hold is M14's forgery case.
    /// The preimage carries the issuer, so the signature breaks; and the anchor
    /// refuses the list on the name before it ever gets that far.
    #[test]
    fn relabelling_the_issuer_does_not_move_the_verdict_to_accept() {
        let mut rsl = CaRoot::generate().create_rsl(Vec::new(), 7, 1).unwrap();
        let attacker = CaRoot::generate();

        rsl.issuer_id = attacker.ca_id().to_vec();
        assert!(!attacker.trust_anchor().verify_rsl(&rsl).unwrap());
    }

    #[test]
    fn every_field_the_signature_covers_moves_the_verdict_under_the_anchor() {
        let (base, anchor) = signed_list_and_anchor();
        assert!(anchor.verify_rsl(&base).unwrap());

        let mut cases: Vec<(RevocationStatusList, &str)> = Vec::new();

        let mut bumped = base.clone();
        bumped.sequence_number += 1;
        cases.push((bumped, "sequence number"));

        let mut later = base.clone();
        later.issued_at += chrono::Duration::seconds(1);
        cases.push((later, "issued_at"));

        let mut longer = base.clone();
        longer.expires_at += chrono::Duration::seconds(1);
        cases.push((longer, "expires_at"));

        let mut extra = base.clone();
        extra.revocations.push(RevokedNode::new(
            vec![42u8; 32],
            chrono::Utc::now(),
            RevocationReason::KeyCompromise,
            vec![43u8; 32],
        ));
        cases.push((extra, "an extra revocation"));

        let mut renamed = base.clone();
        renamed.revocations[0].node_id = vec![7u8; 32];
        cases.push((renamed, "a revoked node id"));

        let mut reason = base.clone();
        reason.revocations[0].reason = RevocationReason::CaCompromise;
        cases.push((reason, "a revocation reason"));

        let mut flipped = base.clone();
        flipped.signature[0] ^= 1;
        cases.push((flipped, "a flipped signature byte"));

        for (tampered, what) in cases {
            assert!(
                !anchor.verify_rsl(&tampered).unwrap(),
                "changing {what} must invalidate the list"
            );
        }
    }

    #[test]
    fn notes_are_audit_text_and_are_not_signed() {
        let (base, anchor) = signed_list_and_anchor();
        let mut annotated = base.clone();
        annotated.revocations[0].notes = Some("ticket OPS-4211".to_string());
        assert!(
            anchor.verify_rsl(&annotated).unwrap(),
            "editing audit notes must not invalidate a published list"
        );
    }

    #[test]
    fn the_committed_anchor_matches_the_committed_root_key() {
        let ca = CaRoot::load_from_file(&fixture("ca-root.pem")).unwrap();
        let anchor = TrustAnchor::load_from_file(&fixture("ca-root.pub.pem")).unwrap();

        assert_eq!(anchor.ca_id(), &ca.ca_id());
        assert_eq!(anchor.public_key().as_slice(), ca.public_key().as_slice());
    }

    #[test]
    fn an_anchor_is_spki_pem_and_survives_a_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("anchor.pub.pem");

        let ca = CaRoot::generate();
        ca.trust_anchor().save_to_file(&path).unwrap();

        let written = fs::read_to_string(&path).unwrap();
        assert!(
            written.starts_with("-----BEGIN PUBLIC KEY-----\n")
                && written.trim_end().ends_with("-----END PUBLIC KEY-----"),
            "an anchor has to be readable by tools this repo does not own, got: {written}"
        );

        let loaded = TrustAnchor::load_from_file(&path).unwrap();
        assert_eq!(loaded, ca.trust_anchor());
        // Re-exporting reproduces the file byte for byte.
        assert_eq!(loaded.to_pem(), ca.trust_anchor().to_pem());
    }

    #[test]
    fn a_root_key_offered_as_an_anchor_is_named_as_the_mistake_it_is() {
        let err = TrustAnchor::load_from_file(&fixture("ca-root.pem"))
            .expect_err("a root key must not load as an anchor");
        let message = err.to_string();
        assert!(
            message.contains("private key"),
            "the error should say what was done, got: {message}"
        );
    }

    #[test]
    fn a_public_key_offered_as_a_root_key_is_refused() {
        assert!(
            CaRoot::load_from_file(&fixture("ca-root.pub.pem")).is_err(),
            "an anchor must not load as a signing key"
        );
    }

    #[test]
    fn junk_is_refused() {
        assert!(TrustAnchor::from_pem("").is_err());
        assert!(TrustAnchor::from_pem("not pem at all").is_err());
        assert!(TrustAnchor::from_pem(
            "-----BEGIN PUBLIC KEY-----\nnot base64 !!!\n-----END PUBLIC KEY-----"
        )
        .is_err());
    }

    #[test]
    fn a_key_of_the_wrong_length_is_refused() {
        assert!(TrustAnchor::from_public_key(&[1u8; 31]).is_err());
        assert!(TrustAnchor::from_public_key(&[1u8; 33]).is_err());
        assert!(TrustAnchor::from_public_key(&[]).is_err());
    }

    #[test]
    fn a_credential_verifies_under_the_anchor() {
        let ca = CaRoot::generate();
        let credential = ca
            .issue_credential(&[5u8; 32], Some(30))
            .expect("issuing a credential");

        assert!(ca.trust_anchor().verify_credential(&credential).unwrap());

        let other = CaRoot::generate();
        assert!(!other.trust_anchor().verify_credential(&credential).unwrap());
    }
}
