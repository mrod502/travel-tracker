//! Rate limiting for occurrence storage.
//!
//! This module provides a rate limiter that prevents database overload by
//! constraining how often a node writes observations for the same device.
//!
//! # Design Principle
//!
//! "Each device, per node, maximum N seconds between writes"
//!
//! This is NOT a hard limit on total throughput - just a constraint on
//! per-device frequency.
//!
//! # Example
//!
//! ```ignore
//! use app::node::rate_limiter::RateLimiter;
//! use std::time::Duration;
//!
//! let limiter = RateLimiter::new(Duration::from_secs(15));
//! let device_hash = vec![0x01; 32];
//!
//! if limiter.should_store(&device_hash) {
//!     // Proceed with storage
//!     store_occurrence(device_hash).await?;
//! } else {
//!     // Silently drop - device seen too recently
//! }
//! ```

use dashmap::DashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Configuration for the rate limiter.
#[derive(Debug, Clone)]
pub struct RateLimiterConfig {
    /// Minimum time between writes for the same device (default: 15 seconds).
    pub threshold: Duration,

    /// Optional maximum cache size (default: unlimited).
    /// Set this in high-density environments to prevent unbounded growth.
    pub max_cache_size: Option<usize>,
}

impl Default for RateLimiterConfig {
    fn default() -> Self {
        Self {
            threshold: Duration::from_secs(15),
            max_cache_size: None,
        }
    }
}

impl RateLimiterConfig {
    /// Create a new config with the specified threshold.
    pub fn with_threshold(threshold: Duration) -> Self {
        Self {
            threshold,
            ..Default::default()
        }
    }

    /// Set the maximum cache size.
    pub fn with_max_cache_size(mut self, size: usize) -> Self {
        self.max_cache_size = Some(size);
        self
    }
}

/// Rate limiter for controlling occurrence write frequency.
///
/// The rate limiter tracks when each device was last seen and prevents
/// storage if the device has been observed within the threshold period.
///
/// # Thread Safety
///
/// This struct is thread-safe and can be shared across threads using `Arc`.
///
/// # Memory Usage
///
/// Each entry in the cache uses approximately 104 bytes:
/// - 32 bytes for device_hash
/// - 8 bytes for Instant
/// - ~64 bytes for DashMap entry overhead
///
/// For 100,000 devices: ~10 MB
/// For 1,000,000 devices: ~100 MB
#[derive(Debug)]
pub struct RateLimiter {
    /// device_hash → last_seen timestamp
    cache: DashMap<Vec<u8>, Instant>,

    /// Minimum time between writes
    threshold: Duration,

    /// Optional max cache size
    max_cache_size: Option<usize>,

    /// Statistics: number of events allowed
    allow_count: AtomicUsize,

    /// Statistics: number of events rate-limited
    deny_count: AtomicUsize,
}

impl RateLimiter {
    /// Create a new rate limiter with the default configuration.
    ///
    /// Default threshold: 15 seconds
    pub fn new() -> Self {
        Self::with_config(RateLimiterConfig::default())
    }

    /// Create a new rate limiter with the specified configuration.
    ///
    /// # Arguments
    ///
    /// * `config` - Rate limiter configuration
    ///
    /// # Example
    ///
    /// ```ignore
    /// use app::node::rate_limiter::{RateLimiter, RateLimiterConfig};
    /// use std::time::Duration;
    ///
    /// let config = RateLimiterConfig::with_threshold(Duration::from_secs(20));
    /// let limiter = RateLimiter::with_config(config);
    /// ```
    pub fn with_config(config: RateLimiterConfig) -> Self {
        Self {
            cache: DashMap::new(),
            threshold: config.threshold,
            max_cache_size: config.max_cache_size,
            allow_count: AtomicUsize::new(0),
            deny_count: AtomicUsize::new(0),
        }
    }

    /// Check if an event should be rate-limited (DROPPED).
    ///
    /// This is a read-only check that does NOT record the event.
    ///
    /// # Arguments
    ///
    /// * `device_hash` - The hash of the device being observed
    ///
    /// # Returns
    ///
    /// * `true` - The event should be dropped (seen too recently)
    /// * `false` - The event is allowed (first seen or threshold expired)
    ///
    /// # Example
    ///
    /// ```ignore
    /// let limiter = RateLimiter::new();
    /// let device_hash = vec![0x01; 32];
    ///
    /// if !limiter.is_rate_limited(&device_hash) {
    ///     limiter.record(&device_hash);
    ///     // Process the event
    /// }
    /// ```
    pub fn is_rate_limited(&self, device_hash: &[u8]) -> bool {
        match self.cache.get(device_hash) {
            Some(last_seen) => {
                let elapsed = Instant::now().duration_since(*last_seen);
                elapsed < self.threshold
            }
            None => false, // First time seeing this device - allow
        }
    }

