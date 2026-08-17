//! Composing and racing [`AbortSignal`]s.
//! Port of `packages/ai/src/utils/{abort-signals,abort}.ts`.
//!
//! The port lives in `pi_core::options`; these are the combinators upstream
//! keeps next to it. `AbortSignal` is a `tokio::sync::watch` receiver rather
//! than a DOM event target, so "add a listener to each source" becomes "one
//! task selecting over all of them". [`CombinedAbortSignal`] owns that task and
//! cancels it on drop, which is upstream's `cleanup()` in RAII form.

use std::future::Future;

use pi_core::options::{AbortHandle, AbortSignal};

use crate::HttpError;

/// The result of [`combine_abort_signals`].
///
/// Holds the linking task alive: dropping this stops propagation, so keep it
/// for as long as the combined signal is in use.
#[derive(Debug)]
pub struct CombinedAbortSignal {
    signal: Option<AbortSignal>,
    /// `None` when no task was needed (zero or one input signal).
    task: Option<tokio::task::JoinHandle<()>>,
}

impl CombinedAbortSignal {
    /// The combined signal, or `None` when there was nothing to combine.
    pub fn signal(&self) -> Option<&AbortSignal> {
        self.signal.as_ref()
    }

    /// The combined signal, defaulting to one that never fires.
    pub fn signal_or_never(&self) -> AbortSignal {
        self.signal.clone().unwrap_or_default()
    }

    pub fn is_aborted(&self) -> bool {
        self.signal.as_ref().is_some_and(|s| s.is_aborted())
    }
}

