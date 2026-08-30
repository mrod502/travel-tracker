//! Simulated fixes, for tests and for development without a receiver.

use async_trait::async_trait;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use super::{Position, PositionError, PositionOrigin, PositionSource};
use crate::node::Clock;

/// What a [`MockPositionSource`] should report on one call.
#[derive(Debug, Clone)]
pub enum MockOutcome {
    /// Report this position.
    Position(Position),
    /// Report nothing: the source is healthy but has no fix.
    Silent,
    /// Fail in this way.
    Error(PositionError),
}

/// Scripted or drifting source.
///
/// A scripted source consumes one [`MockOutcome`] per call and reports silence
/// once the script is exhausted, so a test that calls more times than it
/// scripted gets a clear result rather than a panic.
pub struct MockPositionSource {
    behaviour: Mutex<Behaviour>,
    calls: AtomicUsize,
    clock: Arc<dyn Clock>,
}

#[derive(Debug)]
enum Behaviour {
    /// One outcome per call.
    Script(VecDeque<MockOutcome>),
    /// A straight line due north of `centre`, `step_deg` per call.
    Drift {
        centre: (f64, f64),
        step_deg: f64,
        emitted: u64,
    },
}

/// Roughly 11 m per call: far enough to see movement between occurrences, small
/// enough to stay plausible for a node that is not actually moving.
const DEFAULT_DRIFT_STEP_DEG: f64 = 0.0001;

impl MockPositionSource {
    /// Script the given outcomes.
    ///
    /// A script never consults a clock, so the system clock is used as a
    /// placeholder; use [`MockPositionSource::drift`] for clock-driven output.
    pub fn new(outcomes: Vec<MockOutcome>) -> Self {
        Self::with_clock(
            Behaviour::Script(outcomes.into()),
            Arc::new(crate::node::SystemClock),
        )
    }

    /// Script a position on every call.
    pub fn fixed(position: Position) -> Self {
        Self::new(vec![MockOutcome::Position(position)])
    }

    /// Script a sequence of positions.
    pub fn from_positions(positions: Vec<Position>) -> Self {
        Self::new(positions.into_iter().map(MockOutcome::Position).collect())
    }

    /// Always report no fix.
    pub fn silent() -> Self {
        Self::new(vec![MockOutcome::Silent])
    }

    /// Always fail with `error`.
    pub fn failing(error: PositionError) -> Self {
        Self::new(vec![MockOutcome::Error(error)])
    }

    /// Simulated track north of `centre`, timestamped by `clock`.
    ///
    /// This is what `[location.gps] backend = "mock"` runs, so a node without
    /// hardware still produces location-stamped occurrences.
    pub fn drift(centre: (f64, f64), clock: Arc<dyn Clock>) -> Self {
        Self::with_clock(
            Behaviour::Drift {
                centre,
                step_deg: DEFAULT_DRIFT_STEP_DEG,
                emitted: 0,
            },
            clock,
        )
    }

    fn with_clock(behaviour: Behaviour, clock: Arc<dyn Clock>) -> Self {
        Self {
            behaviour: Mutex::new(behaviour),
            calls: AtomicUsize::new(0),
            clock,
        }
    }

    /// How many times this source has been asked.
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn behaviour(&self) -> MutexGuard<'_, Behaviour> {
        self.behaviour
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[async_trait]
impl PositionSource for MockPositionSource {
    async fn current_position(&self) -> Result<Option<Position>, PositionError> {
        self.calls.fetch_add(1, Ordering::SeqCst);

        match &mut *self.behaviour() {
            Behaviour::Script(script) => match script.pop_front() {
                Some(MockOutcome::Position(position)) => Ok(Some(position)),
                Some(MockOutcome::Silent) => Ok(None),
                Some(MockOutcome::Error(error)) => Err(error),
                None => Ok(None),
            },
            Behaviour::Drift {
                centre,
                step_deg,
                emitted,
            } => {
                let latitude = centre.0 + (*emitted as f64 * *step_deg);
                *emitted += 1;

                let position = Position::new(
                    latitude,
                    centre.1,
                    PositionOrigin::Mock,
                    self.clock.now(),
                )?;
                Ok(Some(position))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::position::test_support::{epoch, gps_fix, ManualClock};
    use std::time::Duration;

    #[tokio::test]
    async fn script_is_consumed_in_order() {
        let source = MockPositionSource::new(vec![
            MockOutcome::Position(gps_fix(1.0)),
            MockOutcome::Silent,
            MockOutcome::Error(PositionError::Timeout {
                timeout: Duration::from_millis(5),
            }),
        ]);

        assert_eq!(
            source.current_position().await.unwrap().unwrap().latitude,
            1.0
        );
        assert!(source.current_position().await.unwrap().is_none());
        assert!(matches!(
            source.current_position().await.unwrap_err(),
            PositionError::Timeout { .. }
        ));
        assert_eq!(source.calls(), 3);
    }

    #[tokio::test]
    async fn exhausted_script_reports_silence_rather_than_panicking() {
        let source = MockPositionSource::from_positions(vec![gps_fix(2.0)]);

        assert!(source.current_position().await.unwrap().is_some());
        assert!(source.current_position().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn drift_moves_north_by_a_step_per_call_and_is_labelled_mock() {
        let clock = Arc::new(ManualClock::new(epoch()));
        let source = MockPositionSource::drift((40.6892, -74.0445), clock.clone());

        let first = source.current_position().await.unwrap().unwrap();
        let second = source.current_position().await.unwrap().unwrap();

        assert_eq!(first.latitude, 40.6892);
        assert!(
            (second.latitude - first.latitude - DEFAULT_DRIFT_STEP_DEG).abs() < 1e-12,
            "expected one step of drift, got {}",
            second.latitude - first.latitude
        );
        assert_eq!(second.longitude, -74.0445);
        assert_eq!(second.origin, PositionOrigin::Mock);
        assert_eq!(second.fixed_at, epoch());
    }
}
