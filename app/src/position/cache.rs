//! Reuse an acquired fix for a bounded time, and back off after a failure.

use async_trait::async_trait;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use log::debug;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use super::{Position, PositionError, PositionSource};
use crate::node::Clock;

/// Cached view over another [`PositionSource`].
///
/// Two windows are kept:
///
/// * `max_age` — a fix younger than this is handed back without touching the
///   receiver, so the node can ask once per occurrence.
/// * `retry_backoff` — after a failure or a silent report the source is left
///   alone for this long, so a node with no receiver does not open a connection
///   per occurrence.
///
/// A stale fix is deliberately never handed out. The position travels inside the
/// signed canonical payload, so an old position presented as current is a false
/// claim, while a NULL location is simply the absence of one. That rule applies
/// to freshly acquired reports too: a receiver repeating a fix it got minutes ago
/// is rejected with [`PositionError::StaleFix`].
pub struct CachedPositionSource {
    inner: Arc<dyn PositionSource>,
    max_age: Duration,
    retry_backoff: Duration,
    clock: Arc<dyn Clock>,
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    last_fix: Option<Position>,
    retry_after: Option<DateTime<Utc>>,
    /// Why the window was opened, so the last attempt's cause keeps being
    /// reported while the source is left alone. `None` means the source was
    /// reachable but had no fix.
    failure: Option<PositionError>,
}

impl CachedPositionSource {
    /// Cache fixes from `inner` for `max_age`, backing off for `retry_backoff`
    /// after anything other than a fix.
    pub fn new(
        inner: Arc<dyn PositionSource>,
        max_age: Duration,
        retry_backoff: Duration,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            inner,
            max_age,
            retry_backoff,
            clock,
            state: Mutex::new(State::default()),
        }
    }

    /// The most recent fix, whether or not it is still fresh.
    pub fn cached_fix(&self) -> Option<Position> {
        self.lock().last_fix.clone()
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // A poisoned lock only means something panicked while updating a cached
        // fix; the fix itself is still fine to read.
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn backoff_deadline(&self, now: DateTime<Utc>) -> DateTime<Utc> {
        let offset =
            ChronoDuration::from_std(self.retry_backoff).unwrap_or_else(|_| ChronoDuration::zero());
        now + offset
    }
}

#[async_trait]
impl PositionSource for CachedPositionSource {
    async fn current_position(&self) -> Result<Option<Position>, PositionError> {
        let now = self.clock.now();

        // Decide under the lock, then answer outside it so the inner source is
        // never awaited while the state is held.
        let cached = {
            let state = self.lock();
            match state.last_fix.as_ref() {
                Some(fix) if !fix.is_stale(self.max_age, now) => Some(Ok(Some(fix.clone()))),
                _ => match state.retry_after {
                    Some(deadline) if now < deadline => {
                        // Repeat the cause rather than inventing a "backing off"
                        // error: an operator reading the log wants "no gpsd at
                        // 127.0.0.1:2947", not a message about a timer.
                        Some(match state.failure.as_ref() {
                            Some(error) => Err(error.clone()),
                            None => Ok(None),
                        })
                    }
                    _ => None,
                },
            }
        };
        if let Some(outcome) = cached {
            return outcome;
        }

        match self.inner.current_position().await {
            Ok(Some(position)) => {
                // gpsd keeps repeating its last TPV when the sky is blocked, so
                // a fresh read is not necessarily a fresh fix. Vet it here: this
                // is the only place that decides what the node is willing to
                // assert about where it is.
                if position.is_stale(self.max_age, now) {
                    let age = position.age(now);
                    let error = PositionError::StaleFix { age };
                    self.record_failure(now, Some(error.clone()));
                    debug!("discarding stale position fix ({age:?})");
                    return Err(error);
                }

                let mut state = self.lock();
                state.last_fix = Some(position.clone());
                state.retry_after = None;
                state.failure = None;
                Ok(Some(position))
            }
            Ok(None) => {
                self.record_failure(now, None);
                debug!("position source reported no fix, retrying after backoff");
                Ok(None)
            }
            Err(error) => {
                self.record_failure(now, Some(error.clone()));
                Err(error)
            }
        }
    }
}

