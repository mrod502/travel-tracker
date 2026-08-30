//! Try sources in order until one reports a fix.

use async_trait::async_trait;
use log::debug;
use std::sync::Arc;

use super::{NoPositionSource, Position, PositionError, PositionSource};

/// Queries sources in priority order, returning the first fix.
///
/// The order encodes trust: a fixed point the operator configured is a
/// deliberate answer, so it is asked before a receiver that might report
/// anything.
pub struct FallbackPositionSource {
    sources: Vec<Arc<dyn PositionSource>>,
}

impl FallbackPositionSource {
    /// Chain the given sources, highest priority first.
    pub fn new(sources: Vec<Arc<dyn PositionSource>>) -> Self {
        Self { sources }
    }

    /// Compose `sources` into the cheapest source that expresses the intent:
    /// no sources is [`NoPositionSource`], one source is used directly, and only
    /// a genuine chain pays for a [`FallbackPositionSource`].
    pub fn compose(mut sources: Vec<Arc<dyn PositionSource>>) -> Arc<dyn PositionSource> {
        match sources.len() {
            0 => Arc::new(NoPositionSource),
            1 => sources.pop().expect("length was just measured"),
            _ => Arc::new(Self::new(sources)),
        }
    }

    /// Number of sources in the chain.
    pub fn len(&self) -> usize {
        self.sources.len()
    }

    /// Whether the chain is empty.
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }
}

#[async_trait]
impl PositionSource for FallbackPositionSource {
    async fn current_position(&self) -> Result<Option<Position>, PositionError> {
        let mut first_error = None;

        for source in &self.sources {
            match source.current_position().await {
                Ok(Some(position)) => return Ok(Some(position)),
                Ok(None) => {}
                Err(error) => {
                    debug!("position source unavailable, trying the next one: {}", error);
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }

        // A broken source outranks a silent one. "No fix yet" is a legitimate
        // state worth reporting as such; a failure is worth reporting as one, and
        // the first failure is the most likely root cause.
        match first_error {
            Some(error) => Err(error),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::position::mock::{MockOutcome, MockPositionSource};
    use crate::position::test_support::gps_fix;

    #[tokio::test]
    async fn first_source_that_reports_wins() {
        let chain = FallbackPositionSource::new(vec![
            Arc::new(MockPositionSource::silent()),
            Arc::new(MockPositionSource::from_positions(vec![gps_fix(7.0)])),
            Arc::new(MockPositionSource::from_positions(vec![gps_fix(8.0)])),
        ]);

        assert_eq!(
            chain.current_position().await.unwrap().unwrap().latitude,
            7.0
        );
    }

    #[tokio::test]
    async fn a_failure_falls_through_to_the_next_source() {
        let second = Arc::new(MockPositionSource::from_positions(vec![gps_fix(9.0)]));
        let chain = FallbackPositionSource::new(vec![
            Arc::new(MockPositionSource::failing(PositionError::Transport(
                "no gpsd".into(),
            ))),
            second.clone(),
        ]);

        assert_eq!(
            chain.current_position().await.unwrap().unwrap().latitude,
            9.0
        );
        assert_eq!(second.calls(), 1);
    }

    #[tokio::test]
    async fn the_first_failure_is_the_one_reported() {
        let chain = FallbackPositionSource::new(vec![
            Arc::new(MockPositionSource::failing(PositionError::Transport(
                "socket refused".into(),
            ))),
            Arc::new(MockPositionSource::failing(PositionError::StaleFix {
                age: std::time::Duration::from_secs(90),
            })),
        ]);

        let error = chain.current_position().await.unwrap_err();
        assert!(matches!(error, PositionError::Transport(_)));
    }

    #[tokio::test]
    async fn an_all_silent_chain_reports_nothing_rather_than_failing() {
        let chain = FallbackPositionSource::new(vec![
            Arc::new(MockPositionSource::silent()),
            Arc::new(MockPositionSource::silent()),
        ]);

        assert!(chain.current_position().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn compose_collapses_degenerate_chains() {
        assert_eq!(
            FallbackPositionSource::compose(Vec::new())
                .current_position()
                .await
                .unwrap(),
            None
        );

        let single = Arc::new(MockPositionSource::from_positions(vec![gps_fix(3.0)]));
        let composed = FallbackPositionSource::compose(vec![single.clone()]);
        assert!(composed.current_position().await.unwrap().is_some());
        assert_eq!(single.calls(), 1, "a one-source chain needs no wrapper");

        let chained = FallbackPositionSource::new(vec![
            Arc::new(MockPositionSource::new(vec![MockOutcome::Silent])),
            single.clone(),
        ]);
        assert_eq!(chained.len(), 2);
        assert!(!chained.is_empty());
        assert!(FallbackPositionSource::new(Vec::new()).is_empty());
    }
}
