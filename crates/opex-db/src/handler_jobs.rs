//! Universal durable queue for File Handler Hub async jobs. Generalizes
//! video_jobs — handler-agnostic (params/result are JSONB catch-alls) and
//! source-agnostic (upload_id for uploaded files, source_ref for external URLs).

use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct HandlerJob {
    pub id: Uuid,
    pub upload_id: Option<Uuid>,
    pub source_ref: Option<String>,
    pub handler_id: String,
    pub agent_name: String,
    pub session_id: Uuid,
    pub params: serde_json::Value,
    pub status: String,
    pub phase: Option<String>,
    pub pct: Option<i32>,
    pub result: Option<serde_json::Value>,
    pub attempts: i32,
}

impl HandlerJob {
    /// Convenience accessor for the failure reason recorded under `result.reason`.
    pub fn error(&self) -> Option<&str> {
        self.result.as_ref()?.get("reason")?.as_str()
    }
}

const COLS: &str = "id, upload_id, source_ref, handler_id, agent_name, session_id, \
                    params, status, phase, pct, result, attempts";

/// Enqueue a queued job. Exactly one of `upload_id` / `source_ref` is normally
/// set (upload-based vs url-based source). Returns the new id.
pub async fn insert_handler_job(
    db: &PgPool,
    upload_id: Option<Uuid>,
    source_ref: Option<&str>,
    handler_id: &str,
    agent_name: &str,
    session_id: Uuid,
    params: &serde_json::Value,
) -> anyhow::Result<Uuid> {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO handler_jobs \
             (upload_id, source_ref, handler_id, agent_name, session_id, params) \
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(upload_id)
    .bind(source_ref)
    .bind(handler_id)
    .bind(agent_name)
    .bind(session_id)
    .bind(params)
    .fetch_one(db)
    .await?;
    Ok(id)
}

/// Atomically claim the oldest queued job (queued → processing, +attempts).
/// SKIP LOCKED keeps concurrent workers from grabbing the same row.
pub async fn claim_next_handler_job(db: &PgPool) -> anyhow::Result<Option<HandlerJob>> {
    let job: Option<HandlerJob> = sqlx::query_as(&format!(
        "UPDATE handler_jobs SET status='processing', attempts=attempts+1, updated_at=now() \
         WHERE id = ( \
             SELECT id FROM handler_jobs WHERE status='queued' \
             ORDER BY created_at LIMIT 1 FOR UPDATE SKIP LOCKED \
         ) RETURNING {COLS}"
    ))
    .fetch_optional(db)
    .await?;
    Ok(job)
}

/// Count jobs currently in flight ('processing'). Used to bound concurrent
/// out-of-process runners (F088): each runner (yt-dlp + ffmpeg + STT + LLM
/// map-reduce) runs for minutes and is fire-and-forget on the toolgate side, so
/// dispatching one job every poll regardless of how many are still running would
/// spawn unbounded concurrent processes and exhaust CPU/RAM.
pub async fn count_processing_handler_jobs(db: &PgPool) -> anyhow::Result<i64> {
    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM handler_jobs WHERE status='processing'")
            .fetch_one(db)
            .await?;
    Ok(n)
}

