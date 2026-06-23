//! Distributed registry for TypeScript-exported DTOs (ts-gen feature only).
//!
//! Each DTO registers itself via `register_ts_dto!{}` next to its struct
//! definition. `gen_ts_types` collects all registrations to build api.generated.ts.

#[cfg(feature = "ts-gen")]
pub struct TsDecl {
    /// Function that produces the `export type Foo = ...` string.
    pub decl_fn: fn() -> String,
    /// Type name for stable sort + deduplication. Must match the Rust struct name.
    pub name: &'static str,
    /// Destination key — partitions registrations into output files.
    /// Known values: "ui" (default, → ui/src/types/api.generated.ts),
    /// "channels" (→ channels/src/types.generated.ts).
    pub dest: &'static str,
}

#[cfg(feature = "ts-gen")]
inventory::collect!(TsDecl);

/// Register a Rust DTO for ts-rs codegen via inventory.
///
/// Each call registers a `TsDecl` that the `gen_ts_types` binary
/// collects via `inventory::iter::<TsDecl>` and emits as a TypeScript
/// `export` declaration.
///
/// # Forms
///
/// - `register_ts_dto!(Type)` — registers `Type` for the **default** UI
///   destination (`ui/src/types/api.generated.ts`). Equivalent to
///   `register_ts_dto!(Type, dest = "ui")`. This single-arg form exists
///   for backward compatibility with the 30+ pre-S6 registrations.
///
/// - `register_ts_dto!(Type, dest = "channels")` — registers `Type` for
///   the channels destination (`channels/src/types.generated.ts`).
///
/// # Adding a new destination
///
/// 1. Add a new dest literal here (any unique `"name"` works; partition
///    happens by string equality).
/// 2. Update `crates/opex-core/src/bin/gen_ts_types.rs::dest_paths`
///    to map the new dest to its output file path + min-count assertion.
/// 3. Document in the relevant codegen design spec.
///
/// # Example
///
/// ```ignore
/// use serde::Serialize;
/// use ts_rs::TS;
///
/// #[derive(Serialize, TS)]
/// pub struct MyDto { field: String }
///
/// crate::register_ts_dto!(MyDto);                    // → ui dest
/// crate::register_ts_dto!(MyDto, dest = "channels"); // → channels dest
/// ```
///
/// # When `ts-gen` feature is disabled
///
/// The macro expands to a no-op so production builds (without
/// `--features ts-gen`) skip ts-rs entirely.
#[cfg(feature = "ts-gen")]
#[macro_export]
macro_rules! register_ts_dto {
    // Form 1: register_ts_dto!(Type) — backward compat, defaults to "ui"
    ($t:ty) => {
        $crate::register_ts_dto!($t, dest = "ui");
    };
    // Form 2: register_ts_dto!(Type, dest = "channels")
    ($t:ty, dest = $dest:literal) => {
        ::inventory::submit! {
            $crate::dto_export::registry::TsDecl {
                decl_fn: || format!(
                    "export {}",
                    <$t as ::ts_rs::TS>::decl(&::ts_rs::Config::default())
                ),
                name: ::std::stringify!($t),
                dest: $dest,
            }
        }
    };
}

#[cfg(not(feature = "ts-gen"))]
#[macro_export]
macro_rules! register_ts_dto {
    ($t:ty) => {};
    ($t:ty, dest = $dest:literal) => {};
}

#[cfg(all(test, feature = "ts-gen"))]
mod tests {
    use super::*;
    use serde::Serialize;
    use ts_rs::TS;

    #[derive(Serialize, TS)]
    #[ts(export)]
    pub struct __RegistryProbe {
        pub field: String,
    }

    crate::register_ts_dto!(__RegistryProbe);

    #[test]
    fn registry_finds_probe() {
        let found = inventory::iter::<TsDecl>
            .into_iter()
            .any(|d| d.name == "__RegistryProbe");
        assert!(found, "__RegistryProbe was not registered via inventory");
    }

    #[derive(Serialize, TS)]
    #[ts(export)]
    pub struct __ChannelsProbe {
        pub field: i32,
    }

    crate::register_ts_dto!(__ChannelsProbe, dest = "channels");

    #[test]
    fn registry_supports_dest_routing() {
        let found: Vec<&TsDecl> = inventory::iter::<TsDecl>
            .into_iter()
            .filter(|d| d.name == "__ChannelsProbe")
            .collect();
        assert_eq!(found.len(), 1, "exactly one __ChannelsProbe registration expected");
        assert_eq!(found[0].dest, "channels");
    }

    #[test]
    fn registry_existing_dto_default_dest_is_ui() {
        let found: Vec<&TsDecl> = inventory::iter::<TsDecl>
            .into_iter()
            .filter(|d| d.name == "__RegistryProbe")
            .collect();
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].dest, "ui",
            "single-arg register_ts_dto!(Type) must default to dest=ui"
        );
    }
}
