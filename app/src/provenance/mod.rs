//! Provenance module for cryptographic signing and verification of occurrences.
//!
//! This module implements the canonical CBOR encoding and Ed25519 signing
//! as specified in `.knowledge/implementation/roadmap/phase_0/canonical-payload-spec.md`.
//!
//! # Overview
//!
//! - [`payload`] - The signed document, as [`payload::PayloadV1`] or
//!   [`payload::PayloadV2`] behind [`payload::CanonicalPayload`]. v2 signs the whole
//!   row a node authors; v1 signs six fields and is accepted, never written.
//! - [`encode`] - CBOR encoding per version, and one decoder that reads the
//!   document's own `schema_version` and refuses a version it cannot name
//! - [`sign`] - Ed25519 signing functions
//! - [`verify`] - Signature verification functions
//!
//! A verifier checks a stored row without knowing its version in advance: the bytes
//! carry the version, so [`encode::decode_payload`] picks the shape. What a given
//! version leaves unsigned is part of what it reports
//! ([`payload::CanonicalPayload::covers_row`]), rather than something an auditor has
//! to remember.
//!
//! # Example
//!
//! ```ignore
//! use app::provenance::{
//!     payload::{PayloadV2, VERSION_V2},
//!     encode::{encode_payload, decode_payload},
//!     sign::sign_payload,
//!     verify::verify_signature,
//! };
//!
//! // Build the payload from the row the node is about to write.
//! let payload = PayloadV2::from_occurrence(&occurrence)?;
//!
//! // Encode to canonical bytes and sign those bytes.
//! let encoded = encode_payload(&payload.into())?;
//! let signature = sign_payload(&private_key, &encoded)?;
//!
//! // Verify, years later, from the stored bytes alone.
//! let decoded = decode_payload(&encoded)?;
//! assert_eq!(decoded.version(), VERSION_V2);
//! assert!(decoded.covers_row());
//! verify_signature(&public_key, &encoded, &signature)?;
//! ```

pub mod encode;
pub mod payload;
pub mod sign;
pub mod verify;

// Re-export for convenience
