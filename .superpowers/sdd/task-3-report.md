# Task 3 Report: PR1 OPEX rebrand — dual-path config resolver + config/opex.toml rename

## Status: DONE

## Resolver + Test (RED → GREEN)

**RED:** Added `crates/opex-gateway-util/src/config_path.rs` with the test but did NOT yet declare `pub mod config_path;` in `lib.rs`. Running `cargo test -p opex-gateway-util` returned 0 tests matched = function not found = RED confirmed.

**Implementation files:**
- Created: `crates/opex-gateway-util/src/config_path.rs` — `resolve_config_path()` + `resolve_config_path_in(base: &Path)`
- Modified: `crates/opex-gateway-util/src/lib.rs` — added `pub mod config_path;`
- Modified: `crates/opex-gateway-util/Cargo.toml` — added `[dev-dependencies] tempfile = "3"`

**GREEN:**
```
test config_path::tests::falls_back_when_new_missing ... ok
test result: ok. 17 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

## Both opex-core call sites (main.rs)

- **Line 278:** `config::AppConfig::load("config/hydeclaw.toml")?` → `config::AppConfig::load(&opex_gateway_util::config_path::resolve_config_path())?`
- **Line 303:** `"config/hydeclaw.toml".to_string()` → `opex_gateway_util::config_path::resolve_config_path()`

## opex-memory-worker handling

`grep -n gateway-util crates/opex-memory-worker/Cargo.toml` → no output (no dep on opex-gateway-util).

Per brief: local inline copy. `crates/opex-memory-worker/src/main.rs` line 40:
```rust
// Before:
let config_path = std::env::args().nth(1).unwrap_or("config/hydeclaw.toml".into());

