//! PII redaction — strips personal identifiers before LLM calls.

use std::sync::LazyLock;
use regex::Regex;

static PHONE_RU: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:\+7|8)[\s\-]?\(?\d{3}\)?[\s\-]?\d{3}[\s\-]?\d{2}[\s\-]?\d{2}").unwrap()
});

static PHONE_INTL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\+\d{1,3}[\s\-]?\(?\d{2,4}\)?[\s\-]?\d{3,4}[\s\-]?\d{2,4}").unwrap()
});

static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[a-zA-Z0-9._%+\-]+@[a-zA-Z0-9.\-]+\.[a-zA-Z]{2,}").unwrap()
});

static CARD_NUMBER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b\d{4}[\s\-]?\d{4}[\s\-]?\d{4}[\s\-]?\d{4}\b").unwrap()
});

/// Matches common API key prefixes (sk-..., Bearer long-token).
static API_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:sk-[a-zA-Z0-9_-]{20,}|Bearer\s+[a-zA-Z0-9._-]{30,})").unwrap()
});

/// Matches long hex tokens (64+ hex chars, common for auth tokens).
static HEX_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b[0-9a-fA-F]{64,}\b").unwrap()
});

/// Redact PII from text. Returns (`redacted_text`, `count_of_redactions`).
pub fn redact(text: &str) -> (String, usize) {
    let mut result = text.to_string();
    let mut count = 0;

    for (pattern, replacement) in [
        (&*PHONE_RU, "[PHONE]"),
        (&*PHONE_INTL, "[PHONE]"),
        (&*EMAIL, "[EMAIL]"),
        (&*CARD_NUMBER, "[CARD]"),
        (&*API_KEY, "[API_KEY]"),
        (&*HEX_TOKEN, "[TOKEN]"),
    ] {
        let matches = pattern.find_iter(&result).count();
        if matches > 0 {
            count += matches;
            result = pattern.replace_all(&result, replacement).into_owned();
        }
    }

    (result, count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_phone_redaction() {
        let (r, c) = redact("Позвони мне +7 (999) 123-45-67 завтра");
        assert!(r.contains("[PHONE]"));
        assert!(!r.contains("999"));
        assert!(c > 0);
    }

    #[test]
    fn test_email_redaction() {
        let (r, _) = redact("Напиши на user@example.com");
        assert!(r.contains("[EMAIL]"));
        assert!(!r.contains("user@example.com"));
    }

    #[test]
    fn test_card_redaction() {
        let (r, _) = redact("Карта 4276 3801 1234 5678");
        assert!(r.contains("[CARD]"));
    }

    #[test]
    fn test_no_false_positives() {
        let (r, c) = redact("Привет, как дела?");
        assert_eq!(r, "Привет, как дела?");
        assert_eq!(c, 0);
    }

    #[test]
    fn test_multiple_phones_count() {
        let (_, c) = redact("Звони +7 999 111-22-33 или +7 888 444-55-66");
        assert_eq!(c, 2);
    }

    #[test]
    fn test_api_key_redaction() {
        let (r, c) = redact("key: sk-proj-abc123xyz789defghijklmnop");
        assert!(r.contains("[API_KEY]"), "got: {}", r);
        assert!(!r.contains("sk-proj-abc123"));
        assert!(c > 0);
    }

    #[test]
    fn test_long_hex_token_redaction() {
        let (r, _) = redact("token=13c43c3f3db4413003f0da16013764d06de812f4066c210f59a99f610b9c665e");
        assert!(r.contains("[TOKEN]"), "got: {}", r);
    }

    #[test]
    fn test_bearer_redaction() {
        let (r, _) = redact("Authorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jV");
        assert!(r.contains("[API_KEY]"), "got: {}", r);
    }

    #[test]
    fn test_short_strings_not_redacted() {
        let (r, c) = redact("model=gpt-4 temperature=0.7");
        assert_eq!(r, "model=gpt-4 temperature=0.7");
        assert_eq!(c, 0);
    }
}
