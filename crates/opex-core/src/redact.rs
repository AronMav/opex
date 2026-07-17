// src/redact.rs
//! Shared secret-redaction helpers.
//!
//! `redact_secrets` (and its supporting machinery) was MOVED here from
//! `tools::yaml_tools` so the OAuth subtree can use it without a circular
//! dependency on the tools layer.  Two new helpers are added: `redact_oauth_str`
//! (OAuth-specific keywords) and `redact_token_in_url` (URL query-param
//! sanitization).  The `yaml_tools` callers now call `crate::redact::redact_secrets`.
//!
//! `redact_url_userinfo` and `redact_terminal_output` (T02 triage, Пункты 3+5)
//! close two additional gaps: bare tokens in URL userinfo
//! (`scheme://TOKEN@host`, no `?query` involved) and unredacted subprocess /
//! container-exec stdout (which can trivially contain an `env`/`printenv`
//! dump of the process environment).

use std::sync::LazyLock;
use regex::Regex;

/// Matches `scheme://userinfo@host` where `userinfo` is 8+ chars of anything
/// but whitespace/`@`/`/`. Covers both bare-token (`https://ghp_xxx@host`)
/// and `user:pass@host` forms. The 8-char floor keeps short conventional
/// forms like `git@github.com` (no `scheme://`, so unmatched anyway) and
/// `admin@host` intact — see regression test.
///
/// The scheme is matched generically (`[a-z][a-z0-9+.-]*://`, RFC 3986 scheme
/// grammar) rather than an explicit allowlist — credential-bearing userinfo
/// isn't scheme-specific (`postgres://`, `mysql://`, `redis://`, `mongodb://`,
/// `amqp://` connection strings are exactly as sensitive as `https://`/`git://`
/// and OPEX's own `DATABASE_URL` is a `postgres://user:pass@host` string).
static URL_USERINFO: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b([a-z][a-z0-9+.-]*://)([^\s@/]{8,})(@)").unwrap()
});

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

