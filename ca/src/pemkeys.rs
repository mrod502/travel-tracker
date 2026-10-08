//! Ed25519 keys as files — the PKCS#8 half.
//!
//! This project has two long-lived private keys: the CA's root key
//! ([`crate::CaRoot`]) and a node's signing key (`app::node::identity::NodeIdentity`).
//! They are different keys with different secrecy, and exactly the same file
//! format problem. Putting the format here means there is one answer to it rather
//! than one per crate, and the answer is the standard one: a
//! `-----BEGIN PRIVATE KEY-----` PKCS#8 PEM document, readable by `openssl pkey`
//! and by any conforming implementation, written with `0600`.
//!
//! # Why v1 and not what dalek writes
//!
//! `SigningKey::to_pkcs8_pem` produces PKCS#8 **v2** — `OneAsymmetricKey` with the
//! public half in a `[1]` attribute. That is legal, and this crate reads it back
//! without complaint, but it is the narrower of the two encodings: OpenSSL 3 parses
//! a v2 Ed25519 key and then refuses it
//! (`STORE routines:ossl_store_handle_load_result:unsupported`), so the first tool
//! an operator reaches for cannot open a key this crate wrote. v1 omits the public
//! field and is read by everything, and it loses nothing: the public half is
//! derived from the secret on load, and where it needs to be published it has its
//! own file ([`crate::TrustAnchor`], SPKI PEM).
//!
//! [`write_signing_key_pem`] therefore builds the `KeypairBytes` by hand —
//! `public_key: None` is what selects v1 — and
//! [`the_written_key_is_pkcs8_v1`] checks the output with a PKCS#8 parser rather
//! than with this module.

use std::path::Path;

use ed25519_dalek::{
    pkcs8::{DecodePrivateKey, EncodePrivateKey, KeypairBytes},
    SigningKey, VerifyingKey,
};

use crate::error::{CaError, Result};

/// Read one PKCS#8 PEM private key.
///
/// `source` is what the error messages call the input — a path, or any label the
/// caller can name it by — because "invalid PEM" with no indication of which of
/// the three key files on this machine is at fault is not an error message.
///
/// # Errors
///
/// * no key found — the file holds something else; if it looks like the bare-hex
///   format this project used before PKCS#8, [`is_legacy_hex_key`] says so and the
///   caller can name the command that converts it
/// * more than one key — a private key file holds exactly one
/// * a key that is not an Ed25519 key
pub fn read_signing_key_pem(contents: &str, source: &str) -> Result<SigningKey> {
    let mut reader = std::io::BufReader::new(contents.as_bytes());
    let mut keys = Vec::new();
    for key in rustls_pemfile::pkcs8_private_keys(&mut reader) {
        keys.push(key.map_err(|e| {
            CaError::InvalidKey(format!("Failed to read PKCS#8 PEM from {source}: {e}"))
        })?);
    }

    match keys.len() {
        1 => SigningKey::from_pkcs8_der(keys[0].secret_pkcs8_der())
            .map_err(|e| CaError::InvalidKey(format!("{source} is not an Ed25519 key: {e}"))),
        0 => Err(CaError::InvalidKey(format!(
            "no PKCS#8 PEM private key found in {source}"
        ))),
        n => Err(CaError::InvalidKey(format!(
            "{source} holds {n} private keys; a key file holds exactly one"
        ))),
    }
}

/// A signing key as a PKCS#8 **v1** PEM document, LF line endings.
///
/// See the module documentation for why this is built by hand instead of calling
/// `SigningKey::to_pkcs8_pem`.
pub fn write_signing_key_pem(signing_key: &SigningKey) -> String {
    let keypair = KeypairBytes {
        secret_key: *signing_key.as_bytes(),
        // The field that selects the version: `None` is v1, `Some` is v2.
        public_key: None,
    };

    keypair
        .to_pkcs8_pem(pkcs8::LineEnding::LF)
        .expect("encoding a private key has no failure mode")
        .to_string()
}

