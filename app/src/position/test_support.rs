//! Deterministic helpers for the position tests.

use chrono::{DateTime, TimeZone, Utc};
use std::sync::atomic::{AtomicI64, Ordering};

use super::Position;
use crate::node::Clock;

/// Clock the tests move by hand, so caching and staleness need no sleeping.
pub struct ManualClock {
    now_ms: AtomicI64,
}

impl ManualClock {
    pub fn new(now: DateTime<Utc>) -> Self {
        Self {
            now_ms: AtomicI64::new(now.timestamp_millis()),
        }
    }

    pub fn advance_ms(&self, millis: i64) {
        self.now_ms.fetch_add(millis, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> DateTime<Utc> {
        Utc.timestamp_millis_opt(self.now_ms.load(Ordering::SeqCst))
            .unwrap()
    }
}

/// `1_700_000_000` UTC, the baseline every position test starts from.
pub fn epoch() -> DateTime<Utc> {
    Utc.timestamp_opt(1_700_000_000, 0).unwrap()
}

/// A GPS fix at `latitude`, `1.0` longitude, acquired at [`epoch`].
pub fn gps_fix(latitude: f64) -> Position {
    gps_fix_at(latitude, epoch())
}

/// A GPS fix acquired at an explicit instant.
pub fn gps_fix_at(latitude: f64, fixed_at: DateTime<Utc>) -> Position {
    Position::new(latitude, 1.0, super::PositionOrigin::Gps, fixed_at).unwrap()
}

/// A fix acquired `age` before [`epoch`], for staleness assertions.
pub fn gps_fix_aged(latitude: f64, age: std::time::Duration) -> Position {
    let offset = chrono::Duration::from_std(age).unwrap();
    Position::new(
        latitude,
        1.0,
        super::PositionOrigin::Gps,
        epoch() - offset,
    )
    .unwrap()
}
