//! LSP server pool + lifecycle manager.
//!
//! # Design overview
//!
//! `LspManager` maintains a pool of `Arc<LspClient>` instances keyed by
//! `(agent_name, language, project_root)`.  The same language-server process is
//! reused across multiple tool calls as long as it is alive and the project root
//! matches.
//!
//! ## Broken-server back-off
//! When `factory.make()` fails, the key is inserted into a `broken` set with a
//! timestamp.  Subsequent calls within `broken_ttl` return an error immediately
//! without attempting to respawn, preventing tight restart loops.
//!
//! ## Cap per agent
//! Before spawning a **new** key for an agent, the pool is checked for that
//! agent's live entries.  If the count equals `max_servers_per_agent`, the
//! least-recently-used entry is evicted (Arc dropped → child killed) to stay
//! within the bound.
//!
//! ## Idle sweeper
//! `sweep_idle()` removes entries whose `last_used` is older than `idle_timeout`.
//! It drops the `Arc<LspClient>`, which kills the child via `kill_on_drop`.
//! A background loop is started via `spawn_sweeper` (called in Task 9 / main.rs).

use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Context as _;
use async_trait::async_trait;
use dashmap::DashMap;
use serde_json::Value;

use super::{
    client::LspClient,
    servers::{resolve_project_root, server_for_path, ServerDef},
    transport::spawn_server,
};
use crate::agent::workspace::read_workspace_file;

// ── LspAction ─────────────────────────────────────────────────────────────────

/// All LSP operations the manager can execute on behalf of the `lsp` agent tool.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum LspAction {
    /// Return diagnostics (errors/warnings) for the file.
    Diagnostics,
    /// Go-to-definition at the given cursor position.
    Definition { line: u32, character: u32 },
    /// Find all references at the given cursor position.
    References { line: u32, character: u32 },
    /// Hover documentation at the given cursor position.
    Hover { line: u32, character: u32 },
    /// List all symbols (outline) in the file.
    Symbols,
    /// Rename the symbol at the given position.
    ///
    /// Returns the raw `WorkspaceEdit` JSON; the caller (Task 10) applies it.
    Rename {
        line: u32,
        character: u32,
        new_name: String,
    },
}

// ── ClientFactory ─────────────────────────────────────────────────────────────

/// Factory for creating `LspClient` instances.
///
/// The real implementation spawns a subprocess; tests inject a fake.
#[async_trait]
pub trait ClientFactory: Send + Sync {
    async fn make(&self, def: &ServerDef, root: &Path) -> anyhow::Result<Arc<LspClient>>;
}

// ── HostClientFactory ─────────────────────────────────────────────────────────

/// Production factory: spawns the language-server process on the host and
/// connects via its stdio pipes.
pub struct HostClientFactory {
    req_timeout: Duration,
}

#[allow(dead_code)]
impl HostClientFactory {
    pub fn new(req_timeout: Duration) -> Self {
        Self { req_timeout }
    }
}

#[async_trait]
impl ClientFactory for HostClientFactory {
    async fn make(&self, def: &ServerDef, root: &Path) -> anyhow::Result<Arc<LspClient>> {
        let (_child, out, inp) = spawn_server(&def.command, root)
            .await
            .with_context(|| format!("spawn LSP server for {:?}", def.language))?;

        let root_uri = format!("file://{}", root.display());
        let client =
            LspClient::connect(out, inp, &root_uri, def.init_options.clone(), self.req_timeout)
                .await
                .with_context(|| format!("LSP handshake for {:?}", def.language))?;

        Ok(Arc::new(client))
    }
}

// ── Pool entry ────────────────────────────────────────────────────────────────

struct PoolEntry {
    client: Arc<LspClient>,
    last_used: Instant,
}

// ── LspManager ────────────────────────────────────────────────────────────────