/// Read one SPKI PEM public key.
///
/// The companion of [`read_signing_key_pem`] for the half that gets handed around:
/// a node's `node_identity.pub.pem` at enrollment, a CA's anchor at startup. A file
/// holding a **private** key is named as such rather than reported as unparseable —
/// pointing at the wrong of the two files beside each other is the obvious mistake,
/// and the consequence of accepting it is distributing a secret.
pub fn read_public_key_pem(contents: &str, source: &str) -> Result<VerifyingKey> {
    use ed25519_dalek::pkcs8::DecodePublicKey;

    let mut reader = std::io::BufReader::new(contents.as_bytes());
    let mut keys = Vec::new();
    for key in rustls_pemfile::public_keys(&mut reader) {
        keys.push(key.map_err(|e| {
            CaError::InvalidKey(format!("Failed to read SPKI PEM from {source}: {e}"))
        })?);
    }

    match keys.len() {
        1 => VerifyingKey::from_public_key_der(keys[0].as_ref())
            .map_err(|e| CaError::InvalidKey(format!("{source} is not an Ed25519 key: {e}"))),
        0 => Err(CaError::InvalidKey(format!(
            "no \"BEGIN PUBLIC KEY\" section found in {source}{}",
            if contents.contains("BEGIN PRIVATE KEY") {
                ", which holds a *private* key — publish the public half instead"
            } else {
                ""
            }
        ))),
        n => Err(CaError::InvalidKey(format!(
            "{source} holds {n} public keys; expected exactly one"
        ))),
    }
}

/// A public key as an SPKI PEM document — `-----BEGIN PUBLIC KEY-----`, the
/// format [`crate::TrustAnchor::from_pem`] reads.
///
/// The public half is not stored in the private key file (see the module docs); when
/// it needs to leave the machine it gets this file. Unlike the private half there is
/// no version trap here — SPKI has one shape, and `openssl pkey -pubout` produces the
/// same bytes.
pub fn write_public_key_pem(verifying_key: &VerifyingKey) -> String {
    use ed25519_dalek::pkcs8::spki::EncodePublicKey;

    verifying_key
        .to_public_key_pem(pkcs8::LineEnding::LF)
        .expect("encoding a public key has no failure mode")
}

/// Write key material to `path`, creating parent directories, and set its mode.
///
/// `permissions` is Unix-only and optional; a caller that passes `None` is writing
/// something that does not need a mode of its own — a private key should always be
/// given `0o600`.
pub fn write_key_file(path: &Path, contents: &str, permissions: Option<u32>) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| {
                CaError::Io(format!(
                    "Failed to create directory {}: {e}",
                    parent.display()
                ))
            })?;
        }
    }

    std::fs::write(path, contents.as_bytes())
        .map_err(|e| CaError::Io(format!("Failed to write {}: {e}", path.display())))?;

    #[cfg(unix)]
    if let Some(perm) = permissions {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(perm)).map_err(|e| {
            CaError::Io(format!(
                "Failed to set permissions on {}: {e}",
                path.display()
            ))
        })?;
    }

    #[cfg(not(unix))]
    let _ = permissions;

    Ok(())
}