pub async fn mark_handler_job_processing(db: &PgPool, id: Uuid) -> anyhow::Result<()> {
    sqlx::query("UPDATE handler_jobs SET status='processing', updated_at=now() WHERE id=$1")
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn update_handler_job_progress(
    db: &PgPool,
    id: Uuid,
    phase: &str,
    pct: i32,
) -> anyhow::Result<()> {
    sqlx::query("UPDATE handler_jobs SET phase=$2, pct=$3, updated_at=now() WHERE id=$1")
        .bind(id)
        .bind(phase)
        .bind(pct)
        .execute(db)
        .await?;
    Ok(())
}

/// Atomically transition `id` from `'processing'` to `'done'`.
/// Returns `true` if the row was actually updated (i.e. it was still
/// `'processing'`); returns `false` if the row was already terminal —
/// callers use this to skip duplicate side-effects on replayed callbacks.
pub async fn mark_handler_job_done(
    db: &PgPool,
    id: Uuid,
    result: &serde_json::Value,
) -> anyhow::Result<bool> {
    let rows = sqlx::query(
        "UPDATE handler_jobs SET status='done', result=$2, updated_at=now() \
         WHERE id=$1 AND status='processing'",
    )
    .bind(id)
    .bind(result)
    .execute(db)
    .await?
    .rows_affected();
    Ok(rows > 0)
}

/// Atomically transition `id` from `'processing'` to `'failed'`.
/// Returns `true` if the row was actually updated (i.e. it was still
/// `'processing'`); returns `false` if the row was already terminal —
/// callers use this to skip duplicate side-effects on replayed callbacks.
pub async fn mark_handler_job_failed(db: &PgPool, id: Uuid, error: &str) -> anyhow::Result<bool> {
    // Store the error string under result.reason so the wire shape stays uniform
    // with ScenarioOutcome ({status, reason}); HandlerJob::error() reads it back.
    let result = serde_json::json!({ "status": "failed", "reason": error });
    let rows = sqlx::query(
        "UPDATE handler_jobs SET status='failed', result=$2, updated_at=now() \
         WHERE id=$1 AND status='processing'",
    )
    .bind(id)
    .bind(result)
    .execute(db)
    .await?
    .rows_affected();
    Ok(rows > 0)
}

/// Reset rows stuck in 'processing' (crash recovery). Jobs attempted 3+ times
/// are marked failed instead of retried (mirrors video_jobs).
/// Returns the number of rows touched (both reset-to-queued and marked-failed).
pub async fn recover_stale_handler_jobs(db: &PgPool) -> anyhow::Result<u64> {
    let res = sqlx::query(
        "UPDATE handler_jobs \
         SET status = CASE WHEN attempts >= 3 THEN 'failed' ELSE 'queued' END, \
             result = CASE WHEN attempts >= 3 \
                          THEN jsonb_build_object('status','failed','reason','exceeded retry limit after crash') \
                          ELSE result END, \
             updated_at = now() \
         WHERE status = 'processing'",
    )
    .execute(db)
    .await?;
    Ok(res.rows_affected())
}

/// List rows stuck in 'processing' whose `updated_at` is older than
/// `older_than_secs` — a healthy in-flight job bumps `updated_at` on every
/// claim / progress post, so an aged row means its out-of-process runner died
/// (crash / OOM / network partition) without posting `/complete`. Used by the
/// worker's runtime sweep (F014) to surface a terminal chat failure instead of
/// leaving the job — and the chat — hanging forever. Deadline must exceed the
/// runner's own wall-clock cap (F016) plus the largest gap between progress
/// posts, so long-video jobs are never falsely reaped.
pub async fn list_stale_processing_jobs(
    db: &PgPool,
    older_than_secs: i64,
) -> anyhow::Result<Vec<HandlerJob>> {
    let jobs: Vec<HandlerJob> = sqlx::query_as(&format!(
        "SELECT {COLS} FROM handler_jobs \
         WHERE status='processing' AND updated_at < now() - make_interval(secs => $1)"
    ))
    .bind(older_than_secs as f64)
    .fetch_all(db)
    .await?;
    Ok(jobs)
}

pub async fn get_handler_job(db: &PgPool, id: Uuid) -> anyhow::Result<Option<HandlerJob>> {
    let job: Option<HandlerJob> =
        sqlx::query_as(&format!("SELECT {COLS} FROM handler_jobs WHERE id=$1"))
            .bind(id)
            .fetch_optional(db)
            .await?;
    Ok(job)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test(migrations = "../../migrations")]
    async fn insert_then_claim_marks_processing(pool: sqlx::PgPool) {
        let sid = uuid::Uuid::new_v4();
        let upload = uuid::Uuid::new_v4();
        let id = insert_handler_job(
            &pool,
            Some(upload),
            None,
            "summarize_video",
            "Atlas",
            sid,
            &serde_json::json!({"language": "ru"}),
        )
        .await
        .unwrap();

        let claimed = claim_next_handler_job(&pool).await.unwrap().expect("a job");
        assert_eq!(claimed.id, id);
        assert_eq!(claimed.status, "processing");
        assert_eq!(claimed.attempts, 1, "claim increments attempts");
        assert_eq!(claimed.handler_id, "summarize_video");
        assert_eq!(claimed.upload_id, Some(upload));
        assert_eq!(claimed.source_ref, None);

        // Only one queued row → second claim finds nothing.
        assert!(claim_next_handler_job(&pool).await.unwrap().is_none());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn insert_url_job_carries_source_ref(pool: sqlx::PgPool) {
        let sid = uuid::Uuid::new_v4();
        let id = insert_handler_job(
            &pool,
            None,
            Some("https://www.youtube.com/watch?v=abc"),
            "summarize_video",
            "Atlas",
            sid,
            &serde_json::json!({"language": "ru"}),
        )
        .await
        .unwrap();
        let row = get_handler_job(&pool, id).await.unwrap().unwrap();
        assert_eq!(row.upload_id, None);
        assert_eq!(
            row.source_ref.as_deref(),
            Some("https://www.youtube.com/watch?v=abc")
        );
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn progress_then_done_persists(pool: sqlx::PgPool) {
        let sid = uuid::Uuid::new_v4();
        let id = insert_handler_job(
            &pool,
            None,
            None,
            "summarize_video",
            "Atlas",
            sid,
            &serde_json::json!({}),
        )
        .await
        .unwrap();
        claim_next_handler_job(&pool).await.unwrap().unwrap();

        update_handler_job_progress(&pool, id, "digest", 42)
            .await
            .unwrap();
        let row = get_handler_job(&pool, id).await.unwrap().unwrap();
        assert_eq!(row.phase.as_deref(), Some("digest"));
        assert_eq!(row.pct, Some(42));

        let transitioned = mark_handler_job_done(
            &pool,
            id,
            &serde_json::json!({"status": "ok", "summary_text": "x"}),
        )
        .await
        .unwrap();
        assert!(transitioned, "first mark_done must return true");
        let row = get_handler_job(&pool, id).await.unwrap().unwrap();
        assert_eq!(row.status, "done");
        assert_eq!(row.result.as_ref().unwrap()["status"], "ok");

        // Idempotency: second call on an already-done row returns false.
        let again = mark_handler_job_done(
            &pool,
            id,
            &serde_json::json!({"status": "ok", "summary_text": "duplicate"}),
        )
        .await
        .unwrap();
        assert!(!again, "replayed mark_done must return false — row is already terminal");
        // The stored result must NOT have been overwritten.
        let row2 = get_handler_job(&pool, id).await.unwrap().unwrap();
        assert_eq!(row2.result.as_ref().unwrap()["summary_text"], "x", "stored result must be unchanged after replayed callback");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn mark_failed_records_reason(pool: sqlx::PgPool) {
        let sid = uuid::Uuid::new_v4();
        let id = insert_handler_job(
            &pool,
            None,
            None,
            "summarize_video",
            "Atlas",
            sid,
            &serde_json::json!({}),
        )
        .await
        .unwrap();
        claim_next_handler_job(&pool).await.unwrap().unwrap();

        let transitioned = mark_handler_job_failed(&pool, id, "boom").await.unwrap();
        assert!(transitioned, "first mark_failed must return true");
        let row = get_handler_job(&pool, id).await.unwrap().unwrap();
        assert_eq!(row.status, "failed");
        assert_eq!(row.error(), Some("boom"));

        // Idempotency: second call on an already-failed row returns false.
        let again = mark_handler_job_failed(&pool, id, "second call").await.unwrap();
        assert!(!again, "replayed mark_failed must return false — row is already terminal");
        let row2 = get_handler_job(&pool, id).await.unwrap().unwrap();
        assert_eq!(row2.error(), Some("boom"), "stored error must be unchanged after replayed callback");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn recover_stale_resets_below_cap_and_fails_at_cap(pool: sqlx::PgPool) {
        let sid = uuid::Uuid::new_v4();

        // Row A: attempts=1, stuck processing → reset to queued.
        let a = insert_handler_job(
            &pool,
            None,
            None,
            "summarize_video",
            "Atlas",
            sid,
            &serde_json::json!({}),
        )
        .await
        .unwrap();
        claim_next_handler_job(&pool).await.unwrap().unwrap(); // attempts=1, processing

        // Row B: force attempts=3, stuck processing → marked failed.
        let b = insert_handler_job(
            &pool,
            None,
            None,
            "summarize_video",
            "Atlas",
            sid,
            &serde_json::json!({}),
        )
        .await
        .unwrap();
        sqlx::query("UPDATE handler_jobs SET status='processing', attempts=3 WHERE id=$1")
            .bind(b)
            .execute(&pool)
            .await
            .unwrap();

        let n = recover_stale_handler_jobs(&pool).await.unwrap();
        assert_eq!(n, 2, "both stuck rows touched");

        let ra = get_handler_job(&pool, a).await.unwrap().unwrap();
        assert_eq!(ra.status, "queued", "attempts<3 resets to queued");

        let rb = get_handler_job(&pool, b).await.unwrap().unwrap();
        assert_eq!(rb.status, "failed", "attempts>=3 marked failed");
        assert_eq!(
            rb.error(),
            Some("exceeded retry limit after crash")
        );
    }
}
