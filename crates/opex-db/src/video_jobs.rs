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
) -> anyhow::Result<Uuid> {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO video_jobs (session_id, agent_name, source_type, source_ref) \
         VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(session_id)
    .bind(agent_name)
    .bind(source_type)
    .bind(source_ref)
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
                   status, summary, error, attempts",
    )
    .fetch_optional(db)
    .await?;
    Ok(job)
}

/// Reset rows stuck in 'processing' (crash recovery) back to 'pending'.
pub async fn recover_stuck_video_jobs(db: &PgPool) -> anyhow::Result<u64> {
    let res = sqlx::query(
        "UPDATE video_jobs SET status='pending', updated_at=NOW() WHERE status='processing'",
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

pub async fn get_video_job(db: &PgPool, id: Uuid) -> anyhow::Result<Option<VideoJob>> {
    let job: Option<VideoJob> = sqlx::query_as(
        "SELECT id, session_id, agent_name, channel_id, source_type, source_ref, \
                status, summary, error, attempts FROM video_jobs WHERE id=$1",
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
        let id = enqueue_video_job(&pool, sid, "Atlas", "file", "https://h/api/uploads/x?sig=1")
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
        enqueue_video_job(&pool, sid, "Atlas", "file", "ref").await.unwrap();
        claim_next_video_job(&pool).await.unwrap().unwrap(); // → processing

        let n = recover_stuck_video_jobs(&pool).await.unwrap();
        assert_eq!(n, 1, "one stuck processing row recovered");

        // Now claimable again.
        assert!(claim_next_video_job(&pool).await.unwrap().is_some());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn done_and_failed_persist(pool: PgPool) {
        let sid = Uuid::new_v4();
        let id = enqueue_video_job(&pool, sid, "Atlas", "url", "https://youtu.be/x").await.unwrap();
        mark_video_job_done(&pool, id, "the summary").await.unwrap();
        let j = get_video_job(&pool, id).await.unwrap().unwrap();
        assert_eq!(j.status, "done");
        assert_eq!(j.summary.as_deref(), Some("the summary"));

        let id2 = enqueue_video_job(&pool, sid, "Atlas", "url", "https://youtu.be/y").await.unwrap();
        mark_video_job_failed(&pool, id2, "yt-dlp: private video").await.unwrap();
        let j2 = get_video_job(&pool, id2).await.unwrap().unwrap();
        assert_eq!(j2.status, "failed");
        assert_eq!(j2.error.as_deref(), Some("yt-dlp: private video"));
    }
}
