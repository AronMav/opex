//! Retry policy with exponential backoff for transient HTTP errors.

use std::time::Duration;

use anyhow::anyhow;
use thiserror::Error;

#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub initial_backoff_ms: u64,
    pub backoff_multiplier: f32,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 2,
            initial_backoff_ms: 250,
            backoff_multiplier: 2.0,
        }
    }
}

impl RetryPolicy {
    pub const NONE: Self = Self {
        max_attempts: 1,
        initial_backoff_ms: 0,
        backoff_multiplier: 1.0,
    };
}

#[derive(Debug, Error)]
pub enum RetryableError {
    #[error("permanent: {0}")]
    Permanent(anyhow::Error),
    #[error("transient: {0}")]
    Transient(anyhow::Error),
}

pub async fn with_retry<F, Fut, T>(
    policy: &RetryPolicy,
    op_name: &str,
    mut op: F,
) -> anyhow::Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, RetryableError>>,
{
    let mut delay_ms = policy.initial_backoff_ms;
    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 1..=policy.max_attempts {
        match op().await {
            Ok(v) => return Ok(v),
            Err(RetryableError::Permanent(e)) => return Err(e),
            Err(RetryableError::Transient(e)) => {
                tracing::warn!(op = op_name, attempt, delay_ms, err = %e, "transient error, retrying");
                last_err = Some(e);
                if attempt < policy.max_attempts {
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                    delay_ms = (delay_ms as f32 * policy.backoff_multiplier) as u64;
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow!("retry exhausted: {}", op_name)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    #[tokio::test(start_paused = true)]
    async fn succeeds_on_first_attempt() {
        let calls = Arc::new(AtomicU32::new(0));
        let calls_c = calls.clone();
        let policy = RetryPolicy::default();
        let res: anyhow::Result<i32> = with_retry(&policy, "ok", || {
            let c = calls_c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Ok(42)
            }
        })
        .await;
        assert_eq!(res.unwrap(), 42);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn retries_transient_then_succeeds() {
        let calls = Arc::new(AtomicU32::new(0));
        let calls_c = calls.clone();
        let policy = RetryPolicy::default();
        let res: anyhow::Result<i32> = with_retry(&policy, "transient", || {
            let c = calls_c.clone();
            async move {
                let n = c.fetch_add(1, Ordering::SeqCst);
                if n < 2 {
                    Err(RetryableError::Transient(anyhow!("502 bad gateway")))
                } else {
                    Ok(7)
                }
            }
        })
        .await;
        assert_eq!(res.unwrap(), 7);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn permanent_fails_immediately() {
        let calls = Arc::new(AtomicU32::new(0));
        let calls_c = calls.clone();
        let policy = RetryPolicy::default();
        let res: anyhow::Result<i32> = with_retry(&policy, "permanent", || {
            let c = calls_c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Err(RetryableError::Permanent(anyhow!("400 bad request")))
            }
        })
        .await;
        assert!(res.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn exhaustion_returns_last_error() {
        let policy = RetryPolicy::default();
        let res: anyhow::Result<i32> = with_retry(&policy, "always-fail", || async {
            Err(RetryableError::Transient(anyhow!("502")))
        })
        .await;
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("502"));
    }
}
