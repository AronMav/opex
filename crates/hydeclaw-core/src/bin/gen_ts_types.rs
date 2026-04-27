//! Codegen binary: generates `ui/src/types/api.generated.ts` from Rust DTOs.
//!
//! Run via: `cargo run --features ts-gen --bin gen_ts_types`
//! Or: `make gen-types` (from the workspace root)
//!
//! Each DTO registers itself via `hydeclaw_core::register_ts_dto!()` next to
//! its struct definition. This binary collects all registrations from
//! `inventory::iter::<TsDecl>` and writes them sorted alphabetically.

use hydeclaw_core::dto_export::registry::TsDecl;
// Glob-import dto_export to force the linker to retain all submodules.
// Each submodule contains `register_ts_dto!{}` invocations that emit
// `inventory::submit!{}` linker sections. As long as the submodule is in the
// reachable graph, its sections survive into the binary.
#[allow(unused_imports)]
use hydeclaw_core::dto_export::*;

fn main() {
    // `inventory::iter::<T>` is a const item, iterated via `.into_iter()`.
    let mut decls: Vec<(&str, String)> = inventory::iter::<TsDecl>
        .into_iter()
        .map(|d| (d.name, (d.decl_fn)()))
        .collect();

    // Stable sort by type name → deterministic output across builds.
    decls.sort_by_key(|(name, _)| *name);

    // Detect duplicate registrations (two structs with the same name).
    let mut seen = std::collections::HashSet::new();
    let dups: Vec<&str> = decls.iter()
        .filter_map(|(name, _)| if !seen.insert(*name) { Some(*name) } else { None })
        .collect();
    if !dups.is_empty() {
        panic!(
            "duplicate ts-rs registrations for: {dups:?}\n\
             Each type name must be unique. If two structs share a name in \
             different modules, only ONE may carry register_ts_dto!()."
        );
    }

    // Sanity check: catches silent registration loss.
    let count = decls.len();
    assert!(
        count >= 31,
        "expected >= 31 registered DTOs, got {count}. \
         Possible cause: linker dropped inventory sections, \
         or a DTO file lost its register_ts_dto! call."
    );

    let header = "// @generated — do not edit by hand.\n\
        // Source of truth: types annotated with #[ts(export)] in crates/hydeclaw-core/.\n\
        // Regenerate with: make gen-types\n\n";

    let body: String = decls.iter()
        .map(|(_, d)| d.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");

    let out_path = std::path::Path::new("ui/src/types/api.generated.ts");
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)
            .unwrap_or_else(|e| panic!("failed to create output dir {}: {e}", parent.display()));
    }
    std::fs::write(out_path, format!("{header}{body}\n"))
        .unwrap_or_else(|e| panic!("failed to write {}: {e}", out_path.display()));

    println!("Generated {} ({count} types)", out_path.display());
}