impl Drop for CombinedAbortSignal {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// A signal that fires as soon as any of `signals` fires.
///
/// Zero signals yield nothing to wait on; one signal is passed straight
/// through, so the common cases allocate no task.
///
/// # Panics
///
/// Must be called from inside a Tokio runtime when given two or more signals.
pub fn combine_abort_signals(signals: &[AbortSignal]) -> CombinedAbortSignal {
    match signals {
        [] => CombinedAbortSignal {
            signal: None,
            task: None,
        },
        [only] => CombinedAbortSignal {
            signal: Some(only.clone()),
            task: None,
        },
        many => {
            let (handle, combined) = AbortHandle::new();
            // Fast path: an already-aborted input needs no task at all.
            if many.iter().any(|s| s.is_aborted()) {
                handle.abort();
                return CombinedAbortSignal {
                    signal: Some(combined),
                    task: None,
                };
            }
            let sources: Vec<AbortSignal> = many.to_vec();
            let task = tokio::spawn(async move {
                let waiters = sources.iter().map(|s| Box::pin(s.aborted()));
                futures_util::future::select_all(waiters).await;
                handle.abort();
            });
            CombinedAbortSignal {
                signal: Some(combined),
                task: Some(task),
            }
        }
    }
}

/// An operation-local signal for public APIs whose signal is optional.
/// Port of `operationSignal`.
pub fn operation_signal(signal: Option<AbortSignal>) -> AbortSignal {
    signal.unwrap_or_default()
}

/// Await `operation`, giving up as soon as `signal` aborts.
/// Port of `raceWithAbortSignal`.
///
/// Note the difference from plain `select!`: dropping a Rust future cancels it,
/// which is exactly what a Swift caller cannot rely on, so anything that must
/// survive cancellation belongs in a spawned task rather than here.
pub async fn race_with_abort_signal<F>(
    operation: F,
    signal: &AbortSignal,
) -> Result<F::Output, HttpError>
where
    F: Future,
{
    if signal.is_aborted() {
        return Err(HttpError::Aborted);
    }
    tokio::select! {
        biased;
        _ = signal.aborted() => Err(HttpError::Aborted),
        value = operation => Ok(value),
    }
}

/// [`race_with_abort_signal`] for an optional signal.
pub async fn race_with_optional_signal<F>(
    operation: F,
    signal: Option<&AbortSignal>,
) -> Result<F::Output, HttpError>
where
    F: Future,
{
    match signal {
        Some(signal) => race_with_abort_signal(operation, signal).await,
        None => Ok(operation.await),
    }
}

/// Sleep, returning [`HttpError::Aborted`] if the signal fires first.
/// This is `abortableSleep` from `provider-retry.ts`.
pub async fn sleep_unless_aborted(
    duration: std::time::Duration,
    signal: Option<&AbortSignal>,
) -> Result<(), HttpError> {
    match signal {
        Some(signal) if signal.is_aborted() => Err(HttpError::Aborted),
        Some(signal) => {
            tokio::select! {
                biased;
                _ = signal.aborted() => Err(HttpError::Aborted),
                _ = tokio::time::sleep(duration) => Ok(()),
            }
        }
        None => {
            tokio::time::sleep(duration).await;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn no_signals_produce_nothing_to_wait_on() {
        let combined = combine_abort_signals(&[]);
        assert!(combined.signal().is_none());
        assert!(!combined.is_aborted());
        assert!(!combined.signal_or_never().is_aborted());
    }

    #[tokio::test]
    async fn a_single_signal_is_passed_through() {
        let (handle, signal) = AbortHandle::new();
        let combined = combine_abort_signals(&[signal]);
        assert!(!combined.is_aborted());
        handle.abort();
        assert!(combined.is_aborted());
    }

    #[tokio::test]
    async fn any_source_aborts_the_combination() {
        for index in 0..3 {
            let handles: Vec<_> = (0..3).map(|_| AbortHandle::new()).collect();
            let signals: Vec<AbortSignal> = handles.iter().map(|(_, s)| s.clone()).collect();
            let combined = combine_abort_signals(&signals);
            assert!(!combined.is_aborted());

            handles[index].0.abort();
            tokio::time::timeout(Duration::from_secs(1), combined.signal_or_never().aborted())
                .await
                .unwrap_or_else(|_| panic!("source {index} did not propagate"));
            assert!(combined.is_aborted());
        }
    }

    #[tokio::test]
    async fn an_already_aborted_source_is_observed_immediately() {
        let (a, signal_a) = AbortHandle::new();
        let (_b, signal_b) = AbortHandle::new();
        a.abort();
        let combined = combine_abort_signals(&[signal_a, signal_b]);
        assert!(combined.is_aborted());
    }

    #[tokio::test]
    async fn racing_returns_the_value_when_nothing_aborts() {
        let (_handle, signal) = AbortHandle::new();
        let value = race_with_abort_signal(async { 42 }, &signal).await.unwrap();
        assert_eq!(value, 42);
    }

    #[tokio::test]
    async fn racing_an_already_aborted_signal_fails_without_polling() {
        let (handle, signal) = AbortHandle::new();
        handle.abort();
        let result = race_with_abort_signal(async { unreachable!("must not run") }, &signal).await;
        assert!(matches!(result, Err(HttpError::Aborted)));
    }

    #[tokio::test]
    async fn racing_gives_up_when_the_signal_fires() {
        let (handle, signal) = AbortHandle::new();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            handle.abort();
        });
        let result =
            race_with_abort_signal(tokio::time::sleep(Duration::from_secs(60)), &signal).await;
        assert!(matches!(result, Err(HttpError::Aborted)));
    }

    #[tokio::test]
    async fn abortable_sleep_wakes_early_on_abort() {
        let (handle, signal) = AbortHandle::new();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(5)).await;
            handle.abort();
        });
        let started = tokio::time::Instant::now();
        let result = sleep_unless_aborted(Duration::from_secs(300), Some(&signal)).await;
        assert!(matches!(result, Err(HttpError::Aborted)));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn abortable_sleep_completes_without_a_signal() {
        assert!(sleep_unless_aborted(Duration::from_millis(50), None)
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn dropping_the_combination_stops_the_linking_task() {
        let (handle, signal_a) = AbortHandle::new();
        let (_b, signal_b) = AbortHandle::new();
        let combined = combine_abort_signals(&[signal_a, signal_b]);
        let escaped = combined.signal_or_never();
        drop(combined);
        handle.abort();
        tokio::task::yield_now().await;
        // Cleanup ran, so the source no longer reaches the escaped signal.
        assert!(!escaped.is_aborted());
    }
}