    /// Record an observation.
    ///
    /// This updates the last-seen timestamp for a device.
    /// Should be called AFTER `is_rate_limited` returns false.
    ///
    /// # Arguments
    ///
    /// * `device_hash` - The hash of the device being observed
    ///
    /// # Example
    ///
    /// ```ignore
    /// let limiter = RateLimiter::new();
    /// let device_hash = vec![0x01; 32];
    ///
    /// if !limiter.is_rate_limited(&device_hash) {
    ///     limiter.record(&device_hash);
    ///     // Process the event
    /// }
    /// ```
    pub fn record(&self, device_hash: &[u8]) {
        // Check if we need to evict entries
        if let Some(max_size) = self.max_cache_size {
            if self.cache.len() >= max_size {
                // Simple eviction: remove oldest entry
                if let Some(oldest) = self.find_oldest_entry() {
                    self.cache.remove(&oldest);
                }
            }
        }

        self.cache.insert(device_hash.to_vec(), Instant::now());
    }

    /// Combined check-and-record (atomic from caller's perspective).
    ///
    /// This method checks if the event is allowed and, if so, records it.
    /// Returns true if the event should be stored, false if rate-limited.
    ///
    /// # Arguments
    ///
    /// * `device_hash` - The hash of the device being observed
    ///
    /// # Returns
    ///
    /// * `true` - Event is allowed and has been recorded
    /// * `false` - Event is rate-limited
    ///
    /// # Example
    ///
    /// ```ignore
    /// let limiter = RateLimiter::new();
    /// let device_hash = vec![0x01; 32];
    ///
    /// if limiter.should_store(&device_hash) {
    ///     // Event is allowed and recorded, proceed with storage
    ///     store_occurrence(device_hash).await?;
    /// }
    /// ```
    pub fn should_store(&self, device_hash: &[u8]) -> bool {
        if self.is_rate_limited(device_hash) {
            self.deny_count.fetch_add(1, Ordering::SeqCst);
            return false;
        }

        self.record(device_hash);
        self.allow_count.fetch_add(1, Ordering::SeqCst);
        true
    }

    /// Get the time since the device was last seen.
    ///
    /// # Arguments
    ///
    /// * `device_hash` - The hash of the device
    ///
    /// # Returns
    ///
    /// * `Some(Duration)` - Time since last seen
    /// * `None` - Device has never been seen
    pub fn time_since_last(&self, device_hash: &[u8]) -> Option<Duration> {
        self.cache
            .get(device_hash)
            .map(|last_seen| Instant::now().duration_since(*last_seen))
    }

    /// Find the oldest entry in the cache (for eviction).
    fn find_oldest_entry(&self) -> Option<Vec<u8>> {
        let mut oldest_time = Instant::now();
        let mut oldest_key: Option<Vec<u8>> = None;

        for entry in self.cache.iter() {
            let (key, &last_seen) = entry.pair();
            if last_seen < oldest_time {
                oldest_time = last_seen;
                oldest_key = Some(key.clone());
            }
        }

        oldest_key
    }

    /// Get statistics about the rate limiter.
    pub fn stats(&self) -> RateLimiterStats {
        let total = self.allow_count.load(Ordering::SeqCst)
            + self.deny_count.load(Ordering::SeqCst);

        let hit_rate = if total > 0 {
            (self.deny_count.load(Ordering::SeqCst) as f64) / (total as f64) * 100.0
        } else {
            0.0
        };

        RateLimiterStats {
            cache_size: self.cache.len(),
            allow_count: self.allow_count.load(Ordering::SeqCst),
            deny_count: self.deny_count.load(Ordering::SeqCst),
            cache_hit_rate: hit_rate,
            threshold_ms: self.threshold.as_millis() as u64,
        }
    }

