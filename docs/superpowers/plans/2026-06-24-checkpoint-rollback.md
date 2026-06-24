# Checkpoint / Rollback Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Снапшотить файлы агента (`agents/{agent}/`) в shadow-git store перед его правками и дать пользователю откатить их командой `/rollback`.

**Architecture:** Один process-wide `CheckpointManager` поверх отдельного bare-git репозитория (`~/.opex/checkpoints/store`), общего на всех агентов; изоляция через env (`GIT_DIR`/`GIT_WORK_TREE`/`GIT_INDEX_FILE` + `GIT_CONFIG_*`); per-`n` refs `refs/checkpoints/{agent}/{n}` с parentless snapshot-коммитами; все store-мутации под одним `tokio::sync::Mutex`. Ленивый `ensure_checkpoint` дёргается из мутирующих workspace-handlers; `/rollback` — slash-команда через `engine_arc`.

**Tech Stack:** Rust 2024, `tokio` (process+sync, feature `full` уже есть), внешний `git` (CLI в PATH), `tempfile` (dev-dep, уже есть), `chrono` (есть). Новых зависимостей НЕТ.

**Спека:** `docs/superpowers/specs/2026-06-24-checkpoint-rollback-design.md` (v2, per-`n` модель).

## Global Constraints

- **rustls-only**, никакого OpenSSL (общий запрет проекта; здесь не задействован).
- **Работа напрямую в `master`** (durable consent пользователя). НЕ создавать ветки.
- **NO git push** без явного разрешения. **NO `Co-Authored-By`** / упоминаний AI в коммитах.
- **TDD**: сначала падающий тест, потом реализация. Частые коммиты.
- **git обязан быть в PATH** — runtime-требование; вся фича **best-effort**: любая git-ошибка → `tracing::warn!`, ход агента НЕ падает. `/rollback` при сбое → внятная ошибка пользователю.
- **Scope снапшота = ровно `{workspace_dir}/agents/{agent}/`.** Shared-каталоги (`tools/`, `skills/`, `mcp/`, `uploads/`, base: `toolgate/`, `channels/`) и корень workspace НЕ снапшотятся.
- **Excludes только через `$GIT_DIR/info/exclude`** — НИКОГДА не писать `.gitignore` в work-tree (work-tree лежит внутри git-репо проекта в dev).
- **Env-изоляция git (каждый вызов):** `GIT_DIR=<store>`, `GIT_WORK_TREE={workspace_dir}/agents/{agent}`, `GIT_INDEX_FILE=<store>/index-{agent}`, `GIT_CONFIG_GLOBAL=<devnull>`, `GIT_CONFIG_SYSTEM=<devnull>`, `GIT_CONFIG_NOSYSTEM=1`, фиксированные `GIT_AUTHOR_*`/`GIT_COMMITTER_*`; `commit-tree` с `--no-gpg-sign`. `<devnull>` = `NUL` на Windows, `/dev/null` иначе.
- **Store init:** `git config gc.auto 0` + `git config core.logAllRefUpdates false`.
- **`agent_name`** валидируется (`^[A-Za-z0-9_-]+$`) перед интерполяцией в ref-путь.
- **Конфиг-дефолты:** `enabled=true`, `keep=50`, `ttl_days=14`, `store_path="~/.opex/checkpoints/store"`, `excludes=[]`, `max_file_size_mb=5`.

## File Structure

- **Create** `crates/opex-core/src/agent/checkpoint_manager.rs` — `CheckpointManager`, `CheckpointMeta`, `RestoreReport`, `RollbackCmd`-парсер (см. Task 9 — парсер живёт в commands.rs), git-обёртки. Зеркало по стилю `approval_manager.rs`/`clarify_manager.rs`.
- **Modify** `crates/opex-core/src/config/mod.rs` — `CheckpointConfig` + поле `checkpoint` в `AppConfig`.
- **Modify** `crates/opex-core/src/agent/mod.rs` — `pub(crate) mod checkpoint_manager;`.
- **Modify** `crates/opex-core/src/agent/agent_config.rs` — поле `checkpoint_manager: Option<Arc<CheckpointManager>>`.
- **Modify** `crates/opex-core/src/main.rs` — конструирование `Arc<CheckpointManager>` + проброс во все сайты сборки `AgentConfig`.
- **Modify** `crates/opex-core/src/agent/tool_handlers/workspace.rs` — авто-`ensure_checkpoint` в 4 мутирующих handler-обёртках.
- **Modify** `crates/opex-core/src/agent/pipeline/bootstrap.rs` — `new_turn` хук.
- **Modify** `crates/opex-core/src/agent/pipeline/commands.rs` — `parse_rollback_command` + arm `"/rollback"`.

---

### Task 1: `CheckpointConfig` + загрузка конфига

**Files:**
- Modify: `crates/opex-core/src/config/mod.rs` (struct рядом с `BackupConfig`, поле в `AppConfig:13-73`)
- Test: тот же файл, модуль `#[cfg(test)]` (или новый внизу)

**Interfaces:**
- Produces: `pub struct CheckpointConfig { pub enabled: bool, pub keep: u32, pub ttl_days: u32, pub store_path: String, pub excludes: Vec<String>, pub max_file_size_mb: u64 }` с `Default`; поле `pub checkpoint: CheckpointConfig` в `AppConfig`.

- [ ] **Step 1: Падающий тест — дефолты и парсинг секции**

В конце `config/mod.rs` (в существующем `#[cfg(test)] mod tests` если он есть, иначе создать):

```rust
#[test]
fn checkpoint_config_defaults() {
    let c = CheckpointConfig::default();
    assert!(c.enabled);
    assert_eq!(c.keep, 50);
    assert_eq!(c.ttl_days, 14);
    assert_eq!(c.max_file_size_mb, 5);
    assert_eq!(c.store_path, "~/.opex/checkpoints/store");
    assert!(c.excludes.is_empty());
}

#[test]
fn checkpoint_config_parses_from_toml() {
    let toml = r#"
        enabled = false
        keep = 10
        ttl_days = 3
        store_path = "/tmp/cp"
        excludes = ["foo", "bar"]
        max_file_size_mb = 2
    "#;
    let c: CheckpointConfig = toml::from_str(toml).unwrap();
    assert!(!c.enabled);
    assert_eq!(c.keep, 10);
    assert_eq!(c.excludes, vec!["foo".to_string(), "bar".to_string()]);
}
```

- [ ] **Step 2: Запустить — убедиться, что не компилируется/падает**

Run: `cargo test --bin opex-core checkpoint_config -- --nocapture`
Expected: FAIL — `cannot find type CheckpointConfig`.

- [ ] **Step 3: Реализация — struct + defaults + поле в AppConfig**

Рядом с `BackupConfig` (config/mod.rs:~224) добавить:

```rust
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct CheckpointConfig {
    #[serde(default = "default_checkpoint_enabled")]
    pub enabled: bool,
    #[serde(default = "default_checkpoint_keep")]
    pub keep: u32,
    #[serde(default = "default_checkpoint_ttl_days")]
    pub ttl_days: u32,
    #[serde(default = "default_checkpoint_store_path")]
    pub store_path: String,
    #[serde(default)]
    pub excludes: Vec<String>,
    #[serde(default = "default_checkpoint_max_file_size_mb")]
    pub max_file_size_mb: u64,
}

fn default_checkpoint_enabled() -> bool { true }
fn default_checkpoint_keep() -> u32 { 50 }
fn default_checkpoint_ttl_days() -> u32 { 14 }
fn default_checkpoint_store_path() -> String { "~/.opex/checkpoints/store".to_string() }
fn default_checkpoint_max_file_size_mb() -> u64 { 5 }

impl Default for CheckpointConfig {
    fn default() -> Self {
        Self {
            enabled: default_checkpoint_enabled(),
            keep: default_checkpoint_keep(),
            ttl_days: default_checkpoint_ttl_days(),
            store_path: default_checkpoint_store_path(),
            excludes: Vec::new(),
            max_file_size_mb: default_checkpoint_max_file_size_mb(),
        }
    }
}
```