/// Case-insensitive (ASCII) substring search returning a byte offset that is
/// always a valid char boundary in `haystack`, starting from `from`.
///
/// Unlike `haystack.to_lowercase().find(needle)`, it never threads an offset
/// from a case-folded copy back into the original: lowercasing can change byte
/// length (e.g. `İ`→`i̇`, `ﬀ`→`ff`), so a `lower` offset can land mid-codepoint
/// in the original and panic when sliced. `needle` is expected to be ASCII (all
/// redaction keywords and `@mention` prefixes are); non-ASCII bytes are matched
/// exactly.
pub(crate) fn ascii_ci_find(haystack: &str, needle: &str, from: usize) -> Option<usize> {
    let hb = haystack.as_bytes();
    let nb = needle.as_bytes();
    if nb.is_empty() {
        return Some(from);
    }
    let mut i = from;
    while i + nb.len() <= hb.len() {
        if haystack.is_char_boundary(i)
            && hb[i..i + nb.len()]
                .iter()
                .zip(nb)
                .all(|(h, n)| h.eq_ignore_ascii_case(n))
        {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Replace the value portion following `keyword` (case-insensitive) with `[REDACTED]`.
/// The value is the contiguous run of characters satisfying `is_value` that follows
/// the keyword and any optional leading non-alphanumeric separator chars.
///
/// All offsets index `input` directly (via [`ascii_ci_find`]) and separators are
/// measured with `len_utf8`, so multi-byte bodies never split a codepoint.
// reviewed: offsets from ascii_ci_find()/len_utf8 sums — char boundaries
#[allow(clippy::string_slice)]
fn redact_pattern_after_keyword(
    input: &str,
    keyword: &str,
    is_value: fn(char) -> bool,
) -> String {
    let mut result = String::with_capacity(input.len());
    let mut pos = 0usize; // byte offset in `input`, always a char boundary

    while pos < input.len() {
        let Some(kw_start) = ascii_ci_find(input, keyword, pos) else {
            result.push_str(&input[pos..]);
            break;
        };
        let kw_end = kw_start + keyword.len(); // keyword is ASCII → char boundary
        result.push_str(&input[pos..kw_end]);

        // Skip separators (=, :, ", space) between keyword and value. Sum
        // len_utf8 (not char count) so the offset stays byte-correct.
        let rest = &input[kw_end..];
        let skip: usize = rest
            .chars()
            .take_while(|&c| !c.is_ascii_alphanumeric())
            .map(|c| c.len_utf8())
            .sum();
        let value_start = kw_end + skip;

        // Find end of value (run of token chars).
        let value_end = value_start
            + input[value_start..]
                .chars()
                .take_while(|&c| is_value(c) && c.is_ascii_alphanumeric())
                .map(|c| c.len_utf8())
                .sum::<usize>();

        if value_end > value_start {
            result.push_str(&input[kw_end..value_start]);
            result.push_str("[REDACTED]");
            pos = value_end;
        } else {
            pos = kw_end; // nothing to redact — advance past keyword
        }
    }
    result
}

// ── Public helpers ────────────────────────────────────────────────────────────

/// Apply the keyword-based secret patterns (Bearer / api_key / api-key /
/// token) to `body` with no truncation. Shared by `redact_secrets` (which
/// truncates first, for error-body use) and `redact_terminal_output` (which
/// must NOT truncate — log tails can legitimately be many KB).
///
/// Simple state-machine redaction — avoids pulling in the `regex` crate for
/// this hot-path helper (regex already compiled elsewhere but we keep this
/// dependency-free for portability).
fn redact_known_keyword_patterns(body: &str) -> String {
    let mut result = body.to_string();

    // Redact Bearer tokens: "Bearer <token>"
    result = redact_pattern_after_keyword(&result, "bearer ", is_token_char);
    // Redact api_key / api-key variants: keyword then optional [ =:"] then value
    result = redact_pattern_after_keyword(&result, "api_key", is_token_char_or_separator);
    result = redact_pattern_after_keyword(&result, "api-key", is_token_char_or_separator);
    // Redact token variants
    result = redact_pattern_after_keyword(&result, "token", is_token_char_or_separator);

    result
}

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
// reviewed: floor_char_boundary-bounded truncation — char boundary
#[allow(clippy::string_slice)]
pub(crate) fn redact_secrets(body: &str) -> String {
    // Truncate first (cheaper than running regex on a multi-MB string).
    // floor to a char boundary so a multi-byte body can't split a codepoint.
    let truncated = if body.len() > ERROR_BODY_MAX_CHARS {
        &body[..body.floor_char_boundary(ERROR_BODY_MAX_CHARS)]
    } else {
        body
    };

    redact_known_keyword_patterns(truncated)
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

/// Redact bare tokens / credentials living in a URL's userinfo component
/// (`scheme://TOKEN@host/...` or `scheme://user:pass@host/...`).
///
/// This is a distinct gap from `redact_token_in_url`: query-string redaction
/// only looks after `?`, but a userinfo token appears *before* the host and
/// carries no recognizable keyword (`token=`, `api_key=`, …), so the
/// keyword-based `redact_secrets`/`redact_oauth_str` never catches it.
///
/// 8+ char floor on the userinfo run avoids mangling short conventional
/// forms (`git@github.com` has no `scheme://` prefix so it's unaffected;
/// `admin@host` is under the floor and left untouched by design).
///
/// T02 triage Пункт 3 (hermes_ref 3483424aa).
pub(crate) fn redact_url_userinfo(s: &str) -> String {
    URL_USERINFO
        .replace_all(s, |caps: &regex::Captures| {
            format!("{}[REDACTED]{}", &caps[1], &caps[3])
        })
        .into_owned()
}

/// Redact token-bearing query parameters from a URL string.
///
/// Applies `redact_oauth_str` to the query portion only so the host/path are
/// not mangled.  Non-secret params are preserved unchanged.  Also runs
/// `redact_url_userinfo` over the whole string so a bare token/credential
/// pair in the userinfo component (before the host, before any `?query`) is
/// covered by the same entry point.
///
/// Used by the `gemini-cloudcode` OAuth subtree (Task 1+).
#[allow(dead_code)]
pub(crate) fn redact_token_in_url(url: &str) -> String {
    let with_userinfo_redacted = redact_url_userinfo(url);
    if let Some(q_start) = with_userinfo_redacted.find('?') {
        let (base, query) = with_userinfo_redacted.split_at(q_start);
        format!("{}{}", base, redact_oauth_str(query))
    } else {
        with_userinfo_redacted
    }
}

/// Redact secrets from subprocess/container-exec terminal output before it
/// reaches the model or an API response.
///
/// Background host processes (`process` tool, action=start) and container `exec` run
/// arbitrary commands (including `env`/`printenv`) whose stdout was
/// previously returned to the model verbatim. Rather than trying to detect
/// "is this an env-dump command" (fragile — command chaining, aliases,
/// wrapper scripts all evade a command-name check), this unconditionally
/// runs the same keyword-based patterns as `redact_secrets` plus
/// `redact_url_userinfo` over the *entire* output. Safer default: redact
/// always, not just when a command looks suspicious.
///
/// Deliberately does NOT go through `redact_secrets` itself: that helper
/// truncates to [`ERROR_BODY_MAX_CHARS`] (200 chars), which is correct for
/// HTTP error bodies but would silently drop most of a multi-line log tail
/// here. Terminal output keeps its own length; only its secret substrings
/// are masked.
///
/// T02 triage Пункт 5 (hermes_ref c1c179a23, severity P0).
pub(crate) fn redact_terminal_output(output: &str) -> String {
    redact_url_userinfo(&redact_known_keyword_patterns(output))
}

/// Strip invisible/zero-width Unicode codepoints commonly used to obfuscate
/// prompt-injection payloads or smuggle hidden instructions past
/// substring-based scanners (T10 побочный пункт B, hermes-parity: align cron
/// invisible-unicode filtering with the install-time scanner).
///
/// OPEX currently has no install-time invisible-unicode scanner to "align
/// with" (unlike hermes, which fixed a drift between two duplicate filters —
/// see T10 triage doc) — this is a NEW, single canonical filter, applied here
/// to cron `task_message` at write-time (`agent/pipeline/cron.rs`) as the
/// first backstop of its kind in this codebase.
///
/// Removes:
/// * Zero-width space/joiner/non-joiner (U+200B–U+200D), word joiner
///   (U+2060), BOM/zero-width-no-break-space (U+FEFF).
/// * Bidi control characters (U+202A–U+202E LRE/RLE/PDF/LRO/RLO,
///   U+2066–U+2069 LRI/RLI/FSI/PDI) — can visually reorder text to hide
///   malicious content.
/// * Other default-ignorable formatting controls in the same block
///   (U+200E/U+200F LRM/RLM, U+061C ALM).
pub(crate) fn strip_invisible_unicode(input: &str) -> String {
    input
        .chars()
        .filter(|c| {
            !matches!(
                *c,
                '\u{200B}'..='\u{200F}' // ZWSP, ZWNJ, ZWJ, LRM, RLM
                | '\u{202A}'..='\u{202E}' // LRE, RLE, PDF, LRO, RLO
                | '\u{2060}'..='\u{2064}' // word joiner, invisible +/=/×, invisible plus separator
                | '\u{2066}'..='\u{2069}' // LRI, RLI, FSI, PDI
                | '\u{FEFF}' // BOM / zero-width no-break space
                | '\u{061C}' // Arabic letter mark
            )
        })
        .collect()
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

    // ── redact_url_userinfo ──────────────────────────────────────────────────

    #[test]
    fn redact_url_userinfo_bare_token() {
        let url = "https://ghp_aaaaaaaaaaaaaaaaaaaa@github.com/o/r.git";
        let out = redact_url_userinfo(url);
        assert!(!out.contains("ghp_aaaaaaaaaaaaaaaaaaaa"), "token must be redacted: {out}");
        assert!(out.contains("@github.com/o/r.git"), "host/path preserved: {out}");
    }

    #[test]
    fn redact_url_userinfo_user_pass() {
        let url = "postgres://user:secretpass@db:5432/x";
        let out = redact_url_userinfo(url);
        assert!(!out.contains("secretpass"), "password must be redacted: {out}");
        assert!(!out.contains("user:secretpass"), "userinfo must be fully redacted: {out}");
        assert!(out.contains("@db:5432/x"), "host/port/path preserved: {out}");
    }

    #[test]
    fn redact_url_userinfo_short_git_shorthand_untouched() {
        // No `scheme://` prefix (SSH shorthand) — must not be mangled.
        let url = "git@github.com:o/r.git";
        assert_eq!(redact_url_userinfo(url), url, "short git@ shorthand must be left untouched");
    }

    #[test]
    fn redact_url_userinfo_via_redact_token_in_url() {
        // redact_token_in_url is the existing public entry point used by the
        // OAuth subtree; it must now also cover userinfo, not just query.
        let url = "https://ghp_aaaaaaaaaaaaaaaaaaaa@github.com/o/r.git?foo=bar";
        let out = redact_token_in_url(url);
        assert!(!out.contains("ghp_aaaaaaaaaaaaaaaaaaaa"), "token must be redacted: {out}");
        assert!(out.contains("@github.com/o/r.git"), "host/path preserved: {out}");
    }

    // ── redact_terminal_output ───────────────────────────────────────────────

    #[test]
    fn redact_terminal_output_masks_env_dump_and_bearer() {
        let output = "MY_TOKEN=abc123opaquevalue\nAuthorization: bearer sk-xxxxxxxxxxxxxxxxxxxxxxxx\nHOME=/home/u";
        let out = redact_terminal_output(output);
        assert!(!out.contains("abc123opaquevalue"), "env token value must be redacted: {out}");
        assert!(!out.contains("sk-xxxxxxxxxxxxxxxxxxxxxxxx"), "bearer token must be redacted: {out}");
        assert!(out.contains("HOME=/home/u"), "benign env line must be preserved: {out}");
    }

    #[test]
    fn redact_terminal_output_does_not_truncate_long_log_tails() {
        // Regression: redact_secrets truncates to ERROR_BODY_MAX_CHARS (200),
        // which is wrong for a multi-line log tail. redact_terminal_output
        // must preserve full length.
        let long = "line of benign log output\n".repeat(20);
        assert!(long.len() > ERROR_BODY_MAX_CHARS);
        let out = redact_terminal_output(&long);
        assert_eq!(out.len(), long.len(), "terminal output must not be truncated: {out}");
    }

    #[test]
    fn redact_terminal_output_redacts_userinfo_url() {
        let output = "cloning from https://ghp_aaaaaaaaaaaaaaaaaaaa@github.com/o/r.git";
        let out = redact_terminal_output(output);
        assert!(!out.contains("ghp_aaaaaaaaaaaaaaaaaaaa"), "URL userinfo token must be redacted: {out}");
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

    #[test]
    fn redact_secrets_multibyte_body_truncates_without_panic() {
        // Byte 200 lands mid-'я' (1 ASCII byte shifts the 2-byte run to odd
        // offsets); the old `&body[..200]` panicked here.
        let long = format!("a{}", "я".repeat(300)); // 601 bytes
        let out = redact_secrets(&long); // must not panic
        assert!(out.chars().count() <= ERROR_BODY_MAX_CHARS);
    }

    #[test]
    fn redact_secrets_multibyte_prefix_redacts_correctly() {
        // 'İ' lowercases to a 3-byte "i̇", so the old to_lowercase()-offset
        // approach diverged from the original's byte layout before the keyword.
        let input = "İ api_key=secret123";
        let out = redact_secrets(input);
        assert!(!out.contains("secret123"), "value must be redacted: {out}");
        assert!(out.starts_with("İ api_key"), "prefix preserved: {out}");
    }

    #[test]
    fn ascii_ci_find_matches_case_insensitively_at_boundaries() {
        assert_eq!(super::ascii_ci_find("xxTOKENyy", "token", 0), Some(2));
        // 'Ы' is 2 bytes; the keyword after it is found at the correct offset.
        assert_eq!(super::ascii_ci_find("Ыtoken", "token", 0), Some(2));
        assert_eq!(super::ascii_ci_find("no match", "token", 0), None);
    }

    // ── strip_invisible_unicode (T10 побочный пункт B) ──────────────────────

    #[test]
    fn strip_invisible_unicode_removes_zero_width_chars() {
        let input = "ig\u{200B}nore\u{200C} previous\u{200D} instructions";
        let out = strip_invisible_unicode(input);
        assert_eq!(out, "ignore previous instructions");
    }

    #[test]
    fn strip_invisible_unicode_removes_bidi_controls() {
        let input = "safe\u{202E}\u{202D}text";
        let out = strip_invisible_unicode(input);
        assert_eq!(out, "safetext");
    }

    #[test]
    fn strip_invisible_unicode_removes_bom_and_word_joiner() {
        let input = "\u{FEFF}hello\u{2060}world";
        let out = strip_invisible_unicode(input);
        assert_eq!(out, "helloworld");
    }

    #[test]
    fn strip_invisible_unicode_preserves_normal_text() {
        let input = "Run daily backup at 3am — check /var/log for errors.";
        assert_eq!(strip_invisible_unicode(input), input);
    }

    #[test]
    fn strip_invisible_unicode_preserves_cyrillic() {
        let input = "Проверь почту каждый день в 9 утра";
        assert_eq!(strip_invisible_unicode(input), input);
    }

    // ── redact_terminal_output on cron-shaped output (T10 побочный пункт A) ─

    #[test]
    fn redact_terminal_output_redacts_leaked_secret_in_cron_reply() {
        // Simulates a cron task whose agent invoked a tool that echoed back
        // a secret in its tool-result text, which then landed in the final
        // assistant reply.
        let reply = "Job complete. Debug info: api_key=sk_live_abcdef123456 was used.";
        let out = redact_terminal_output(reply);
        assert!(!out.contains("sk_live_abcdef123456"), "secret must be redacted: {out}");
        assert!(out.contains("Job complete"), "benign text must survive: {out}");
    }
}
