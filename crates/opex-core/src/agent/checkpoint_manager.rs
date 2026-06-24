//! Shadow-git checkpoint store. Снапшотит `agents/{agent}/` в отдельный bare-git
//! репозиторий (НЕ рабочий git проекта) перед правками агента; даёт откат.
//! Порт Hermes `tools/checkpoint_manager.py`. Best-effort: git-ошибки логируются,
//! ход агента не падает. Все store-мутации сериализованы `store_lock`.

use std::cmp::Reverse;
use std::path::{Path, PathBuf};
use std::process::Output;

use crate::config::CheckpointConfig;

/// SHA пустого git-дерева (для diff первого чекпойнта).
pub(crate) const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// Результат операции restore.
pub(crate) struct RestoreReport {
    /// Номер чекпойнта, к которому выполнен откат.
    pub n: usize,
    /// Файлы, затронутые операцией (changed при full restore; [file] при single-file).
    pub files: Vec<String>,
    /// Номер нового forward-чекпойнта («restore of n»), None если дерево не изменилось.
    pub new_checkpoint: Option<usize>,
}

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
    ///
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
                if let Ok(meta) = tokio::fs::metadata(wt_root.join(rel)).await
                    && meta.len() > limit
                {
                    self.git_ok(agent, wt, &["rm", "--cached", "--quiet", "--", rel]).await.ok();
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

    pub(crate) fn enabled(&self) -> bool {
        self.config.enabled
    }

    /// Вернуть список чекпойнтов агента, newest-first (по n убыв.).
    pub(crate) async fn list_checkpoints(&self, agent: &str) -> anyhow::Result<Vec<CheckpointMeta>> {
        if !self.config.enabled {
            return Ok(Vec::new());
        }
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
        if !self.config.enabled {
            anyhow::bail!("checkpoints disabled");
        }
        let refname = self.resolve_n(agent, n).await?;
        self.git_ok(agent, workspace_dir, &["diff", &refname, "--", "."]).await
    }

    // ── Restore ───────────────────────────────────────────────────────────────

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

    // ── Prune / new_turn ──────────────────────────────────────────────────────

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

        let refs = self.git_bare_ok(&[
            "for-each-ref", "--sort=-refname",
            "--format=%(refname) %(committerdate:unix)",
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
        entries.sort_unstable_by_key(|b| Reverse(b.0));

        let now = chrono::Utc::now().timestamp();
        let ttl_secs = self.config.ttl_days as i64 * 86_400;
        let keep = self.config.keep as usize;

        let mut deleted = false;
        for (idx, (_n, refname, ts)) in entries.iter().enumerate() {
            let beyond_keep = idx >= keep;
            let too_old = ttl_secs > 0 && (now - *ts) > ttl_secs;
            if beyond_keep || too_old {
                self.git_bare_ok(&["update-ref", "-d", refname]).await.ok();
                deleted = true;
            }
        }
        if deleted {
            self.git_bare(&["gc", "--prune=now", "--quiet"]).await.ok();
            self.repair_bare_repo_dirs()?;
        }
        Ok(())
    }

    /// Откатить workspace агента к чекпойнту n.
    ///
    /// - `file = None` — exact-tree restore: index := tree(n), checkout-index, clean untracked.
    /// - `file = Some(rel)` — single-file restore: только этот файл.
    ///
    /// После отката автоматически создаётся новый чекпойнт «restore of n» (forward-only).
    pub(crate) async fn restore(
        &self,
        agent: &str,
        workspace_dir: &str,
        n: usize,
        file: Option<&str>,
    ) -> anyhow::Result<RestoreReport> {
        if !self.config.enabled {
            anyhow::bail!("checkpoints disabled");
        }
        let _guard = self.store_lock.lock().await;
        let refname = self.resolve_n(agent, n).await?;
        let wt = workspace_dir;
        // Восстановить scope-каталог агента если был удалён — best-effort.
        tokio::fs::create_dir_all(self.work_tree(workspace_dir, agent)).await.ok();

        let files: Vec<String> = if let Some(f) = file {
            // single-file restore: anti-traversal сначала
            Self::validate_rel_path(f)?;
            self.git_ok(agent, wt, &["checkout", &refname, "--", f]).await?;
            vec![f.to_string()]
        } else {
            // exact-tree restore: собрать список изменённых файлов до отката
            let changed = self
                .git_ok(agent, wt, &["diff", "--name-only", &refname, "--", "."])
                .await
                .unwrap_or_default()
                .lines()
                .filter(|l| !l.is_empty())
                .map(|s| s.to_string())
                .collect::<Vec<_>>();

            // index := дерево чекпойнта n
            self.git_ok(agent, wt, &["read-tree", &refname]).await?;
            // выписать индекс в work-tree
            self.git_ok(agent, wt, &["checkout-index", "-f", "-a"]).await?;
            // удалить файлы, которых нет в индексе; excludes из info/exclude защищают артефакты
            self.git_ok(agent, wt, &["clean", "-fd"]).await?;
            changed
        };

        // forward-only: зафиксировать состояние после отката новым чекпойнтом
        let new_checkpoint = self.commit_snapshot(agent, wt, &format!("restore of {n}")).await?;
        Ok(RestoreReport { n, files, new_checkpoint })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CheckpointConfig;
    use tokio::fs;

    fn mgr_at(store: &std::path::Path) -> CheckpointManager {
        let cfg = CheckpointConfig { store_path: store.to_str().unwrap().to_string(), ..Default::default() };
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

    #[test]
    fn validate_rel_path_cases() {
        // ── reject: пустая строка ──────────────────────────────────────────────
        assert!(CheckpointManager::validate_rel_path("").is_err(), "empty must fail");

        // ── reject: unix absolute ──────────────────────────────────────────────
        assert!(CheckpointManager::validate_rel_path("/etc/passwd").is_err(), "/etc/passwd must fail");

        // ── reject: traversal с компонентом ".." ──────────────────────────────
        assert!(CheckpointManager::validate_rel_path("../secret").is_err(), "../secret must fail");
        assert!(CheckpointManager::validate_rel_path("a/../../b").is_err(), "a/../../b must fail");

        // ── accept: обычные относительные пути ────────────────────────────────
        assert!(CheckpointManager::validate_rel_path("a.md").is_ok(), "a.md must pass");
        assert!(CheckpointManager::validate_rel_path("notes/x.md").is_ok(), "notes/x.md must pass");
    }

    #[tokio::test]
    async fn prune_by_keep() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        let ws = tmp.path().join("ws");
        let cfg = CheckpointConfig {
            store_path: store.to_str().unwrap().to_string(),
            keep: 2,
            ttl_days: 3650, // не мешает count-cap
            ..Default::default()
        };
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
        let cfg = CheckpointConfig {
            store_path: store.to_str().unwrap().to_string(),
            keep: 50,
            ttl_days: 7,
            ..Default::default()
        };
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

    #[tokio::test]
    async fn disabled_list_returns_empty_and_no_store() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        let ws = tmp.path().join("ws");
        let cfg = CheckpointConfig {
            enabled: false,
            store_path: store.to_str().unwrap().to_string(),
            ..Default::default()
        };
        let m = CheckpointManager::new(cfg);
        let agent = "Agent";

        // list_checkpoints при disabled → пустой Vec, store НЕ создан
        let list = m.list_checkpoints(agent).await.unwrap();
        assert!(list.is_empty(), "disabled: list должен быть пустым");
        assert!(
            !store.join("HEAD").exists(),
            "disabled: store-каталог не должен быть создан"
        );

        // diff при disabled → ошибка, store по-прежнему не создан
        assert!(
            m.diff(agent, ws.to_str().unwrap(), 1).await.is_err(),
            "disabled: diff должен вернуть Err"
        );
        assert!(
            !store.join("HEAD").exists(),
            "disabled: store не должен появиться после diff"
        );
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

    #[tokio::test]
    async fn ensure_checkpoint_respects_excludes_and_size() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        let ws = tmp.path().join("ws");
        let cfg = CheckpointConfig {
            store_path: store.to_str().unwrap().to_string(),
            max_file_size_mb: 1,
            ..Default::default()
        };
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
