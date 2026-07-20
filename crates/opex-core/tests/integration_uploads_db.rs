//! End-to-end: agent_icon upsert/lookup/delete + tool_output retention +
//! cleanup against a real ephemeral PostgreSQL container (Task 13 of the
//! uploads-to-db migration plan).

mod support;

use support::TestHarness;

use opex_core::db::uploads;
use opex_core::uploads::{mint_uploads_url, verify_uploads_url};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn icon_roundtrip_db_layer() {
    let h = TestHarness::new().await.expect("ephemeral PG");
    let pool = h.pool();
    let png = b"\x89PNG\r\n\x1a\nbogus-but-fine-for-test".to_vec();

    let id = uploads::upsert_agent_icon(pool, "Opex", "image/png", &png)
        .await
        .expect("upsert");
    assert!(
        uploads::lookup_agent_icon_id(pool, "Opex")
            .await
            .unwrap()
            .is_some(),
        "icon row visible after upsert"
    );

    let row = uploads::get_by_id(pool, id)
        .await
        .expect("get_by_id")
        .expect("row exists");
    assert_eq!(row.mime, "image/png");
    assert_eq!(row.data, png);
    assert!(
        row.expires_at.is_none(),
        "agent_icon rows are permanent (expires_at = NULL)"
    );

    // Sign + verify roundtrip with the same upload-HMAC key.
    let key = [123u8; 32];
    let url = mint_uploads_url("http://h", id, &key, 60);
    let qs = url.split('?').nth(1).expect("signed URL has query");
    let mut sig = String::new();
    let mut exp = 0u64;
    for kv in qs.split('&') {
        let (k, v) = kv.split_once('=').expect("k=v pair");
        match k {
            "sig" => sig = v.to_string(),
            "exp" => exp = v.parse().expect("exp is u64"),
            _ => {}
        }
    }
    assert!(verify_uploads_url(id, &sig, exp, &key).is_ok());

    assert_eq!(
        uploads::delete_agent_icon(pool, "Opex").await.unwrap(),
        1,
        "delete returns 1 affected row"
    );
    assert!(uploads::lookup_agent_icon_id(pool, "Opex")
        .await
        .unwrap()
        .is_none());
    assert!(uploads::get_by_id(pool, id).await.unwrap().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tool_output_with_retention_then_cleanup() {
    let h = TestHarness::new().await.expect("ephemeral PG");
    let pool = h.pool();

    // Fresh tool_output row — retention 30 days, expires in the future.
    let _id = uploads::insert_with_retention(
        pool,
        "tool_output",
        Some("msg-uuid"),
        "audio/mp3",
        b"audio-content",
        30,
        None,
    )
    .await
    .expect("insert tool_output");

    // Already-expired tool_output row inserted directly via SQL so we don't
    // race the test clock.
    let sha = vec![0u8; 32];
    sqlx::query(
        r#"INSERT INTO uploads (id, owner_type, owner_id, mime, data, sha256, size_bytes, expires_at)
           VALUES (gen_random_uuid(), 'tool_output', 'old', 'a', $1, $2, 1, NOW() - INTERVAL '1 day')"#,
    )
    .bind(b"a".to_vec())
    .bind(&sha)
    .execute(pool)
    .await
    .expect("seed expired row");

    let deleted = uploads::cleanup_expired(pool).await.expect("cleanup");
    assert_eq!(deleted, 1, "exactly one expired row swept");

    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM uploads")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(remaining, 1, "only the fresh tool_output survives");
}
