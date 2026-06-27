//! Durable queue for FSE video-summarization jobs. Pure sqlx leaf module
//! (no crate::* refs) — mirrors memory_queries / sessions placement.

use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct VideoJob {
    pub id: Uuid,
    pub session_id: Uuid,
    pub agent_name: String,
    pub channel_id: Option<Uuid>,
    pub source_type: String,
    pub source_ref: String,
    pub source_title: Option<String>,
    pub status: String,
    pub summary: Option<String>,
    pub error: Option<String>,
    pub attempts: i32,
}

/// Insert a pending job. Returns the new id.
pub async fn enqueue_video_job(
    db: &PgPool,
    session_id: Uuid,
    agent_name: &str,
    source_type: &str,
    source_ref: &str,
    source_title: Option<&str>,
) -> anyhow::Result<Uuid> {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO video_jobs (session_id, agent_name, source_type, source_ref, source_title) \
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(session_id)
    .bind(agent_name)
    .bind(source_type)
    .bind(source_ref)
    .bind(source_title)
    .fetch_one(db)
    .await?;
    Ok(id)
}

/// Atomically claim the oldest pending job (pending → processing, +attempts).
/// SKIP LOCKED keeps concurrent workers from grabbing the same row.
pub async fn claim_next_video_job(db: &PgPool) -> anyhow::Result<Option<VideoJob>> {
    let job: Option<VideoJob> = sqlx::query_as(
        "UPDATE video_jobs SET status='processing', attempts=attempts+1, updated_at=NOW() \
         WHERE id = ( \
             SELECT id FROM video_jobs WHERE status='pending' \
             ORDER BY created_at LIMIT 1 FOR UPDATE SKIP LOCKED \
         ) \
         RETURNING id, session_id, agent_name, channel_id, source_type, source_ref, \
                   source_title, status, summary, error, attempts",
    )
    .fetch_optional(db)
    .await?;
    Ok(job)
}

/// Reset rows stuck in 'processing' (crash recovery).
/// Jobs that have already been attempted 3+ times are marked 'failed'
/// instead of being retried — a crashed job that consistently kills the
/// worker would otherwise loop forever.
pub async fn recover_stuck_video_jobs(db: &PgPool) -> anyhow::Result<u64> {
    let res = sqlx::query(
        "UPDATE video_jobs \
         SET status   = CASE WHEN attempts >= 3 THEN 'failed' ELSE 'pending' END, \
             error    = CASE WHEN attempts >= 3 THEN 'exceeded retry limit after crash' ELSE error END, \
             updated_at = NOW() \
         WHERE status = 'processing'",
    )
    .execute(db)
    .await?;
    Ok(res.rows_affected())
}

