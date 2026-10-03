//! Replayable observation timestamps.
//!
//! Identity resolution is driven by *elapsed time*: the matching window, the
//! time-continuity scorer, and expiry are all intervals. A single clock cannot
//! serve both jobs the resolver does with time:
//!
//! - **Live capture** needs a clock that never runs backwards, because an NTP
//!   correction of a few seconds during a scan would reorder observations, make
//!   `elapsed` negative, and turn a stable interval estimate to noise.
//! - **Replay** needs a clock that survives the process: [`std::time::Instant`]
//!   has no epoch, cannot be written to a row, and cannot be compared with an
//!   `Instant` from another process — so an observation stamped with one can
//!   never be re-resolved after a restart.
//!
//! [`ObservationTime`] therefore carries **both**: an epoch-anchored wall clock
//! reading and, when the value was produced by a live capture, the monotonic
//! instant read alongside it. The pair is what makes the same observation type
//! usable for a radio and for last month's rows.
//!
//! # Which clock a comparison uses
//!
//! [`ObservationTime::elapsed_since`] and [`ObservationTime::ordering`] prefer
//! the monotonic reading when *both* sides have one — that is the case where a
//! wall-clock step could otherwise corrupt the interval — and fall back to the
//! wall clock otherwise. Two values written to a database and read back both
//! have no monotonic component, so replay compares wall clocks, which is the
//! only common ground a stored row has.
//!
//! [`PartialOrd`]/[`Ord`] are implemented on the **wall clock** alone, so
//! sorting a replay by `timestamp` is deterministic and does not depend on which
//! values happened to keep a monotonic reading.
//!
//! # Example
//!
//! ```
//! use bt_iden::time::ObservationTime;
//! use std::time::{Duration, SystemTime};
//!
//! // Live capture: both clocks.
//! let live = ObservationTime::now();
//! assert!(live.monotonic().is_some());
//!
//! // Replay: a row only knows its wall clock.
//! let stored = ObservationTime::from_wall(live.wall());
//! assert!(stored.monotonic().is_none());
//!
//! // Elapsed still works across the two, and never underflows.
//! let later = ObservationTime::from_wall(live.wall() + Duration::from_secs(2));
//! assert_eq!(later.elapsed_since(&stored), Duration::from_secs(2));
//! assert_eq!(stored.elapsed_since(&later), Duration::ZERO);
//! ```

use std::cmp::Ordering;
use std::fmt;
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// An observation timestamp: an epoch-anchored wall clock plus, when the capture
/// process had one, the monotonic instant read alongside it.
#[derive(Debug, Clone, Copy)]
pub struct ObservationTime {
    wall: SystemTime,
    monotonic: Option<Instant>,
}

impl ObservationTime {
    /// Stamps a value happening now, recording both clocks.
    ///
    /// This is what a capture path uses: the wall clock is what gets stored, the
    /// monotonic instant is what keeps intervals sane for the rest of this
    /// process's life.
    pub fn now() -> Self {
        Self {
            wall: SystemTime::now(),
            monotonic: Some(Instant::now()),
        }
    }

    /// Builds a value from a wall clock reading alone — the replay case, where
    /// the only thing that survived is the column.
    pub fn from_wall(wall: SystemTime) -> Self {
        Self {
            wall,
            monotonic: None,
        }
    }

    /// Builds a value from a monotonic instant alone, by anchoring it to a
    /// wall-clock reading captured once per process.
    ///
    /// The anchor is established the first time this is called, so the further a
    /// caller's instant is from that moment, the more drift the derived wall
    /// clock carries. Use it where a value only ever lived as an `Instant`; a
    /// capture path that can read both clocks should call
    /// [`ObservationTime::now`] instead.
    pub fn from_monotonic(monotonic: Instant) -> Self {
        let (anchor_instant, anchor_wall) = *anchor();
        Self {
            wall: shift(anchor_wall, anchor_instant, monotonic),
            monotonic: Some(monotonic),
        }
    }

    /// Builds a value from an explicit pair, for callers that read both clocks
    /// at the same instant and want to say so.
    ///
    /// A `wall` that disagrees with `monotonic` is stored as given: the monotonic
    /// reading wins for intervals between two live values, the wall clock wins
    /// for anything that outlives the process.
    pub fn from_parts(wall: SystemTime, monotonic: Option<Instant>) -> Self {
        Self { wall, monotonic }
    }

    /// The epoch-anchored reading — the half that is storable.
    pub fn wall(&self) -> SystemTime {
        self.wall
    }

    /// The process-local monotonic reading, when this value has one.
    ///
    /// `None` means "this value came from a stored row or from another process",
    /// never "the clock failed".
    pub fn monotonic(&self) -> Option<Instant> {
        self.monotonic
    }

