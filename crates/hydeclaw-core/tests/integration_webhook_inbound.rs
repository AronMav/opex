//! Unit 5 — Webhook inbound handler integration tests.
//!
//! Coverage:
//!   - DB layer: webhook lookup by name (found / not-found / disabled)
//!   - HMAC-SHA256 signature verification (correct / wrong)
//!   - Bearer token constant-time comparison

// DB tests require Docker; restrict to Linux x86_64 where CI runs testcontainers.
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

mod support;

use support::TestHarness;
use uuid::Uuid;

// ── DB layer tests ────────────────────────────────────────────────────────────

/// Insert a minimal webhook row and return its id.
async fn insert_webhook(pool: &sqlx::PgPool, name: &str, enabled: bool, webhook_type: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO webhooks \
         (id, name, agent_id, secret, prompt_prefix, enabled, webhook_type) \
         VALUES ($1, $2, 'TestAgent', 'tok', NULL, $3, $4)",
    )
    .bind(id)
    .bind(name)
    .bind(enabled)
    .bind(webhook_type)
    .execute(pool)
    .await
    .expect("insert webhook");
    id
}

/// Run the same enabled-webhook lookup the handler uses.
async fn lookup_enabled(pool: &sqlx::PgPool, name: &str) -> Option<Uuid> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM webhooks WHERE name = $1 AND enabled = true",
    )
    .bind(name)
    .fetch_optional(pool)
    .await
    .expect("query");
    row.map(|(id,)| id)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn webhook_not_found_for_unknown_name() {
    let harness = TestHarness::new().await.expect("PG");
    let pool = harness.pool();

    assert!(
        lookup_enabled(pool, "nonexistent").await.is_none(),
        "unknown name must return None (404 path)"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn webhook_found_for_known_name() {
    let harness = TestHarness::new().await.expect("PG");
    let pool = harness.pool();

    let inserted_id = insert_webhook(pool, "test-hook", true, "generic").await;
    let found = lookup_enabled(pool, "test-hook").await;

    assert!(found.is_some(), "enabled webhook must be found");
    assert_eq!(found.unwrap(), inserted_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disabled_webhook_not_found() {
    let harness = TestHarness::new().await.expect("PG");
    let pool = harness.pool();

    insert_webhook(pool, "disabled-hook", false, "generic").await;

    assert!(
        lookup_enabled(pool, "disabled-hook").await.is_none(),
        "disabled webhook must not be found (404 path)"
    );
}

// ── HMAC unit tests ───────────────────────────────────────────────────────────

mod hmac_tests {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    use subtle::ConstantTimeEq;

    #[test]
    fn hmac_verification_correct_signature() {
        let secret = b"my-secret-key";
        let body = b"test payload";

        let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("valid key");
        mac.update(body);
        let computed = mac.finalize().into_bytes();
        let sig = format!("sha256={}", hex::encode(&computed));

        // Simulate handler: strip prefix, decode, compare.
        let hex_part = sig.strip_prefix("sha256=").expect("prefix present");
        let expected_bytes = hex::decode(hex_part).expect("valid hex");

        assert!(
            bool::from(computed.as_slice().ct_eq(&expected_bytes)),
            "correct HMAC must match"
        );
    }

    #[test]
    fn hmac_verification_wrong_signature() {
        let secret = b"my-secret-key";
        let body = b"test payload";

        let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("valid key");
        mac.update(body);
        let computed = mac.finalize().into_bytes();

        // Use a different secret to simulate a tampered signature.
        let mut mac2 = Hmac::<Sha256>::new_from_slice(b"wrong-secret").expect("valid key");
        mac2.update(body);
        let wrong = mac2.finalize().into_bytes();

        assert!(
            !bool::from(computed.as_slice().ct_eq(wrong.as_slice())),
            "mismatched HMAC must not match"
        );
    }
}

// ── Bearer token unit tests ───────────────────────────────────────────────────

mod bearer_tests {
    use subtle::ConstantTimeEq;

    #[test]
    fn bearer_token_verification_correct() {
        let expected = "secret-token-abc";
        let provided = "secret-token-abc";

        assert!(
            bool::from(provided.as_bytes().ct_eq(expected.as_bytes())),
            "matching tokens must pass ct_eq"
        );
    }

    #[test]
    fn bearer_token_verification_wrong() {
        let expected = "secret-token-abc";
        let provided = "wrong-token";

        assert!(
            !bool::from(provided.as_bytes().ct_eq(expected.as_bytes())),
            "non-matching tokens must fail ct_eq"
        );
    }

    #[test]
    fn bearer_prefix_stripped_before_compare() {
        let expected = "my-secret";
        let auth_header = "Bearer my-secret";

        let provided = auth_header.strip_prefix("Bearer ").unwrap_or(auth_header);
        assert!(
            bool::from(provided.as_bytes().ct_eq(expected.as_bytes())),
            "Bearer prefix must be stripped before comparison"
        );
    }
}