/// Does this look like the bare-hex root key file used before PKCS#8?
///
/// A 32-byte secret read as the wrong format yields a confident, entirely wrong
/// identity, so a reader that expects PEM refuses this rather than guessing — and
/// names the command that converts it.
pub fn is_legacy_hex_key(contents: &str) -> bool {
    let trimmed = contents.trim();
    trimmed.len() == 64 && trimmed.chars().all(|c| c.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::OsRng;

    fn key() -> SigningKey {
        SigningKey::generate(&mut OsRng)
    }

    /// The claim this module exists to keep: the file we write is PKCS#8 v1, read
    /// back by a PKCS#8 parser rather than by `read_signing_key_pem`. `SigningKey`'s
    /// own encoder writes v2, which OpenSSL 3 rejects for Ed25519.
    #[test]
    fn the_written_key_is_pkcs8_v1() {
        let signing_key = key();
        let pem = write_signing_key_pem(&signing_key);

        let mut reader = std::io::BufReader::new(pem.as_bytes());
        let der = rustls_pemfile::pkcs8_private_keys(&mut reader)
            .next()
            .expect("a private key section")
            .expect("parses as PKCS#8");
        let info = pkcs8::PrivateKeyInfo::try_from(der.secret_pkcs8_der())
            .expect("a PKCS#8 OneAsymmetricKey");

        assert_eq!(
            info.version(),
            pkcs8::Version::V1,
            "a v2 key is unreadable by OpenSSL 3 for Ed25519"
        );
    }

    #[test]
    fn a_key_written_here_reads_back_as_the_same_key() {
        let signing_key = key();
        let pem = write_signing_key_pem(&signing_key);
        let loaded = read_signing_key_pem(&pem, "test").unwrap();

        assert_eq!(loaded.verifying_key(), signing_key.verifying_key());
    }

    #[test]
    fn a_v2_key_is_still_readable() {
        let signing_key = key();
        let pem = signing_key
            .to_pkcs8_pem(pkcs8::LineEnding::LF)
            .expect("dalek can encode its own key");

        let mut reader = std::io::BufReader::new(pem.as_bytes());
        let der = rustls_pemfile::pkcs8_private_keys(&mut reader)
            .next()
            .unwrap()
            .unwrap();
        let info = pkcs8::PrivateKeyInfo::try_from(der.secret_pkcs8_der()).unwrap();
        assert_eq!(info.version(), pkcs8::Version::V2, "the input is v2");

        // Reading both versions is what lets a key written by the earlier code —
        // or by another tool — load without a migration.
        let loaded = read_signing_key_pem(pem.as_str(), "v2 test key").unwrap();
        assert_eq!(loaded.verifying_key(), signing_key.verifying_key());
    }

    #[test]
    fn a_file_that_is_not_a_private_key_says_which_one_was_expected() {
        let err = read_signing_key_pem("not a key at all", "/etc/btmon/node_identity.pem")
            .expect_err("text is not a key");

        let message = err.to_string();
        assert!(
            message.contains("/etc/btmon/node_identity.pem"),
            "{message}"
        );
        assert!(message.contains("PKCS#8"), "{message}");
    }

    #[test]
    fn two_keys_in_one_file_are_refused() {
        let both = format!(
            "{}{}",
            write_signing_key_pem(&key()),
            write_signing_key_pem(&key())
        );

        let err = read_signing_key_pem(&both, "two.pem").unwrap_err();
        assert!(err.to_string().contains("exactly one"), "{err}");
    }

    #[test]
    fn the_legacy_format_is_recognised_by_its_shape_only() {
        assert!(is_legacy_hex_key(&"ab".repeat(32)));
        assert!(is_legacy_hex_key(&format!("{}\n", "ab".repeat(32))));
        // Not hex, the wrong length, or a PEM body.
        assert!(!is_legacy_hex_key("zz".repeat(32).as_str()));
        assert!(!is_legacy_hex_key(&"ab".repeat(31)));
        assert!(!is_legacy_hex_key("-----BEGIN PRIVATE KEY-----\nMCow\n"));
    }

    #[test]
    fn a_public_key_written_here_reads_back_as_the_same_key() {
        let signing_key = key();
        let pem = write_public_key_pem(&signing_key.verifying_key());

        let loaded = read_public_key_pem(&pem, "test").unwrap();
        assert_eq!(loaded.as_bytes(), signing_key.verifying_key().as_bytes());
    }

    /// The mistake this catches is reaching for the private key beside the public one
    /// — the two files sit in the same directory and differ by six characters.
    #[test]
    fn a_private_key_where_a_public_one_was_expected_is_named_not_misparsed() {
        let err = read_public_key_pem(
            &write_signing_key_pem(&key()),
            "/var/lib/btmon/node_identity.pem",
        )
        .unwrap_err();

        let message = err.to_string();
        assert!(
            message.contains("/var/lib/btmon/node_identity.pem"),
            "{message}"
        );
        assert!(message.contains("private"), "{message}");
    }
}
