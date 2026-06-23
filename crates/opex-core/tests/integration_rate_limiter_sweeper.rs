//! RES-04: validate sweep() evicts expired rate-limiter entries.
//!
//! Phase 62 RES-04 factored the inline-on-write eviction out of the hot path
//! (record_failure / check) into a dedicated `sweep()` method plus a 60-second
//! background tokio task. These tests use the `__test_insert` / `__test_len`
//! backdoors (doc(hidden)) to exercise `sweep()` in isolation — no HTTP server,
//! no real clock, no await on the 60s timer.

use opex_core::gateway::middleware::{AuthRateLimiter, RequestRateLimiter};
use std::time::{Duration, Instant};
use tokio::time::timeout;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auth_sweep_evicts_all_expired() {
    timeout(Duration::from_secs(10), async {
        let limiter = AuthRateLimiter::new(3, 10);
        let expired = Instant::now() - Duration::from_secs(30);
        for i in 0..100 {
            limiter
                .__test_insert(&format!("10.0.0.{i}"), expired, None)
                .await;
        }
        assert_eq!(limiter.__test_len().await, 100, "precondition: 100 inserted");

        limiter.sweep().await;
        assert_eq!(
            limiter.__test_len().await,
            0,
            "all 100 expired entries must be evicted"
        );
    })
    .await
    .expect("test timeout");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auth_sweep_preserves_fresh_evicts_expired() {
    timeout(Duration::from_secs(10), async {
        let limiter = AuthRateLimiter::new(3, 10);
        let expired = Instant::now() - Duration::from_secs(30);
        let fresh = Instant::now();
        for i in 0..50 {
            limiter
                .__test_insert(&format!("10.0.0.{i}"), expired, None)
                .await;
        }
        for i in 0..50 {
            limiter
                .__test_insert(&format!("10.0.1.{i}"), fresh, None)
                .await;
        }
        assert_eq!(limiter.__test_len().await, 100, "precondition");

        limiter.sweep().await;
        assert_eq!(
            limiter.__test_len().await,
            50,
            "only 50 fresh entries must remain"
        );
    })
    .await
    .expect("test timeout");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auth_sweep_preserves_locked_out_entries_in_window() {
    timeout(Duration::from_secs(10), async {
        let limiter = AuthRateLimiter::new(3, 10);
        // Locked until 5s in the future — must be preserved.
        let locked_until = Some(Instant::now() + Duration::from_secs(5));
        for i in 0..10 {
            limiter
                .__test_insert(&format!("10.0.2.{i}"), Instant::now(), locked_until)
                .await;
        }

        limiter.sweep().await;
        assert_eq!(
            limiter.__test_len().await,
            10,
            "locked-until-future entries must survive sweep"
        );
    })
    .await
    .expect("test timeout");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_sweep_evicts_stale_window_entries() {
    timeout(Duration::from_secs(10), async {
        let limiter = RequestRateLimiter::new(300);
        let stale = Instant::now() - Duration::from_secs(120);
        let fresh = Instant::now();
        for i in 0..20 {
            limiter.__test_insert(&format!("10.0.3.{i}"), stale).await;
        }
        for i in 0..5 {
            limiter.__test_insert(&format!("10.0.4.{i}"), fresh).await;
        }
        assert_eq!(limiter.__test_len().await, 25, "precondition");

        limiter.sweep().await;
        assert_eq!(limiter.__test_len().await, 5, "only 5 fresh must remain");
    })
    .await
    .expect("test timeout");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sweep_on_empty_limiter_is_noop() {
    timeout(Duration::from_secs(10), async {
        let auth = AuthRateLimiter::new(3, 10);
        auth.sweep().await;
        assert_eq!(auth.__test_len().await, 0);

        let req = RequestRateLimiter::new(300);
        req.sweep().await;
        assert_eq!(req.__test_len().await, 0);
    })
    .await
    .expect("test timeout");
}
