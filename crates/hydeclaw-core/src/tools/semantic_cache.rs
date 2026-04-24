use sqlx::{PgPool, Row};
use anyhow::Result;
use serde_json::Value;
use std::sync::Arc;
use crate::memory::EmbeddingService;

pub struct SemanticCache;

impl SemanticCache {
    pub async fn check(
        db: &PgPool,
        embedder: &Arc<dyn EmbeddingService>,
        tool_name: &str,
        query_text: &str,
        similarity_threshold: f32,
    ) -> Result<Option<String>> {
        let embedding = embedder.embed(query_text).await?;
        let embedding_vec: Vec<f32> = embedding.to_vec();

        // Search for similar queries in the cache that haven't expired
        let row = sqlx::query(
            r#"
            SELECT result_json
            FROM tool_execution_cache
            WHERE tool_name = $1
              AND expires_at > now()
              AND (1 - (query_embedding <=> $2::vector)) > $3
            ORDER BY query_embedding <=> $2::vector ASC
            LIMIT 1
            "#,
        )
        .bind(tool_name)
        .bind(embedding_vec)
        .bind(similarity_threshold)
        .fetch_optional(db)
        .await?;

        Ok(row.map(|r| {
            let val: Value = r.get("result_json");
            val.to_string()
        }))
    }

    pub async fn store(
        db: &PgPool,
        embedder: &Arc<dyn EmbeddingService>,
        tool_name: &str,
        query_text: &str,
        result: &str,
        ttl_secs: i64,
    ) -> Result<()> {
        let embedding = embedder.embed(query_text).await?;
        let embedding_vec: Vec<f32> = embedding.to_vec();
        let result_json: Value = serde_json::from_str(result).unwrap_or_else(|_| Value::String(result.to_string()));

        sqlx::query(
            r#"
            INSERT INTO tool_execution_cache (tool_name, query_embedding, result_json, expires_at)
            VALUES ($1, $2::vector, $3, now() + make_interval(secs => $4))
            "#,
        )
        .bind(tool_name)
        .bind(embedding_vec)
        .bind(result_json)
        .bind(ttl_secs as f64)
        .execute(db)
        .await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::memory::EmbeddingService;
    use crate::memory::embedding::FakeEmbedder;
    use std::sync::Arc;

    #[tokio::test]
    async fn test_semantic_cache_embedding_flow() {
        let embedder = Arc::new(FakeEmbedder { available: true }) as Arc<dyn EmbeddingService>;

        // Real DB testing requires a running Postgres, but we can verify the
        // embedding extraction flow which is used by SemanticCache.
        let query = "what is the weather in Samara?";
        let embedding = embedder.embed(query).await.unwrap();

        assert_eq!(embedding.len(), 4); // FakeEmbedder returns fixed 4-dim stub vector
    }
}
