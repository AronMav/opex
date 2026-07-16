//! Codegen binary: generates `ui/src/types/api.generated.ts` from Rust DTOs.
//!
//! Run via: `cargo run --features ts-gen --bin gen_ts_types`
//! Or: `make gen-types` (from the workspace root)
//!
//! Each DTO registers itself via `opex_core::register_ts_dto!()` next to
//! its struct definition. This binary collects all registrations from
//! `inventory::iter::<TsDecl>` and writes them sorted alphabetically.

use opex_core::dto_export::registry::TsDecl;
// Glob-import dto_export to force the linker to retain all submodules.
// Each submodule contains `register_ts_dto!{}` invocations that emit
// `inventory::submit!{}` linker sections. As long as the submodule is in the
// reachable graph, its sections survive into the binary.
#[allow(unused_imports)]
use opex_core::dto_export::*;

fn main() {
    let mut decls: Vec<&TsDecl> = inventory::iter::<TsDecl>.into_iter().collect();
    decls.sort_by_key(|d| d.name);

    // Detect duplicate registrations (two registrations with the same name).
    let mut seen = std::collections::HashSet::new();
    let dups: Vec<&str> = decls.iter()
        .filter_map(|d| if !seen.insert(d.name) { Some(d.name) } else { None })
        .collect();
    if !dups.is_empty() {
        panic!(
            "duplicate ts-rs registrations for: {dups:?}\n\
             Each type name must be unique. If two structs share a name in \
             different modules, only ONE may carry register_ts_dto!()."
        );
    }

    // Partition by dest.
    let mut by_dest: std::collections::HashMap<&str, Vec<&TsDecl>> =
        std::collections::HashMap::new();
    for d in decls {
        by_dest.entry(d.dest).or_default().push(d);
    }

    // Min-count assertions catch accidental registration loss (linker dropped
    // inventory section, dropped register_ts_dto call, etc.).
    // ui count: tightened to current production count (38, verified via
    //   `grep -rn "register_ts_dto!" crates/opex-core/src --include="*.rs"
    //     | grep -v "macro_rules\|stringify\|#!\[\|//"`).
    // channels count: 6 — MediaType, MediaAttachment, IncomingMessageDto,
    //   ChannelActionDto, ChannelInbound, ChannelOutbound (registered via
    //   crates/opex-core/src/dto_export/channels_ts.rs).
    let dest_paths: &[(&str, &str, usize)] = &[
        ("ui",       "ui/src/types/api.generated.ts",         38),
        ("channels", "channels/src/types.generated.ts",        6),
        ("ui-sse",   "ui/src/types/sse.generated.ts",          9),
        ("ui-ws",    "ui/src/types/ws.generated.ts",           3),
    ];

    let header = "// @generated — do not edit by hand.\n\
        // Source of truth: types annotated with #[ts(export)] in crates/opex-core/.\n\
        // Regenerate with: make gen-types\n\n";

    for (dest, path, min_count) in dest_paths {
        let group = by_dest.remove(dest).unwrap_or_default();
        let count = group.len();
        assert!(
            count >= *min_count,
            "expected >= {min_count} registered DTOs for dest={dest}, got {count}. \
             Possible cause: linker dropped inventory sections, \
             or a DTO file lost its register_ts_dto! call."
        );

        let body: String = group.iter()
            .map(|d| (d.decl_fn)())
            .collect::<Vec<_>>()
            .join("\n\n");

        let out_path = std::path::Path::new(path);
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)
                .unwrap_or_else(|e| panic!("failed to create output dir {}: {e}", parent.display()));
        }
        std::fs::write(out_path, format!("{header}{body}\n"))
            .unwrap_or_else(|e| panic!("failed to write {}: {e}", out_path.display()));

        println!("Generated {} ({count} types, dest={dest})", out_path.display());
    }

    // Sanity: no unrouted dests (catches typos in `dest = "..."`).
    if !by_dest.is_empty() {
        let extras: Vec<&str> = by_dest.keys().copied().collect();
        panic!(
            "registered DTOs with unknown dest values: {extras:?}\n\
             Add the new dest to dest_paths above OR fix the typo in \
             register_ts_dto!(.., dest = \"...\")."
        );
    }
}