pub async fn mark_video_job_done(db: &PgPool, id: Uuid, summary: &str) -> anyhow::Result<()> {
    sqlx::query("UPDATE video_jobs SET status='done', summary=$2, updated_at=NOW() WHERE id=$1")
        .bind(id)
        .bind(summary)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn mark_video_job_failed(db: &PgPool, id: Uuid, error: &str) -> anyhow::Result<()> {
    sqlx::query("UPDATE video_jobs SET status='failed', error=$2, updated_at=NOW() WHERE id=$1")
        .bind(id)
        .bind(error)
        .execute(db)
        .await?;
    Ok(())
}

/// Find an active (pending/processing) job for the same source in this session
/// within the dedup window — used to suppress duplicate enqueues from client
/// resubmits (e.g. mobile page reload). Returns the existing job id if found.
pub async fn find_recent_active_video_job(
    db: &PgPool,
    session_id: Uuid,
    source_ref: &str,
) -> anyhow::Result<Option<Uuid>> {
    let id: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM video_jobs \
         WHERE session_id = $1 AND source_ref = $2 \
           AND status IN ('pending','processing') \
           AND created_at > NOW() - INTERVAL '2 minutes' \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(session_id)
    .bind(source_ref)
    .fetch_optional(db)
    .await?;
    Ok(id)
}

pub async fn get_video_job(db: &PgPool, id: Uuid) -> anyhow::Result<Option<VideoJob>> {
    let job: Option<VideoJob> = sqlx::query_as(
        "SELECT id, session_id, agent_name, channel_id, source_type, source_ref, \
                source_title, status, summary, error, attempts FROM video_jobs WHERE id=$1",
    )
    .bind(id)
    .fetch_optional(db)
    .await?;
    Ok(job)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test(migrations = "../../migrations")]
    async fn enqueue_then_claim_marks_processing(pool: PgPool) {
        let sid = Uuid::new_v4();
        let id = enqueue_video_job(&pool, sid, "Atlas", "file", "https://h/api/uploads/x?sig=1", None)
            .await
            .unwrap();

        let claimed = claim_next_video_job(&pool).await.unwrap().expect("a job");
        assert_eq!(claimed.id, id);
        assert_eq!(claimed.status, "processing");
        assert_eq!(claimed.attempts, 1, "claim increments attempts");

        // A second claim finds nothing (only one pending row, now processing).
        assert!(claim_next_video_job(&pool).await.unwrap().is_none());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn recover_resets_processing_to_pending(pool: PgPool) {
        let sid = Uuid::new_v4();
        enqueue_video_job(&pool, sid, "Atlas", "file", "ref", None).await.unwrap();
        claim_next_video_job(&pool).await.unwrap().unwrap(); // → processing, attempts=1

        let n = recover_stuck_video_jobs(&pool).await.unwrap();
        assert_eq!(n, 1, "one stuck processing row recovered");

        // Now claimable again (attempts=1 < 3, so reset to pending).
        assert!(claim_next_video_job(&pool).await.unwrap().is_some());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn recover_caps_at_three_attempts(pool: PgPool) {
        let sid = Uuid::new_v4();
        enqueue_video_job(&pool, sid, "Atlas", "file", "crasher", None).await.unwrap();

        // Simulate three claim→crash cycles by claiming then force-resetting to
        // processing so the attempts counter accumulates to 3.
        for _ in 0..3 {
            // claim increments attempts and sets status=processing
            let job = claim_next_video_job(&pool).await.unwrap().expect("should be claimable");
            // Simulate a crash: worker dies mid-processing.
            // Reset status back to pending so the next iteration can claim again
            // (mimics what recover_stuck_video_jobs does for attempts < 3).
            // After 3 claims, attempts = 3 and we call recover to trigger the cap.
            sqlx::query("UPDATE video_jobs SET status='pending' WHERE id=$1")
                .bind(job.id)
                .execute(&pool)
                .await
                .unwrap();
        }

        // At this point the row has attempts=3 and status='pending'.
        // Claim once more to set status='processing' with attempts staying 3+.
        // (claim increments to 4, but the cap fires on attempts >= 3.)
        // Actually claim sets attempts to 4 here; the cap threshold is >= 3 so
        // we still want to verify >= 3 triggers the cap.  Force the row directly.
        sqlx::query("UPDATE video_jobs SET status='processing', attempts=3 WHERE session_id=$1")
            .bind(sid)
            .execute(&pool)
            .await
            .unwrap();

        let n = recover_stuck_video_jobs(&pool).await.unwrap();
        assert_eq!(n, 1, "one row recovered");

        let job = sqlx::query_as::<_, VideoJob>(
            "SELECT id, session_id, agent_name, channel_id, source_type, source_ref, \
                    source_title, status, summary, error, attempts FROM video_jobs WHERE session_id=$1",
        )
        .bind(sid)
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(job.status, "failed", "job with attempts=3 must be marked failed");
        assert_eq!(
            job.error.as_deref(),
            Some("exceeded retry limit after crash"),
            "error message must be set"
        );
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn recover_pending_below_cap_still_resets(pool: PgPool) {
        let sid = Uuid::new_v4();
        enqueue_video_job(&pool, sid, "Atlas", "file", "recoverable", None).await.unwrap();

        // Two claims (attempts=2), then force-stuck in processing.
        for _ in 0..2 {
            claim_next_video_job(&pool).await.unwrap().expect("claimable");
            sqlx::query("UPDATE video_jobs SET status='pending' WHERE session_id=$1")
                .bind(sid)
                .execute(&pool)
                .await
                .unwrap();
        }
        sqlx::query("UPDATE video_jobs SET status='processing', attempts=2 WHERE session_id=$1")
            .bind(sid)
            .execute(&pool)
            .await
            .unwrap();

        recover_stuck_video_jobs(&pool).await.unwrap();

        let job = sqlx::query_as::<_, VideoJob>(
            "SELECT id, session_id, agent_name, channel_id, source_type, source_ref, \
                    source_title, status, summary, error, attempts FROM video_jobs WHERE session_id=$1",
        )
        .bind(sid)
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(job.status, "pending", "attempts=2 < 3, must reset to pending");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn done_and_failed_persist(pool: PgPool) {
        let sid = Uuid::new_v4();
        let id = enqueue_video_job(&pool, sid, "Atlas", "url", "https://youtu.be/x", None).await.unwrap();
        mark_video_job_done(&pool, id, "the summary").await.unwrap();
        let j = get_video_job(&pool, id).await.unwrap().unwrap();
        assert_eq!(j.status, "done");
        assert_eq!(j.summary.as_deref(), Some("the summary"));

        let id2 = enqueue_video_job(&pool, sid, "Atlas", "url", "https://youtu.be/y", None).await.unwrap();
        mark_video_job_failed(&pool, id2, "yt-dlp: private video").await.unwrap();
        let j2 = get_video_job(&pool, id2).await.unwrap().unwrap();
        assert_eq!(j2.status, "failed");
        assert_eq!(j2.error.as_deref(), Some("yt-dlp: private video"));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn find_recent_active_returns_existing_job(pool: PgPool) {
        let sid = Uuid::new_v4();
        let source = "https://h/api/uploads/dup-video?sig=1";

        let id = enqueue_video_job(&pool, sid, "Atlas", "file", source, None)
            .await
            .unwrap();

        // Same session + same source_ref within 2 minutes → dedup fires.
        let found = find_recent_active_video_job(&pool, sid, source)
            .await
            .unwrap();
        assert_eq!(found, Some(id), "must return the existing pending job id");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn find_recent_active_different_source_ref_returns_none(pool: PgPool) {
        let sid = Uuid::new_v4();
        enqueue_video_job(&pool, sid, "Atlas", "file", "https://h/api/uploads/a?sig=1", None)
            .await
            .unwrap();

        // Different source_ref → no match.
        let found = find_recent_active_video_job(&pool, sid, "https://h/api/uploads/b?sig=1")
            .await
            .unwrap();
        assert!(found.is_none(), "different source_ref must not match");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn find_recent_active_unknown_session_returns_none(pool: PgPool) {
        let sid = Uuid::new_v4();
        let source = "https://h/api/uploads/orphan?sig=1";

        // Non-existent session → no rows.
        let found = find_recent_active_video_job(&pool, sid, source).await.unwrap();
        assert!(found.is_none(), "non-existent session must return None");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn find_recent_active_done_job_returns_none(pool: PgPool) {
        let sid = Uuid::new_v4();
        let source = "https://h/api/uploads/done-vid?sig=1";

        let id = enqueue_video_job(&pool, sid, "Atlas", "url", source, None)
            .await
            .unwrap();
        mark_video_job_done(&pool, id, "summary text").await.unwrap();

        // status='done' is NOT in ('pending','processing') → dedup must ignore it.
        let found = find_recent_active_video_job(&pool, sid, source).await.unwrap();
        assert!(found.is_none(), "done job must not block re-enqueue");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn enqueue_persists_source_title(pool: PgPool) {
        let sid = Uuid::new_v4();
        let id = enqueue_video_job(&pool, sid, "Atlas", "file", "https://h/api/uploads/x?sig=1", Some("Лекция по Rust.mp4"))
            .await.unwrap();
        let j = get_video_job(&pool, id).await.unwrap().unwrap();
        assert_eq!(j.source_title.as_deref(), Some("Лекция по Rust.mp4"));

        // None is allowed (url jobs may have no title yet)
        let id2 = enqueue_video_job(&pool, sid, "Atlas", "url", "https://youtu.be/x", None).await.unwrap();
        let j2 = get_video_job(&pool, id2).await.unwrap().unwrap();
        assert!(j2.source_title.is_none());
    }
}