/// Connection pool + lifecycle manager for LSP servers.
///
/// One `LspManager` is shared across all agents (via `Arc<LspManager>` in
/// `AppState`).  Concurrent callers are serialised per key by `DashMap`'s
/// per-shard locking; the happy path (reuse a live client) holds no lock across
/// an await point.
#[allow(dead_code)]
pub struct LspManager {
    /// Live clients: (agent, language, root_string) → entry.
    pool: DashMap<(String, String, String), PoolEntry>,
    /// Keys that recently failed to spawn.
    broken: DashMap<(String, String, String), Instant>,

    // ── config ────────────────────────────────────────────────────────────────
    /// How long to wait for individual LSP requests.
    request_timeout: Duration,
    /// Remove idle entries after this duration.
    idle_timeout: Duration,
    /// Do not retry a broken key within this window.
    broken_ttl: Duration,
    /// Maximum concurrent servers per agent name.
    max_servers_per_agent: usize,

    factory: Arc<dyn ClientFactory>,
}

#[allow(dead_code)]
impl LspManager {
    /// Create a new manager with the given configuration values.
    pub fn new(
        request_timeout: Duration,
        idle_timeout: Duration,
        broken_ttl: Duration,
        max_servers_per_agent: usize,
        factory: Arc<dyn ClientFactory>,
    ) -> Self {
        Self {
            pool: DashMap::new(),
            broken: DashMap::new(),
            request_timeout,
            idle_timeout,
            broken_ttl,
            max_servers_per_agent,
            factory,
        }
    }

    /// Create a manager with the real subprocess factory.
    pub fn with_host_factory(
        request_timeout: Duration,
        idle_timeout: Duration,
        broken_ttl: Duration,
        max_servers_per_agent: usize,
    ) -> Arc<Self> {
        Arc::new(Self::new(
            request_timeout,
            idle_timeout,
            broken_ttl,
            max_servers_per_agent,
            Arc::new(HostClientFactory::new(request_timeout)),
        ))
    }

    // ── public API ─────────────────────────────────────────────────────────────

    /// Execute `action` on the file at `file_rel` inside `workspace_dir/agents/{agent}/`.
    ///
    /// The manager:
    /// 1. Determines which LSP server covers the file extension.
    /// 2. Resolves the project root (bounded to the agent directory).
    /// 3. Gets or spawns a pooled `LspClient` for `(agent, language, root)`.
    /// 4. Reads the file content and calls the matching client operation.
    /// 5. Formats the result as human-readable text.
    pub async fn op(
        &self,
        agent: &str,
        workspace_dir: &str,
        file_rel: &str,
        action: LspAction,
    ) -> anyhow::Result<String> {
        // ── 1. resolve server + root ───────────────────────────────────────────
        let def = server_for_path(file_rel)
            .ok_or_else(|| anyhow::anyhow!("no language server for this file type: {file_rel}"))?;

        let root = resolve_project_root(
            workspace_dir,
            agent,
            file_rel,
            &def.root_markers,
        )
        .await
        .with_context(|| format!("resolve project root for {file_rel}"))?;

        let key = (
            agent.to_owned(),
            def.language.to_owned(),
            root.display().to_string(),
        );

        // ── 2. get or spawn client ─────────────────────────────────────────────
        let client = self.get_or_spawn(&key, &def, &root).await?;

        // ── 3. read file content + build URI ──────────────────────────────────
        let text = read_workspace_file(workspace_dir, agent, file_rel).await?;

        // Resolve the host-absolute path for the file URI.
        // `validate_workspace_path` returns the canonicalised abs path.
        let file_abs = {
            use crate::agent::workspace::validate_workspace_path;
            validate_workspace_path(workspace_dir, agent, file_rel).await?
        };
        let uri = format!("file://{}", file_abs.display());

        let language_id = def.language;

        // ── 4. dispatch action ────────────────────────────────────────────────
        let result = match action {
            LspAction::Diagnostics => {
                let collect = Duration::from_secs(2);
                let diags = client.diagnostics(&uri, &text, language_id, collect).await;
                format_diagnostics(&uri, &diags)
            }

            LspAction::Definition { line, character } => {
                let v = client.definition(&uri, &text, language_id, line, character).await?;
                format_locations(&v)
            }

            LspAction::References { line, character } => {
                let v = client.references(&uri, &text, language_id, line, character).await?;
                format_locations(&v)
            }

            LspAction::Hover { line, character } => {
                let v = client.hover(&uri, &text, language_id, line, character).await?;
                format_hover(&v)
            }

            LspAction::Symbols => {
                let v = client.document_symbols(&uri, &text, language_id).await?;
                format_symbols(&v)
            }

            LspAction::Rename { line, character, new_name } => {
                // Guard: refuse if server uses utf-16 positions (byte offsets would mismatch).
                if client.position_encoding() != "utf-8" {
                    anyhow::bail!(
                        "rename unavailable: server uses utf-16 positions"
                    );
                }
                let edit = client
                    .rename(&uri, &text, language_id, line, character, &new_name)
                    .await?;
                serde_json::to_string(&edit).context("serialize WorkspaceEdit")?
            }
        };

        Ok(result)
    }

