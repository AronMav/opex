//! Operator-configurable domain blocklist for agent-initiated web fetches.

/// Case-insensitive host match. `*.evil.tld` matches `evil.tld` and any subdomain;
/// otherwise an exact host match.
pub fn host_blocked(host: &str, globs: &[String]) -> bool {
    // Strip a trailing root dot: `ads.tracker.com.` resolves identically to
    // `ads.tracker.com` in DNS but would otherwise slip past both the exact and
    // `*.`-glob matches (F110). Mirrors net/ssrf.rs's metadata-blocklist guard.
    let host = host.trim().trim_end_matches('.').to_lowercase();
    if host.is_empty() {
        return false;
    }
    globs.iter().any(|g| {
        let g = g.trim().to_lowercase();
        if let Some(suffix) = g.strip_prefix("*.") {
            host == suffix || host.ends_with(&format!(".{suffix}"))
        } else {
            host == g
        }
    })
}

/// Parse the host from a URL and test it against the blocklist. Unparseable → false.
pub fn url_blocked(url: &str, globs: &[String]) -> bool {
    match url::Url::parse(url) {
        Ok(u) => u.host_str().map(|h| host_blocked(h, globs)).unwrap_or(false),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn globs() -> Vec<String> {
        vec!["*.evil.tld".into(), "ads.example.com".into()]
    }

    #[test]
    fn glob_matches_sub_and_apex() {
        assert!(host_blocked("a.evil.tld", &globs()));
        assert!(host_blocked("evil.tld", &globs()));
        assert!(host_blocked("ADS.EXAMPLE.COM", &globs()));
    }

    #[test]
    fn non_match_and_empty() {
        assert!(!host_blocked("good.tld", &globs()));
        assert!(!host_blocked("notevil.tld", &globs()));
        assert!(!host_blocked("x.tld", &[]));
    }

    #[test]
    fn trailing_dot_fqdn_still_blocked() {
        // F110: a trailing root dot must not bypass the blocklist.
        assert!(host_blocked("ads.example.com.", &globs()));
        assert!(host_blocked("a.evil.tld.", &globs()));
        assert!(host_blocked("evil.tld.", &globs()));
        assert!(url_blocked("https://sub.evil.tld./x", &globs()));
    }

    #[test]
    fn url_parsing() {
        assert!(url_blocked("https://a.evil.tld/path?q=1", &globs()));
        assert!(!url_blocked("https://good.tld/", &globs()));
        assert!(!url_blocked("not a url", &globs()));
    }
}
