//! CA root key management.
//!
//! The root key is one Ed25519 keypair in a PKCS#8 PEM file, and everything this CA
//! signs is attributable to it: `ca_id` is `SHA-256` of the public half, so the
//! identity a revocation list carries is a property of the key rather than a field
//! someone typed. The counterpart for consumers — who must not hold this file — is
//! [`crate::TrustAnchor`].

use ed25519_dalek::SigningKey;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::credential::Credential;
use crate::error::{CaError, Result};
use crate::revocation::{RevocationReason, RevocationStatusList, RevokedNode};
use crate::signing::sign_credential;

/// CA root keypair and associated operations.
///
/// The `CaRoot` represents the Certificate Authority's master keypair.
/// It is responsible for:
/// - Generating credentials for nodes
/// - Verifying credentials it has issued
/// - Managing its own key storage
///
/// # Security
///
/// The CA root private key is highly sensitive and should be:
/// - Stored securely (encrypted at rest)
/// - Accessible only to authorized processes
/// - Backed up in secure locations
/// - Potentially stored in an HSM for production use
#[derive(Debug, Clone)]
pub struct CaRoot {
    /// The CA's Ed25519 signing key.
    signing_key: SigningKey,

    /// Cached public key bytes (to avoid temporary value issues).
    public_key_bytes: [u8; 32],

    /// Path to the root key file (if loaded from disk).
    key_path: Option<PathBuf>,
}

impl CaRoot {
    /// Generate a new CA root keypair.
    ///
    /// This should only be done once during CA initialization.
    ///
    /// # Returns
    ///
    /// A new `CaRoot` instance with a randomly generated keypair.
    pub fn generate() -> Self {
        let mut csprng = OsRng;
        let signing_key = SigningKey::generate(&mut csprng);
        let public_key_bytes = *signing_key.verifying_key().as_bytes();

        CaRoot {
            signing_key,
            public_key_bytes,
            key_path: None,
        }
    }

    /// Load the CA root keypair from a PKCS#8 PEM file.
    ///
    /// The file is the standard `-----BEGIN PRIVATE KEY-----` form (RFC 5958 with
    /// the RFC 8410 Ed25519 algorithm identifier), so `rustls-pemfile`, the `pkcs8`
    /// crate, and any other conforming implementation read the same file. Nothing
    /// here is a format of our own.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the PKCS#8 PEM private key
    ///
    /// # Returns
    ///
    /// A `CaRoot` instance loaded from the file, or an error if the file cannot be
    /// read, holds no PKCS#8 private key, or holds more than one.
    ///
    /// A key written in the bare-hex format this project used before PKCS#8 is
    /// refused with the command that converts it, rather than being guessed at: a
    /// 32-byte secret read as the wrong format produces a confident, entirely wrong
    /// CA identity, and every list it signs afterward is unverifiable by the real CA.
    pub fn load_from_file(path: &Path) -> Result<Self> {
        let contents = fs::read_to_string(path).map_err(|e| {
            CaError::RootKey(format!("Failed to read key file {}: {e}", path.display()))
        })?;

        let signing_key = Self::decode_pkcs8_pem(&contents, path)?;
        let public_key_bytes = *signing_key.verifying_key().as_bytes();

        Ok(CaRoot {
            signing_key,
            public_key_bytes,
            key_path: Some(path.to_path_buf()),
        })
    }

    /// Decode one PKCS#8 PEM private key, or say why the file is not one.
    fn decode_pkcs8_pem(contents: &str, path: &Path) -> Result<SigningKey> {
        if crate::pemkeys::is_legacy_hex_key(contents) {
            return Err(CaError::RootKey(format!(
                "{} is a hex-encoded root key, the format used before PKCS#8. Convert it \
                 with `app ca ca-migrate-key --from {}` and point [ca].key_path at the \
                 .pem that writes; the identity survives the conversion, so lists this \
                 CA already published keep verifying.",
                path.display(),
                path.display()
            )));
        }

        crate::pemkeys::read_signing_key_pem(contents, &path.display().to_string())
            .map_err(|e| CaError::RootKey(e.to_string()))
    }

    /// Save the CA root private key as PKCS#8 PEM.
    ///
    /// The file is PKCS#8 **v1** — see [`crate::pemkeys`], which owns that choice and
    /// its reasoning, and writes a node's signing key the same way. [`load_from_file`]
    /// reads v1 and v2 alike, so a key written by another tool loads without a
    /// migration.
    ///
    /// # Arguments
    ///
    /// * `path` - Path where the key should be saved
    /// * `permissions` - File permissions (default: 0o600 on Unix)
    ///
    /// # Security
    ///
    /// This writes the private key to disk. Ensure:
    /// - The directory is secure
    /// - File permissions are restrictive (0o600)
    /// - The file is encrypted if possible
    pub fn save_to_file(&self, path: &Path, permissions: Option<u32>) -> Result<()> {
        let pem = crate::pemkeys::write_signing_key_pem(&self.signing_key);

        crate::pemkeys::write_key_file(path, &pem, Some(permissions.unwrap_or(0o600)))
            .map_err(|e| CaError::RootKey(e.to_string()))
    }

    /// Get the CA's public key.
    pub fn public_key(&self) -> Vec<u8> {
        self.public_key_bytes.to_vec()
    }

