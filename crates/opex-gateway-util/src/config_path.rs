//! Dual-path конфиг: предпочитает config/opex.toml, fallback config/hydeclaw.toml.
//! Fallback удаляется в PR3 после переименования файла на сервере.
use std::path::Path;

/// Резолвит путь конфига относительно текущей рабочей директории.
pub fn resolve_config_path() -> String {
    resolve_config_path_in(Path::new("."))
}

/// Тестируемое ядро: резолвит относительно `base`.
pub fn resolve_config_path_in(base: &Path) -> String {
    if base.join("config/opex.toml").exists() {
        "config/opex.toml".to_string()
    } else {
        "config/hydeclaw.toml".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn falls_back_when_new_missing() {
        // chooses_existing: при наличии только legacy выбирается legacy
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("config/hydeclaw.toml");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, "x").unwrap();
        assert_eq!(
            resolve_config_path_in(dir.path()),
            "config/hydeclaw.toml"
        );
        std::fs::write(dir.path().join("config/opex.toml"), "y").unwrap();
        assert_eq!(resolve_config_path_in(dir.path()), "config/opex.toml");
    }
}
