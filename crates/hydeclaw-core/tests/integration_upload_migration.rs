//! Tests for db::upload_migration — logic tests are DB-free.
//! DB-dependent tests are marked #[ignore] and require a live PostgreSQL instance.

use hydeclaw_core::db::upload_migration::sign_uploads_in_value;
use hydeclaw_core::uploads::{derive_upload_key, HISTORICAL_URL_TTL_SECS};
use regex::Regex;
use serde_json::json;

fn test_key() -> [u8; 32] {
    derive_upload_key(&[0xAB_u8; 32])
}

fn upload_re() -> Regex {
    Regex::new(r"/uploads/([a-f0-9\-]+\.[a-z0-9.]+)").unwrap()
}

// ── Test 1: unsigned URL gets replaced ────────────────────────────────────

#[test]
fn unsigned_url_in_string_is_replaced() {
    let key = test_key();
    let re = upload_re();
    let mut val = json!("/uploads/abc123de-f456-7890-abcd-ef1234567890.png");
    let count = sign_uploads_in_value(&mut val, &re, &key);
    assert_eq!(count, 1);
    let s = val.as_str().unwrap();
    assert!(s.contains("/uploads/abc123de-f456-7890-abcd-ef1234567890.png"));
    assert!(s.contains("?sig="), "expected signed URL, got: {s}");
}

// ── Test 2: already-signed URL is unchanged ───────────────────────────────

#[test]
fn already_signed_url_is_unchanged() {
    let key = test_key();
    let re = upload_re();
    // Build a real signed URL so the regex excludes it correctly
    let signed = hydeclaw_core::uploads::mint_signed_url(
        "",
        "abc123de-f456-7890-abcd-ef1234567890.png",
        &key,
        HISTORICAL_URL_TTL_SECS,
    );
    let mut val = serde_json::Value::String(signed.clone());
    let count = sign_uploads_in_value(&mut val, &re, &key);
    assert_eq!(count, 0, "signed URL must not be re-signed");
    assert_eq!(val.as_str().unwrap(), signed);
}

// ── Test 5: regex only matches /uploads/, not /workspace-files/ ──────────

#[test]
fn regex_precision_ignores_workspace_files() {
    let key = test_key();
    let re = upload_re();
    let mut val = json!("/workspace-files/some/path/abc123de-f456-7890-abcd-ef1234567890.md");
    let count = sign_uploads_in_value(&mut val, &re, &key);
    assert_eq!(count, 0, "workspace-files path must not be matched");
}

// ── Test 6: URL nested inside tool_result.content array is replaced ───────

#[test]
fn unsigned_url_in_nested_tool_result_is_replaced() {
    let key = test_key();
    let re = upload_re();
    let mut val = json!([
        {
            "type": "tool_result",
            "tool_use_id": "toolu_01",
            "content": [
                {
                    "type": "text",
                    "text": "File saved: /uploads/abc123de-f456-7890-abcd-ef1234567890.png"
                }
            ]
        }
    ]);
    let count = sign_uploads_in_value(&mut val, &re, &key);
    assert_eq!(count, 1);
    let text = val[0]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("?sig="), "nested URL must be signed, got: {text}");
}

// ── Test 3 (DB): gate prevents second run ─────────────────────────────────

#[tokio::test]
#[ignore = "requires PostgreSQL — run with DATABASE_URL set"]
async fn second_run_returns_zero_and_makes_no_writes() {
    let db_url = std::env::var("DATABASE_URL").expect("DATABASE_URL required");
    let pool = sqlx::PgPool::connect(&db_url).await.unwrap();
    let key = test_key();

    // Seed the gate as if migration already ran
    sqlx::query(
        "INSERT INTO system_flags (key, value) \
         VALUES ('upload_sigs_migrated_v1', 'true'::jsonb) \
         ON CONFLICT (key) DO UPDATE SET value = 'true'::jsonb, updated_at = now()",
    )
    .execute(&pool)
    .await
    .unwrap();

    let n = hydeclaw_core::db::upload_migration::run_upload_signature_migration(
        &pool, &key,
    )
    .await
    .unwrap();

    assert_eq!(n, 0, "gate must short-circuit and return 0");

    // Cleanup
    sqlx::query("DELETE FROM system_flags WHERE key = 'upload_sigs_migrated_v1'")
        .execute(&pool)
        .await
        .unwrap();
}

// ── Test 4 (DB): per-row error doesn't abort the whole migration ──────────
// Omitted from automated suite — requires injecting a deliberate DB error.
// Covered by code inspection: errors are caught per-row with tracing::warn! + continue.
