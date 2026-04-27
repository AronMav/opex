/// `MemoryAdmin` — administrative operations on the memory store.
///
/// Static methods for language detection/validation and instance methods
/// that delegate to `crate::db::memory_queries`.
use sqlx::PgPool;

pub struct MemoryAdmin {
    pub db: PgPool,
}

impl MemoryAdmin {
    /// Auto-detect FTS language from agent language code (e.g. "ru" -> "russian").
    pub fn detect_fts_language(agent_lang: &str) -> String {
        match agent_lang {
            "ru" => "russian",
            "en" => "english",
            "es" => "spanish",
            "de" => "german",
            "fr" => "french",
            "pt" => "portuguese",
            "it" => "italian",
            "nl" => "dutch",
            "sv" => "swedish",
            "no" | "nb" => "norwegian",
            "da" => "danish",
            "fi" => "finnish",
            "hu" => "hungarian",
            "ro" => "romanian",
            "tr" => "turkish",
            _ => "simple", // fallback for unsupported languages
        }.to_string()
    }

    /// Validate FTS language is safe for SQL interpolation (lowercase ASCII only).
    pub fn validated_fts_language(lang: &str) -> anyhow::Result<String> {
        anyhow::ensure!(
            !lang.is_empty() && lang.chars().all(|c| c.is_ascii_lowercase()),
            "invalid FTS language: {lang}"
        );
        Ok(lang.to_string())
    }

    /// Rebuild all tsv columns with the given FTS language.
    pub async fn rebuild_fts(&self, fts_language: &str) -> anyhow::Result<u64> {
        crate::db::memory_queries::rebuild_fts(&self.db, fts_language).await
    }

    /// Delete all chunks with a given source (e.g. filename).
    pub async fn delete_by_source(&self, source: &str) -> anyhow::Result<u64> {
        crate::db::memory_queries::delete_by_source(&self.db, source).await
    }

    /// Wipe all memory for an agent.
    pub async fn wipe_agent_memory(&self, agent_id: &str) -> anyhow::Result<u64> {
        crate::db::memory_queries::wipe_agent_memory(&self.db, agent_id).await
    }

    /// Insert a reindex task into the memory worker queue.
    pub async fn enqueue_reindex_task(&self, params: serde_json::Value) -> anyhow::Result<uuid::Uuid> {
        crate::db::memory_queries::enqueue_reindex_task(&self.db, params).await
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_fts_language_known() {
        assert_eq!(MemoryAdmin::detect_fts_language("ru"), "russian");
        assert_eq!(MemoryAdmin::detect_fts_language("en"), "english");
        assert_eq!(MemoryAdmin::detect_fts_language("es"), "spanish");
        assert_eq!(MemoryAdmin::detect_fts_language("de"), "german");
        assert_eq!(MemoryAdmin::detect_fts_language("fr"), "french");
        assert_eq!(MemoryAdmin::detect_fts_language("pt"), "portuguese");
        assert_eq!(MemoryAdmin::detect_fts_language("it"), "italian");
        assert_eq!(MemoryAdmin::detect_fts_language("nl"), "dutch");
        assert_eq!(MemoryAdmin::detect_fts_language("sv"), "swedish");
        assert_eq!(MemoryAdmin::detect_fts_language("no"), "norwegian");
        assert_eq!(MemoryAdmin::detect_fts_language("nb"), "norwegian");
        assert_eq!(MemoryAdmin::detect_fts_language("da"), "danish");
        assert_eq!(MemoryAdmin::detect_fts_language("fi"), "finnish");
        assert_eq!(MemoryAdmin::detect_fts_language("hu"), "hungarian");
        assert_eq!(MemoryAdmin::detect_fts_language("ro"), "romanian");
        assert_eq!(MemoryAdmin::detect_fts_language("tr"), "turkish");
    }

    #[test]
    fn detect_fts_language_unknown_fallback() {
        assert_eq!(MemoryAdmin::detect_fts_language("xx"), "simple");
        assert_eq!(MemoryAdmin::detect_fts_language(""), "simple");
    }

    #[test]
    fn validated_fts_rejects_injection() {
        assert!(MemoryAdmin::validated_fts_language("russian").is_ok());
        assert!(MemoryAdmin::validated_fts_language("english").is_ok());
        assert!(MemoryAdmin::validated_fts_language("simple").is_ok());
        // Must reject non-lowercase or suspicious input
        assert!(MemoryAdmin::validated_fts_language("Russian").is_err());
        assert!(MemoryAdmin::validated_fts_language("english; DROP TABLE").is_err());
        assert!(MemoryAdmin::validated_fts_language("").is_err());
    }
}
