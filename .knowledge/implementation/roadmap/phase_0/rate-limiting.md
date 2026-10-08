# Rate Limiting Strategy (Phase 0)

**Status:** 📋 **DESIGN PHASE**  
**Date:** 2026-08-15

---

## Overview

Rate limiting prevents database overload by constraining how often a node writes observations for the same device. This is **enforced at the application layer**, before any database write.

---

## Design Principle

**"Each device, per node, maximum N seconds between writes"**

Not a hard limit on total throughput - just a constraint on per-device frequency.

---

## Target Parameters

### Current Assumption (To Be Validated)

| Parameter | Value | Rationale |
|-----------|-------|-----------|
| **Rate limit threshold** | 15 seconds (midpoint of 10-20s range) | Balance between data freshness and DB load |
| **Scope** | Per `(node_id, device_hash)` pair | Same device at different nodes = separate counters |
| **Storage** | In-memory cache (DashMap) | Fast lookups, no persistence needed |

### Why 10-20s Range?

Most BLE devices advertise at intervals:
- **iBeacons/Eddystone:** Typically 100ms - 1s (too frequent!)
- **Smartphones:** ~1-5 seconds when scanning
- **Wearables/Fitbits:** ~1-10 seconds
- **IoT sensors:** ~1-60 seconds (varies widely)

**Problem:** Without rate limiting, a single beacon could generate 3600+ writes/hour.

**Goal:** Cap at ~240-360 writes/device/hour (10-15s threshold).

---

## Implementation Design

### Data Structure

```rust
use dashmap::DashMap;
use std::time::{Instant, Duration};

pub struct RateLimiter {
    /// device_hash → last_seen timestamp
    cache: DashMap<Vec<u8>, Instant>,
    
    /// Minimum time between writes (e.g., 15 seconds)
    threshold: Duration,
    
    /// Optional: max cache size (prevent unbounded growth)
    max_size: Option<usize>,
}
```

### Core Algorithm

```rust
impl RateLimiter {
    /// Check if event should be rate-limited (DROPPED)
    pub fn is_rate_limited(&self, device_hash: &[u8]) -> bool {
        let now = Instant::now();
        
        match self.cache.get(device_hash) {
            Some(last_seen) => {
                let elapsed = now.duration_since(*last_seen);
                if elapsed < self.threshold {
                    true  // DROP - too soon
                } else {
                    false  // ALLOW - time to write
                }
            }
            None => {
                // First time seeing this device - allow
                false
            }
        }
    }

    /// Record observation (must be called AFTER is_rate_limited returns false)
    pub fn record(&self, device_hash: &[u8]) {
        self.cache.insert(device_hash.to_vec(), Instant::now());
    }

    /// Combined check-and-record (atomic from caller's perspective)
    pub fn should_store(&self, device_hash: &[u8]) -> bool {
        if self.is_rate_limited(device_hash) {
            return false;
        }
        
        self.record(device_hash);
        true
    }
}
```

### Integration with FullNode

```rust
impl FullNode {
    async fn handle_device_discovered(&self, device: &BluetoothDevice) -> Result<()> {
        // Compute device hash
        let device_hash = compute_device_hash(&device.address);

        // Rate limiting check (MUST happen before DB write)
        if self.rate_limiter.should_store(&device_hash) {
            // Proceed with storage
            self.store_occurrence(device).await?;
        } else {
            // Silently drop - device seen too recently
            debug!(
                "Rate limiting device {} (last seen {}ms ago)",
                hex::encode(&device_hash),
                self.rate_limiter.time_since_last(&device_hash).unwrap_or(0)
            );
        }

        Ok(())
    }
}
```

---

## Configuration

### Environment Variables

```bash
# Rate limit threshold in milliseconds (default: 15000 = 15s)
RATE_LIMIT_MS=15000

# Optional: max cache size (default: unlimited)
RATE_LIMIT_MAX_CACHE_SIZE=100000
```

### Config Struct

```rust
pub struct RateLimiterConfig {
    pub threshold_ms: u64,
    pub max_cache_size: Option<usize>,
}

impl Default for RateLimiterConfig {
    fn default() -> Self {
        Self {
            threshold_ms: 15_000,  // 15 seconds
            max_cache_size: None,
        }
    }
}
```

---

## Validation Required (Phase 0 Exit Criteria)

### Metrics to Collect

During Phase 0 testing, measure:

1. **Raw advertisement rate** (before rate limiting)
   - Devices discovered per hour
   - Advertisements received per device per hour
   - Total events (before filtering)

2. **Rate-limited write rate** (after filtering)
   - Occurrences written per hour
   - Reduction ratio (raw → stored)
   - Per-device distribution (some devices > others?)

3. **Cache behavior**
   - Cache hit rate (rate-limited vs. first-seen)
   - Cache size over time
   - Memory usage

### Validation Thresholds

| Metric | Target | Too Low | Too High |
|--------|--------|---------|----------|
| **Reduction ratio** | 10:1 to 50:1 | < 5:1 (not enough filtering) | > 100:1 (too aggressive) |
| **Writes/device/hour** | 240-360 | < 120 (missing data) | > 600 (overloading DB) |
| **Cache hit rate** | 80-95% | < 70% (threshold too high) | > 98% (threshold too low) |

### Adjustment Strategy

Based on measured metrics:

| Scenario | Action | Example |
|----------|--------|---------|
| Too many writes | **Increase** threshold | 15s → 20s |
| Too few writes | **Decrease** threshold | 15s → 10s |
| Cache memory high | **Add** cache size limit | None → 100K entries |
| High variance by device | **Consider** per-device config | Dense areas: 20s, sparse: 10s |