impl CachedPositionSource {
    /// Open the backoff window, remembering why.
    fn record_failure(&self, now: DateTime<Utc>, failure: Option<PositionError>) {
        let mut state = self.lock();
        state.retry_after = Some(self.backoff_deadline(now));
        state.failure = failure;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::position::mock::{MockOutcome, MockPositionSource};
    use crate::position::test_support::{epoch, gps_fix, gps_fix_aged, gps_fix_at, ManualClock};

    struct Harness {
        inner: Arc<MockPositionSource>,
        clock: Arc<ManualClock>,
        source: CachedPositionSource,
    }

    fn harness(outcomes: Vec<MockOutcome>, max_age: Duration) -> Harness {
        let inner = Arc::new(MockPositionSource::new(outcomes));
        let clock = Arc::new(ManualClock::new(epoch()));
        let source = CachedPositionSource::new(
            inner.clone(),
            max_age,
            Duration::from_secs(30),
            clock.clone(),
        );
        Harness {
            inner,
            clock,
            source,
        }
    }

    #[tokio::test]
    async fn fresh_fix_is_served_without_querying_the_source() {
        let h = harness(
            vec![MockOutcome::Position(gps_fix(10.0))],
            Duration::from_secs(60),
        );

        for _ in 0..3 {
            let got = h.source.current_position().await.unwrap().unwrap();
            assert_eq!(got.latitude, 10.0);
        }

        assert_eq!(h.inner.calls(), 1, "cache absorbs repeat calls");
    }

    #[tokio::test]
    async fn fix_older_than_max_age_is_refreshed() {
        let later = epoch() + chrono::Duration::milliseconds(61_000);
        let h = harness(
            vec![
                MockOutcome::Position(gps_fix(10.0)),
                MockOutcome::Position(gps_fix_at(11.0, later)),
            ],
            Duration::from_secs(60),
        );

        assert_eq!(
            h.source.current_position().await.unwrap().unwrap().latitude,
            10.0
        );
        h.clock.advance_ms(61_000);
        assert_eq!(
            h.source.current_position().await.unwrap().unwrap().latitude,
            11.0
        );
        assert_eq!(h.inner.calls(), 2);
    }

    #[tokio::test]
    async fn a_stale_report_is_rejected_and_not_reused() {
        let h = harness(
            vec![
                MockOutcome::Position(gps_fix_aged(10.0, Duration::from_secs(3_600))),
                MockOutcome::Error(PositionError::Transport("antenna unplugged".into())),
            ],
            Duration::from_secs(60),
        );

        let error = h.source.current_position().await.unwrap_err();
        assert!(matches!(error, PositionError::StaleFix { .. }));
        assert!(
            h.source.cached_fix().is_none(),
            "a rejected fix is not remembered"
        );

        // Once the backoff expires, the rejected report must not be the fallback.
        h.clock.advance_ms(30_001);
        let error = h.source.current_position().await.unwrap_err();
        assert!(matches!(error, PositionError::Transport(_)));
    }

    #[tokio::test]
    async fn failure_opens_a_backoff_window_that_keeps_reporting_the_cause() {
        let h = harness(
            vec![
                MockOutcome::Error(PositionError::Transport("no gpsd".into())),
                MockOutcome::Position(gps_fix(12.0)),
            ],
            Duration::from_secs(60),
        );

        assert!(matches!(
            h.source.current_position().await.unwrap_err(),
            PositionError::Transport(_)
        ));
        assert_eq!(h.inner.calls(), 1);

        let error = h.source.current_position().await.unwrap_err();
        assert!(
            matches!(&error, PositionError::Transport(why) if why == "no gpsd"),
            "the backoff window must repeat the real cause, not a timer message"
        );
        assert_eq!(h.inner.calls(), 1, "backoff must not reach the source");

        h.clock.advance_ms(30_001);
        assert_eq!(
            h.source.current_position().await.unwrap().unwrap().latitude,
            12.0
        );
        assert_eq!(h.inner.calls(), 2);
    }

    #[tokio::test]
    async fn a_silent_source_backs_off_but_reports_no_error() {
        let h = harness(vec![MockOutcome::Silent], Duration::from_secs(60));

        assert!(h.source.current_position().await.unwrap().is_none());
        assert!(h.source.current_position().await.unwrap().is_none());
        assert_eq!(
            h.inner.calls(),
            1,
            "silence is worth retrying, just not yet"
        );
    }
}