    /// Remove entries whose `last_used` is older than `idle_timeout`.
    ///
    /// Dropping the `Arc<LspClient>` kills the server process (kill_on_drop).
    /// A clean LSP `shutdown` + `exit` sequence is not sent here (v1 acceptably
    /// drops the process; a future revision can add graceful shutdown).
    pub async fn sweep_idle(&self) {
        let threshold = Instant::now()
            .checked_sub(self.idle_timeout)
            .unwrap_or_else(Instant::now);

        let stale_keys: Vec<_> = self
            .pool
            .iter()
            .filter(|e| e.value().last_used < threshold)
            .map(|e| e.key().clone())
            .collect();

        for key in stale_keys {
            self.pool.remove(&key);
        }

        // Also expire broken-set entries.
        let broken_stale: Vec<_> = self
            .broken
            .iter()
            .filter(|e| e.value().elapsed() > self.broken_ttl)
            .map(|e| e.key().clone())
            .collect();
        for key in broken_stale {
            self.broken.remove(&key);
        }
    }

    /// Spawn a background loop that calls [`sweep_idle`] every 60 seconds.
    ///
    /// Call once in main.rs after constructing the manager.
    pub fn spawn_sweeper(self: Arc<Self>) {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            loop {
                interval.tick().await;
                self.sweep_idle().await;
            }
        });
    }

    // ── private helpers ────────────────────────────────────────────────────────

    /// Return a live `Arc<LspClient>` for `key`, spawning a new one if needed.
    ///
    /// The critical invariant: no lock is held across an `.await` point.
    async fn get_or_spawn(
        &self,
        key: &(String, String, String),
        def: &ServerDef,
        root: &Path,
    ) -> anyhow::Result<Arc<LspClient>> {
        // Fast path: live client in pool.
        if let Some(mut entry) = self.pool.get_mut(key)
            && entry.client.is_alive()
        {
            entry.last_used = Instant::now();
            return Ok(Arc::clone(&entry.client));
        }
        // Dead client (or no entry): fall through to respawn.

        // Remove dead entry if present (idempotent if already gone).
        self.pool.remove(key);

        // Check broken-set.
        if let Some(ts) = self.broken.get(key)
            && ts.elapsed() < self.broken_ttl
        {
            anyhow::bail!(
                "LSP server for {:?} at '{}' failed recently; \
                 retry in {:.0}s",
                key.1,
                key.2,
                (self.broken_ttl.saturating_sub(ts.elapsed())).as_secs_f64()
            );
        }
        self.broken.remove(key);

        // Enforce per-agent cap: if already at the limit, evict LRU.
        self.maybe_evict_lru_for_agent(&key.0);

        // Spawn new client.
        let client = match self.factory.make(def, root).await {
            Ok(c) => c,
            Err(e) => {
                self.broken.insert(key.clone(), Instant::now());
                return Err(e.context(format!(
                    "spawn LSP server for {:?} (marked broken for {:?})",
                    def.language, self.broken_ttl
                )));
            }
        };

        self.pool.insert(
            key.clone(),
            PoolEntry {
                client: Arc::clone(&client),
                last_used: Instant::now(),
            },
        );

        Ok(client)
    }

    /// If this agent already has `max_servers_per_agent` live entries, evict
    /// the one with the oldest `last_used`.
    fn maybe_evict_lru_for_agent(&self, agent: &str) {
        // Collect this agent's entries and find the LRU key.
        let agent_keys: Vec<_> = self
            .pool
            .iter()
            .filter(|e| e.key().0 == agent && e.value().client.is_alive())
            .map(|e| (e.key().clone(), e.value().last_used))
            .collect();

        if agent_keys.len() < self.max_servers_per_agent {
            return;
        }

        // Find the least-recently-used entry.
        let lru = agent_keys
            .iter()
            .min_by_key(|(_, last_used)| *last_used);

        if let Some((lru_key, _)) = lru {
            self.pool.remove(lru_key);
        }
    }
}

