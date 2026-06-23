//! Env helper: reads `OPEX_<suffix>`. Centralizes the `OPEX_` prefix so call
//! sites pass only the suffix (AUTH_TOKEN, MASTER_KEY, …).

/// Returns the value of the `OPEX_<suffix>` environment variable, if set.
pub fn env_var(suffix: &str) -> Option<String> {
    std::env::var(format!("OPEX_{suffix}")).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_opex_env() {
        let s = "GATEWAY_UTIL_ENV_TEST";
        unsafe {
            std::env::remove_var(format!("OPEX_{s}"));
        }
        assert_eq!(env_var(s), None);
        unsafe {
            std::env::set_var(format!("OPEX_{s}"), "v");
        }
        assert_eq!(env_var(s).as_deref(), Some("v"));
        unsafe {
            std::env::remove_var(format!("OPEX_{s}"));
        }
    }
}