---

## Edge Cases

### 1. Node Restart

**Problem:** In-memory cache is lost on restart.

**Impact:** Temporary burst of writes as devices are "first seen" again.

**Mitigation:** Acceptable for Phase 0. Can persist cache to disk if needed.

### 2. Clock Changes

**Problem:** `Instant::now()` is monotonic (not affected by clock changes), but system time changes don't affect it.

**Impact:** None - `Instant` is safe for duration measurement.

### 3. Cache Bloat

**Problem:** If node sees many unique devices, cache grows unbounded.

**Mitigation:** 
- Set `max_cache_size` in high-density environments
- Optional: LRU eviction (not implemented in Phase 0)

### 4. Duplicate Advertisements

**Problem:** Same device, same timestamp (rare race condition).

**Impact:** First write succeeds, second is rate-limited.

**Mitigation:** Acceptable - duplicates are extremely rare.

---

## Testing

### Unit Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rate_limit_allows_first_event() {
        let limiter = RateLimiter::new(Duration::from_secs(15));
        let device_hash = vec![0x01; 32];

        assert!(!limiter.is_rate_limited(&device_hash));  // First event allowed
    }

    #[test]
    fn test_rate_limit_blocks_within_threshold() {
        let limiter = RateLimiter::new(Duration::from_secs(15));
        let device_hash = vec![0x01; 32];

        limiter.record(&device_hash);
        
        // Immediately check again - should be limited
        assert!(limiter.is_rate_limited(&device_hash));
    }

    #[test]
    fn test_rate_limit_allows_after_threshold() {
        let limiter = RateLimiter::new(Duration::from_millis(100));
        let device_hash = vec![0x01; 32];

        limiter.record(&device_hash);
        
        // Wait for threshold to expire
        std::thread::sleep(Duration::from_millis(150));
        
        assert!(!limiter.is_rate_limited(&device_hash));
    }

    #[test]
    fn test_different_devices_independent() {
        let limiter = RateLimiter::new(Duration::from_secs(15));
        let device_a = vec![0x01; 32];
        let device_b = vec![0x02; 32];

        limiter.record(&device_a);
        
        // Device B should not be affected
        assert!(!limiter.is_rate_limited(&device_b));
    }

    #[test]
    fn test_should_store_atomic() {
        let limiter = RateLimiter::new(Duration::from_secs(15));
        let device_hash = vec![0x01; 32];

        // First call should succeed
        assert!(limiter.should_store(&device_hash));

        // Second call immediately after should fail
        assert!(!limiter.should_store(&device_hash));
    }
}
```

### Integration Tests

```rust
#[tokio::test]
async fn test_rate_limiting_in_capture_flow() {
    let limiter = RateLimiter::new(Duration::from_millis(100));
    let pool = create_test_pool().await;
    let repo = OccurrenceRepository::new(pool);

    // Simulate rapid device discoveries
    let device_hash = vec![0x01; 32];
    let mut stored_count = 0;

    for i in 0..10 {
        if limiter.should_store(&device_hash) {
            // Would write to DB
            stored_count += 1;
        }
        
        // Simulate advertisement interval (50ms)
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // At 50ms interval with 100ms threshold, expect ~50% pass rate
    // Actually: first one passes, then every other one passes
    assert!(stored_count >= 4 && stored_count <= 6);
}
```

---

## Performance Considerations

### Memory Usage

**Per-entry overhead:**
- `Vec<u8>` (32 bytes device_hash) + `Instant` (8 bytes) = ~40 bytes data
- DashMap entry overhead = ~64 bytes
- **Total per entry:** ~104 bytes

**Example cache sizes:**
- 10,000 devices = ~1 MB
- 100,000 devices = ~10 MB
- 1,000,000 devices = ~100 MB

**Conclusion:** Cache memory is negligible for typical deployments (< 10MB for hundreds of devices).

### Lookup Performance

**DashMap characteristics:**
- O(1) average lookup
- Lock-free reads (sharded Mutexes)
- Concurrent access safe

**Expected latency:** < 1μs per lookup

**Conclusion:** Negligible impact on event processing pipeline.

---

## Future Enhancements (Post-Phase 0)

### 1. Persistent Cache

Store cache to disk across restarts:
```rust
pub struct PersistentRateLimiter {
    cache: DashMap<Vec<u8>, Instant>,
    persistence_path: PathBuf,
    // Periodic sync to disk
}
```

### 2. Per-Device Configuration

Different thresholds for different device types:
```rust
pub struct AdaptiveRateLimiter {
    default_threshold: Duration,
    device_thresholds: DashMap<Vec<u8>, Duration>,  // Per-device override
}
```

### 3. Sliding Window

Instead of fixed threshold, use sliding window for smoother rate limiting:
```rust
pub struct SlidingWindowRateLimiter {
    window: DashMap<Vec<u8>, Vec<Instant>>,  // Timestamp history
    window_duration: Duration,
    max_events_per_window: usize,
}
```

---

## References

- **Phase 0 Overview:** [`README.md`](./README.md)
- **Data Model:** [`../../architecture/data-model.md`](../../architecture/data-model.md)
- **Storage:** [`../../architecture/storage.md`](../../architecture/storage.md)

---

**Last Updated:** 2026-08-15  
**Status:** 📋 **DESIGN PHASE** - Awaiting real-world validation
