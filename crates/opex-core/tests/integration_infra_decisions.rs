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

#[sqlx::test(migrations = "../../migrations")]
async fn resolve_marks_status(pool: PgPool) {
    let cmds = serde_json::json!(["docker rm x"]);
    let id = ind::create(&pool, "docker-x-1", "d", "a", &cmds, "pending", 7)
        .await
        .unwrap();
    let d = ind::resolve_strict(&pool, id, "rejected", "owner").await.unwrap();
    assert_eq!(d.status, "rejected");
    assert_eq!(d.resolved_by.as_deref(), Some("owner"));
}

// C1 (final review): `has_recent` used to only suppress on pending/done/
// dismissed/rejected — approved and failed fell through, causing the
// watchdog to respawn a triage session every cycle after approve (during
// execution) or after a failed fix (endless retrigger).
#[sqlx::test(migrations = "../../migrations")]
async fn has_recent_suppresses_approved(pool: PgPool) {
    let cmds = serde_json::json!([]);
    let id = ind::create(&pool, "docker-approved-1", "d", "a", &cmds, "pending", 7)
        .await
        .unwrap();
    ind::resolve_strict(&pool, id, "approved", "owner").await.unwrap();
    assert!(
        ind::has_recent(&pool, "docker-approved-1", 24).await.unwrap(),
        "approved decision within cooldown must suppress retrigger"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn has_recent_suppresses_failed(pool: PgPool) {
    let cmds = serde_json::json!([]);
    let id = ind::create(&pool, "docker-failed-1", "d", "a", &cmds, "pending", 7)
        .await
        .unwrap();
    ind::mark_status(&pool, id, "failed").await.unwrap();
    assert!(
        ind::has_recent(&pool, "docker-failed-1", 24).await.unwrap(),
        "failed decision within cooldown must suppress retrigger (no infinite loop)"
    );
}

// I1 (final review): the `triaging` anchor row inserted by `api_infra_event`
// before spawning the isolated session must itself suppress `has_recent`,
// otherwise a crashed/errored Opex session (no decision recorded) would be
// respawned every watchdog cycle.
#[sqlx::test(migrations = "../../migrations")]
async fn has_recent_suppresses_triaging_anchor(pool: PgPool) {
    let cmds = serde_json::json!([]);
    ind::create(&pool, "docker-triage-1", "auto-triage in progress", "", &cmds, "triaging", 1)
        .await
        .unwrap();
    assert!(ind::has_recent(&pool, "docker-triage-1", 24).await.unwrap());
}

// I1: `expire_stale` must also clean up abandoned `triaging` anchors (e.g. if
// Opex died and never followed up), not just `pending`.
#[sqlx::test(migrations = "../../migrations")]
async fn expire_stale_clears_triaging(pool: PgPool) {
    let cmds = serde_json::json!([]);
    let id = ind::create(&pool, "docker-stale-triage-1", "auto-triage in progress", "", &cmds, "triaging", 1)
        .await
        .unwrap();
    // Force the anchor into the past so the lazy-TTL sweep picks it up.
    sqlx::query("UPDATE infra_decisions SET expires_at = now() - interval '1 hour' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let n = ind::expire_stale(&pool).await.unwrap();
    assert_eq!(n, 1);
    let got = ind::get(&pool, id).await.unwrap().unwrap();
    assert_eq!(got.status, "expired");
}
