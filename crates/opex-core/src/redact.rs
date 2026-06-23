// src/redact.rs
//! Shared secret-redaction helpers.
//!
//! `redact_secrets` (and its supporting machinery) was MOVED here from
//! `tools::yaml_tools` so the OAuth subtree can use it without a circular
//! dependency on the tools layer.  Two new helpers are added: `redact_oauth_str`
//! (OAuth-specific keywords) and `redact_token_in_url` (URL query-param
//! sanitization).  The `yaml_tools` callers now call `crate::redact::redact_secrets`.

// ── Shared constant ───────────────────────────────────────────────────────────

/// Maximum characters from an HTTP error response body to include in error
/// messages.  Limits leakage while still providing enough context to diagnose
/// the failure.
///
/// MOVED here from `tools::yaml_tools` alongside `redact_secrets`; yaml_tools
/// re-imports with `use crate::redact::ERROR_BODY_MAX_CHARS`.
pub(crate) const ERROR_BODY_MAX_CHARS: usize = 200;

// ── Core redaction machinery ──────────────────────────────────────────────────

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')
}

fn is_token_char_or_separator(c: char) -> bool {
    // Skip separators (=, :, ", space) before the actual value
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '=' | ':' | '"' | ' ')
}

/// Replace the value portion following `keyword` (case-insensitive) with `[REDACTED]`.
/// The value is the contiguous run of characters satisfying `is_value` that follows
/// the keyword and any optional leading non-alphanumeric separator chars.
fn redact_pattern_after_keyword(
    input: &str,
    keyword: &str,
    is_value: fn(char) -> bool,
) -> String {
    let lower = input.to_lowercase();
    let mut result = String::with_capacity(input.len());
    let mut pos = 0usize;

    while pos < input.len() {
        if let Some(rel) = lower[pos..].find(keyword) {
            let kw_start = pos + rel;
            let kw_end = kw_start + keyword.len();
            result.push_str(&input[pos..kw_end]);

            // Skip separators (=, :, ", space) between keyword and value
            let rest = &input[kw_end..];
            let skip = rest.chars().take_while(|&c| !c.is_ascii_alphanumeric()).count();
            let value_start = kw_end + skip;

            // Find end of value (run of token chars)
            let value_end = value_start
                + input[value_start..]
                    .chars()
                    .take_while(|&c| is_value(c) && c.is_ascii_alphanumeric())
                    .map(|c| c.len_utf8())
                    .sum::<usize>();

            if value_end > value_start {
                // push separators then redacted value
                result.push_str(&input[kw_end..value_start]);
                result.push_str("[REDACTED]");
                pos = value_end;
            } else {
                // Nothing to redact — advance past keyword
                pos = kw_end;
            }
        } else {
            result.push_str(&input[pos..]);
            break;
        }
    }
    result
}

// ── Public helpers ────────────────────────────────────────────────────────────

/// Redact common secret patterns from a string before it is included in error
/// messages or audit logs.  The redacted string is also truncated to
/// [`ERROR_BODY_MAX_CHARS`] so that large response bodies don't bloat logs.
///
/// Patterns redacted (case-insensitive):
/// - `Bearer <token>`
/// - `api_key=<value>` / `api-key=<value>` / `api_key: <value>` etc.
/// - `token=<value>` / `token: <value>` etc.
///
/// MOVED from `tools::yaml_tools::redact_secrets` (D7). Callers in
/// `yaml_tools` updated to `crate::redact::redact_secrets`.
pub(crate) fn redact_secrets(body: &str) -> String {
    // Truncate first (cheaper than running regex on a multi-MB string).
    let truncated = if body.len() > ERROR_BODY_MAX_CHARS {
        &body[..ERROR_BODY_MAX_CHARS]
    } else {
        body
    };

    // Simple state-machine redaction — avoids pulling in the `regex` crate
    // for this hot-path helper (regex already compiled elsewhere but we keep
    // this dependency-free for portability).
    let mut result = truncated.to_string();

    // Redact Bearer tokens: "Bearer <token>"
    result = redact_pattern_after_keyword(&result, "bearer ", is_token_char);
    // Redact api_key / api-key variants: keyword then optional [ =:"] then value
    result = redact_pattern_after_keyword(&result, "api_key", is_token_char_or_separator);
    result = redact_pattern_after_keyword(&result, "api-key", is_token_char_or_separator);
    // Redact token variants
    result = redact_pattern_after_keyword(&result, "token", is_token_char_or_separator);

    result
}

