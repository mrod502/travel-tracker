//! Deterministic helpers for the position tests.

use chrono::{DateTime, TimeZone, Utc};
use std::sync::atomic::{AtomicI64, Ordering};

use super::Position;
use crate::node::Clock;

/// Clock the tests move by hand, so caching and staleness need no sleeping.
pub struct ManualClock {
    now_ms: AtomicI64,
    /// Where `new` started, so the monotonic reading can advance by the same
    /// amount the wall reading does.
    start_ms: i64,
    base: std::time::Instant,
}

impl ManualClock {
    pub fn new(now: DateTime<Utc>) -> Self {
        let now_ms = now.timestamp_millis();
        Self {
            now_ms: AtomicI64::new(now_ms),
            start_ms: now_ms,
            base: std::time::Instant::now(),
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

    /// Advances with [`ManualClock::advance_ms`], so a test that moves time once
    /// moves both readings together.
    ///
    /// Sampling windows are intervals and are deliberately not measured on the wall
    /// clock, which is why the trait separates the two. A test clock that left
    /// `monotonic` at its default would move the node's timestamps while its windows
    /// stood still, and no amount of advancing would ever make a device due.
    fn monotonic(&self) -> std::time::Instant {
        let elapsed_ms = self
            .now_ms
            .load(Ordering::SeqCst)
            .saturating_sub(self.start_ms)
            .max(0);
        self.base + std::time::Duration::from_millis(elapsed_ms as u64)
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
    Position::new(latitude, 1.0, super::PositionOrigin::Gps, epoch() - offset).unwrap()
}
