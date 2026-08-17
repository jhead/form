//! Port of `packages/ai/src/auth/oauth/device-code.ts` (RFC 8628 polling).

use std::future::Future;
use std::time::Duration;

use pi_core::options::AbortSignal;
use tokio::time::Instant;

use crate::error::AuthError;

const TIMEOUT_MESSAGE: &str = "Device flow timed out";
const SLOW_DOWN_TIMEOUT_MESSAGE: &str = "Device flow timed out after one or more slow_down responses. This is often caused by clock drift in WSL or VM environments. Please sync or restart the VM clock and try again.";
const MINIMUM_INTERVAL: Duration = Duration::from_millis(1000);
/// RFC 8628 §3.2: with no server `interval`, the client must use 5 seconds.
const DEFAULT_POLL_INTERVAL_SECONDS: f64 = 5.0;
/// RFC 8628 §3.5: `slow_down` means increase the interval by 5 seconds.
const SLOW_DOWN_INTERVAL_INCREMENT: Duration = Duration::from_millis(5000);

/// One poll's outcome.
#[derive(Debug, Clone, PartialEq)]
pub enum PollResult<T> {
    Pending,
    SlowDown { interval_seconds: Option<f64> },
    Failed { message: String },
    Complete(T),
}

#[derive(Debug, Clone, Default)]
pub struct DeviceCodePollOptions {
    pub interval_seconds: Option<f64>,
    pub expires_in_seconds: Option<f64>,
    /// GitHub and Kimi reject a poll issued before the first interval elapses.
    pub wait_before_first_poll: bool,
    pub signal: AbortSignal,
}

/// Sleep, waking early (and failing) if `signal` fires.
pub async fn abortable_sleep(duration: Duration, signal: &AbortSignal) -> Result<(), AuthError> {
    if signal.is_aborted() {
        return Err(AuthError::Cancelled);
    }
    tokio::select! {
        biased;
        _ = signal.aborted() => Err(AuthError::Cancelled),
        _ = tokio::time::sleep(duration) => Ok(()),
    }
}

fn interval_from_seconds(seconds: f64) -> Duration {
    Duration::from_millis((seconds * 1000.0).floor().max(0.0) as u64).max(MINIMUM_INTERVAL)
}

