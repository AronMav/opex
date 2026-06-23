//! File Scenario Engine (FSE) core module.
//!
//! Phase 4 lands the security backstop here: the hard-coded allowlist
//! constant, the caller-independent write-time + dispatch-time validators
//! (shared by the HTTP write path and the agent `file_scenario` tool), the
//! operator-editable closed-domain toggle storage, and the idempotent
//! default-binding seeder. Later phases mount the sniffer, dispatcher, and
//! outcome contract under the same module.

pub mod allowlist;
pub mod allowlist_store;
pub mod seeder;

// Flat public surface — the import path downstream phases use.
#[allow(unused_imports)] // Phase 5+: consumed by HTTP route handlers + agent file_scenario tool
pub use allowlist::{
    AllowlistError, FSE_DEFAULT_ALLOWLIST, is_allowed_for_autorun, validate_allowlist_toggle,
    validate_binding_write,
};
pub use allowlist_store::{get_enabled_allowlist, set_enabled_allowlist};
pub use seeder::seed_default_file_scenarios;

#[cfg(test)]
mod reexport_tests {
    use super::*;

    #[test]
    fn public_surface_is_re_exported_from_module_root() {
        // These paths are what Phase 5+ (HTTP handlers / agent tool / dispatcher)
        // import — guard them so a refactor cannot silently break the surface.
        assert_eq!(FSE_DEFAULT_ALLOWLIST.len(), 4);
        let full: Vec<String> = FSE_DEFAULT_ALLOWLIST.iter().map(|s| s.to_string()).collect();
        assert!(validate_binding_write("tool", "transcribe", true, &full).is_ok());
        assert!(is_allowed_for_autorun("describe", &full));
        assert!(validate_allowlist_toggle(&["save".to_string()]).is_ok());
    }
}
