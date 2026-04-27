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
}

#[cfg(feature = "ts-gen")]
inventory::collect!(TsDecl);

/// Registers a DTO with the TypeScript export registry.
///
/// Expands to nothing when `ts-gen` feature is OFF — safe to call unconditionally
/// from DTO definition sites without `#[cfg]` gating at the call site.
///
/// ```ignore
/// use serde::Serialize;
/// use ts_rs::TS;
///
/// #[derive(Serialize, TS)]
/// #[ts(export)]
/// pub struct MyDto { ... }
///
/// hydeclaw_core::register_ts_dto!(MyDto);
/// ```
#[cfg(feature = "ts-gen")]
#[macro_export]
macro_rules! register_ts_dto {
    ($t:ty) => {
        ::inventory::submit! {
            $crate::dto_export::registry::TsDecl {
                decl_fn: || format!(
                    "export {}",
                    <$t as ::ts_rs::TS>::decl(&::ts_rs::Config::default())
                ),
                name: ::std::stringify!($t),
            }
        }
    };
}

#[cfg(not(feature = "ts-gen"))]
#[macro_export]
macro_rules! register_ts_dto {
    ($t:ty) => {};
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
}
