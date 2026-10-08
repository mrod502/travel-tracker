# Clock Discipline Strategy (Phase 0)

**Status:** 📋 **DESIGN PHASE**  
**Date:** 2026-08-15

---

## Overview

Clock discipline ensures that timestamps in occurrence records are accurate and consistent. For Phase 0 (single node), this is **minimal** - we just need to ensure the system clock is NTP-synced.

---

## Design Principle

**"Record both corrected and local timestamps for auditing"**

- `observed_at` = Sync-corrected UTC timestamp (authoritative)
- `observed_at_node_local` = Raw node-local timestamp (for drift auditing)

---

## Phase 0: Single Node Assumption

### Current State

For Phase 0, clock discipline is **intentionally minimal**:

1. **Use system clock** (`chrono::Utc::now()`)
2. **Rely on host OS NTP** (assumed to be active)
3. **No cross-node sync needed** (only one node)

### Rationale

- Phase 0 validates data model and local API
- Multi-node clock sync is Phase 1+ concern
- Most modern OSes have NTP enabled by default

---

## Implementation (Phase 0)

### Clock Trait Abstraction

```rust
use chrono::{DateTime, Utc};

/// Abstract clock for testing and future NTP integration
pub trait Clock: Send + Sync {
    /// Get current UTC timestamp (authoritative)
    fn now(&self) -> DateTime<Utc>;
    
    /// Get node-local timestamp (may drift)
    /// For Phase 0: same as now()
    /// For Phase 1+: raw adapter time before sync correction
    fn now_local(&self) -> DateTime<Utc> {
        self.now()  // Default: no distinction in Phase 0
    }
}

/// System clock implementation (Phase 0)
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
    
    fn now_local(&self) -> DateTime<Utc> {
        Utc::now()  // No distinction in Phase 0
    }
}
```

### Integration with FullNode

```rust
pub struct FullNode {
    // ... other fields ...
    clock: Box<dyn Clock>,
}

impl FullNode {
    pub async fn store_occurrence(&self, device: &BluetoothDevice) -> Result<()> {
        let observed_at = self.clock.now();
        let observed_at_node_local = self.clock.now_local();

        let occurrence = Occurrence::builder()
            .observed_at(observed_at)
            .observed_at_node_local(observed_at_node_local)
            // ... other fields ...
            .build();

        OccurrenceRepository::create(self.pool, &occurrence).await?;
        
        Ok(())
    }
}
```

---

## Validation Required (Phase 0 Exit Criteria)

### Clock Drift Measurement

**Action:** Measure system clock drift over 24 hours

**Method:**
```bash
# On target deployment host
while true; do
    date -u +"%Y-%m-%dT%H:%M:%S.%3NZ"
    ntpdate -q pool.ntp.org 2>/dev/null | grep offset
    sleep 3600  # Every hour
done
```

**Expected Results:**
- Typical drift: < 100ms over 24h with NTP
- Acceptable for Phase 0: < 1s over 24h

### NTP Status Verification

**Check if NTP is active:**
```bash
# Check systemd-timesyncd status
systemctl status systemd-timesyncd

# Or check ntpd/chronyd
systemctl status ntpd
systemctl status chronyd
```

**Expected output:** Active (running)

---

## Future Enhancements (Phase 1+)

### Multi-Node Clock Sync

When multiple nodes sync data, clock drift becomes critical:

```
Node A (drift: +500ms)    Node B (drift: -300ms)
       │                        │
       ├─ sees device at T      ├─ sees same device at T+200ms
       └─ reports T             └─ reports T+200ms
    
Result: Same device appears 500ms apart in database
```

### NTP Client Integration

For Phase 1, add explicit NTP client:

```rust
use ntp_client::{Response, NtpClient};

pub struct NtpClock {
    ntp_server: String,
}

impl Clock for NtpClock {
    fn now(&self) -> DateTime<Utc> {
        // Query NTP server for accurate time
        let response = NtpClient::from_pool_name(&self.ntp_server)
            .unwrap()
            .get_time()
            .unwrap();
        
        // Convert to DateTime<Utc>
        DateTime::from_timestamp(
            response.system_time.sec as i64,
            response.system_time.nsec
        )
        .unwrap()
    }
    
    fn now_local(&self) -> DateTime<Utc> {
        // Return actual system clock (may drift)
        Utc::now()
    }
}
```