    /// Get the CA's public key as a slice.
    pub fn public_key_slice(&self) -> &[u8; 32] {
        &self.public_key_bytes
    }

    /// Get the CA's public key as a Vec.
    pub fn public_key_vec(&self) -> Vec<u8> {
        self.public_key()
    }

    /// Get the CA's private key.
    ///
    /// # Warning
    ///
    /// This exposes the raw private key bytes. Handle with extreme care.
    pub fn private_key(&self) -> &[u8] {
        self.signing_key.as_bytes()
    }

    /// Get the keypair path (if loaded from file).
    pub fn key_path(&self) -> Option<&Path> {
        self.key_path.as_deref()
    }

    /// The CA's identifier: `SHA-256(public key)`, 32 raw bytes.
    ///
    /// This is what `issuer_id` carries in every list this CA signs and every
    /// credential it issues. Hex is for people — use
    /// [`ca_id_hex`](Self::ca_id_hex) to print it.
    pub fn ca_id(&self) -> [u8; 32] {
        use sha2::{Digest, Sha256};
        Sha256::digest(self.public_key_bytes).into()
    }

    /// [`ca_id`](Self::ca_id) spelled in lowercase hex, for logs and terminal
    /// output. Nothing that stores or signs an identifier uses this.
    pub fn ca_id_hex(&self) -> String {
        hex::encode(self.ca_id())
    }

    /// Issue a credential for a node's public key.
    ///
    /// # Arguments
    ///
    /// * `signing_public_key` - The node's Ed25519 public key (32 bytes)
    /// * `validity_days` - How many days the credential should be valid (None = default)
    ///
    /// # Returns
    ///
    /// A signed `Credential` that attests to the node's key.
    pub fn issue_credential(
        &self,
        signing_public_key: &[u8],
        validity_days: Option<u64>,
    ) -> Result<Credential> {
        let issued_at = chrono::Utc::now();
        let expires_at = validity_days.map(|days| issued_at + chrono::Duration::days(days as i64));

        // Sign the credential
        let ca_signature =
            sign_credential(&self.signing_key, signing_public_key, issued_at, expires_at)?;

        Credential::new(
            signing_public_key.to_vec(),
            ca_signature,
            issued_at,
            expires_at,
            // Names this CA so a holder knows which anchor to check it against.
            // Not inside the signed payload, so it is a pointer, not a claim:
            // verification checks the key it was handed, then that the key is this
            // one (see `TrustAnchor::verify_credential`).
            Some(self.ca_id().to_vec()),
        )
    }

    /// Verify a credential was issued by this CA.
    ///
    /// # Arguments
    ///
    /// * `credential` - The credential to verify
    ///
    /// # Returns
    ///
    /// `Ok(())` if the credential is valid and was issued by this CA.
    ///
    /// Delegates the signature to [`crate::signing::verify_credential_signature`]
    /// so the CA and a holder of its public key check the same bytes; this method
    /// adds the expiry window, which is the CA's own policy rather than part of
    /// what was signed.
    pub fn verify_credential(&self, credential: &Credential) -> Result<()> {
        credential.is_valid_now()?;
        self.verify_credential_signature(credential)
    }

    /// Verify a credential's signature is valid (without checking expiration).
    ///
    /// This is useful for historical verification where expiration doesn't matter.
    pub fn verify_credential_signature(&self, credential: &Credential) -> Result<()> {
        if !crate::signing::verify_credential_signature(&self.public_key_bytes, credential)? {
            return Err(CaError::Verification(
                "credential signature does not belong to this CA".to_string(),
            ));
        }

        Ok(())
    }

    /// Revoke a node by adding it to a revocation list.
    ///
    /// # Arguments
    ///
    /// * `node_id` - The node identifier to revoke (SHA-256 of signing public key)
    /// * `signing_public_key` - The node's public key (for verification)
    /// * `reason` - Reason for revocation
    /// * `notes` - Optional notes about the revocation
    ///
    /// # Returns
    ///
    /// A `RevokedNode` entry that can be added to an RSL.
    pub fn revoke_node(
        &self,
        node_id: Vec<u8>,
        signing_public_key: Vec<u8>,
        reason: RevocationReason,
        notes: Option<String>,
    ) -> RevokedNode {
        let mut revocation =
            RevokedNode::new(node_id, chrono::Utc::now(), reason, signing_public_key);
        revocation.notes = notes;
        revocation
    }

    /// Create a signed Revocation Status List (RSL).
    ///
    /// # Arguments
    ///
    /// * `revocations` - List of revoked nodes
    /// * `validity_days` - How many days the RSL should be valid
    /// * `sequence_number` - This CA's next sequence number, from durable
    ///   state — see [`sign_rsl`](Self::sign_rsl) for why the caller supplies
    ///   it and there is no default.
    ///
    /// # Returns
    ///
    /// A signed `RevocationStatusList` ready for distribution.
    pub fn create_rsl(
        &self,
        revocations: Vec<RevokedNode>,
        validity_days: u64,
        sequence_number: u64,
    ) -> Result<RevocationStatusList> {
        let unsigned = RevocationStatusList::builder(self.ca_id())
            .sequence_number(sequence_number)
            .validity_days(validity_days)
            .add_revocations(revocations)
            .build_unsigned();

        self.sign_rsl(unsigned)
    }

