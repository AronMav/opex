//! Config path resolution. The app config lives at `config/opex.toml`.

/// Path to the app config file (relative to the working directory).
pub fn resolve_config_path() -> String {
    "config/opex.toml".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_opex_config_path() {
        assert_eq!(resolve_config_path(), "config/opex.toml");
    }
}
