//! File Scenario Engine (FSE) allowlist surface — surviving after legacy retirement.
//!
//! Keeps the hard-coded allowlist constant, the closed-domain toggle validator,
//! the dispatch-time autorun re-check, and the operator-editable toggle storage.
//! These are shared with the File Handler Hub (`match_buttons` / the new
//! `/api/handlers/allowlist` admin route). The seeder and the binding-write
//! validator went with the legacy `file_scenarios` bindings table.

pub mod allowlist;
pub mod allowlist_store;

// Flat public surface consumed by the hub's match_buttons + the handlers-admin route.
#[allow(unused_imports)]
pub use allowlist::{
    AllowlistError, FSE_DEFAULT_ALLOWLIST, is_allowed_for_autorun, validate_allowlist_toggle,
};
pub use allowlist_store::{
    get_enabled_allowlist, get_enabled_allowlist_strict, set_enabled_allowlist_checked,
};
// set_enabled_allowlist is pub in allowlist_store and tested there directly;
// it is intentionally NOT re-exported from the module root because the PUT
// handler now uses set_enabled_allowlist_checked (strict, propagates errors)
// and no other call-site in the crate uses the lenient variant via fse::.
#[allow(unused_imports)]
pub use allowlist_store::set_enabled_allowlist;

#[cfg(test)]
mod reexport_tests {
    use super::*;

    #[test]
    fn public_surface_is_re_exported_from_module_root() {
        assert_eq!(FSE_DEFAULT_ALLOWLIST.len(), 5);
        let full: Vec<String> = FSE_DEFAULT_ALLOWLIST.iter().map(|s| s.to_string()).collect();
        assert!(is_allowed_for_autorun("describe", &full));
        assert!(validate_allowlist_toggle(&["save".to_string()]).is_ok());
    }
}
