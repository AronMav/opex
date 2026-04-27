//! Memory FTS-language utility functions.
//!
//! Pure helpers used at startup (`main.rs`) to map agent language codes to
//! PostgreSQL FTS configuration names, and to validate language names before
//! interpolating into SQL. Were previously wrapped in a `MemoryAdmin` struct
//! that was never instantiated; collapsed to free `pub fn` for clarity.

/// Auto-detect FTS language from agent language code (e.g. "ru" -> "russian").
/// Falls back to "simple" for unsupported languages.
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

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_fts_language_known() {
        assert_eq!(detect_fts_language("ru"), "russian");
        assert_eq!(detect_fts_language("en"), "english");
        assert_eq!(detect_fts_language("es"), "spanish");
        assert_eq!(detect_fts_language("de"), "german");
        assert_eq!(detect_fts_language("fr"), "french");
        assert_eq!(detect_fts_language("pt"), "portuguese");
        assert_eq!(detect_fts_language("it"), "italian");
        assert_eq!(detect_fts_language("nl"), "dutch");
        assert_eq!(detect_fts_language("sv"), "swedish");
        assert_eq!(detect_fts_language("no"), "norwegian");
        assert_eq!(detect_fts_language("nb"), "norwegian");
        assert_eq!(detect_fts_language("da"), "danish");
        assert_eq!(detect_fts_language("fi"), "finnish");
        assert_eq!(detect_fts_language("hu"), "hungarian");
        assert_eq!(detect_fts_language("ro"), "romanian");
        assert_eq!(detect_fts_language("tr"), "turkish");
    }

    #[test]
    fn detect_fts_language_unknown_fallback() {
        assert_eq!(detect_fts_language("xx"), "simple");
        assert_eq!(detect_fts_language(""), "simple");
    }

    #[test]
    fn validated_fts_rejects_injection() {
        assert!(validated_fts_language("russian").is_ok());
        assert!(validated_fts_language("english").is_ok());
        assert!(validated_fts_language("simple").is_ok());
        // Must reject non-lowercase or suspicious input
        assert!(validated_fts_language("Russian").is_err());
        assert!(validated_fts_language("english; DROP TABLE").is_err());
        assert!(validated_fts_language("").is_err());
    }
}
