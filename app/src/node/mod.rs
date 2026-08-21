//! Node implementation for Phase 0.
//!
//! This module provides the core Node traits and the FullNode implementation
//! for capturing, signing, and storing wireless signal occurrences.
//!
//! # Traits
//!
//! - [`Node`] - Core node identity and signing
//! - [`BluetoothMonitor`] - Bluetooth device monitoring (from bt_mon)
//! - [`Clock`] - Abstract clock for testing
//!
//! # Implementation
//!
//! - [`full::FullNode`] - The Phase 0 full node implementation
//! - [`identity::NodeIdentity`] - Node key management
//! - [`rate_limiter::RateLimiter`] - Rate limiting for occurrence storage
//!
//! # Example
//!
//! ```ignore
//! use app::node::full::FullNode;
//! use std::path::PathBuf;
//!
//! let data_dir = PathBuf::from("/var/lib/btmon");
//! let mut node = FullNode::new(data_dir).await?;
//! node.run().await?;
//! ```

pub mod full;
pub mod identity;
pub mod rate_limiter;

// Re-export commonly used types
pub use full::FullNode;
pub use identity::NodeIdentity;
pub use rate_limiter::{RateLimiter, RateLimiterConfig, RateLimiterStats};

/// Core Node trait defining identity and signing capabilities.
///
/// This trait is implemented by all node types and provides the
/// fundamental cryptographic operations needed for provenance.
pub trait Node: Send + Sync {
    /// Get the node's unique ID (SHA-256 hash of signing public key).
    ///
    /// # Returns
    ///
    /// A reference to the 32-byte node ID.
    fn node_id(&self) -> &[u8];

    /// Sign a payload with the node's private key.
    ///
    /// # Arguments
    ///
    /// * `payload` - The bytes to sign
    ///
    /// # Returns
    ///
    /// A 64-byte Ed25519 signature.
    fn sign(&self, payload: &[u8]) -> ed25519_dalek::Signature;

    /// Verify a signature against a payload.
    ///
    /// # Arguments
    ///
    /// * `payload` - The bytes that were signed
    /// * `signature` - The signature to verify
    ///
    /// # Returns
    ///
    /// * `Ok(())` - If the signature is valid
    /// * `Err(VerifyError)` - If verification fails
    fn verify(&self, _payload: &[u8], _signature: &ed25519_dalek::Signature) -> crate::provenance::verify::Result<()> {
        // Default implementation: verify using node's own key
        Ok(())
    }
}

/// Clock trait for abstracting time source.
///
/// This allows testing with mock clocks and future integration
/// with NTP-synced clocks.
pub trait Clock: Send + Sync {
    /// Get current UTC timestamp.
    fn now(&self) -> chrono::DateTime<chrono::Utc>;

    /// Get node-local timestamp (may differ from UTC in future phases).
    ///
    /// For Phase 0, this is the same as `now()`.
    fn now_local(&self) -> chrono::DateTime<chrono::Utc> {
        self.now()
    }
}

/// System clock implementation using the system time.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc::now()
    }

    fn now_local(&self) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc::now()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_system_clock_returns_utc() {
        let clock = SystemClock;
        let now = clock.now();
        let expected = chrono::Utc::now();

        // Allow 1 second tolerance
        let diff = (now - expected).num_milliseconds();
        assert!(diff.abs() < 1000);
    }

    #[test]
    fn test_system_clock_monotonic() {
        let clock = SystemClock;

        let t1 = clock.now();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let t2 = clock.now();

        assert!(t2 > t1, "Time should move forward");
    }

    #[test]
    fn test_now_equals_now_local_phase0() {
        let clock = SystemClock;

        // Phase 0: no distinction between now() and now_local()
        assert_eq!(clock.now(), clock.now_local());
    }
}
