#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use opex_db::infra_decisions as ind;
use sqlx::PgPool;

#[sqlx::test(migrations = "../../migrations")]
async fn create_then_get_roundtrip(pool: PgPool) {
    let cmds = serde_json::json!(["docker rm foo"]);
    let id = ind::create(&pool, "docker-foo-1", "diag", "rm it", &cmds, "pending", 7)
        .await
        .unwrap();
    let got = ind::get(&pool, id).await.unwrap().unwrap();
    assert_eq!(got.container, "docker-foo-1");
    assert_eq!(got.status, "pending");
    assert_eq!(got.proposed_commands, cmds);
}

#[sqlx::test(migrations = "../../migrations")]
async fn unique_pending_per_container(pool: PgPool) {
    let cmds = serde_json::json!([]);
    ind::create(&pool, "docker-bar-1", "d", "a", &cmds, "pending", 7)
        .await
        .unwrap();
    let second = ind::create(&pool, "docker-bar-1", "d", "a", &cmds, "pending", 7).await;
    assert!(second.is_err(), "второй pending на тот же контейнер должен упасть на UNIQUE index");
}

#[sqlx::test(migrations = "../../migrations")]
async fn resolve_strict_rejects_double(pool: PgPool) {
    let cmds = serde_json::json!([]);
    let id = ind::create(&pool, "docker-baz-1", "d", "a", &cmds, "pending", 7)
        .await
        .unwrap();
    ind::resolve_strict(&pool, id, "approved", "owner").await.unwrap();
    let again = ind::resolve_strict(&pool, id, "rejected", "owner").await;
    assert!(matches!(again, Err(ind::InfraError::AlreadyResolved { .. })));
}

#[sqlx::test(migrations = "../../migrations")]
async fn has_recent_debounce(pool: PgPool) {
    let cmds = serde_json::json!([]);
    assert!(!ind::has_recent(&pool, "docker-qux-1", 24).await.unwrap());
    ind::create(&pool, "docker-qux-1", "d", "a", &cmds, "dismissed", 7)
        .await
        .unwrap();
    assert!(ind::has_recent(&pool, "docker-qux-1", 24).await.unwrap());
}
