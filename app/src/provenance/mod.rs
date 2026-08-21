//! Provenance module for cryptographic signing and verification of occurrences.
//!
//! This module implements the canonical CBOR encoding and Ed25519 signing
//! as specified in `.knowledge/implementation/roadmap/phase_0/canonical-payload-spec.md`.
//!
//! # Overview
//!
//! - [`payload::CanonicalPayload`] - The structured payload before encoding
//! - [`encode`] - CBOR encoding/decoding functions
//! - [`sign`] - Ed25519 signing functions
//! - [`verify`] - Signature verification functions
//!
//! # Example
//!
//! ```ignore
//! use app::provenance::{
//!     payload::CanonicalPayload,
//!     encode::{encode_payload, decode_payload},
//!     sign::sign_payload,
//!     verify::verify_signature,
//! };
//!
//! // Build payload
//! let payload = CanonicalPayload { /* ... */ };
//!
//! // Encode to canonical bytes
//! let encoded = encode_payload(&payload)?;
//!
//! // Sign
//! let signature = sign_payload(&private_key, &encoded)?;
//!
//! // Verify
//! verify_signature(&public_key, &encoded, &signature)?;
//! ```

pub mod encode;
pub mod payload;
pub mod sign;
pub mod verify;