    /// Sign a list an [`RslManager`](crate::RslManager) has already produced.
    ///
    /// The sequence number, the validity window and the revocation entries are
    /// decided when a list is generated, and the signature has to cover *those*
    /// values. Rebuilding a list at the signing site from only the revocations
    /// resets the rest — including the counter whose whole job is to make an
    /// old list recognisable as an old list — so a publisher signs the document
    /// it was handed rather than a fresh copy of it.
    ///
    /// # Returns
    ///
    /// * `Ok(rsl)` - `rsl` with this CA as issuer and a signature over its own
    ///   sequence, validity window and entries
    /// * `Err` - If the sequence is 0 (nothing has been issued yet, so there is
    ///   nothing to publish) or the list already names a different issuer: this
    ///   key can only speak for itself, and signing another CA's list would
    ///   produce a valid-looking credential it cannot stand behind.
    pub fn sign_rsl(&self, rsl: RevocationStatusList) -> Result<RevocationStatusList> {
        use ed25519_dalek::Signer;

        if rsl.sequence_number == 0 {
            return Err(CaError::InvalidCredential(
                "RSL sequence numbers start at 1; 0 means no list has been issued yet".to_string(),
            ));
        }

        let ca_id = self.ca_id();
        if !rsl.issuer_id.is_empty() && rsl.issuer_id != ca_id {
            return Err(CaError::InvalidCredential(format!(
                "RSL names issuer {} but this key is {}",
                hex::encode(&rsl.issuer_id),
                hex::encode(ca_id)
            )));
        }

        let mut rsl = rsl;
        rsl.issuer_id = ca_id.to_vec();
        let payload = self.rsl_payload_bytes(
            &rsl.revocations,
            rsl.issued_at,
            rsl.expires_at,
            rsl.sequence_number,
        )?;
        rsl.signature = self.signing_key.sign(&payload).to_bytes().to_vec();

        Ok(rsl)
    }

    /// Verify a Revocation Status List's signature.
    ///
    /// # Arguments
    ///
    /// * `rsl` - The RSL to verify
    ///
    /// # Returns
    ///
    /// `Ok(true)` if the signature is valid and from this CA.
    ///
    /// Delegates to [`crate::signing::verify_rsl_signature`], the consumer-facing
    /// form of the same check. The CA keeps its own entry point because callers
    /// already hold a `CaRoot`; the point of the delegation is that there is now
    /// one implementation of the layout, not two that can drift.
    pub fn verify_rsl(&self, rsl: &RevocationStatusList) -> Result<bool> {
        crate::signing::verify_rsl_signature(&self.public_key_bytes, rsl)
    }

    /// The public half of this CA, as something a node can be given.
    ///
    /// Use this to publish an anchor (`ca ca-export-anchor`); a node that has the
    /// anchor can verify this CA's credentials and revocation lists without ever
    /// holding [`CaRoot`].
    pub fn trust_anchor(&self) -> crate::anchor::TrustAnchor {
        // Built from the public bytes rather than stored, so an anchor can never
        // reach the signing key it was derived from.
        crate::anchor::TrustAnchor::from_public_key(&self.public_key_bytes)
            .expect("a CA's own public key is always a valid anchor")
    }

    /// Encode the RSL payload for signing.
    ///
    /// The layout lives in [`crate::signing::build_rsl_payload`] so that a verifier
    /// holding only the public key rebuilds the same bytes rather than a second
    /// implementation of them. The golden vector in `the_rsl_preimage_is_pinned`
    /// is what keeps this extraction from being a silent format change for lists
    /// already published.
    fn rsl_payload_bytes(
        &self,
        revocations: &[RevokedNode],
        issued_at: chrono::DateTime<chrono::Utc>,
        expires_at: chrono::DateTime<chrono::Utc>,
        sequence_number: u64,
    ) -> Result<Vec<u8>> {
        Ok(crate::signing::build_rsl_payload(
            &self.ca_id(),
            revocations,
            issued_at,
            expires_at,
            sequence_number,
        ))
    }

    /// Read a root key written in the bare-hex format this project used before
    /// PKCS#8.
    ///
    /// [`CaRoot::load_from_file`] refuses such a file, because reading a 32-byte
    /// secret through the wrong decoder silently yields a *different* CA rather than
    /// failing. This is the way out, not the way in: `ca ca-migrate-key` uses it to
    /// rewrite the file as PKCS#8 PEM. The identity survives the move — same secret,
    /// same public key, same `ca_id` — so lists this CA published before the
    /// conversion still verify afterwards.
    pub fn load_legacy_hex_file(path: &Path) -> Result<Self> {
        let contents = fs::read_to_string(path).map_err(|e| {
            CaError::RootKey(format!(
                "Failed to read legacy key file {}: {e}",
                path.display()
            ))
        })?;

        let secret = hex::decode(contents.trim()).map_err(|e| {
            CaError::RootKey(format!(
                "{} is not a hex-encoded legacy root key: {e}",
                path.display()
            ))
        })?;

        let mut ca = Self::from_signing_bytes(&secret)?;
        ca.key_path = Some(path.to_path_buf());
        Ok(ca)
    }