    /// Clear all entries from the cache.
    ///
    /// This is useful for testing or when you want to reset the rate limiter.
    pub fn clear(&self) {
        self.cache.clear();
        self.allow_count.store(0, Ordering::SeqCst);
        self.deny_count.store(0, Ordering::SeqCst);
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

/// Statistics about the rate limiter.
#[derive(Debug, Clone)]
pub struct RateLimiterStats {
    /// Current number of entries in the cache.
    pub cache_size: usize,

    /// Number of events allowed through.
    pub allow_count: usize,

    /// Number of events rate-limited.
    pub deny_count: usize,

    /// Cache hit rate as a percentage (0-100).
    pub cache_hit_rate: f64,

    /// Configured threshold in milliseconds.
    pub threshold_ms: u64,
}

impl std::fmt::Display for RateLimiterStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RateLimiterStats {{
  cache_size: {},
  allow_count: {},
  deny_count: {},
  cache_hit_rate: {:.1}%,
  threshold: {}ms
}}",
            self.cache_size,
            self.allow_count,
            self.deny_count,
            self.cache_hit_rate,
            self.threshold_ms
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rate_limit_allows_first_event() {
        let limiter = RateLimiter::new();
        let device_hash = vec![0x01; 32];

        assert!(!limiter.is_rate_limited(&device_hash));
    }

    #[test]
    fn test_rate_limit_blocks_within_threshold() {
        let limiter = RateLimiter::with_config(RateLimiterConfig::with_threshold(
            Duration::from_millis(100),
        ));
        let device_hash = vec![0x01; 32];

        limiter.record(&device_hash);

        // Immediately check again - should be limited
        assert!(limiter.is_rate_limited(&device_hash));
    }

    #[test]
    fn test_rate_limit_allows_after_threshold() {
        let limiter = RateLimiter::with_config(RateLimiterConfig::with_threshold(
            Duration::from_millis(100),
        ));
        let device_hash = vec![0x01; 32];

        limiter.record(&device_hash);

        // Wait for threshold to expire
        std::thread::sleep(Duration::from_millis(150));

        assert!(!limiter.is_rate_limited(&device_hash));
    }

    #[test]
    fn test_different_devices_independent() {
        let limiter = RateLimiter::new();
        let device_a = vec![0x01; 32];
        let device_b = vec![0x02; 32];

        limiter.record(&device_a);

        // Device B should not be affected
        assert!(!limiter.is_rate_limited(&device_b));
    }

    #[test]
    fn test_should_store_atomic() {
        let limiter = RateLimiter::new();
        let device_hash = vec![0x01; 32];

        // First call should succeed
        assert!(limiter.should_store(&device_hash));

        // Second call immediately after should fail
        assert!(!limiter.should_store(&device_hash));
    }

    #[test]
    fn test_should_store_allows_after_threshold() {
        let limiter = RateLimiter::with_config(RateLimiterConfig::with_threshold(
            Duration::from_millis(100),
        ));
        let device_hash = vec![0x01; 32];

        // First store succeeds
        assert!(limiter.should_store(&device_hash));

        // Immediately after, should be blocked
        assert!(!limiter.should_store(&device_hash));

        // After threshold, should succeed again
        std::thread::sleep(Duration::from_millis(150));
        assert!(limiter.should_store(&device_hash));
    }

    #[test]
    fn test_stats_tracking() {
        let limiter = RateLimiter::with_config(RateLimiterConfig::with_threshold(
            Duration::from_millis(100),
        ));

        let device_a = vec![0x01; 32];
        let device_b = vec![0x02; 32];

        // First event for device A - allowed
        assert!(limiter.should_store(&device_a));

        // Second event for device A - blocked
        assert!(!limiter.should_store(&device_a));

        // First event for device B - allowed
        assert!(limiter.should_store(&device_b));

        let stats = limiter.stats();
        assert_eq!(stats.allow_count, 2);
        assert_eq!(stats.deny_count, 1);
        assert_eq!(stats.cache_size, 2);
    }

    #[test]
    fn test_max_cache_size_eviction() {
        let limiter = RateLimiter::with_config(
            RateLimiterConfig::with_threshold(Duration::from_secs(60))
                .with_max_cache_size(3),
        );

        // Add 3 devices
        limiter.should_store(&vec![1u8; 32]);
        limiter.should_store(&vec![2u8; 32]);
        limiter.should_store(&vec![3u8; 32]);

        assert_eq!(limiter.cache.len(), 3);

        // Add 4th device - should trigger eviction
        limiter.should_store(&vec![4u8; 32]);

        // Should still be at max size
        assert_eq!(limiter.cache.len(), 3);
    }

    #[test]
    fn test_clear() {
        let limiter = RateLimiter::new();
        let device_hash = vec![0x01; 32];

        limiter.should_store(&device_hash);
        assert_eq!(limiter.cache.len(), 1);

        limiter.clear();
        assert_eq!(limiter.cache.len(), 0);
        assert_eq!(limiter.stats().allow_count, 0);
    }

    #[test]
    fn test_time_since_last() {
        let limiter = RateLimiter::with_config(RateLimiterConfig::with_threshold(
            Duration::from_secs(60),
        ));
        let device_hash = vec![0x01; 32];

        // Not seen yet
        assert!(limiter.time_since_last(&device_hash).is_none());

        // Record
        limiter.record(&device_hash);

        // Should have time
        let time = limiter.time_since_last(&device_hash).unwrap();
        assert!(time < Duration::from_millis(100));
    }
}