    /// `true` when this value carries a monotonic reading, i.e. when it was
    /// captured rather than replayed.
    pub fn is_live(&self) -> bool {
        self.monotonic.is_some()
    }

    /// Ordering between two values, preferring the monotonic clock when both
    /// sides have one.
    ///
    /// Use this for comparisons *between two observations*. Sorting and database
    /// keys use the wall clock ([`Ord`]), the only reading that survives a
    /// restart.
    pub fn ordering(&self, other: &Self) -> Ordering {
        match (self.monotonic, other.monotonic) {
            (Some(mine), Some(theirs)) => mine.cmp(&theirs),
            _ => self.wall.cmp(&other.wall),
        }
    }

    /// How much time passed between `earlier` and `self`, saturating at zero.
    ///
    /// Out-of-order input is a fact of replay — rows read back in a different
    /// order than they were written, a batch replayed twice — so a negative
    /// interval answers [`Duration::ZERO`] instead of panicking, which is exactly
    /// what the plain `Instant::duration_since` this replaces does on that input.
    pub fn elapsed_since(&self, earlier: &Self) -> Duration {
        match (self.monotonic, earlier.monotonic) {
            (Some(mine), Some(theirs)) => mine.saturating_duration_since(theirs),
            _ => self
                .wall
                .duration_since(earlier.wall)
                .unwrap_or(Duration::ZERO),
        }
    }

    /// Subtracts an interval, dropping the monotonic reading when that clock cannot
    /// go back far enough.
    ///
    /// How far back a monotonic clock reaches is platform-dependent — it counts from
    /// boot on Linux, from an unspecified origin elsewhere — so a window start that
    /// is reachable for the wall clock may have no monotonic meaning. Its wall clock
    /// is still the right answer and comparisons then fall back to the wall clock.
    /// Returns `None` only when the wall clock cannot go back that far.
    pub fn checked_sub(&self, duration: Duration) -> Option<Self> {
        let wall = self.wall.checked_sub(duration)?;
        Some(Self {
            wall,
            monotonic: self.monotonic.and_then(|m| m.checked_sub(duration)),
        })
    }

    /// Microseconds since the Unix epoch, for storage and for hashing.
    ///
    /// `None` only for a wall clock before 1970, which no capture produces and
    /// which a `TIMESTAMPTZ` column could not hold anyway.
    pub fn micros_since_epoch(&self) -> Option<u128> {
        self.wall
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|d| d.as_micros())
    }

    /// Rebuilds a wall-clock-only value from [`ObservationTime::micros_since_epoch`].
    pub fn from_micros_since_epoch(micros: u128) -> Self {
        Self::from_wall(UNIX_EPOCH + Duration::from_micros(micros as u64))
    }
}

impl Default for ObservationTime {
    fn default() -> Self {
        Self::from_wall(UNIX_EPOCH)
    }
}

/// Shifts both clocks forward, so `stamp + Duration` reads naturally in a resolver
/// or a test.
///
/// Adding an absurd duration panics on the monotonic clock, exactly as
/// `Instant + Duration` does.
impl std::ops::Add<Duration> for ObservationTime {
    type Output = ObservationTime;

    fn add(self, duration: Duration) -> Self {
        Self {
            wall: self.wall + duration,
            monotonic: self.monotonic.map(|m| m + duration),
        }
    }
}

/// Shifts both clocks back, dropping the monotonic reading when that clock cannot go
/// back far enough. Panics on a wall clock pushed before the epoch, as
/// `SystemTime - Duration` does.
impl std::ops::Sub<Duration> for ObservationTime {
    type Output = ObservationTime;

    fn sub(self, duration: Duration) -> Self {
        Self {
            wall: self.wall - duration,
            monotonic: self.monotonic.and_then(|m| m.checked_sub(duration)),
        }
    }
}

/// Equality on the wall clock: the only reading two processes can both mean.
impl PartialEq for ObservationTime {
    fn eq(&self, other: &Self) -> bool {
        self.wall == other.wall
    }
}

impl Eq for ObservationTime {}

/// Ordering on the wall clock, so a replay sorted by this field is reproducible.
///
/// Between two live values use [`ObservationTime::ordering`].
impl PartialOrd for ObservationTime {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ObservationTime {
    fn cmp(&self, other: &Self) -> Ordering {
        self.wall.cmp(&other.wall)
    }
}

/// Formats as `seconds.microseconds+00:00`, the same microsecond-anchored
/// spelling the signed payload uses for `observed_at`.
impl fmt::Display for ObservationTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.wall.duration_since(UNIX_EPOCH) {
            Ok(elapsed) => {
                write!(
                    f,
                    "{}.{:06}+00:00",
                    elapsed.as_secs(),
                    elapsed.subsec_micros()
                )
            }
            Err(err) => {
                let before = err.duration();
                write!(
                    f,
                    "-{}.{:06}+00:00",
                    before.as_secs(),
                    before.subsec_micros()
                )
            }
        }
    }
}