// After:
let config_path = std::env::args().nth(1).unwrap_or_else(|| {
    if std::path::Path::new("config/opex.toml").exists() {
        "config/opex.toml".into()
    } else {
        "config/hydeclaw.toml".into()
    }
});
```
No new dependency added to memory-worker.

## Config git mv

```
git mv config/hydeclaw.toml config/opex.toml
```

## Path-string grep results

### Rust files (`grep -rn 'hydeclaw\.toml' --include='*.rs' .`)

| File | Lines | Action |
|------|-------|--------|
| `crates/opex-core/src/main.rs:278,303` | Two load/watcher call sites | **Fixed** → `resolve_config_path()` |
| `crates/opex-memory-worker/src/main.rs:40` | Default arg | **Fixed** → local resolver closure |
| `crates/opex-core/tests/integration_backup_size_cap.rs:52-66` | Candidates list + expect/assert messages | **Fixed** → prepended `config/opex.toml` candidates, updated messages |
| `crates/opex-memory-worker/tests/integration_memory_worker_notify.rs:95-97` | `write_worker_config` writes THEN loads `hydeclaw.toml` in same temp dir | **Left as-is** (self-consistent: both the `fs::write` and the `load_config` call use `hydeclaw.toml` within the same temp dir; the resolver never touches this path) |
| `crates/opex-core/src/config/mod.rs:90,1139,1270,1735` | Doc comment prose only | Left — not runtime paths |
| `crates/opex-core/src/main.rs:188,309,792` | Inline code comments | Left — not runtime paths |
| `crates/opex-core/src/gateway/mod.rs:139` | User-facing error message string | Left for PR2 — prose string, not a file path |
| `crates/opex-core/src/gateway/handlers/config.rs:303,345,354,366,373,383,391,408,426,459,600,648,649` | Runtime config R/W handler (PUT /api/config) | **Left for PR2** — these are config-editing endpoints; they will break at runtime post-rename until PR2 wires `resolve_config_path()` into the handler |
| `crates/opex-core/src/gateway/handlers/config.rs:536,554,569,570` | Backup-file comments and naming | Left for PR2 — prose/naming only |
| `crates/opex-core/src/gateway/handlers/curator.rs:94,109` | Curator handler reads config path | **Left for PR2** — same issue as config.rs handler |
| `crates/opex-core/src/gateway/handlers/monitoring/doctor.rs:757` | Doctor check help text | Left — prose string, not a path |

### Non-Rust deploy files (`grep -rn 'config/hydeclaw.toml' --include='*.toml' --include='*.sh' --include='Makefile' .`)

| File | Note |
|------|------|
| `setup.sh:395-658` | Install/setup script — explicit PR2 scope |
| `uninstall.sh:27-29` | Uninstall script — explicit PR2 scope |
| `update.sh:34` | Update script — explicit PR2 scope |

## cargo check results

```
cargo check -p opex-core          → Finished dev (no errors)
cargo check -p opex-memory-worker → Finished dev (no errors)
cargo check -p opex-core --tests  → Finished dev (no errors)
```

## Known PR2 items left

1. `setup.sh`, `uninstall.sh`, `update.sh` — deploy scripts; explicit PR2 scope per brief.

---

## Fix: config writers dual-path

### Sites changed

**`crates/opex-core/src/gateway/handlers/config.rs`**

- `api_update_config` (~line 303): `let config_path = "config/hydeclaw.toml"` → `let config_path = opex_gateway_util::config_path::resolve_config_path()`. All downstream uses in the function body updated to `&config_path`: backup read (`tokio::fs::read_to_string`), `restore_and_fail!` macro (`tokio::fs::write`), `update_service_urls`, `update_memory_config`, `update_subagents_enabled`, `update_limits_config`, `update_public_url`, `update_backup_config`, `update_curator_config`, `update_agent_tool_config`, validation `AppConfig::load`, restore write on validation failure, hot-reload `AppConfig::load`.
- `api_export_config` (~line 600): added `let config_path = opex_gateway_util::config_path::resolve_config_path();`; read changed to `&config_path`.
- `api_import_config` (~lines 648-649): added `let config_path = opex_gateway_util::config_path::resolve_config_path();`; `rotate_config_backups` and `std::fs::write` both changed to `&config_path`.

**`crates/opex-core/src/gateway/handlers/curator.rs`**

- `api_curator_config_put` (~lines 94, 109): added `let config_path = opex_gateway_util::config_path::resolve_config_path();` before first use; both `update_curator_config` and hot-reload `AppConfig::load` changed to `&config_path`.

### Type plumbing

`resolve_config_path()` returns `String`. All call sites that previously took `&str` now receive `&config_path` (coerces `String → &str`). The macro `restore_and_fail!` captures `config_path` from the enclosing scope and also uses `&config_path`. One `resolve_config_path()` call per handler — reads and writes within each handler are guaranteed to hit the same file.

### Build check results

```
cargo check -p opex-core          → Finished dev [unoptimized + debuginfo] (no errors)
cargo check -p opex-core --tests  → Finished dev [unoptimized + debuginfo] (no errors)
```

### Deferred-test command (server only)

```
cargo test -p opex-core -- config 2>&1 | tail -30
```

Runs the config API integration tests; links the opex-core test binary — requires live Postgres + `DATABASE_URL` set; OOMs locally on Windows.

### Residual `config/hydeclaw.toml` hits in `crates/opex-core/src`

`grep -rn 'config/hydeclaw.toml' crates/opex-core/src` → **no output** (zero hits).

Previously deferred items from task-3 that are now resolved:

- `config.rs:303,345,354,366,373,383,391,408,426,459,600,648,649` — all fixed.
- `curator.rs:94,109` — fixed.

Remaining legitimately deferred items (no handler code, not runtime paths):

- `crates/opex-core/src/gateway/handlers/config.rs:536,554,569,570` — comments inside `rotate_config_backups` describing the naming pattern; prose only, not runtime strings.
- `crates/opex-core/src/gateway/mod.rs:139` — user-facing bail! error message; prose, PR2/brand phase.
- `crates/opex-core/src/gateway/handlers/monitoring/doctor.rs:757` — doctor check help text; prose string.
- `crates/opex-core/src/config/mod.rs:90,1139,1270,1735` — doc comment prose only.
- `crates/opex-core/src/main.rs:188,309,792` — inline code comments.

These are all confirmed non-runtime-path strings — not fix targets per brief.