/// Replace OAuth-specific token values in `s` with `[REDACTED]` before logging.
///
/// Covers (case-insensitive):
/// - `Bearer <token>`
/// - `access_token=<val>`
/// - `refresh_token=<val>`
/// - `client_secret=<val>`
///
/// Used by the `gemini-cloudcode` OAuth subtree (Task 1+).
#[allow(dead_code)]
pub(crate) fn redact_oauth_str(s: &str) -> String {
    let mut out = s.to_string();
    out = redact_pattern_after_keyword(&out, "bearer ", is_token_char);
    out = redact_pattern_after_keyword(&out, "access_token", is_token_char_or_separator);
    out = redact_pattern_after_keyword(&out, "refresh_token", is_token_char_or_separator);
    out = redact_pattern_after_keyword(&out, "client_secret", is_token_char_or_separator);
    out
}

/// Redact token-bearing query parameters from a URL string.
///
/// Applies `redact_oauth_str` to the query portion only so the host/path are
/// not mangled.  Non-secret params are preserved unchanged.
///
/// Used by the `gemini-cloudcode` OAuth subtree (Task 1+).
#[allow(dead_code)]
pub(crate) fn redact_token_in_url(url: &str) -> String {
    if let Some(q_start) = url.find('?') {
        let (base, query) = url.split_at(q_start);
        format!("{}{}", base, redact_oauth_str(query))
    } else {
        url.to_string()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── redact_oauth_str ─────────────────────────────────────────────────────

    #[test]
    fn redact_bearer_token() {
        let s = "Authorization: Bearer ya29abc123";
        let out = redact_oauth_str(s);
        assert!(!out.contains("ya29abc123"), "token must be redacted: {out}");
        assert!(out.contains("[REDACTED]"), "must have [REDACTED]: {out}");
    }

    #[test]
    fn redact_refresh_token_in_body() {
        let s = "refresh_token=1xyzABCDEF&grant_type=refresh_token";
        let out = redact_oauth_str(s);
        assert!(!out.contains("1xyzABCDEF"), "refresh token must be redacted: {out}");
    }

    #[test]
    fn plain_text_unchanged() {
        let s = "error: bad_request";
        assert_eq!(redact_oauth_str(s), s);
    }

    #[test]
    fn redact_access_token() {
        let s = "access_token=mytoken123&scope=openid";
        let out = redact_oauth_str(s);
        assert!(!out.contains("mytoken123"), "access_token value must be redacted: {out}");
        assert!(out.contains("[REDACTED]"), "must have [REDACTED]: {out}");
    }

    #[test]
    fn redact_client_secret() {
        let s = "client_secret=supersecret&client_id=abc";
        let out = redact_oauth_str(s);
        assert!(!out.contains("supersecret"), "client_secret value must be redacted: {out}");
    }

    // ── redact_token_in_url ──────────────────────────────────────────────────

    #[test]
    fn redact_token_in_url_strips_access_token_param() {
        let url = "https://example.com/path?access_token=ya29abc&other=val";
        let out = redact_token_in_url(url);
        assert!(!out.contains("ya29abc"), "token in URL must be redacted: {out}");
        assert!(out.contains("other=val"), "non-secret params must be preserved: {out}");
    }

    #[test]
    fn redact_token_in_url_no_query_unchanged() {
        let url = "https://example.com/path";
        assert_eq!(redact_token_in_url(url), url);
    }

    // ── redact_secrets (MOVED from yaml_tools) ───────────────────────────────

    #[test]
    fn redact_secrets_bearer_token_is_redacted() {
        let input = r#"{"error":"invalid request","Authorization":"Bearer eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9"}"#;
        let out = redact_secrets(input);
        assert!(!out.contains("eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9"), "raw JWT must not appear: {out}");
        assert!(out.contains("[REDACTED]"), "must contain [REDACTED]: {out}");
    }

    #[test]
    fn redact_secrets_plain_text_untouched() {
        let input = "error: resource not found, id=12345";
        let out = redact_secrets(input);
        assert_eq!(out, input, "plain error text must not be modified");
    }

    #[test]
    fn redact_secrets_truncates_long_body() {
        let long = "x".repeat(ERROR_BODY_MAX_CHARS + 100);
        let out = redact_secrets(&long);
        assert_eq!(out.len(), ERROR_BODY_MAX_CHARS, "output must be truncated to {ERROR_BODY_MAX_CHARS} chars");
    }

    #[test]
    fn redact_secrets_short_body_not_truncated() {
        let input = "short error";
        let out = redact_secrets(input);
        assert_eq!(out, input, "short body must not be truncated or modified");
    }

    #[test]
    fn redact_secrets_api_key_pattern_redacted() {
        let input = "invalid api_key abcdef123456 provided";
        let out = redact_secrets(input);
        assert!(!out.contains("abcdef123456"), "api_key value must be redacted: {out}");
        assert!(out.contains("[REDACTED]"), "must contain [REDACTED]: {out}");
    }

    #[test]
    fn redact_secrets_redacts_known_keywords() {
        let s = "api_key=secret123&other=val";
        let out = redact_secrets(s);
        assert!(!out.contains("secret123"), "api_key value must be redacted: {out}");
    }
}
