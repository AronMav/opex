//! Dual-read env: читает OPEX_<suffix>, при отсутствии — HYDECLAW_<suffix>.
//! Fallback удаляется в PR3 после миграции .env на сервере.

/// Возвращает значение env-переменной по суффиксу, предпочитая префикс `OPEX_`.
pub fn env_var(suffix: &str) -> Option<String> {
    std::env::var(format!("OPEX_{suffix}"))
        .ok()
        .or_else(|| std::env::var(format!("HYDECLAW_{suffix}")).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_opex_then_falls_back_to_hydeclaw() {
        let s = "PR1_ENV_TEST_KEY"; // уникальный суффикс, чтобы не конфликтовать
        unsafe { std::env::remove_var(format!("OPEX_{s}")); std::env::remove_var(format!("HYDECLAW_{s}")); }
        assert_eq!(env_var(s), None);

        unsafe { std::env::set_var(format!("HYDECLAW_{s}"), "legacy"); }
        assert_eq!(env_var(s).as_deref(), Some("legacy")); // fallback

        unsafe { std::env::set_var(format!("OPEX_{s}"), "new"); }
        assert_eq!(env_var(s).as_deref(), Some("new")); // приоритет OPEX

        unsafe { std::env::remove_var(format!("OPEX_{s}")); std::env::remove_var(format!("HYDECLAW_{s}")); }
    }
}
