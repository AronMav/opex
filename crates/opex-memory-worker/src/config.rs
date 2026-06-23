use serde::Deserialize;

/// REF-04 wake-mode for the memory worker main loop.
///
/// - `Listen` (default): primary wake via `PgListener` on `memory_tasks_new`;
///   `poll_interval_secs` becomes the catch-up safety-net tick.
/// - `Poll`: pure-polling mode — operator escape hatch (HCS-4 back-compat / debug).
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NotifyMode {
    #[default]
    Listen,
    Poll,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemoryWorkerConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_5")]
    pub poll_interval_secs: u64,
    #[serde(default)]
    pub notify_mode: NotifyMode,
}

impl Default for MemoryWorkerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            poll_interval_secs: 5,
            notify_mode: NotifyMode::Listen,
        }
    }
}

fn default_true() -> bool { true }
fn default_5() -> u64 { 5 }

#[derive(Debug, Deserialize)]
struct AppConfigPartial {
    #[serde(default)]
    pub memory_worker: MemoryWorkerConfig,
    pub database: DatabaseConfig,
    #[serde(default)]
    pub memory: MemoryConfig,
    /// Base URL for toolgate. Falls back to env `TOOLGATE_URL`, then localhost:9011.
    pub toolgate_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DatabaseConfig {
    pub url: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct MemoryConfig {
    pub workspace_dir: Option<String>,
}

pub struct WorkerConfig {
    pub worker: MemoryWorkerConfig,
    pub database_url: String,
    pub toolgate_url: String,
    pub workspace_dir: String,
    pub fts_language: String,
}

/// Detect FTS language from agent language code.
fn detect_fts_language(lang: &str) -> &'static str {
    match lang {
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
        _ => "simple",
    }
}

/// Read language from the base agent's TOML config.
///
/// The base agent (`base = true`) sets the deployment locale via its `[agent] language`
/// field (e.g. "ru", "en"). The memory worker reads this to select the correct
/// `PostgreSQL` FTS dictionary for `to_tsvector()`.
///
/// Scans all agent TOML files in config/agents/ and picks the first one with `base = true`.
fn read_base_agent_language(config_path: &str) -> String {
    let config_dir = std::path::Path::new(config_path)
        .parent()
        .unwrap_or(std::path::Path::new("config"));
    let agents_dir = config_dir.join("agents");

    #[derive(Deserialize, Default)]
    struct AgentSection {
        #[serde(default)]
        language: String,
        #[serde(default)]
        base: bool,
    }
    #[derive(Deserialize)]
    struct AgentPartial {
        #[serde(default)]
        agent: AgentSection,
    }

    if let Ok(entries) = std::fs::read_dir(&agents_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&path)
                && let Ok(a) = toml::from_str::<AgentPartial>(&text)
                    && a.agent.base && !a.agent.language.is_empty() {
                        return detect_fts_language(&a.agent.language).to_string();
                    }
        }
    }
    "simple".to_string()
}

pub fn load_config(path: &str) -> anyhow::Result<WorkerConfig> {
    let text = std::fs::read_to_string(path)?;
    let cfg: AppConfigPartial = toml::from_str(&text)?;
    let db_url = std::env::var("DATABASE_URL").unwrap_or(cfg.database.url);
    let fts_language = read_base_agent_language(path);
    let toolgate_url = cfg.toolgate_url
        .filter(|u| !u.is_empty())
        .or_else(|| std::env::var("TOOLGATE_URL").ok())
        .unwrap_or_else(|| "http://localhost:9011".to_string());

    Ok(WorkerConfig {
        worker: cfg.memory_worker,
        database_url: db_url,
        toolgate_url,
        workspace_dir: cfg.memory.workspace_dir.unwrap_or_else(|| "workspace".to_string()),
        fts_language,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_fts_language_russian() {
        assert_eq!(detect_fts_language("ru"), "russian");
    }

    #[test]
    fn test_detect_fts_language_english() {
        assert_eq!(detect_fts_language("en"), "english");
    }

    #[test]
    fn test_detect_fts_language_unknown() {
        assert_eq!(detect_fts_language("xx"), "simple");
    }

    #[test]
    fn test_default_config() {
        let cfg = MemoryWorkerConfig::default();
        assert!(cfg.enabled);
        assert_eq!(cfg.poll_interval_secs, 5);
        assert_eq!(cfg.notify_mode, NotifyMode::Listen);
    }

    #[test]
    fn test_notify_mode_default_is_listen() {
        // REF-04: LISTEN/NOTIFY is the primary wake signal by default.
        assert_eq!(NotifyMode::default(), NotifyMode::Listen);
    }

    #[test]
    fn test_notify_mode_deserialize_listen() {
        let cfg: MemoryWorkerConfig = toml::from_str(
            r#"
            enabled = true
            poll_interval_secs = 5
            notify_mode = "listen"
            "#,
        )
        .expect("parse listen config");
        assert_eq!(cfg.notify_mode, NotifyMode::Listen);
    }

    #[test]
    fn test_notify_mode_deserialize_poll() {
        let cfg: MemoryWorkerConfig = toml::from_str(
            r#"
            enabled = true
            poll_interval_secs = 5
            notify_mode = "poll"
            "#,
        )
        .expect("parse poll config");
        assert_eq!(cfg.notify_mode, NotifyMode::Poll);
    }

    #[test]
    fn test_poll_interval_secs_preserved() {
        // poll_interval_secs key MUST remain functional as the catch-up tick interval.
        let cfg: MemoryWorkerConfig = toml::from_str(
            r#"
            poll_interval_secs = 17
            "#,
        )
        .expect("parse poll interval override");
        assert_eq!(cfg.poll_interval_secs, 17);
    }
}