/// One wall clock per process, paired with the monotonic instant at that moment,
/// used only by [`ObservationTime::from_monotonic`].
fn anchor() -> &'static (Instant, SystemTime) {
    static ANCHOR: OnceLock<(Instant, SystemTime)> = OnceLock::new();
    ANCHOR.get_or_init(|| (Instant::now(), SystemTime::now()))
}

/// Moves `base` by `instant - anchor`, in whichever direction, saturating at the
/// epoch rather than panicking on a pre-1970 result.
fn shift(base: SystemTime, anchor: Instant, instant: Instant) -> SystemTime {
    if instant >= anchor {
        base + instant.duration_since(anchor)
    } else {
        base.checked_sub(anchor.duration_since(instant))
            .unwrap_or(UNIX_EPOCH)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: u64) -> ObservationTime {
        ObservationTime::from_wall(UNIX_EPOCH + Duration::from_secs(secs))
    }

    #[test]
    fn a_live_stamp_carries_both_clocks() {
        let live = ObservationTime::now();
        assert!(live.is_live());
        assert!(live.micros_since_epoch().is_some());
    }

    #[test]
    fn a_wall_only_value_is_never_live() {
        assert!(!at(1_700_000_000).is_live());
    }

    #[test]
    fn elapsed_across_a_replayed_and_a_live_value_uses_the_wall_clock() {
        let live = ObservationTime::now();
        let replayed = ObservationTime::from_wall(live.wall() + Duration::from_millis(250));
        assert_eq!(replayed.elapsed_since(&live), Duration::from_millis(250));
    }

    #[test]
    fn elapsed_saturates_instead_of_panicking_on_out_of_order_input() {
        let earlier = at(100);
        let later = at(200);
        assert_eq!(later.elapsed_since(&earlier), Duration::from_secs(100));
        assert_eq!(earlier.elapsed_since(&later), Duration::ZERO);
    }

    #[test]
    fn two_live_values_order_by_the_monotonic_clock() {
        let first = Instant::now();
        let second = first + Duration::from_secs(5);
        // Wall clocks that contradict the monotonic ones, to prove which wins.
        let a = ObservationTime::from_parts(UNIX_EPOCH + Duration::from_secs(500), Some(first));
        let b = ObservationTime::from_parts(UNIX_EPOCH + Duration::from_secs(100), Some(second));
        assert_eq!(a.ordering(&b), Ordering::Less);
        assert_eq!(a.elapsed_since(&b), Duration::ZERO);
        assert_eq!(b.elapsed_since(&a), Duration::from_secs(5));
        // Sorting still follows the wall clock.
        assert!(a > b);
    }

    #[test]
    fn a_window_start_far_back_stays_consistent_with_its_own_clocks() {
        let live = ObservationTime::now();
        let year = Duration::from_secs(60 * 60 * 24 * 365);
        let window = live.checked_sub(year).expect("a year back is reachable");
        assert!(window.wall() < live.wall());
        // Whether the monotonic reading survives depends on how far that clock can
        // go back, which differs by platform; what must not differ is the interval.
        assert_eq!(live.elapsed_since(&window), year);
    }

    #[test]
    fn a_window_start_beyond_the_wall_clock_is_none_rather_than_a_wrap() {
        let live = ObservationTime::now();
        assert!(
            live.checked_sub(Duration::from_secs(u64::MAX - 1))
                .is_none()
        );
    }

    #[test]
    fn an_anchored_instant_gets_a_wall_clock_close_to_now() {
        let before = SystemTime::now();
        let derived = ObservationTime::from_monotonic(Instant::now());
        let after = SystemTime::now();
        assert!(derived.wall() >= before - Duration::from_secs(1));
        assert!(derived.wall() <= after + Duration::from_secs(1));
    }

    #[test]
    fn micros_round_trip_through_a_stored_form() {
        let value = at(1_700_000_000);
        let micros = value.micros_since_epoch().unwrap();
        assert_eq!(ObservationTime::from_micros_since_epoch(micros), value);
    }

    #[test]
    fn display_prints_microseconds_since_the_epoch() {
        assert_eq!(at(5).to_string(), "5.000000+00:00");
        let value = ObservationTime::from_wall(UNIX_EPOCH + Duration::new(1_700_000_000, 123_000));
        assert_eq!(value.to_string(), "1700000000.000123+00:00");
    }
}