В `AppConfig` (после `pub security: SecurityConfig,`) добавить:

```rust
    #[serde(default)]
    pub checkpoint: CheckpointConfig,
```

- [ ] **Step 4: Запустить — тесты зелёные**

Run: `cargo test --bin opex-core checkpoint_config -- --nocapture`
Expected: PASS (2 теста).

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/config/mod.rs
git commit -m "feat(checkpoint): CheckpointConfig + секция [checkpoint] в AppConfig"
```

---

### Task 2: `CheckpointManager` каркас — store init, env, git-обёртки, repair, валидация

**Files:**
- Create: `crates/opex-core/src/agent/checkpoint_manager.rs`
- Modify: `crates/opex-core/src/agent/mod.rs` (добавить `pub(crate) mod checkpoint_manager;`)
- Test: модуль `#[cfg(test)]` в `checkpoint_manager.rs`

**Interfaces:**
- Consumes: `crate::config::CheckpointConfig` (Task 1).
- Produces:
  - `pub(crate) struct CheckpointManager { config: CheckpointConfig, store_path: PathBuf, store_lock: tokio::sync::Mutex<()> }`
  - `pub(crate) fn new(config: CheckpointConfig) -> Self`
  - `fn expand_tilde(s: &str) -> PathBuf`
  - `fn validate_agent_name(agent: &str) -> anyhow::Result<()>`
  - `async fn git(&self, agent: &str, workspace_dir: &str, args: &[&str]) -> anyhow::Result<std::process::Output>`
  - `async fn git_ok(&self, agent: &str, workspace_dir: &str, args: &[&str]) -> anyhow::Result<String>`
  - `async fn ensure_store(&self) -> anyhow::Result<()>`
  - `fn repair_bare_repo_dirs(&self) -> anyhow::Result<()>`
  - const `DEFAULT_EXCLUDES: &[&str]`

- [ ] **Step 1: Падающий тест — store init + repair + валидация имени**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CheckpointConfig;

    fn mgr_at(store: &std::path::Path) -> CheckpointManager {
        let mut cfg = CheckpointConfig::default();
        cfg.store_path = store.to_str().unwrap().to_string();
        CheckpointManager::new(cfg)
    }

    #[tokio::test]
    async fn ensure_store_creates_bare_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        let m = mgr_at(&store);
        m.ensure_store().await.unwrap();
        assert!(store.join("HEAD").exists());
        assert!(store.join("refs").join("checkpoints").exists());
        assert!(store.join("info").join("exclude").exists());
        // идемпотентность
        m.ensure_store().await.unwrap();
    }

    #[test]
    fn agent_name_validation() {
        assert!(CheckpointManager::validate_agent_name("Main_Agent-1").is_ok());
        assert!(CheckpointManager::validate_agent_name("").is_err());
        assert!(CheckpointManager::validate_agent_name("../etc").is_err());
        assert!(CheckpointManager::validate_agent_name("a/b").is_err());
    }
}
```

- [ ] **Step 2: Запустить — FAIL**

Run: `cargo test --bin opex-core checkpoint_manager -- --nocapture`
Expected: FAIL — модуль/типы не найдены.

- [ ] **Step 3: Реализация каркаса**

В `agent/mod.rs` добавить рядом с прочими `mod`-декларациями:

```rust
pub(crate) mod checkpoint_manager;
```

Создать `crates/opex-core/src/agent/checkpoint_manager.rs`:

```rust
//! Shadow-git checkpoint store. Снапшотит `agents/{agent}/` в отдельный bare-git
//! репозиторий (НЕ рабочий git проекта) перед правками агента; даёт откат.
//! Порт Hermes `tools/checkpoint_manager.py`. Best-effort: git-ошибки логируются,
//! ход агента не падает. Все store-мутации сериализованы `store_lock`.

use std::path::{Path, PathBuf};
use std::process::Output;

use crate::config::CheckpointConfig;

/// Каталоги/паттерны, никогда не попадающие в снапшот (cost + safety).
pub(crate) const DEFAULT_EXCLUDES: &[&str] = &[
    ".git/", "node_modules/", "target/", "dist/", "build/", ".cache/",
    "*.tmp", "*.log", "*.lock",
    "*.png", "*.jpg", "*.jpeg", "*.gif", "*.webp", "*.mp3", "*.mp4",
    "*.wav", "*.ogg", "*.zip", "*.tar", "*.gz", "*.bin", "*.pdf",
];

pub(crate) struct CheckpointManager {
    config: CheckpointConfig,
    store_path: PathBuf,
    /// Сериализует ВСЕ store-мутирующие операции (add/write-tree/commit/update-ref/prune/gc).
    store_lock: tokio::sync::Mutex<()>,
}

fn expand_tilde(s: &str) -> PathBuf {
    if let Some(rest) = s.strip_prefix("~/").or_else(|| s.strip_prefix("~\\")) {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_default();
        if !home.is_empty() {
            return Path::new(&home).join(rest);
        }
    }
    PathBuf::from(s)
}

impl CheckpointManager {
    pub(crate) fn new(config: CheckpointConfig) -> Self {
        let store_path = expand_tilde(&config.store_path);
        Self { config, store_path, store_lock: tokio::sync::Mutex::new(()) }
    }