    /// Rebuild a root key from 32 raw Ed25519 secret bytes.
    ///
    /// Exists for format migrations; [`CaRoot::generate`] and
    /// [`CaRoot::load_from_file`] are the normal paths. Nothing is interpreted beyond
    /// length, which is why callers should arrive from a key *file* rather than from
    /// bytes written into source.
    pub fn from_signing_bytes(secret: &[u8]) -> Result<Self> {
        let secret: [u8; 32] = secret.try_into().map_err(|_| {
            CaError::RootKey(format!(
                "an Ed25519 secret is 32 bytes, got {}",
                secret.len()
            ))
        })?;

        let signing_key = SigningKey::from_bytes(&secret);
        let public_key_bytes = *signing_key.verifying_key().as_bytes();

        Ok(Self {
            signing_key,
            public_key_bytes,
            key_path: None,
        })
    }
}

/// Configuration for CA root key storage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaRootConfig {
    /// Path to the CA root key file.
    pub key_path: PathBuf,

    /// Whether to auto-generate a key if not exists.
    pub auto_generate: bool,

    /// File permissions for the key file (Unix only, default: 0o600).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permissions: Option<u32>,
}

impl Default for CaRootConfig {
    fn default() -> Self {
        Self {
            key_path: PathBuf::from("/var/lib/btmon/ca/root_key.pem"),
            auto_generate: true,
            permissions: None,
        }
    }
}

impl CaRootConfig {
    /// Load or create the CA root based on this configuration.
    pub fn load_or_create(&self) -> Result<CaRoot> {
        let path = &self.key_path;

        if path.exists() {
            CaRoot::load_from_file(path)
        } else if self.auto_generate {
            // Ensure parent directory exists
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| CaError::RootKey(format!("Failed to create dir: {}", e)))?;
            }