### Clock Sync Protocol (Phase 1+)

For nodes without NTP access (e.g., over LoRa), implement sync protocol:

```
Aggregator (NTP-synced)          Signal Node
       │                               │
       ├─→ SYNCHRONIZE(t_aggregator)   │
       │                               ├─ receives at t_node_local
       │                               ├─ computes offset = t_aggregator - t_node_local
       │                               └─ applies offset to future timestamps
       │
```

---

## Testing

### Unit Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_system_clock_returns_utc() {
        let clock = SystemClock;
        let now = clock.now();
        
        // Should be current UTC time (allow 1 second tolerance)
        let expected = Utc::now();
        let diff = (now - expected).num_milliseconds();
        assert!(diff.abs() < 1000);  // Within 1 second
    }

    #[test]
    fn test_system_clock_monotonic() {
        let clock = SystemClock;
        
        let t1 = clock.now();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let t2 = clock.now();
        
        assert!(t2 > t1);  // Time should move forward
    }

    #[test]
    fn test_now_equals_now_local_phase0() {
        let clock = SystemClock;
        
        // Phase 0: no distinction between now() and now_local()
        assert_eq!(clock.now(), clock.now_local());
    }
}
```

### Integration Tests

```rust
#[tokio::test]
async fn test_timestamps_in_database() {
    let pool = create_test_pool().await;
    let clock = SystemClock;
    
    let before = clock.now();
    
    // Create occurrence
    let occurrence = create_test_occurrence(&clock).await;
    OccurrenceRepository::create(pool, &occurrence).await.unwrap();
    
    let after = clock.now();
    
    // Retrieve and verify timestamps
    let loaded = OccurrenceRepository::get_by_id(pool, &occurrence.occurrence_id)
        .await
        .unwrap();

    // Should be within bounds
    assert!(loaded.observed_at >= before);
    assert!(loaded.observed_at <= after);
    assert_eq!(loaded.observed_at, loaded.observed_at_node_local);  // Phase 0
}
```

---

## Configuration

### Environment Variables

```bash
# NTP server for clock sync (Phase 1+)
NTP_SERVER=pool.ntp.org

# Clock sync interval in seconds (Phase 1+)
CLOCK_SYNC_INTERVAL_SECS=3600
```

### Config Struct

```rust
pub struct ClockConfig {
    pub ntp_server: Option<String>,
    pub sync_interval_secs: u64,
}

impl Default for ClockConfig {
    fn default() -> Self {
        Self {
            ntp_server: None,  // Phase 0: no explicit NTP
            sync_interval_secs: 3600,  // Sync hourly (Phase 1+)
        }
    }
}
```

---

## Troubleshooting

### Symptoms of Clock Issues

| Symptom | Possible Cause | Action |
|---------|----------------|--------|
| Timestamps in future | Clock ahead, or NTP not active | Check NTP status, sync manually |
| Timestamps in past | Clock behind, or NTP not active | Check NTP status, sync manually |
| Large gaps between `observed_at` and `observed_at_node_local` | Significant drift detected | Investigate NTP configuration |
| Occurrences out of order in queries | Clock moved backward (NTP step) | Check system logs for clock adjustments |

### Manual Sync Commands

```bash
# Force immediate sync (systemd-timesyncd)
sudo systemctl restart systemd-timesyncd

# Force immediate sync (ntpd)
sudo ntpd -q -p pool.ntp.org

# Check current offset
ntpdate -q pool.ntp.org
```

---

## References

- **Phase 0 Overview:** [`README.md`](./README.md)
- **Data Model:** [`../../architecture/data-model.md`](../../architecture/data-model.md)
- **NTP Client (Rust):** https://crates.io/crates/ntp-client

---

**Last Updated:** 2026-08-15  
**Status:** 📋 **DESIGN PHASE** - Phase 0 uses minimal implementation