/// Poll a device-authorization grant until it completes, fails or expires.
pub async fn poll_device_code_flow<T, F, Fut>(
    options: DeviceCodePollOptions,
    poll: F,
) -> Result<T, AuthError>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<PollResult<T>, AuthError>>,
{
    let start = Instant::now();
    let deadline = options
        .expires_in_seconds
        .filter(|s| s.is_finite())
        .map(|seconds| start + Duration::from_millis((seconds * 1000.0).max(0.0) as u64));

    let mut interval = interval_from_seconds(
        options
            .interval_seconds
            .unwrap_or(DEFAULT_POLL_INTERVAL_SECONDS),
    );
    let mut slow_down_responses = 0u32;

    let remaining = |deadline: Option<Instant>| -> Option<Duration> {
        match deadline {
            Some(deadline) => deadline.checked_duration_since(Instant::now()),
            None => Some(Duration::MAX),
        }
    };

    if options.wait_before_first_poll {
        if let Some(remaining) = remaining(deadline) {
            if !remaining.is_zero() {
                abortable_sleep(interval.min(remaining), &options.signal).await?;
            }
        }
    }

    while remaining(deadline).is_some_and(|r| !r.is_zero()) {
        if options.signal.is_aborted() {
            return Err(AuthError::Cancelled);
        }

        match poll().await? {
            PollResult::Complete(value) => return Ok(value),
            PollResult::Failed { message } => return Err(AuthError::oauth(message)),
            PollResult::SlowDown { interval_seconds } => {
                slow_down_responses += 1;
                // Prefer the server-provided interval (GitHub reports the new
                // required minimum in `interval`); a purely client-tracked value
                // risks polling early forever under WSL/VM clock drift.
                interval = match interval_seconds {
                    Some(seconds) if seconds.is_finite() && seconds > 0.0 => {
                        interval_from_seconds(seconds)
                    }
                    _ => (interval + SLOW_DOWN_INTERVAL_INCREMENT).max(MINIMUM_INTERVAL),
                };
            }
            PollResult::Pending => {}
        }

        let Some(remaining) = remaining(deadline).filter(|r| !r.is_zero()) else {
            break;
        };
        abortable_sleep(interval.min(remaining), &options.signal).await?;
    }

    Err(AuthError::timed_out(if slow_down_responses > 0 {
        SLOW_DOWN_TIMEOUT_MESSAGE
    } else {
        TIMEOUT_MESSAGE
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::options::AbortHandle;
    use std::sync::Mutex;

    /// Elapsed test-clock time at each poll, in milliseconds since start.
    struct PollLog {
        start: Instant,
        times: Mutex<Vec<u64>>,
    }

    impl PollLog {
        fn new() -> Self {
            Self {
                start: Instant::now(),
                times: Mutex::new(Vec::new()),
            }
        }

        fn record(&self) {
            let elapsed = self.start.elapsed().as_millis() as u64;
            self.times.lock().unwrap().push(elapsed);
        }

        fn times(&self) -> Vec<u64> {
            self.times.lock().unwrap().clone()
        }
    }

    #[tokio::test(start_paused = true)]
    async fn polls_immediately_and_returns_the_completed_value() {
        let log = PollLog::new();

        let result = poll_device_code_flow(
            DeviceCodePollOptions {
                interval_seconds: Some(2.0),
                expires_in_seconds: Some(30.0),
                ..Default::default()
            },
            || async {
                log.record();
                Ok(if log.times().len() == 1 {
                    PollResult::Pending
                } else {
                    PollResult::Complete("token")
                })
            },
        )
        .await
        .unwrap();

        assert_eq!(result, "token");
        assert_eq!(log.times(), vec![0, 2000]);
    }

    #[tokio::test(start_paused = true)]
    async fn can_wait_before_the_first_poll() {
        let log = PollLog::new();

        let result = poll_device_code_flow(
            DeviceCodePollOptions {
                interval_seconds: Some(2.0),
                expires_in_seconds: Some(30.0),
                wait_before_first_poll: true,
                ..Default::default()
            },
            || async {
                log.record();
                Ok(PollResult::Complete("token"))
            },
        )
        .await
        .unwrap();

        assert_eq!(result, "token");
        assert_eq!(log.times(), vec![2000]);
    }

    #[tokio::test(start_paused = true)]
    async fn slow_down_without_a_server_interval_adds_five_seconds() {
        let log = PollLog::new();

        poll_device_code_flow(
            DeviceCodePollOptions {
                interval_seconds: Some(2.0),
                expires_in_seconds: Some(900.0),
                ..Default::default()
            },
            || async {
                log.record();
                Ok(if log.times().len() == 1 {
                    PollResult::SlowDown {
                        interval_seconds: None,
                    }
                } else {
                    PollResult::Complete("token")
                })
            },
        )
        .await
        .unwrap();

        assert_eq!(log.times(), vec![0, 7000]);
    }

    #[tokio::test(start_paused = true)]
    async fn honors_a_server_provided_slow_down_interval() {
        let log = PollLog::new();

        poll_device_code_flow(
            DeviceCodePollOptions {
                interval_seconds: Some(2.0),
                expires_in_seconds: Some(900.0),
                ..Default::default()
            },
            || async {
                log.record();
                Ok(if log.times().len() == 1 {
                    PollResult::SlowDown {
                        interval_seconds: Some(30.0),
                    }
                } else {
                    PollResult::Complete("token")
                })
            },
        )
        .await
        .unwrap();

        assert_eq!(log.times(), vec![0, 30000]);
    }

    #[tokio::test(start_paused = true)]
    async fn cancels_an_in_flight_wait() {
        let (handle, signal) = AbortHandle::new();
        let poller = poll_device_code_flow(
            DeviceCodePollOptions {
                interval_seconds: Some(5.0),
                expires_in_seconds: Some(30.0),
                signal,
                ..Default::default()
            },
            || async { Ok::<_, AuthError>(PollResult::<&str>::Pending) },
        );

        let aborter = async {
            tokio::time::sleep(Duration::from_millis(1)).await;
            handle.abort();
        };

        let (result, ()) = tokio::join!(poller, aborter);
        assert!(result.unwrap_err().is_cancelled());
    }

    #[tokio::test(start_paused = true)]
    async fn times_out_at_the_expiry_deadline() {
        let error = poll_device_code_flow(
            DeviceCodePollOptions {
                interval_seconds: Some(60.0),
                expires_in_seconds: Some(900.0),
                ..Default::default()
            },
            || async { Ok::<_, AuthError>(PollResult::<&str>::Pending) },
        )
        .await
        .unwrap_err();

        assert_eq!(error.message(), TIMEOUT_MESSAGE);
    }

    #[tokio::test(start_paused = true)]
    async fn reports_the_clock_drift_hint_after_a_slow_down() {
        let error = poll_device_code_flow(
            DeviceCodePollOptions {
                interval_seconds: Some(60.0),
                expires_in_seconds: Some(120.0),
                ..Default::default()
            },
            || async {
                Ok::<_, AuthError>(PollResult::<&str>::SlowDown {
                    interval_seconds: None,
                })
            },
        )
        .await
        .unwrap_err();

        assert_eq!(error.message(), SLOW_DOWN_TIMEOUT_MESSAGE);
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_poll_surfaces_its_message() {
        let error = poll_device_code_flow(
            DeviceCodePollOptions {
                interval_seconds: Some(1.0),
                expires_in_seconds: Some(30.0),
                ..Default::default()
            },
            || async {
                Ok::<_, AuthError>(PollResult::<&str>::Failed {
                    message: "Kimi Code login was denied.".into(),
                })
            },
        )
        .await
        .unwrap_err();

        assert_eq!(error.code(), "oauth");
        assert!(error.message().contains("denied"));
    }
}