// ── Result formatters ─────────────────────────────────────────────────────────

/// Format LSP diagnostics as `path:line:col [severity] message (source)`.
///
/// LSP positions are 0-based; we display +1 for human readability.
/// LSP severity: 1=error, 2=warning, 3=information, 4=hint.
fn format_diagnostics(file_uri: &str, diags: &[Value]) -> String {
    if diags.is_empty() {
        return "No diagnostics.".to_owned();
    }

    let path = uri_to_display(file_uri);
    let mut lines = Vec::with_capacity(diags.len());

    for d in diags {
        let line = d["range"]["start"]["line"]
            .as_u64()
            .unwrap_or(0)
            + 1;
        let col = d["range"]["start"]["character"]
            .as_u64()
            .unwrap_or(0)
            + 1;
        let sev = match d["severity"].as_u64().unwrap_or(1) {
            1 => "error",
            2 => "warning",
            3 => "info",
            _ => "hint",
        };
        let msg = d["message"].as_str().unwrap_or("").trim();
        let src = d["source"].as_str().unwrap_or("");
        lines.push(format!("{path}:{line}:{col} [{sev}] {msg} ({src})"));
    }

    lines.join("\n")
}

/// Format an LSP location or location-array as `path:line:col` per entry.
fn format_locations(v: &Value) -> String {
    let locs = match v {
        Value::Array(a) => a.as_slice().to_vec(),
        Value::Object(_) => vec![v.clone()],
        _ => return "No locations found.".to_owned(),
    };

    if locs.is_empty() {
        return "No locations found.".to_owned();
    }

    locs.iter()
        .map(|loc| {
            let uri = loc["uri"]
                .as_str()
                .or_else(|| loc["targetUri"].as_str())
                .unwrap_or("?");
            let line = loc["range"]["start"]["line"]
                .as_u64()
                .or_else(|| loc["targetSelectionRange"]["start"]["line"].as_u64())
                .unwrap_or(0)
                + 1;
            let col = loc["range"]["start"]["character"]
                .as_u64()
                .or_else(|| loc["targetSelectionRange"]["start"]["character"].as_u64())
                .unwrap_or(0)
                + 1;
            format!("{}:{line}:{col}", uri_to_display(uri))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Extract the text content from an LSP hover result.
fn format_hover(v: &Value) -> String {
    if v.is_null() {
        return "No hover information.".to_owned();
    }
    // `contents` can be a string, `{ value }`, or `{ kind, value }`, or an array.
    let contents = &v["contents"];
    match contents {
        Value::String(s) => s.clone(),
        Value::Object(_) => contents["value"]
            .as_str()
            .unwrap_or_else(|| contents.as_str().unwrap_or(""))
            .to_owned(),
        Value::Array(arr) => arr
            .iter()
            .map(|c| {
                if let Some(s) = c.as_str() {
                    s.to_owned()
                } else {
                    c["value"].as_str().unwrap_or("").to_owned()
                }
            })
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => "No hover information.".to_owned(),
    }
}

/// Format document symbols as an outline: `name (kind) — line N`.
fn format_symbols(v: &Value) -> String {
    let syms = match v {
        Value::Array(a) => a.clone(),
        _ => return "No symbols found.".to_owned(),
    };
    if syms.is_empty() {
        return "No symbols found.".to_owned();
    }

    let mut out = Vec::new();
    collect_symbols(&syms, 0, &mut out);
    out.join("\n")
}

fn collect_symbols(syms: &[Value], depth: usize, out: &mut Vec<String>) {
    let indent = "  ".repeat(depth);
    for sym in syms {
        let name = sym["name"].as_str().unwrap_or("?");
        let kind = symbol_kind(sym["kind"].as_u64().unwrap_or(0));
        let line = sym["range"]["start"]["line"]
            .as_u64()
            .unwrap_or(0)
            + 1;
        out.push(format!("{indent}{name} ({kind}) — line {line}"));

        // Recurse into children (DocumentSymbol has children, SymbolInformation does not).
        if let Some(children) = sym["children"].as_array() {
            collect_symbols(children, depth + 1, out);
        }
    }
}

/// Convert a numeric LSP symbol kind to a short label.
fn symbol_kind(k: u64) -> &'static str {
    match k {
        1 => "file",
        2 => "module",
        3 => "namespace",
        4 => "package",
        5 => "class",
        6 => "method",
        7 => "property",
        8 => "field",
        9 => "constructor",
        10 => "enum",
        11 => "interface",
        12 => "function",
        13 => "variable",
        14 => "constant",
        15 => "string",
        16 => "number",
        17 => "boolean",
        18 => "array",
        19 => "object",
        20 => "key",
        21 => "null",
        22 => "enum_member",
        23 => "struct",
        24 => "event",
        25 => "operator",
        26 => "type_parameter",
        _ => "symbol",
    }
}

/// Strip `file://` prefix for compact display, keeping the host-absolute path.
fn uri_to_display(uri: &str) -> &str {
    uri.strip_prefix("file://").unwrap_or(uri)
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::lsp::{client::LspClient, framing};
    use async_trait::async_trait;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    // ── minimal mock LSP server (same protocol as client.rs tests) ────────────

    /// Spawn a mock server task over a duplex pair and return a connected
    /// `LspClient` in ~50ms.
    async fn mock_client() -> Arc<LspClient> {
        let (cr, sw) = tokio::io::duplex(65536);
        let (sr, cw) = tokio::io::duplex(65536);
        tokio::spawn(mock_server(sr, sw));
        let client =
            LspClient::connect(cr, cw, "file:///root", serde_json::json!({}), Duration::from_secs(5))
                .await
                .expect("mock client connect");
        Arc::new(client)
    }

    /// Minimal mock LSP server: handles `initialize`, then echoes null to
    /// everything.
    async fn mock_server(mut r: tokio::io::DuplexStream, mut w: tokio::io::DuplexStream) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let mut buf: Vec<u8> = Vec::new();
        let mut tmp = [0u8; 4096];

        // Read one frame.
        async fn read_one(
            r: &mut tokio::io::DuplexStream,
            buf: &mut Vec<u8>,
        ) -> Option<serde_json::Value> {
            let mut tmp = [0u8; 4096];
            loop {
                if let Some(msg) = framing::try_decode(buf) {
                    return serde_json::from_str(&msg).ok();
                }
                match r.read(&mut tmp).await {
                    Ok(0) | Err(_) => return None,
                    Ok(n) => buf.extend_from_slice(&tmp[..n]),
                }
            }
        }

        // initialize
        let init = read_one(&mut r, &mut buf).await.unwrap();
        let init_id = init["id"].as_i64().unwrap();
        let _ = w
            .write_all(&framing::encode_message(
                &serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": init_id,
                    "result": { "capabilities": { "positionEncoding": "utf-8" } }
                })
                .to_string(),
            ))
            .await;

        // drain all remaining requests with null results
        loop {
            // Drain buffered + incoming bytes.
            let _ = tokio::time::timeout(Duration::from_millis(200), r.read(&mut tmp)).await;
            while let Some(msg) = framing::try_decode(&mut buf) {
                let v: serde_json::Value = match serde_json::from_str(&msg) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let id = match v.get("id") {
                    Some(id) => id.clone(),
                    None => continue, // notification
                };
                if v.get("method").is_some() {
                    // It's a request — reply null.
                    let _ = w
                        .write_all(&framing::encode_message(
                            &serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": null
                            })
                            .to_string(),
                        ))
                        .await;
                }
            }
        }
    }

    // ── fake ClientFactory ────────────────────────────────────────────────────

    /// Factory that always returns a fresh mock client.
    struct FakeFactory {
        make_count: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ClientFactory for FakeFactory {
        async fn make(&self, _def: &ServerDef, _root: &Path) -> anyhow::Result<Arc<LspClient>> {
            self.make_count.fetch_add(1, Ordering::SeqCst);
            Ok(mock_client().await)
        }
    }

    /// Factory that always errors.
    struct FailFactory {
        make_count: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ClientFactory for FailFactory {
        async fn make(&self, _def: &ServerDef, _root: &Path) -> anyhow::Result<Arc<LspClient>> {
            self.make_count.fetch_add(1, Ordering::SeqCst);
            anyhow::bail!("intentional factory failure")
        }
    }

    // ── helpers ───────────────────────────────────────────────────────────────

    /// Create a temporary workspace with a Python file so `server_for_path` and
    /// `resolve_project_root` succeed.
    fn make_workspace() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        let agent_dir = ws.join("agents").join("TestAgent");
        std::fs::create_dir_all(&agent_dir).unwrap();
        // pyproject.toml as project marker
        std::fs::write(agent_dir.join("pyproject.toml"), "").unwrap();
        // the Python file the tests operate on
        std::fs::write(agent_dir.join("app.py"), "x = 1\n").unwrap();
        tmp
    }

    fn manager_with(factory: Arc<dyn ClientFactory>) -> LspManager {
        LspManager::new(
            Duration::from_secs(5),  // request_timeout
            Duration::from_secs(60), // idle_timeout
            Duration::from_secs(30), // broken_ttl
            4,                       // max_servers_per_agent
            factory,
        )
    }

    // ── Test 1: reuses pooled client for same key ─────────────────────────────

    #[tokio::test]
    async fn reuses_pooled_client_for_same_key() {
        let count = Arc::new(AtomicUsize::new(0));
        let factory = Arc::new(FakeFactory {
            make_count: Arc::clone(&count),
        });
        let mgr = manager_with(factory);
        let tmp = make_workspace();
        let ws = tmp.path().to_str().unwrap();

        // First op — must spawn a new client.
        let _ = mgr
            .op("TestAgent", ws, "app.py", LspAction::Symbols)
            .await;

        // Second op on the same (agent, language, root) — must reuse.
        let _ = mgr
            .op("TestAgent", ws, "app.py", LspAction::Symbols)
            .await;

        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "factory.make() should be called exactly once for the same key"
        );
    }

    // ── Test 2: broken spawn not retried within TTL ───────────────────────────

    #[tokio::test]
    async fn broken_spawn_not_retried_within_ttl() {
        let count = Arc::new(AtomicUsize::new(0));
        let factory = Arc::new(FailFactory {
            make_count: Arc::clone(&count),
        });
        let mgr = manager_with(factory);
        let tmp = make_workspace();
        let ws = tmp.path().to_str().unwrap();

        // First op — factory errs, key goes into broken set.
        let r1 = mgr.op("TestAgent", ws, "app.py", LspAction::Symbols).await;
        assert!(r1.is_err(), "expected error on first op");
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "factory.make() must be called once on first failure"
        );

        // Second op within broken_ttl — must fail WITHOUT another make attempt.
        let r2 = mgr.op("TestAgent", ws, "app.py", LspAction::Symbols).await;
        assert!(r2.is_err(), "expected error on second op within broken_ttl");
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "factory.make() must NOT be called again within broken_ttl"
        );
    }

    // ── Test 3: LRU eviction when per-agent cap is reached ───────────────────

    #[tokio::test]
    async fn lru_eviction_when_cap_reached() {
        let count = Arc::new(AtomicUsize::new(0));
        let factory = Arc::new(FakeFactory {
            make_count: Arc::clone(&count),
        });
        // cap = 1 to make it easy to trigger eviction
        let mgr = LspManager::new(
            Duration::from_secs(5),
            Duration::from_secs(60),
            Duration::from_secs(30),
            1, // max 1 server per agent
            factory,
        );

        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        // Two distinct roots for the same agent + same language (python).
        let proj_a = ws.join("agents").join("A").join("proj_a");
        let proj_b = ws.join("agents").join("A").join("proj_b");
        std::fs::create_dir_all(&proj_a).unwrap();
        std::fs::create_dir_all(&proj_b).unwrap();
        std::fs::write(proj_a.join("pyproject.toml"), "").unwrap();
        std::fs::write(proj_b.join("pyproject.toml"), "").unwrap();
        std::fs::write(proj_a.join("a.py"), "x=1\n").unwrap();
        std::fs::write(proj_b.join("b.py"), "y=2\n").unwrap();

        // First op on proj_a — spawns server #1.
        let _ = mgr.op("A", ws.to_str().unwrap(), "proj_a/a.py", LspAction::Symbols).await;
        assert_eq!(count.load(Ordering::SeqCst), 1);

        // Second op on proj_b — cap=1, so must evict proj_a and spawn server #2.
        let _ = mgr.op("A", ws.to_str().unwrap(), "proj_b/b.py", LspAction::Symbols).await;
        assert_eq!(count.load(Ordering::SeqCst), 2, "should spawn a 2nd server");

        // Pool must contain exactly 1 entry.
        assert_eq!(mgr.pool.len(), 1, "pool should have exactly 1 entry after eviction");
    }

    // ── Test 4: idle sweeper removes old entries ──────────────────────────────

    #[tokio::test]
    async fn sweep_removes_idle_entries() {
        let count = Arc::new(AtomicUsize::new(0));
        let factory = Arc::new(FakeFactory {
            make_count: Arc::clone(&count),
        });
        // idle_timeout = 1ms so everything is immediately stale
        let mgr = LspManager::new(
            Duration::from_secs(5),
            Duration::from_millis(1),
            Duration::from_secs(30),
            4,
            factory,
        );

        let tmp = make_workspace();
        let ws = tmp.path().to_str().unwrap();

        let _ = mgr.op("TestAgent", ws, "app.py", LspAction::Symbols).await;
        assert_eq!(mgr.pool.len(), 1);

        // Wait past idle_timeout then sweep.
        tokio::time::sleep(Duration::from_millis(5)).await;
        mgr.sweep_idle().await;

        assert_eq!(mgr.pool.len(), 0, "pool should be empty after sweep");
    }

    // ── Unit tests for formatters ─────────────────────────────────────────────

    #[test]
    fn format_diags_empty() {
        assert_eq!(format_diagnostics("file:///x", &[]), "No diagnostics.");
    }

    #[test]
    fn format_diags_error() {
        let d = serde_json::json!({
            "range": {"start": {"line": 2, "character": 4}},
            "severity": 1,
            "message": "undefined name",
            "source": "pyright"
        });
        let out = format_diagnostics("file:///foo/bar.py", &[d]);
        assert_eq!(out, "/foo/bar.py:3:5 [error] undefined name (pyright)");
    }

    #[test]
    fn format_locations_array() {
        let locs = serde_json::json!([{
            "uri": "file:///a/b.py",
            "range": {"start": {"line": 9, "character": 0}}
        }]);
        let out = format_locations(&locs);
        assert_eq!(out, "/a/b.py:10:1");
    }

    #[test]
    fn format_hover_string_contents() {
        let v = serde_json::json!({"contents": "hello world"});
        assert_eq!(format_hover(&v), "hello world");
    }

    #[test]
    fn format_symbols_nested() {
        let v = serde_json::json!([{
            "name": "MyClass",
            "kind": 5,
            "range": {"start": {"line": 0, "character": 0}},
            "children": [{
                "name": "my_method",
                "kind": 6,
                "range": {"start": {"line": 2, "character": 4}}
            }]
        }]);
        let out = format_symbols(&v);
        assert!(out.contains("MyClass (class) — line 1"));
        assert!(out.contains("  my_method (method) — line 3"));
    }
}
