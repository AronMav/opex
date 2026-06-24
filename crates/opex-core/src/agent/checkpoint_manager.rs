//! Shadow-git checkpoint store. Снапшотит `agents/{agent}/` в отдельный bare-git
//! репозиторий (НЕ рабочий git проекта) перед правками агента; даёт откат.
//! Порт Hermes `tools/checkpoint_manager.py`. Best-effort: git-ошибки логируются,
//! ход агента не падает. Все store-мутации сериализованы `store_lock`.

use std::path::{Path, PathBuf};
use std::process::Output;

use crate::config::CheckpointConfig;

/// SHA пустого git-дерева (для diff первого чекпойнта).
pub(crate) const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// Метаданные одного чекпойнта (для list_checkpoints).
pub(crate) struct CheckpointMeta {
    pub n: usize,
    pub commit: String,
    pub created: String,
    pub summary: String,
}

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

    pub(crate) fn validate_agent_name(agent: &str) -> anyhow::Result<()> {
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

    /// git для ref/object-операций: только GIT_DIR + изоляция конфигов, БЕЗ
    /// GIT_WORK_TREE/GIT_INDEX_FILE. Безопасно когда scope-каталог агента не существует.
    async fn git_bare(&self, args: &[&str]) -> anyhow::Result<Output> {
        let devnull = if cfg!(windows) { "NUL" } else { "/dev/null" };
        let out = tokio::process::Command::new("git")
            .env("GIT_DIR", &self.store_path)
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

    /// git_bare, который падает (bail) при ненулевом статусе; возвращает stdout как String.
    async fn git_bare_ok(&self, args: &[&str]) -> anyhow::Result<String> {
        let out = self.git_bare(args).await?;
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
    ///
    /// **Lock-free по замыслу.** `store_lock` здесь намеренно НЕ захватывается:
    /// - мутирующие вызыватели (`ensure_checkpoint`, `prune`) держат `store_lock` при вызове,
    ///   добавление второго захвата вызвало бы reentrant-deadlock (tokio::Mutex не реентрантен);
    /// - read-only вызыватель (`list_checkpoints`) допускает безвредную идемпотентную гонку —
    ///   `git init`, `git config` и перезапись `info/exclude` идемпотентны.
    /// Это не противоречит доку модуля «все store-мутации сериализованы store_lock»: там речь
    /// о снапшотах/прунинге, а не о bootstrap-инициализации репо.
    pub(crate) async fn ensure_store(&self) -> anyhow::Result<()> {
        let devnull = if cfg!(windows) { "NUL" } else { "/dev/null" };
        if !self.store_path.join("HEAD").exists() {
            tokio::fs::create_dir_all(&self.store_path).await?;
            let out = tokio::process::Command::new("git")
                .arg("init").arg("--bare")
                .arg(&self.store_path)
                .env("GIT_CONFIG_GLOBAL", devnull)
                .env("GIT_CONFIG_SYSTEM", devnull)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output().await?;
            if !out.status.success() {
                anyhow::bail!("git init --bare failed: {}", String::from_utf8_lossy(&out.stderr));
            }
            for kv in [("gc.auto", "0"), ("core.logAllRefUpdates", "false")] {
                let out = tokio::process::Command::new("git")
                    .arg("--git-dir").arg(&self.store_path)
                    .arg("config").arg(kv.0).arg(kv.1)
                    .env("GIT_CONFIG_GLOBAL", devnull)
                    .env("GIT_CONFIG_SYSTEM", devnull)
                    .env("GIT_CONFIG_NOSYSTEM", "1")
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

    /// Наибольший существующий n для агента (0 если refs нет).
    async fn max_existing_n(&self, agent: &str) -> anyhow::Result<usize> {
        let refs = self.git_bare_ok(&[
            "for-each-ref", "--format=%(refname)",
            &format!("refs/checkpoints/{agent}/*"),
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
            let last_tree = self.git_bare_ok(&[
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

    /// Проверить, что чекпойнт n существует; вернуть полное имя ref.
    async fn resolve_n(&self, agent: &str, n: usize) -> anyhow::Result<String> {
        Self::validate_agent_name(agent)?;
        let refname = format!("refs/checkpoints/{agent}/{n}");
        let out = self.git_bare(&["rev-parse", "--verify", "--quiet", &refname]).await?;
        if !out.status.success() {
            anyhow::bail!("checkpoint {n} not found");
        }
        Ok(refname)
    }

    /// Вернуть список чекпойнтов агента, newest-first (по n убыв.).
    pub(crate) async fn list_checkpoints(&self, agent: &str) -> anyhow::Result<Vec<CheckpointMeta>> {
        Self::validate_agent_name(agent)?;
        self.ensure_store().await?;
        let refs = self.git_bare_ok(&[
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
            let commit = self.git_bare_ok(&["rev-parse", &refname]).await?.trim().to_string();
            let created = self.git_bare_ok(&[
                "show", "-s", "--format=%cI", &refname,
            ]).await?.trim().to_string();
            // shortstat этого снапшота относительно предыдущего (или пустого дерева).
            let prev = if n > 1 {
                format!("refs/checkpoints/{agent}/{}", n - 1)
            } else {
                EMPTY_TREE.to_string()
            };
            let summary = self.git_bare_ok(&[
                "diff", "--shortstat", &prev, &refname,
            ]).await.unwrap_or_default().trim().to_string();
            out.push(CheckpointMeta { n, commit, created, summary });
        }
        Ok(out)
    }

    /// Diff между чекпойнтом n и текущим состоянием workspace_dir.
    pub(crate) async fn diff(&self, agent: &str, workspace_dir: &str, n: usize) -> anyhow::Result<String> {
        let refname = self.resolve_n(agent, n).await?;
        self.git_ok(agent, workspace_dir, &["diff", &refname, "--", "."]).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CheckpointConfig;
    use tokio::fs;

    fn mgr_at(store: &std::path::Path) -> CheckpointManager {
        let mut cfg = CheckpointConfig::default();
        cfg.store_path = store.to_str().unwrap().to_string();
        CheckpointManager::new(cfg)
    }

    async fn write_scope(ws: &std::path::Path, agent: &str, rel: &str, content: &str) {
        let p = ws.join("agents").join(agent).join(rel);
        fs::create_dir_all(p.parent().unwrap()).await.unwrap();
        fs::write(p, content).await.unwrap();
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
}
