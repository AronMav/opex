//! Config path resolution. The app config lives at `config/opex.toml`.
use std::path::Path;

/// Path to the app config file (relative to the working directory).
pub fn resolve_config_path() -> String {
    "config/opex.toml".to_string()
}

/// Test/compat seam — resolves relative to `base`.
pub fn resolve_config_path_in(base: &Path) -> String {
    base.join("config/opex.toml").to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_opex_config_path() {
        assert_eq!(resolve_config_path(), "config/opex.toml");
        // _in is a compat seam; just assert it embeds the fixed config name.
        assert!(resolve_config_path_in(Path::new("base")).ends_with("opex.toml"));
    }
}
