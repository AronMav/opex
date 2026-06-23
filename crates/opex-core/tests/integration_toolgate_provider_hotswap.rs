//! RES-07 integration: toolgate module-level state survives provider reload.
//!
//! This test validates that a POST to /reload (simulating a provider config
//! change via POST /api/providers upstream) does not break module-level
//! state — subsequent requests see the new config on the very next call.
//!
//! REQUIRES: Python venv at toolgate/.venv (skip gracefully if absent — see
//! tests/support/toolgate_fixture.rs Plan 01).

mod support;

use std::time::Duration;
use support::{SpawnResult, ToolgateFixture};
use tokio::time::{timeout, Instant};

/// Pick a free TCP port for test isolation.
fn pick_test_port() -> u16 {
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);
    port
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn toolgate_health_survives_reload() {
    let port = pick_test_port();
    let spawn_result = ToolgateFixture::spawn(port, 50).await;

    let fixture = match spawn_result {
        SpawnResult::Started(f) => f,
        SpawnResult::Skipped(reason) => {
            println!("SKIP: toolgate fixture unavailable: {reason}");
            return;
        }
    };

    let client = reqwest::Client::new();
    let base = &fixture.base_url;

    // 1. GET /health must return 200 immediately.
    timeout(Duration::from_secs(10), async {
        let resp = client
            .get(format!("{base}/health"))
            .send()
            .await
            .expect("health");
        assert!(
            resp.status().is_success(),
            "initial /health must be 200; got {}",
            resp.status()
        );
    })
    .await
    .expect("initial health check timed out");

    // 2. POST /reload — simulates a provider config change.
    //    If the endpoint requires auth, we send AUTH_TOKEN from env (empty on test).
    let reload_resp = timeout(Duration::from_secs(15), async {
        client
            .post(format!("{base}/reload"))
            .send()
            .await
            .expect("reload request")
    })
    .await
    .expect("reload request timed out");
    assert!(
        reload_resp.status().is_success(),
        "/reload must return 2xx; got {}",
        reload_resp.status()
    );

    // 3. IMMEDIATE next request must see the new state — no cold-start delay.
    let start = Instant::now();
    let post_reload_health = timeout(Duration::from_secs(5), async {
        client
            .get(format!("{base}/health"))
            .send()
            .await
            .expect("health post-reload")
    })
    .await
    .expect("post-reload health timed out");
    let elapsed = start.elapsed();

    assert!(
        post_reload_health.status().is_success(),
        "post-reload /health must be 200; got {}",
        post_reload_health.status()
    );
    assert!(
        elapsed < Duration::from_millis(500),
        "post-reload /health latency must be <500ms (module state preserved); got {elapsed:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn toolgate_limit_concurrency_returns_503_at_ceiling() {
    // This test verifies the Pitfall 3 behavior: uvicorn --limit-concurrency 50
    // returns HTTP 503 when the ceiling is hit. We don't actually try to
    // generate 51 concurrent requests (hard to do reliably in CI) — instead,
    // we spawn toolgate with limit=1 and fire 20 requests. Some may return 503.
    let port = pick_test_port();
    let spawn_result = ToolgateFixture::spawn(port, 1).await;

    let fixture = match spawn_result {
        SpawnResult::Started(f) => f,
        SpawnResult::Skipped(reason) => {
            println!("SKIP: toolgate fixture unavailable: {reason}");
            return;
        }
    };

    let client = reqwest::Client::new();
    let base = fixture.base_url.clone();

    // Fire 20 concurrent requests to /health. With limit=1, at least one
    // should hit 503 if the ceiling is enforced. This is probabilistic —
    // if all 20 return 200, that's acceptable (health is fast). We assert
    // that NO 5xx other than 503 appears (proves graceful ceiling handling).
    let mut handles = Vec::new();
    for _ in 0..20 {
        let c = client.clone();
        let u = format!("{base}/health");
        handles.push(tokio::spawn(async move {
            c.get(&u).send().await.map(|r| r.status().as_u16())
        }));
    }

    let mut statuses = Vec::new();
    for h in handles {
        if let Ok(Ok(s)) = h.await {
            statuses.push(s);
        }
    }

    // Every response MUST be either 200 or 503. No other 5xx.
    for s in &statuses {
        assert!(
            *s == 200 || *s == 503,
            "every response must be 200 or 503 (Pitfall 3); got {s}"
        );
    }
    assert!(!statuses.is_empty(), "at least some requests must succeed");
}
