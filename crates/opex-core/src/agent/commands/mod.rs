//! Единый реестр команд чата (спек 2026-07-09).
use std::sync::LazyLock;

pub mod spec;
pub mod registry;
pub mod builtin;

/// Синглтон реестра builtins. Валидируется при первом обращении; паника при
/// невалидности — конфигурация команд статична и обязана быть корректной.
pub static COMMAND_REGISTRY: LazyLock<registry::CommandRegistry> = LazyLock::new(|| {
    registry::CommandRegistry::from_sources(&[&builtin::BuiltinCommandSource])
        .expect("builtin command registry must validate")
});