    fn validate_agent_name(agent: &str) -> anyhow::Result<()> {
        if agent.is_empty()
            || !agent.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            anyhow::bail!("invalid agent name for checkpoint ref: {:?}", agent);
        }
        Ok(())
    }

    fn work_tree(&self, workspace_dir: &str, agent: &str) -> String {
        Path::new(workspace_dir).join("agents").join(agent).to_string_lossy().into_owned()
    }

    fn index_file(&self, agent: &str) -> PathBuf {
        self.store_path.join(format!("index-{agent}"))
    }

    /// Запустить git с полным изолирующим env. Возвращает сырой Output (статус не проверяется).
    async fn git(&self, agent: &str, workspace_dir: &str, args: &[&str]) -> anyhow::Result<Output> {
        let devnull = if cfg!(windows) { "NUL" } else { "/dev/null" };
        let out = tokio::process::Command::new("git")
            .env("GIT_DIR", &self.store_path)
            .env("GIT_WORK_TREE", self.work_tree(workspace_dir, agent))
            .env("GIT_INDEX_FILE", self.index_file(agent))
            .env("GIT_CONFIG_GLOBAL", devnull)
            .env("GIT_CONFIG_SYSTEM", devnull)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "OPEX")
            .env("GIT_AUTHOR_EMAIL", "checkpoint@opex.local")
            .env("GIT_COMMITTER_NAME", "OPEX")
            .env("GIT_COMMITTER_EMAIL", "checkpoint@opex.local")
            .args(args)
            .output()
            .await?;
        Ok(out)
    }

    /// git, который падает (bail) при ненулевом статусе; возвращает stdout как String.
    async fn git_ok(&self, agent: &str, workspace_dir: &str, args: &[&str]) -> anyhow::Result<String> {
        let out = self.git(agent, workspace_dir, args).await?;
        if !out.status.success() {
            anyhow::bail!("git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Пересоздать структуру bare-репо, если `gc` её снёс (порт Hermes `_repair_bare_repo_dirs`).
    fn repair_bare_repo_dirs(&self) -> anyhow::Result<()> {
        for d in ["refs", "refs/checkpoints", "objects", "objects/pack", "objects/info"] {
            std::fs::create_dir_all(self.store_path.join(d)).ok();
        }
        let head = self.store_path.join("HEAD");
        if !head.exists() {
            std::fs::write(&head, "ref: refs/heads/main\n").ok();
        }
        Ok(())
    }

    /// Идемпотентно создать bare-store, выставить gc.auto=0 + logAllRefUpdates=false,
    /// записать info/exclude. Дёргать перед любой операцией.
    pub(crate) async fn ensure_store(&self) -> anyhow::Result<()> {
        if !self.store_path.join("HEAD").exists() {
            tokio::fs::create_dir_all(&self.store_path).await?;
            let out = tokio::process::Command::new("git")
                .arg("init").arg("--bare")
                .arg(&self.store_path)
                .output().await?;
            if !out.status.success() {
                anyhow::bail!("git init --bare failed: {}", String::from_utf8_lossy(&out.stderr));
            }
            for kv in [("gc.auto", "0"), ("core.logAllRefUpdates", "false")] {
                let out = tokio::process::Command::new("git")
                    .arg("--git-dir").arg(&self.store_path)
                    .arg("config").arg(kv.0).arg(kv.1)
                    .output().await?;
                if !out.status.success() {
                    anyhow::bail!("git config {} failed: {}", kv.0, String::from_utf8_lossy(&out.stderr));
                }
            }
        }
        self.repair_bare_repo_dirs()?;
        let mut excludes: Vec<String> = DEFAULT_EXCLUDES.iter().map(|s| s.to_string()).collect();
        excludes.extend(self.config.excludes.iter().cloned());
        let info_dir = self.store_path.join("info");
        tokio::fs::create_dir_all(&info_dir).await.ok();
        tokio::fs::write(info_dir.join("exclude"), excludes.join("\n") + "\n").await?;
        Ok(())
    }
}
```

- [ ] **Step 4: Запустить — тесты зелёные**

Run: `cargo test --bin opex-core checkpoint_manager -- --nocapture`
Expected: PASS (`ensure_store_creates_bare_repo`, `agent_name_validation`).

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/agent/checkpoint_manager.rs crates/opex-core/src/agent/mod.rs
git commit -m "feat(checkpoint): CheckpointManager каркас (store init, env-изоляция, repair, валидация)"
```

---

### Task 3: `ensure_checkpoint` — снапшот, no-op, excludes, max_file_size, per-n ref

**Files:**
- Modify: `crates/opex-core/src/agent/checkpoint_manager.rs`
- Test: модуль `#[cfg(test)]` там же

**Interfaces:**
- Consumes: `git`/`git_ok`/`ensure_store`/`work_tree`/`index_file` (Task 2).
- Produces:
  - `pub(crate) async fn ensure_checkpoint(&self, agent: &str, workspace_dir: &str) -> anyhow::Result<Option<usize>>` (None = no-op)
  - `async fn max_existing_n(&self, agent: &str) -> anyhow::Result<usize>` (0 если нет refs)
  - `async fn commit_snapshot(&self, agent: &str, workspace_dir: &str, msg: &str) -> anyhow::Result<Option<usize>>` (общий снапшот-движок; `ensure_checkpoint` = тонкая обёртка с msg `"checkpoint"`; Task 5 переиспользует с `"restore of N"`)
  - const `EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904"`

- [ ] **Step 1: Падающие тесты — создание, no-op, новый коммит, excludes, размер**

Добавить в `#[cfg(test)] mod tests` (helper создаёт scope-каталог агента и пишет файл):

```rust
    use tokio::fs;

    async fn write_scope(ws: &std::path::Path, agent: &str, rel: &str, content: &str) {
        let p = ws.join("agents").join(agent).join(rel);
        fs::create_dir_all(p.parent().unwrap()).await.unwrap();
        fs::write(p, content).await.unwrap();
    }

    #[tokio::test]
    async fn ensure_checkpoint_creates_and_noops() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        let ws = tmp.path().join("ws");
        let m = mgr_at(&store);
        let agent = "Agent";
        write_scope(&ws, agent, "notes.md", "v1").await;

        let n1 = m.ensure_checkpoint(agent, ws.to_str().unwrap()).await.unwrap();
        assert_eq!(n1, Some(1));
        assert!(store.join("refs/checkpoints/Agent/1").exists());

        // без изменений → no-op
        let n_noop = m.ensure_checkpoint(agent, ws.to_str().unwrap()).await.unwrap();
        assert_eq!(n_noop, None);

        // правка → новый чекпойнт 2
        write_scope(&ws, agent, "notes.md", "v2").await;
        let n2 = m.ensure_checkpoint(agent, ws.to_str().unwrap()).await.unwrap();
        assert_eq!(n2, Some(2));
    }

    #[tokio::test]
    async fn ensure_checkpoint_respects_excludes_and_size() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        let ws = tmp.path().join("ws");
        let mut cfg = CheckpointConfig::default();
        cfg.store_path = store.to_str().unwrap().to_string();
        cfg.max_file_size_mb = 1;
        let m = CheckpointManager::new(cfg);
        let agent = "Agent";

        write_scope(&ws, agent, "keep.md", "small").await;
        write_scope(&ws, agent, "node_modules/x.js", "junk").await; // excluded
        // 2 MB файл → исключается по размеру
        write_scope(&ws, agent, "big.bin", &"a".repeat(2 * 1024 * 1024)).await;

        let n = m.ensure_checkpoint(agent, ws.to_str().unwrap()).await.unwrap();
        assert_eq!(n, Some(1));
        let tracked = m.git_ok(agent, ws.to_str().unwrap(),
            &["ls-tree", "-r", "--name-only", "refs/checkpoints/Agent/1"]).await.unwrap();
        assert!(tracked.contains("keep.md"));
        assert!(!tracked.contains("node_modules"));
        assert!(!tracked.contains("big.bin"));
    }
```

- [ ] **Step 2: Запустить — FAIL**

Run: `cargo test --bin opex-core checkpoint_manager::tests::ensure_checkpoint -- --nocapture`
Expected: FAIL — метод не найден.

- [ ] **Step 3: Реализация**

Добавить в `impl CheckpointManager` (const `EMPTY_TREE` — рядом с `DEFAULT_EXCLUDES`):

```rust
/// SHA пустого git-дерева (для diff первого чекпойнта).
pub(crate) const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
```

```rust
    /// Наибольший существующий n для агента (0 если refs нет).
    async fn max_existing_n(&self, agent: &str) -> anyhow::Result<usize> {
        let refs = self.git_ok(agent, ".", &[
            "for-each-ref", "--format=%(refname)",
            &format!("refs/checkpoints/{agent}"),
        ]).await.unwrap_or_default();
        let max = refs.lines()
            .filter_map(|r| r.rsplit('/').next())
            .filter_map(|s| s.parse::<usize>().ok())
            .max()
            .unwrap_or(0);
        Ok(max)
    }

    /// Снять снапшот scope в новый per-n ref. None = дерево не изменилось (no-op).
    async fn commit_snapshot(&self, agent: &str, workspace_dir: &str, msg: &str) -> anyhow::Result<Option<usize>> {
        Self::validate_agent_name(agent)?;
        self.ensure_store().await?;
        let wt = workspace_dir;

        // Стейджим всё (excludes из info/exclude применяются автоматически).
        self.git_ok(agent, wt, &["add", "-A"]).await?;

        // max_file_size: убрать из индекса файлы крупнее лимита.
        if self.config.max_file_size_mb > 0 {
            let limit = self.config.max_file_size_mb * 1024 * 1024;
            let staged = self.git_ok(agent, wt, &["diff", "--cached", "--name-only"]).await?;
            let wt_root = Path::new(workspace_dir).join("agents").join(agent);
            for rel in staged.lines().filter(|l| !l.is_empty()) {
                if let Ok(meta) = tokio::fs::metadata(wt_root.join(rel)).await {
                    if meta.len() > limit {
                        self.git_ok(agent, wt, &["rm", "--cached", "--quiet", "--", rel]).await.ok();
                    }
                }
            }
        }

        let tree = self.git_ok(agent, wt, &["write-tree"]).await?.trim().to_string();

        // no-op, если дерево совпало с последним чекпойнтом.
        let last_n = self.max_existing_n(agent).await?;
        if last_n > 0 {
            let last_tree = self.git_ok(agent, wt, &[
                "rev-parse", &format!("refs/checkpoints/{agent}/{last_n}^{{tree}}"),
            ]).await?.trim().to_string();
            if last_tree == tree {
                return Ok(None);
            }
        }

        let commit = self.git_ok(agent, wt, &[
            "commit-tree", &tree, "--no-gpg-sign", "-m", msg,
        ]).await?.trim().to_string();
        let next_n = last_n + 1;
        self.git_ok(agent, wt, &[
            "update-ref", &format!("refs/checkpoints/{agent}/{next_n}"), &commit,
        ]).await?;
        Ok(Some(next_n))
    }

    /// Ленивый baseline: снять снапшот scope перед правкой. None = нет изменений.
    pub(crate) async fn ensure_checkpoint(&self, agent: &str, workspace_dir: &str) -> anyhow::Result<Option<usize>> {
        if !self.config.enabled {
            return Ok(None);
        }
        let _guard = self.store_lock.lock().await;
        self.commit_snapshot(agent, workspace_dir, "checkpoint").await
    }
```

Примечание: `commit-tree` без `-p` → parentless. `rev-parse refs/.../{n}^{tree}` через фигурные скобки `^{{tree}}` в format-строке.

- [ ] **Step 4: Запустить — тесты зелёные**

Run: `cargo test --bin opex-core checkpoint_manager::tests::ensure_checkpoint -- --nocapture`
Expected: PASS (2 теста).

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/agent/checkpoint_manager.rs
git commit -m "feat(checkpoint): ensure_checkpoint — снапшот, no-op по tree, excludes, max_file_size"
```

---

### Task 4: `list_checkpoints` + `diff`

**Files:**
- Modify: `crates/opex-core/src/agent/checkpoint_manager.rs`
- Test: модуль `#[cfg(test)]` там же

**Interfaces:**
- Consumes: `git_ok`, `EMPTY_TREE`, `ensure_checkpoint` (для setup в тестах).
- Produces:
  - `pub(crate) struct CheckpointMeta { pub n: usize, pub commit: String, pub created: String, pub summary: String }` (`created` = ISO commit-date строкой; `summary` = shortstat)
  - `pub(crate) async fn list_checkpoints(&self, agent: &str) -> anyhow::Result<Vec<CheckpointMeta>>` (newest-first по n)
  - `pub(crate) async fn diff(&self, agent: &str, workspace_dir: &str, n: usize) -> anyhow::Result<String>`
  - `async fn resolve_n(&self, agent: &str, n: usize) -> anyhow::Result<String>` (ref-имя или bail при отсутствии)

- [ ] **Step 1: Падающие тесты**

```rust
    #[tokio::test]
    async fn list_and_diff() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        let ws = tmp.path().join("ws");
        let m = mgr_at(&store);
        let agent = "Agent";

        write_scope(&ws, agent, "a.md", "one").await;
        m.ensure_checkpoint(agent, ws.to_str().unwrap()).await.unwrap();
        write_scope(&ws, agent, "a.md", "two").await;
        m.ensure_checkpoint(agent, ws.to_str().unwrap()).await.unwrap();

        let list = m.list_checkpoints(agent).await.unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].n, 2); // newest first
        assert_eq!(list[1].n, 1);
        assert!(!list[0].created.is_empty());

        // diff чекпойнта 1 против текущего ("two") должен показать изменение
        let d = m.diff(agent, ws.to_str().unwrap(), 1).await.unwrap();
        assert!(d.contains("one") || d.contains("two"), "diff: {d}");

        // несуществующий N
        assert!(m.diff(agent, ws.to_str().unwrap(), 99).await.is_err());
    }
```

- [ ] **Step 2: Запустить — FAIL**

Run: `cargo test --bin opex-core checkpoint_manager::tests::list_and_diff -- --nocapture`
Expected: FAIL — методы не найдены.

- [ ] **Step 3: Реализация**

```rust
pub(crate) struct CheckpointMeta {
    pub n: usize,
    pub commit: String,
    pub created: String,
    pub summary: String,
}
```

```rust
    /// Проверить, что чекпойнт n существует; вернуть полное имя ref.
    async fn resolve_n(&self, agent: &str, n: usize) -> anyhow::Result<String> {
        Self::validate_agent_name(agent)?;
        let refname = format!("refs/checkpoints/{agent}/{n}");
        let out = self.git(agent, ".", &["rev-parse", "--verify", "--quiet", &refname]).await?;
        if !out.status.success() {
            anyhow::bail!("checkpoint {n} not found");
        }
        Ok(refname)
    }

    pub(crate) async fn list_checkpoints(&self, agent: &str) -> anyhow::Result<Vec<CheckpointMeta>> {
        Self::validate_agent_name(agent)?;
        self.ensure_store().await?;
        let refs = self.git_ok(agent, ".", &[
            "for-each-ref", "--format=%(refname)", &format!("refs/checkpoints/{agent}"),
        ]).await.unwrap_or_default();

        let mut ns: Vec<usize> = refs.lines()
            .filter_map(|r| r.rsplit('/').next())
            .filter_map(|s| s.parse::<usize>().ok())
            .collect();
        ns.sort_unstable_by(|a, b| b.cmp(a)); // newest (наибольший n) первым

        let mut out = Vec::with_capacity(ns.len());
        for n in ns {
            let refname = format!("refs/checkpoints/{agent}/{n}");
            let commit = self.git_ok(agent, ".", &["rev-parse", &refname]).await?.trim().to_string();
            let created = self.git_ok(agent, ".", &[
                "show", "-s", "--format=%cI", &refname,
            ]).await?.trim().to_string();
            // shortstat этого снапшота относительно предыдущего (или пустого дерева).
            let prev = if n > 1 { format!("refs/checkpoints/{agent}/{}", n - 1) } else { EMPTY_TREE.to_string() };
            let summary = self.git_ok(agent, ".", &[
                "diff", "--shortstat", &prev, &refname,
            ]).await.unwrap_or_default().trim().to_string();
            out.push(CheckpointMeta { n, commit, created, summary });
        }
        Ok(out)
    }

    pub(crate) async fn diff(&self, agent: &str, workspace_dir: &str, n: usize) -> anyhow::Result<String> {
        let refname = self.resolve_n(agent, n).await?;
        self.git_ok(agent, workspace_dir, &["diff", &refname, "--", "."]).await
    }
```

- [ ] **Step 4: Запустить — тест зелёный**

Run: `cargo test --bin opex-core checkpoint_manager::tests::list_and_diff -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/agent/checkpoint_manager.rs
git commit -m "feat(checkpoint): list_checkpoints + diff"
```

---

### Task 5: `restore` (exact-tree) + валидация file-path

**Files:**
- Modify: `crates/opex-core/src/agent/checkpoint_manager.rs`
- Test: модуль `#[cfg(test)]` там же

**Interfaces:**
- Consumes: `resolve_n`, `commit_snapshot`, `git_ok`, `store_lock`.
- Produces:
  - `pub(crate) struct RestoreReport { pub n: usize, pub files: Vec<String>, pub new_checkpoint: Option<usize> }`
  - `pub(crate) async fn restore(&self, agent: &str, workspace_dir: &str, n: usize, file: Option<&str>) -> anyhow::Result<RestoreReport>`
  - `fn validate_rel_path(rel: &str) -> anyhow::Result<()>` (лексический anti-traversal: не absolute, без компонента `..`, не пустой)

- [ ] **Step 1: Падающие тесты — полный restore (exact-tree), один файл, traversal**

```rust
    #[tokio::test]
    async fn restore_exact_tree_and_single_file() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        let ws = tmp.path().join("ws");
        let m = mgr_at(&store);
        let agent = "Agent";
        let scope = ws.join("agents").join(agent);

        write_scope(&ws, agent, "a.md", "v1").await;
        m.ensure_checkpoint(agent, ws.to_str().unwrap()).await.unwrap(); // cp 1

        // правим a.md и добавляем новый файл b.md → cp 2
        write_scope(&ws, agent, "a.md", "v2").await;
        write_scope(&ws, agent, "b.md", "new").await;
        m.ensure_checkpoint(agent, ws.to_str().unwrap()).await.unwrap(); // cp 2

        // exact-tree restore к cp 1: a.md→v1, b.md удалён
        let rep = m.restore(agent, ws.to_str().unwrap(), 1, None).await.unwrap();
        assert_eq!(rep.n, 1);
        assert!(rep.new_checkpoint.is_some());
        assert_eq!(fs::read_to_string(scope.join("a.md")).await.unwrap(), "v1");
        assert!(!scope.join("b.md").exists(), "b.md должен быть удалён exact-tree restore");

        // single-file restore: вернуть только a.md из cp 2 (=v2)
        let rep2 = m.restore(agent, ws.to_str().unwrap(), 2, Some("a.md")).await.unwrap();
        assert_eq!(rep2.files, vec!["a.md".to_string()]);
        assert_eq!(fs::read_to_string(scope.join("a.md")).await.unwrap(), "v2");
    }

    #[tokio::test]
    async fn restore_rejects_traversal_and_bad_n() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        let ws = tmp.path().join("ws");
        let m = mgr_at(&store);
        let agent = "Agent";
        write_scope(&ws, agent, "a.md", "v1").await;
        m.ensure_checkpoint(agent, ws.to_str().unwrap()).await.unwrap();

        assert!(m.restore(agent, ws.to_str().unwrap(), 1, Some("../../etc/passwd")).await.is_err());
        assert!(m.restore(agent, ws.to_str().unwrap(), 99, None).await.is_err());
    }
```

- [ ] **Step 2: Запустить — FAIL**

Run: `cargo test --bin opex-core checkpoint_manager::tests::restore -- --nocapture`
Expected: FAIL.

- [ ] **Step 3: Реализация**

```rust
pub(crate) struct RestoreReport {
    pub n: usize,
    pub files: Vec<String>,
    pub new_checkpoint: Option<usize>,
}
```

```rust
    /// Лексический anti-traversal для restore-file (файл может НЕ существовать на диске,
    /// поэтому проверяем без canonicalize): не absolute, без компонента "..", не пустой.
    fn validate_rel_path(rel: &str) -> anyhow::Result<()> {
        let p = Path::new(rel);
        if rel.is_empty() || p.is_absolute() {
            anyhow::bail!("invalid restore path: {:?}", rel);
        }
        for comp in p.components() {
            use std::path::Component;
            match comp {
                Component::Normal(_) | Component::CurDir => {}
                _ => anyhow::bail!("path escapes scope: {:?}", rel),
            }
        }
        Ok(())
    }

    pub(crate) async fn restore(
        &self, agent: &str, workspace_dir: &str, n: usize, file: Option<&str>,
    ) -> anyhow::Result<RestoreReport> {
        if !self.config.enabled {
            anyhow::bail!("checkpoints disabled");
        }
        let _guard = self.store_lock.lock().await;
        let refname = self.resolve_n(agent, n).await?;
        let wt = workspace_dir;

        let files: Vec<String> = if let Some(f) = file {
            Self::validate_rel_path(f)?;
            self.git_ok(agent, wt, &["checkout", &refname, "--", f]).await?;
            vec![f.to_string()]
        } else {
            // exact-tree: индекс = дерево N, выписать в work-tree, вычистить добавленное после N.
            let changed = self.git_ok(agent, wt, &["diff", "--name-only", &refname, "--", "."])
                .await.unwrap_or_default()
                .lines().map(|s| s.to_string()).collect::<Vec<_>>();
            self.git_ok(agent, wt, &["read-tree", &refname]).await?;
            self.git_ok(agent, wt, &["checkout-index", "-f", "-a"]).await?;
            self.git_ok(agent, wt, &["clean", "-fd"]).await?;
            changed
        };

        // forward-only: зафиксировать состояние после отката новым чекпойнтом.
        let new_checkpoint = self.commit_snapshot(agent, wt, &format!("restore of {n}")).await?;
        Ok(RestoreReport { n, files, new_checkpoint })
    }
```

Примечание: `read-tree`+`checkout-index -f -a`+`clean -fd` под общим `GIT_INDEX_FILE`; `clean` уважает `info/exclude` (не сносит `node_modules` и т.п.). `commit_snapshot` уже берёт текущий индекс — но после restore индекс = дерево N; `add -A` внутри `commit_snapshot` пере-синхронизирует индекс с work-tree (идентично), снапшот корректен.

- [ ] **Step 4: Запустить — тесты зелёные**

Run: `cargo test --bin opex-core checkpoint_manager::tests::restore -- --nocapture`
Expected: PASS (2 теста).

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/agent/checkpoint_manager.rs
git commit -m "feat(checkpoint): exact-tree restore + single-file + anti-traversal"
```

---

### Task 6: `prune` (keep + ttl) + `new_turn`

**Files:**
- Modify: `crates/opex-core/src/agent/checkpoint_manager.rs`
- Test: модуль `#[cfg(test)]` там же

**Interfaces:**
- Consumes: `git`/`git_ok`, `store_lock`, `ensure_store`.
- Produces:
  - `pub(crate) async fn new_turn(&self, agent: &str) -> anyhow::Result<()>` (граница хода → prune)
  - `async fn prune(&self, agent: &str) -> anyhow::Result<()>`

- [ ] **Step 1: Падающие тесты — keep и ttl**

```rust
    #[tokio::test]
    async fn prune_by_keep() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        let ws = tmp.path().join("ws");
        let mut cfg = CheckpointConfig::default();
        cfg.store_path = store.to_str().unwrap().to_string();
        cfg.keep = 2;
        cfg.ttl_days = 3650; // не мешает count-cap
        let m = CheckpointManager::new(cfg);
        let agent = "Agent";

        for v in ["a", "b", "c"] {
            write_scope(&ws, agent, "f.md", v).await;
            m.ensure_checkpoint(agent, ws.to_str().unwrap()).await.unwrap();
        }
        // 3 чекпойнта, keep=2 → new_turn должен снести cp 1
        m.new_turn(agent).await.unwrap();
        let list = m.list_checkpoints(agent).await.unwrap();
        let ns: Vec<usize> = list.iter().map(|c| c.n).collect();
        assert_eq!(ns, vec![3, 2]);
        assert!(!store.join("refs/checkpoints/Agent/1").exists());
    }

    #[tokio::test]
    async fn prune_by_ttl() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        let ws = tmp.path().join("ws");
        let mut cfg = CheckpointConfig::default();
        cfg.store_path = store.to_str().unwrap().to_string();
        cfg.keep = 50;
        cfg.ttl_days = 7;
        let m = CheckpointManager::new(cfg);
        let agent = "Agent";

        // cp 1 с датой 30 дней назад (бэкдейт через GIT_COMMITTER_DATE напрямую).
        write_scope(&ws, agent, "f.md", "old").await;
        m.ensure_checkpoint(agent, ws.to_str().unwrap()).await.unwrap();
        // переписать дату коммита cp 1 на 30 дней назад вручную:
        let old_date = "2000-01-01T00:00:00 +0000";
        // создаём бэкдейтнутый коммит того же дерева и переставляем ref
        let tree = m.git_ok(agent, ws.to_str().unwrap(),
            &["rev-parse", "refs/checkpoints/Agent/1^{tree}"]).await.unwrap().trim().to_string();
        let commit = run_git_with_date(&store, &ws, agent, &tree, old_date).await;
        m.git_ok(agent, ws.to_str().unwrap(),
            &["update-ref", "refs/checkpoints/Agent/1", &commit]).await.unwrap();

        write_scope(&ws, agent, "f.md", "fresh").await;
        m.ensure_checkpoint(agent, ws.to_str().unwrap()).await.unwrap(); // cp 2 свежий

        m.new_turn(agent).await.unwrap();
        let list = m.list_checkpoints(agent).await.unwrap();
        let ns: Vec<usize> = list.iter().map(|c| c.n).collect();
        assert_eq!(ns, vec![2], "старше ttl_days cp 1 должен быть удалён");
    }

    // Тест-хелпер: коммит дерева с заданной committer/author датой.
    async fn run_git_with_date(
        store: &std::path::Path, ws: &std::path::Path, agent: &str, tree: &str, date: &str,
    ) -> String {
        let devnull = if cfg!(windows) { "NUL" } else { "/dev/null" };
        let out = tokio::process::Command::new("git")
            .env("GIT_DIR", store)
            .env("GIT_INDEX_FILE", store.join(format!("index-{agent}")))
            .env("GIT_WORK_TREE", ws.join("agents").join(agent))
            .env("GIT_CONFIG_GLOBAL", devnull).env("GIT_CONFIG_SYSTEM", devnull).env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "OPEX").env("GIT_AUTHOR_EMAIL", "checkpoint@opex.local")
            .env("GIT_COMMITTER_NAME", "OPEX").env("GIT_COMMITTER_EMAIL", "checkpoint@opex.local")
            .env("GIT_AUTHOR_DATE", date).env("GIT_COMMITTER_DATE", date)
            .args(["commit-tree", tree, "--no-gpg-sign", "-m", "backdated"])
            .output().await.unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }
```

- [ ] **Step 2: Запустить — FAIL**

Run: `cargo test --bin opex-core checkpoint_manager::tests::prune -- --nocapture`
Expected: FAIL.

- [ ] **Step 3: Реализация**

```rust
    /// Граница нового хода: prune старья. Best-effort.
    pub(crate) async fn new_turn(&self, agent: &str) -> anyhow::Result<()> {
        if !self.config.enabled {
            return Ok(());
        }
        let _guard = self.store_lock.lock().await;
        self.prune(agent).await
    }

    /// Удалить refs за пределом keep (по убыванию n) ИЛИ старше ttl_days (по commit-date).
    async fn prune(&self, agent: &str) -> anyhow::Result<()> {
        Self::validate_agent_name(agent)?;
        self.ensure_store().await?;

        let refs = self.git_ok(agent, ".", &[
            "for-each-ref", "--sort=-refname", "--format=%(refname) %(committerdate:unix)",
            &format!("refs/checkpoints/{agent}"),
        ]).await.unwrap_or_default();

        // Список (n, refname, ts) отсортирован по убыванию n.
        let mut entries: Vec<(usize, String, i64)> = refs.lines().filter_map(|line| {
            let mut it = line.split_whitespace();
            let refname = it.next()?.to_string();
            let ts: i64 = it.next()?.parse().ok()?;
            let n: usize = refname.rsplit('/').next()?.parse().ok()?;
            Some((n, refname, ts))
        }).collect();
        entries.sort_unstable_by(|a, b| b.0.cmp(&a.0));

        let now = chrono::Utc::now().timestamp();
        let ttl_secs = self.config.ttl_days as i64 * 86_400;
        let keep = self.config.keep as usize;

        let mut deleted = false;
        for (idx, (_n, refname, ts)) in entries.iter().enumerate() {
            let beyond_keep = idx >= keep;
            let too_old = ttl_secs > 0 && (now - *ts) > ttl_secs;
            if beyond_keep || too_old {
                self.git_ok(agent, ".", &["update-ref", "-d", refname]).await.ok();
                deleted = true;
            }
        }
        if deleted {
            self.git(agent, ".", &["gc", "--prune=now", "--quiet"]).await.ok();
            self.repair_bare_repo_dirs()?;
        }
        Ok(())
    }
```

Примечание: `--sort=-refname` сортирует лексикографически (для надёжной числовой сортировки дополнительно сортируем `entries` по `n`). После `gc` зовём `repair_bare_repo_dirs` (страховка от снесённых каталогов).

- [ ] **Step 4: Запустить — тесты зелёные**

Run: `cargo test --bin opex-core checkpoint_manager -- --nocapture`
Expected: PASS — все тесты модуля (Task 2–6).

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/agent/checkpoint_manager.rs
git commit -m "feat(checkpoint): prune (keep+ttl) + new_turn"
```

---

### Task 7: Проводка в `AgentConfig` + конструирование в `main.rs`

**Files:**
- Modify: `crates/opex-core/src/agent/agent_config.rs` (поле в struct + все конструкторы)
- Modify: `crates/opex-core/src/main.rs` (создать `Arc<CheckpointManager>`, прокинуть)
- Modify: все прочие сайты сборки `AgentConfig` (найти grep'ом)

**Interfaces:**
- Consumes: `CheckpointManager` (Task 6), `CheckpointConfig` (Task 1).
- Produces: `AgentConfig.checkpoint_manager: Option<std::sync::Arc<crate::agent::checkpoint_manager::CheckpointManager>>`. **Один и тот же** `Arc` у всех агентов (process-wide store-Mutex обязан быть общим).

- [ ] **Step 1: Найти все сайты сборки `AgentConfig`**

Run: `rg -n "AgentConfig \{" crates/opex-core/src`
Зафиксировать список (ожидается: загрузчик агентов в main.rs/agent loader, hot-start в CRUD-хендлере, тест-хелперы). Каждый сайт получит новое поле.

- [ ] **Step 2: Добавить поле в struct**

В `agent/agent_config.rs`, секция Infra (после `pub tool_exec_ctx: ...`):

```rust
    /// Process-wide shadow-git чекпойнты (общий instance на всех агентов).
    /// `None` при `[checkpoint] enabled=false` или если store недоступен на старте.
    pub checkpoint_manager: Option<std::sync::Arc<crate::agent::checkpoint_manager::CheckpointManager>>,
```

- [ ] **Step 3: Сконструировать manager в main.rs и прокинуть**

В `main.rs` рядом с `tool_exec_ctx` (main.rs:~590) добавить:

```rust
    let checkpoint_mgr = std::sync::Arc::new(
        crate::agent::checkpoint_manager::CheckpointManager::new(cfg.checkpoint.clone()),
    );
```

В каждом месте сборки `AgentConfig` в main.rs/agent-loader добавить поле
`checkpoint_manager: Some(checkpoint_mgr.clone()),`. Если `AgentConfig` собирается
через билдер/функцию-загрузчик, прокинуть `checkpoint_mgr` параметром в эту функцию.
Для **hot-start CRUD-сайта** (создание агента через API) — прокинуть тот же `Arc`
через `AgentDeps` (добавить поле `checkpoint_mgr: Arc<CheckpointManager>` в
`gateway::AgentDeps`, заполнить в main.rs, читать при сборке `AgentConfig`).

- [ ] **Step 4: Закрыть тест-хелперы и прочие сайты**

В тест-конструкторах `AgentConfig` (и любых не-prod сайтах из Step 1) добавить
`checkpoint_manager: None,`.

- [ ] **Step 5: Проверка компиляции + clippy**

Run: `cargo check --all-targets`
Expected: PASS (поле добавлено везде).
Run: `cargo clippy --all-targets -- -D warnings`
Expected: PASS — теперь методы менеджера достижимы из prod-кода (Task 8–10 их используют; на этом шаге достаточно, что поле и конструктор компилируются без dead_code на самом поле; если clippy ругается на неиспользуемые методы — это ОК закрыть в Task 8–10, но поле/конструктор/`new` должны быть чисты).

- [ ] **Step 6: Commit**

```bash
git add crates/opex-core/src/agent/agent_config.rs crates/opex-core/src/main.rs crates/opex-core/src/gateway/state.rs
git commit -m "feat(checkpoint): проводка CheckpointManager в AgentConfig + main.rs (общий Arc)"
```

---

### Task 8: Авто-`ensure_checkpoint` в мутирующих workspace-handlers

**Files:**
- Modify: `crates/opex-core/src/agent/tool_handlers/workspace.rs` (4 обёртки `SystemToolHandler::handle`)
- Test: `crates/opex-core/src/agent/pipeline/handlers.rs` (`#[cfg(test)]`) ИЛИ интеграционный тест в `tool_handlers/workspace.rs`

**Interfaces:**
- Consumes: `deps.cfg.checkpoint_manager` (Task 7), `deps.agent_name`, `deps.workspace_dir`.
- Produces: побочный эффект — снапшот перед мутацией. Без изменения сигнатур чистых `ph::handle_workspace_*`.

- [ ] **Step 1: Падающий тест — write через handler триггерит чекпойнт**

> Тест строит `ToolDeps` сложно; вместо этого тестируем чистую точку: добавить
> в `tool_handlers/workspace.rs` свободную функцию-хелпер `maybe_checkpoint` и
> покрыть её напрямую (а handler-обёртки её просто зовут).

```rust
#[cfg(test)]
mod cp_tests {
    use super::*;

    #[tokio::test]
    async fn maybe_checkpoint_snaps_then_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        let ws = tmp.path().join("ws");
        let mut cfg = crate::config::CheckpointConfig::default();
        cfg.store_path = store.to_str().unwrap().to_string();
        let mgr = std::sync::Arc::new(
            crate::agent::checkpoint_manager::CheckpointManager::new(cfg)
        );
        // подготовить scope
        let p = ws.join("agents").join("Agent").join("x.md");
        tokio::fs::create_dir_all(p.parent().unwrap()).await.unwrap();
        tokio::fs::write(&p, "v1").await.unwrap();

        maybe_checkpoint(&Some(mgr.clone()), "Agent", ws.to_str().unwrap()).await;
        assert!(store.join("refs/checkpoints/Agent/1").exists());

        // None-менеджер — не паникует
        maybe_checkpoint(&None, "Agent", ws.to_str().unwrap()).await;
    }
}
```

- [ ] **Step 2: Запустить — FAIL**

Run: `cargo test --bin opex-core maybe_checkpoint -- --nocapture`
Expected: FAIL — `maybe_checkpoint` не найдена.

- [ ] **Step 3: Реализация — хелпер + вызовы в 4 обёртках**

В `tool_handlers/workspace.rs` (верх файла, свободная функция):

```rust
/// Best-effort снапшот scope перед мутацией. Любая ошибка → warn, не блокирует.
pub(crate) async fn maybe_checkpoint(
    mgr: &Option<std::sync::Arc<crate::agent::checkpoint_manager::CheckpointManager>>,
    agent_name: &str,
    workspace_dir: &str,
) {
    if let Some(cm) = mgr {
        if let Err(e) = cm.ensure_checkpoint(agent_name, workspace_dir).await {
            tracing::warn!(agent = %agent_name, error = %e, "checkpoint ensure failed (non-fatal)");
        }
    }
}
```

В начале каждой из 4 обёрток (`WorkspaceWriteHandler`, `WorkspaceEditHandler`,
`WorkspaceDeleteHandler`, `WorkspaceRenameHandler`) `async fn handle(...)` — ПЕРЕД
вызовом `ph::handle_workspace_*`:

```rust
        maybe_checkpoint(&deps.cfg.checkpoint_manager, deps.agent_name, deps.workspace_dir).await;
```

(пример для write — остальные три аналогично, перед своим `ph::handle_workspace_{edit,delete,rename}`).

- [ ] **Step 4: Запустить — тест зелёный + сборка**

Run: `cargo test --bin opex-core maybe_checkpoint -- --nocapture`
Expected: PASS.
Run: `cargo check --all-targets`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/agent/tool_handlers/workspace.rs
git commit -m "feat(checkpoint): авто-снапшот перед workspace write/edit/delete/rename"
```

---

### Task 9: `new_turn` хук в bootstrap

**Files:**
- Modify: `crates/opex-core/src/agent/pipeline/bootstrap.rs` (после `claim_session_with_retry`)
- Test: smoke в том же файле (best-effort, без паники при `None`)

**Interfaces:**
- Consumes: `engine.cfg().checkpoint_manager` (Task 7), `engine.cfg().agent.name`.
- Produces: вызов `new_turn` на входе в ход (граница → prune).

- [ ] **Step 1: Реализация хука**

В `bootstrap()` (bootstrap.rs), сразу ПОСЛЕ успешного `claim_session_with_retry`
(там, где `session_id` уже занят), добавить:

```rust
    // Граница нового хода: prune старья (best-effort, не блокирует ход).
    if let Some(cm) = engine.cfg().checkpoint_manager.as_ref() {
        if let Err(e) = cm.new_turn(&engine.cfg().agent.name).await {
            tracing::warn!(error = %e, "checkpoint new_turn failed (non-fatal)");
        }
    }
```

- [ ] **Step 2: Проверка компиляции**

Run: `cargo check --all-targets`
Expected: PASS.

- [ ] **Step 3: Smoke-тест (best-effort при None)**

Если в bootstrap.rs нет лёгкого способа сконструировать `engine` — достаточно
покрытия `new_turn` юнит-тестом из Task 6 и проверки, что хук компилируется и
обёрнут в `if let Some` (не паникует при `None`). Зафиксировать в отчёте, что
прямой bootstrap-тест не добавлялся из-за тяжёлого конструктора `AgentEngine`
(покрытие — через unit `prune_*` + ручной smoke на сервере).

- [ ] **Step 4: Commit**

```bash
git add crates/opex-core/src/agent/pipeline/bootstrap.rs
git commit -m "feat(checkpoint): new_turn хук на входе в ход (prune)"
```

---

### Task 10: `/rollback` slash-команда

**Files:**
- Modify: `crates/opex-core/src/agent/pipeline/commands.rs` (парсер + arm `"/rollback"`)
- Test: `#[cfg(test)]` в `commands.rs` (парсер) + smoke на формат вывода

**Interfaces:**
- Consumes: `ctx.engine_arc` → `engine.cfg().checkpoint_manager` + `engine.cfg().workspace_dir` + `engine.cfg().agent.name`; методы `list_checkpoints`/`restore`/`diff` (Task 4–5).
- Produces:
  - `pub fn parse_rollback_command(arg: &str) -> RollbackCmd`
  - `pub enum RollbackCmd { List, To(usize), Diff(usize), File(usize, String) }`
  - arm `"/rollback"` в `handle_command` → `Option<Result<String>>`.

- [ ] **Step 1: Падающий тест — парсер**

В `#[cfg(test)] mod tests` в `commands.rs`:

```rust
    #[test]
    fn rollback_parse() {
        use super::{parse_rollback_command, RollbackCmd};
        assert!(matches!(parse_rollback_command(""), RollbackCmd::List));
        assert!(matches!(parse_rollback_command("list"), RollbackCmd::List));
        assert!(matches!(parse_rollback_command("2"), RollbackCmd::To(2)));
        assert!(matches!(parse_rollback_command("diff 3"), RollbackCmd::Diff(3)));
        match parse_rollback_command("2 file notes/x.md") {
            RollbackCmd::File(2, p) => assert_eq!(p, "notes/x.md"),
            other => panic!("unexpected: {other:?}"),
        }
        // мусор → List (безопасный дефолт)
        assert!(matches!(parse_rollback_command("garbage"), RollbackCmd::List));
    }
```

- [ ] **Step 2: Запустить — FAIL**

Run: `cargo test --bin opex-core rollback_parse -- --nocapture`
Expected: FAIL — `parse_rollback_command` не найдена.

- [ ] **Step 3: Реализация парсера**

В `commands.rs` (рядом с прочими парсерами, добавить `#[derive(Debug)]`):

```rust
#[derive(Debug)]
pub enum RollbackCmd {
    List,
    To(usize),
    Diff(usize),
    File(usize, String),
}

pub fn parse_rollback_command(arg: &str) -> RollbackCmd {
    let a = arg.trim();
    if a.is_empty() || a == "list" {
        return RollbackCmd::List;
    }
    if let Some(rest) = a.strip_prefix("diff ") {
        if let Ok(n) = rest.trim().parse::<usize>() {
            return RollbackCmd::Diff(n);
        }
        return RollbackCmd::List;
    }
    // "N file <path>"
    let mut it = a.split_whitespace();
    if let Some(first) = it.next() {
        if let Ok(n) = first.parse::<usize>() {
            match it.next() {
                Some("file") => {
                    let path = it.collect::<Vec<_>>().join(" ");
                    if !path.is_empty() {
                        return RollbackCmd::File(n, path);
                    }
                    return RollbackCmd::List;
                }
                None => return RollbackCmd::To(n),
                _ => return RollbackCmd::List,
            }
        }
    }
    RollbackCmd::List
}
```

- [ ] **Step 4: Реализация arm `"/rollback"` в `handle_command`**

В `match command { ... }` рядом с `"/compact"` добавить arm:

```rust
        "/rollback" => {
            let Some(engine) = ctx.engine_arc.clone() else {
                return Some(Ok("Откат недоступен в этом контексте.".to_string()));
            };
            let Some(cm) = engine.cfg().checkpoint_manager.clone() else {
                return Some(Ok("Чекпойнты отключены.".to_string()));
            };
            let ws = engine.cfg().workspace_dir.clone();
            let agent = engine.cfg().agent.name.clone();
            let cmd = parse_rollback_command(args);
            let result = match cmd {
                RollbackCmd::List => match cm.list_checkpoints(&agent).await {
                    Ok(list) if list.is_empty() => "Чекпойнтов нет.".to_string(),
                    Ok(list) => {
                        let mut s = String::from("Чекпойнты (свежие сверху):\n");
                        for c in list.iter().take(30) {
                            s.push_str(&format!("  {}. {}  {}\n", c.n, c.created, c.summary));
                        }
                        s.push_str("\n`/rollback N` — откат · `/rollback diff N` — показать · `/rollback N file <путь>` — один файл");
                        s
                    }
                    Err(e) => format!("Ошибка списка чекпойнтов: {e}"),
                },
                RollbackCmd::Diff(n) => match cm.diff(&agent, &ws, n).await {
                    Ok(d) if d.trim().is_empty() => format!("Чекпойнт {n}: отличий нет."),
                    Ok(d) => {
                        let body: String = d.lines().take(200).collect::<Vec<_>>().join("\n");
                        format!("Diff против чекпойнта {n}:\n```diff\n{body}\n```")
                    }
                    Err(e) => format!("Ошибка diff: {e}"),
                },
                RollbackCmd::To(n) => match cm.restore(&agent, &ws, n, None).await {
                    Ok(rep) => format!(
                        "Откат к чекпойнту {n} выполнен ({} файлов). Текущее состояние сохранено{}.",
                        rep.files.len(),
                        rep.new_checkpoint.map(|c| format!(" как чекпойнт {c}")).unwrap_or_default(),
                    ),
                    Err(e) => format!("Ошибка отката: {e}"),
                },
                RollbackCmd::File(n, path) => match cm.restore(&agent, &ws, n, Some(&path)).await {
                    Ok(_) => format!("Файл `{path}` восстановлен из чекпойнта {n}."),
                    Err(e) => format!("Ошибка отката файла: {e}"),
                },
            };
            Some(Ok(result))
        }
```

> Примечание: `/rollback` — admin/owner-операция пользователя; вывод plain-text
> (без новых ключей локализации — YAGNI). Если `handle_command` требует, чтобы все
> arm'ы использовали `s.<key>`, оставить plain-строки как здесь (другие arm'ы это
> допускают для динамического текста).

- [ ] **Step 5: Запустить — тест парсера зелёный + сборка + clippy**

Run: `cargo test --bin opex-core rollback_parse -- --nocapture`
Expected: PASS.
Run: `cargo check --all-targets && cargo clippy --all-targets -- -D warnings`
Expected: PASS (весь код менеджера теперь используется из prod-путей).

- [ ] **Step 6: Commit**

```bash
git add crates/opex-core/src/agent/pipeline/commands.rs
git commit -m "feat(checkpoint): /rollback slash-команда (list/N/diff/file)"
```

---

## Self-Review

**1. Spec coverage:**
- §1-2 store/env-isolation → Task 2 (`git`/`ensure_store`). ✓
- §3 scope=agents/{agent} → `work_tree()` (Task 2). ✓
- §4 excludes via info/exclude → `ensure_store` (Task 2). ✓
- §5 max_file_size → Task 3. ✓
- §6 per-n refs + parentless → Task 3. ✓
- §7-8 ленивый ensure_checkpoint + baseline → Task 3 + Task 8. ✓
- §9 /rollback → Task 10. ✓
- §10 API (ensure/list/restore/diff/new_turn) → Task 3-6. ✓
- §11 shared Arc placement → Task 7. ✓
- §12 валидация agent_name/N/file → Task 2 (`validate_agent_name`), Task 4 (`resolve_n`), Task 5 (`validate_rel_path`). ✓
- §13 exact-tree restore → Task 5. ✓
- §14 retention keep+ttl → Task 6. ✓
- §15 lazy prune в new_turn → Task 6 + Task 9. ✓
- §16 store-Mutex + logAllRefUpdates + repair → Task 2/3/6. ✓
- §17 конфиг → Task 1. ✓
- §18 best-effort → Task 8/9 (warn, не падает). ✓

**2. Placeholder scan:** код полный во всех шагах; нет «TBD»/«handle errors». Task 7 Step 1 — это «найти сайты grep'ом» (легитимный шаг discovery, не плейсхолдер; точный список зависит от кодовой базы).

**3. Type consistency:** `ensure_checkpoint`/`commit_snapshot` → `Option<usize>`; `restore` → `RestoreReport{n,files,new_checkpoint}`; `list_checkpoints` → `Vec<CheckpointMeta{n,commit,created,summary}>`; `RollbackCmd{List,To,Diff,File}` — консистентны между Task 3-6 и Task 10. `checkpoint_manager: Option<Arc<CheckpointManager>>` — одинаково в Task 7/8/9/10. ✓