            let ca = CaRoot::generate();
            ca.save_to_file(
                path,
                self.permissions.or(Some(crate::CA_ROOT_KEY_PERMISSIONS)),
            )?;
            Ok(ca)
        } else {
            Err(CaError::RootKeyNotInitialized)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Key material lives in `tests/fixtures`, generated by
    /// `tests/fixtures/make-fixtures.rs`. Nothing here embeds a key: a byte string
    /// in source is invisible to whoever has to rotate or audit it, and it cannot be
    /// opened with a standard tool to check what it says.
    pub(super) const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

    #[test]
    fn test_generate_and_issue_credential() {
        let ca = CaRoot::generate();

        let node_public_key = vec![1u8; 32];
        let credential = ca.issue_credential(&node_public_key, Some(90)).unwrap();

        // Verify the credential
        ca.verify_credential(&credential).unwrap();

        assert_eq!(credential.signing_public_key, node_public_key);
        assert!(credential.expires_at.is_some());
    }

    #[test]
    fn test_save_and_load_keypair() {
        let temp_dir = TempDir::new().unwrap();
        let key_path = temp_dir.path().join("root_key.pem");

        // Generate and save
        let ca1 = CaRoot::generate();
        ca1.save_to_file(&key_path, Some(0o600)).unwrap();

        // Load and verify
        let ca2 = CaRoot::load_from_file(&key_path).unwrap();

        assert_eq!(ca1.public_key_vec(), ca2.public_key_vec());
    }

    /// The whole point of the format change is that another program can read the
    /// file, so assert the file is PKCS#8 PEM and not merely "something we accept".
    #[test]
    fn the_root_key_file_is_pkcs8_pem() {
        let temp_dir = TempDir::new().unwrap();
        let key_path = temp_dir.path().join("root_key.pem");

        CaRoot::generate()
            .save_to_file(&key_path, Some(0o600))
            .unwrap();

        let written = fs::read_to_string(&key_path).unwrap();
        assert!(
            written.starts_with("-----BEGIN PRIVATE KEY-----\n")
                && written.trim_end().ends_with("-----END PRIVATE KEY-----"),
            "expected a PKCS#8 PEM envelope, got: {written}"
        );

        // The committed fixture loads through the same path, which is what proves
        // the encoder and the reader agree with the wider ecosystem rather than
        // only with each other.
        let from_fixture = CaRoot::load_from_file(&PathBuf::from(FIXTURES).join("ca-root.pem"));
        assert!(
            from_fixture.is_ok(),
            "the committed fixture should load: {:?}",
            from_fixture.err()
        );
    }

    /// The version number inside the envelope is not a detail. `SigningKey::to_pkcs8_pem`
    /// writes v2 (`OneAsymmetricKey` with the public key in an `[1]` attribute) and
    /// OpenSSL 3 parses such a key and then refuses it, so `openssl pkey` — how an
    /// operator inspects, converts, or moves a key — fails on a file this crate wrote.
    /// Both the fixture and freshly generated keys have to be the form every reader
    /// takes.
    #[test]
    fn the_root_key_file_is_pkcs8_v1_so_other_tools_can_open_it() {
        let temp_dir = TempDir::new().unwrap();
        let generated = temp_dir.path().join("root_key.pem");
        CaRoot::generate()
            .save_to_file(&generated, Some(0o600))
            .unwrap();

        for path in [generated, PathBuf::from(FIXTURES).join("ca-root.pem")] {
            let contents = fs::read_to_string(&path).unwrap();
            let mut reader = contents.as_bytes();
            let der = rustls_pemfile::pkcs8_private_keys(&mut reader)
                .next()
                .expect("a key in the file")
                .unwrap();

            let info = pkcs8::PrivateKeyInfo::try_from(der.secret_pkcs8_der()).unwrap();
            assert_eq!(
                info.version(),
                pkcs8::Version::V1,
                "{} is PKCS#8 {:?}; v2 is the one OpenSSL 3 will not load",
                path.display(),
                info.version()
            );
        }
    }

    /// Reading stays wider than writing on purpose. A v2 root key produced by another
    /// implementation is still a valid key, and refusing it would strand a deployment
    /// that generated its key with a different tool.
    #[test]
    fn a_pkcs8_v2_root_key_still_loads() {
        use ed25519_dalek::pkcs8::EncodePrivateKey;

        let ca = CaRoot::generate();
        let v2 = ca
            .signing_key
            .to_pkcs8_pem(pkcs8::LineEnding::LF)
            .expect("encoding a generated key");

        // Prove this really is the other version, so the test below cannot pass by
        // quietly comparing v1 with v1.
        let mut reader = v2.as_bytes();
        let der = rustls_pemfile::pkcs8_private_keys(&mut reader)
            .next()
            .expect("a key in the document")
            .unwrap();
        let info = pkcs8::PrivateKeyInfo::try_from(der.secret_pkcs8_der()).unwrap();
        assert_eq!(info.version(), pkcs8::Version::V2);

        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("v2_root_key.pem");
        fs::write(&path, v2.as_bytes()).unwrap();

        assert_eq!(CaRoot::load_from_file(&path).unwrap().ca_id(), ca.ca_id());
    }

    #[test]
    fn a_legacy_hex_key_is_refused_and_names_the_conversion_command() {
        let legacy = PathBuf::from(FIXTURES).join("pre-change-key.hex");

        let err =
            CaRoot::load_from_file(&legacy).expect_err("a hex root key must not load as PKCS#8");
        let message = err.to_string();
        assert!(
            message.contains("ca-migrate-key"),
            "the operator should be told what to run, got: {message}"
        );
    }

    /// Migrating must preserve the identity: the CA's id is SHA-256 of its public
    /// key, so a key that changed on the way into PEM would invalidate every list it
    /// had already published.
    #[test]
    fn a_legacy_hex_key_migrates_to_the_same_ca() {
        let legacy =
            CaRoot::load_legacy_hex_file(&PathBuf::from(FIXTURES).join("pre-change-key.hex"))
                .expect("the fixture is a valid legacy key");
        let pem = CaRoot::load_from_file(&PathBuf::from(FIXTURES).join("pre-change-key.pem"))
            .expect("the fixture is a valid PKCS#8 key");

        assert_eq!(legacy.ca_id(), pem.ca_id());

        // And the migrated file is readable by the normal path, completing the loop.
        let temp_dir = TempDir::new().unwrap();
        let out = temp_dir.path().join("migrated.pem");
        legacy.save_to_file(&out, Some(0o600)).unwrap();
        assert_eq!(CaRoot::load_from_file(&out).unwrap().ca_id(), pem.ca_id());
    }

    #[test]
    fn a_public_key_file_is_not_a_root_key() {
        let anchor = PathBuf::from(FIXTURES).join("ca-root.pub.pem");

        assert!(
            CaRoot::load_from_file(&anchor).is_err(),
            "a public key must not be loadable as a signing key"
        );
    }

    #[test]
    fn two_keys_in_one_file_are_refused() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("two.pem");

        let one = PathBuf::from(FIXTURES).join("ca-root.pem");
        let other = PathBuf::from(FIXTURES).join("pre-change-key.pem");
        let both = format!(
            "{}{}",
            fs::read_to_string(&one).unwrap(),
            fs::read_to_string(&other).unwrap()
        );
        fs::write(&path, both).unwrap();

        let err = CaRoot::load_from_file(&path).expect_err("exactly one key per CA root file");
        assert!(err.to_string().contains("exactly one"), "got: {err}");
    }

    #[test]
    fn test_verify_invalid_credential() {
        let ca = CaRoot::generate();

        let node_public_key = vec![1u8; 32];
        let mut credential = ca.issue_credential(&node_public_key, Some(90)).unwrap();

        // Tamper with the credential
        credential.signing_public_key[0] ^= 1;

        assert!(ca.verify_credential(&credential).is_err());
    }

    #[test]
    fn test_expired_credential_fails_verification() {
        let ca = CaRoot::generate();

        let node_public_key = vec![1u8; 32];
        let credential = ca.issue_credential(&node_public_key, Some(1)).unwrap();

        // Wait for expiration (simulate by modifying credential)
        let mut expired_credential = credential.clone();
        expired_credential.issued_at = chrono::Utc::now() - chrono::Duration::days(100);
        expired_credential.expires_at = Some(chrono::Utc::now() - chrono::Duration::days(50));

        assert!(ca.verify_credential(&expired_credential).is_err());
    }

    #[test]
    fn test_credential_verification_signature_only() {
        let ca = CaRoot::generate();

        let node_public_key = vec![1u8; 32];
        let credential = ca.issue_credential(&node_public_key, Some(90)).unwrap();

        // Signature verification should succeed even for expired creds
        assert!(ca.verify_credential_signature(&credential).is_ok());
    }

    #[test]
    fn test_ca_id_generation() {
        use sha2::{Digest, Sha256};

        let ca = CaRoot::generate();
        let ca_id = ca.ca_id();

        // 32 raw bytes: the digest itself, not its hex spelling.
        assert_eq!(ca_id.len(), 32);
        assert_eq!(
            ca.ca_id_hex(),
            hex::encode(Sha256::digest(ca.public_key_slice()))
        );
        // Hex is what a human sees, and it is still 64 characters of it.
        assert!(ca.ca_id_hex().chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(ca.ca_id_hex().len(), 64);
    }

    #[test]
    fn test_create_and_verify_rsl() {
        let ca = CaRoot::generate();

        // Create some revoked nodes
        let revocation1 = ca.revoke_node(
            vec![1u8; 32],
            vec![2u8; 32],
            RevocationReason::KeyCompromise,
            Some("Test revocation".to_string()),
        );

        let revocation2 = ca.revoke_node(
            vec![3u8; 32],
            vec![4u8; 32],
            RevocationReason::PolicyViolation,
            None,
        );

        // Create RSL
        let rsl = ca.create_rsl(vec![revocation1, revocation2], 7, 1).unwrap();

        // Verify RSL
        assert!(ca.verify_rsl(&rsl).unwrap());
        assert_eq!(rsl.revocation_count(), 2);
        assert!(rsl.is_valid_now());
    }

    // The sequence number is only an anti-replay counter if a receiver can tell
    // it has not been touched. Bumping it is how an old list gets presented as
    // a new one, so it has to be inside the signature.
    #[test]
    fn the_sequence_number_is_inside_the_signature() {
        let ca = CaRoot::generate();
        let revocation = ca.revoke_node(
            vec![1u8; 32],
            vec![2u8; 32],
            RevocationReason::KeyCompromise,
            None,
        );

        let mut rsl = ca.create_rsl(vec![revocation], 7, 1).unwrap();
        assert!(ca.verify_rsl(&rsl).unwrap());

        rsl.sequence_number = 2;
        assert!(
            !ca.verify_rsl(&rsl).unwrap(),
            "relabeling a list as newer must invalidate it"
        );
    }

    #[test]
    fn a_list_numbered_zero_cannot_be_signed() {
        let ca = CaRoot::generate();

        // 0 is "no list has been published", not a list in its own right.
        let err = ca
            .create_rsl(Vec::new(), 7, 0)
            .expect_err("0 must not be publishable");
        assert!(matches!(err, CaError::InvalidCredential(_)));
    }

    #[test]
    fn sign_rsl_keeps_the_values_the_manager_already_chose() {
        use crate::revocation::{RevocationStatusList, RevokedNode};

        let ca = CaRoot::generate();
        let unsigned = RevocationStatusList::builder(ca.ca_id())
            .sequence_number(5)
            .validity_days(30)
            .add_revocation(RevokedNode::new(
                vec![1u8; 32],
                chrono::Utc::now(),
                RevocationReason::KeyCompromise,
                vec![2u8; 32],
            ))
            .build_unsigned();

        let (issued_at, expires_at) = (unsigned.issued_at, unsigned.expires_at);
        let signed = ca.sign_rsl(unsigned).unwrap();

        // Signing is not re-generating: the document that leaves here is the one
        // the sequence and validity window were chosen for.
        assert_eq!(signed.sequence_number, 5);
        assert_eq!(signed.issued_at, issued_at);
        assert_eq!(signed.expires_at, expires_at);
        assert_eq!(signed.revocation_count(), 1);
        assert!(ca.verify_rsl(&signed).unwrap());
    }

    #[test]
    fn a_key_cannot_sign_a_list_naming_another_ca() {
        use crate::revocation::RevocationStatusList;

        let ca = CaRoot::generate();
        let other = CaRoot::generate();

        let theirs = RevocationStatusList::builder(other.ca_id())
            .sequence_number(1)
            .validity_days(7)
            .build_unsigned();

        assert!(matches!(
            ca.sign_rsl(theirs),
            Err(CaError::InvalidCredential(_))
        ));
    }

    #[test]
    fn test_tampered_rsl_fails_verification() {
        let ca = CaRoot::generate();

        let revocation = ca.revoke_node(
            vec![1u8; 32],
            vec![2u8; 32],
            RevocationReason::KeyCompromise,
            None,
        );

        let mut rsl = ca.create_rsl(vec![revocation], 7, 1).unwrap();

        // Tamper with the signature
        rsl.signature[0] ^= 1;

        // Verification should fail
        assert!(!ca.verify_rsl(&rsl).unwrap());
    }

    #[test]
    fn test_rsl_from_different_ca_fails_verification() {
        let ca1 = CaRoot::generate();
        let ca2 = CaRoot::generate();

        let revocation = ca1.revoke_node(
            vec![1u8; 32],
            vec![2u8; 32],
            RevocationReason::KeyCompromise,
            None,
        );

        let rsl = ca1.create_rsl(vec![revocation], 7, 1).unwrap();

        // Verify with different CA should fail
        assert!(!ca2.verify_rsl(&rsl).unwrap());
    }

    // ---- fixtures -------------------------------------------------------
    //
    // Key material and pinned byte vectors live in `tests/fixtures`, never here. A
    // key embedded in source is invisible to whoever has to rotate or audit it,
    // cannot be opened with a standard tool to check what it says, and tends to
    // outlive the person who pasted it.

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(FIXTURES).join(name)
    }

    fn read_fixture(name: &str) -> String {
        fs::read_to_string(fixture(name)).unwrap_or_else(|e| {
            panic!(
                "missing fixture {}: {e}\n\
                 regenerate deliberately with FIXTURES_UPDATE=1 cargo test -p ca --lib \
                 regenerate_fixtures, then read the diff before committing",
                fixture(name).display()
            )
        })
    }

    /// The revocations every pinned fixture in this directory was built from.
    ///
    /// Change these and `pre-change-key.preimage.hex` stops matching, which is the
    /// point: a fixture is only worth having if changing what it pins fails a test
    /// rather than quietly rewriting history.
    fn fixture_revocations() -> Vec<RevokedNode> {
        use chrono::TimeZone;

        vec![
            RevokedNode::new(
                vec![1u8; 32],
                chrono::Utc.timestamp_opt(1_699_900_000, 0).unwrap(),
                RevocationReason::KeyCompromise,
                vec![2u8; 32],
            ),
            RevokedNode::new(
                hex::decode("aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899")
                    .unwrap(),
                chrono::Utc.timestamp_opt(1_699_999_999, 0).unwrap(),
                RevocationReason::CaCompromise,
                Vec::new(),
            ),
        ]
    }

    const FIXTURE_ISSUED_AT: i64 = 1_700_000_000;
    const FIXTURE_EXPIRES_AT: i64 = 1_700_060_480;
    const FIXTURE_SEQUENCE: u64 = 7;

    /// Rewrites the generated files in `tests/fixtures`.
    ///
    /// A no-op unless asked for, so it cannot touch a checkout by accident:
    ///
    /// ```text
    /// FIXTURES_UPDATE=1 cargo test -p ca --lib regenerate_fixtures
    /// ```
    ///
    /// Three files are inputs and are never written here:
    ///
    /// - `pre-change-key.hex` — the legacy-format key the migration test starts from.
    /// - `pre-change-key.preimage.hex` — the pinned preimage. Regenerating it would
    ///   replace the pin with a copy of whatever the code does today, which is
    ///   exactly the nothing that [`the_rsl_preimage_is_pinned`] exists to catch. To
    ///   re-capture it honestly, derive the CA id with `openssl pkey -pubout` and
    ///   assemble the layout a second time from `ca/tests/fixtures/README.md` — an
    ///   implementation that is not this one — and only write the file where the two
    ///   agree.
    /// - `legacy-hex-issuer.preimage.hex` — the same list under the pre-2026-10-07
    ///   layout, kept so that change stays a pinned fact
    ///   ([`the_previous_layout_differed_only_in_the_issuer`]).
    #[test]
    fn regenerate_fixtures() {
        if std::env::var_os("FIXTURES_UPDATE").is_none() {
            return;
        }

        use chrono::TimeZone;
        let issued_at = chrono::Utc.timestamp_opt(FIXTURE_ISSUED_AT, 0).unwrap();
        let expires_at = chrono::Utc.timestamp_opt(FIXTURE_EXPIRES_AT, 0).unwrap();

        // 1. The legacy key, as PKCS#8 PEM — written through the same code path the
        //    migration command uses, so the fixture cannot drift from the tool.
        let legacy = CaRoot::load_legacy_hex_file(&fixture("pre-change-key.hex"))
            .expect("pre-change-key.hex is the legacy-format input");
        legacy
            .save_to_file(&fixture("pre-change-key.pem"), Some(0o600))
            .unwrap();

        // 2. The CA the anchor tests use, plus its published anchor. Re-saved rather
        //    than kept, so both files are always the bytes this code writes — a
        //    fixture produced by some other tool would stop proving the reader and
        //    the writer agree. The key itself is preserved, so a published list stays
        //    signed by the same CA across regenerations.
        let root_path = fixture("ca-root.pem");
        let ca = match CaRoot::load_from_file(&root_path) {
            Ok(ca) => ca,
            Err(e) => {
                assert!(
                    !root_path.exists(),
                    "ca-root.pem exists but will not load: {e}"
                );
                CaRoot::generate()
            }
        };
        ca.save_to_file(&root_path, Some(0o600)).unwrap();
        ca.trust_anchor()
            .save_to_file(&fixture("ca-root.pub.pem"))
            .unwrap();

        // 3. A published list, produced the way `ca ca-generate-rsl` produces one:
        //    the builder numbers it, `sign_rsl` signs what it is handed.
        let mut unsigned = RevocationStatusList::builder(ca.ca_id())
            .sequence_number(1)
            .add_revocations(fixture_revocations())
            .build_unsigned();
        unsigned.issued_at = issued_at;
        unsigned.expires_at = expires_at;
        let published = ca.sign_rsl(unsigned).unwrap();

        let signed_over = crate::signing::build_rsl_payload(
            &ca.ca_id(),
            &published.revocations,
            published.issued_at,
            published.expires_at,
            published.sequence_number,
        );
        fs::write(
            fixture("rsl-seq1.preimage.hex"),
            format!("{}\n", hex::encode(signed_over)),
        )
        .unwrap();
        fs::write(
            fixture("rsl-seq1.json"),
            format!(
                "{}\n",
                serde_json::to_string_pretty(&published).expect("an RSL serialises")
            ),
        )
        .unwrap();

        println!("fixtures rewritten in {FIXTURES}");
    }

    /// The RSL preimage is a wire format: the database holds the signed list, and
    /// a verifier that rebuilds different bytes rejects every list its CA has
    /// already issued. So the expected bytes live in a file, auditable outside a
    /// compiler, rather than in a constant next to the code that produces them.
    ///
    /// *Re-pinned 2026-10-07*, when the issuer went into the preimage as its 32
    /// bytes instead of 64 hex characters. A pin is worthless if the change that
    /// breaks it just rewrites it, so these bytes were not taken from the builder:
    /// the CA id came from `openssl pkey -in pre-change-key.pem -pubout`, and the
    /// layout was assembled a second time, independently, from the description in
    /// `ca/tests/fixtures/README.md`. Both agree with the file, and
    /// [`the_previous_layout_differed_only_in_the_issuer`] pins what the change was
    /// against the bytes it replaced.
    #[test]
    fn the_rsl_preimage_is_pinned() {
        use chrono::TimeZone;

        // The CA here is a fixture, not a literal: its id is the first 64 bytes of
        // the pinned preimage, so hardcoding it would let the two agree with each
        // other while both drifted from the key.
        let ca = CaRoot::load_from_file(&fixture("pre-change-key.pem"))
            .expect("pre-change-key.pem fixture loads");

        let actual = crate::signing::build_rsl_payload(
            &ca.ca_id(),
            &fixture_revocations(),
            chrono::Utc.timestamp_opt(FIXTURE_ISSUED_AT, 0).unwrap(),
            chrono::Utc.timestamp_opt(FIXTURE_EXPIRES_AT, 0).unwrap(),
            FIXTURE_SEQUENCE,
        );

        assert_eq!(
            hex::encode(actual),
            read_fixture("pre-change-key.preimage.hex").trim(),
            "the RSL preimage changed shape; every list published before this point \
             would now fail to verify"
        );
    }

    /// What the 2026-10-07 layout change actually was, pinned against the bytes it
    /// replaced instead of described in prose.
    ///
    /// `legacy-hex-issuer.preimage.hex` is the same list built under the previous
    /// layout, captured from the code before the change. Everything after the
    /// issuer field is identical: the whole difference is `00 00 00 40` followed by
    /// 64 hex characters becoming `00 00 00 20` followed by the 32-byte digest.
    ///
    /// So a list signed under the old layout does not verify under the new one, and
    /// nothing here pretends otherwise. When the bytes beneath a signature change,
    /// the honest outcomes are republishing the document or refusing it — never
    /// reading it both ways, which is how a signature stops meaning anything.
    #[test]
    fn the_previous_layout_differed_only_in_the_issuer() {
        use chrono::TimeZone;

        let ca = CaRoot::load_from_file(&fixture("pre-change-key.pem"))
            .expect("pre-change-key.pem fixture loads");
        let current = crate::signing::build_rsl_payload(
            &ca.ca_id(),
            &fixture_revocations(),
            chrono::Utc.timestamp_opt(FIXTURE_ISSUED_AT, 0).unwrap(),
            chrono::Utc.timestamp_opt(FIXTURE_EXPIRES_AT, 0).unwrap(),
            FIXTURE_SEQUENCE,
        );

        let legacy = hex::decode(read_fixture("legacy-hex-issuer.preimage.hex").trim())
            .expect("the legacy preimage is hex");

        let (legacy_len, rest) = legacy.split_at(4);
        assert_eq!(
            legacy_len,
            64u32.to_be_bytes(),
            "the old prefix counted hex characters"
        );
        assert_eq!(&rest[..64], ca.ca_id_hex().as_bytes());

        // Swap the issuer field for the current spelling and the old document is
        // the new one, byte for byte.
        let mut rebuilt = Vec::new();
        rebuilt.extend_from_slice(&32u32.to_be_bytes());
        rebuilt.extend_from_slice(&ca.ca_id());
        rebuilt.extend_from_slice(&rest[64..]);
        assert_eq!(rebuilt, current);
    }

    /// The consumer's view: a list that came out of storage, unmodified, verified by
    /// a node holding nothing but the published anchor. Signing and verifying inside
    /// one process proves less than this does.
    #[test]
    fn a_published_list_verifies_under_a_published_anchor() {
        let anchor = crate::TrustAnchor::load_from_file(&fixture("ca-root.pub.pem"))
            .expect("ca-root.pub.pem fixture loads");
        let published: RevocationStatusList =
            serde_json::from_str(&read_fixture("rsl-seq1.json")).expect("fixture is an RSL");

        assert_eq!(published.issuer_id, anchor.ca_id());
        assert!(anchor.verify_rsl(&published).unwrap());
        assert_eq!(published.revocation_count(), 2);

        // What was signed is on disk beside it, and agrees.
        let rebuilt = crate::signing::build_rsl_payload(
            &published.issuer_id,
            &published.revocations,
            published.issued_at,
            published.expires_at,
            published.sequence_number,
        );
        assert_eq!(
            hex::encode(rebuilt),
            read_fixture("rsl-seq1.preimage.hex").trim()
        );
    }

    #[test]
    fn test_rsl_node_revocation_check() {
        let ca = CaRoot::generate();

        let revoked_node_id = vec![1u8; 32];
        let active_node_id = vec![2u8; 32];

        let revoked_node = ca.revoke_node(
            revoked_node_id.clone(),
            vec![3u8; 32],
            RevocationReason::KeyCompromise,
            None,
        );

        let rsl = ca.create_rsl(vec![revoked_node], 7, 1).unwrap();

        assert!(rsl.is_node_revoked(&revoked_node_id));
        assert!(!rsl.is_node_revoked(&active_node_id));
    }
}
