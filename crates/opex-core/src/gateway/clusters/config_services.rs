use std::sync::Arc;
#[cfg(test)]
use tokio::sync::RwLock;
use crate::config::{AppConfig, ConfigApiWriteFlag, SharedConfig};

// ── ConfigServices cluster ─────────────────────────────────────────────────

/// Cluster holding the application configuration and related synchronization
/// primitives (hot-reload wrapper, write lock, API write flag).
#[derive(Clone)]
pub struct ConfigServices {
    /// The loaded application configuration (snapshot at startup or last reload).
    pub config: AppConfig,
    /// Shared, hot-reloadable config handle (`Arc<RwLock<AppConfig>>`).
    pub shared_config: SharedConfig,
    /// Serialises concurrent config-file writes from the API.
    pub config_write_lock: Arc<tokio::sync::Mutex<()>>,
    /// Set to `true` by the API writer so the file-watcher skips one reload cycle.
    pub config_api_write_flag: ConfigApiWriteFlag,
}

impl ConfigServices {
    pub fn new(
        config: AppConfig,
        shared_config: SharedConfig,
        config_write_lock: Arc<tokio::sync::Mutex<()>>,
        config_api_write_flag: ConfigApiWriteFlag,
    ) -> Self {
        Self {
            config,
            shared_config,
            config_write_lock,
            config_api_write_flag,
        }
    }

    /// Construct a minimal `ConfigServices` for unit tests.
    /// Uses an in-memory `AppConfig` parsed from the smallest valid TOML snippet.
    #[cfg(test)]
    pub fn test_new() -> Self {
        let config: AppConfig = toml::from_str(
            r#"
            [gateway]
            listen = "0.0.0.0:18789"
            [database]
            url = "postgres://localhost/test"
            "#,
        )
        .expect("minimal AppConfig should parse");

        let shared_config: SharedConfig = Arc::new(RwLock::new(config.clone()));
        let config_write_lock = Arc::new(tokio::sync::Mutex::new(()));
        let config_api_write_flag: ConfigApiWriteFlag =
            Arc::new(std::sync::atomic::AtomicBool::new(false));

        Self {
            config,
            shared_config,
            config_write_lock,
            config_api_write_flag,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_services_write_lock_is_shared_on_clone() {
        let cs = ConfigServices::test_new();
        let cs2 = cs.clone();
        assert!(Arc::ptr_eq(&cs.config_write_lock, &cs2.config_write_lock));
    }
}
