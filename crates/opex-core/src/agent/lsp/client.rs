//! LSP client: handshake, request/notify, server-request replies.
//!
//! # Concurrency notes
//!
//! The writer (`SharedWriter`) is shared between the public `request`/`notify`
//! methods and the read-loop task that replies to server-initiated requests
//! (e.g. `workspace/configuration`).  Both paths hold the tokio `Mutex` only
//! for the duration of a single `write_all` call — the lock is **released
//! before any other await**, so deadlock is structurally impossible.
//!
//! The diagnostics map uses a `std::sync::Mutex` (not tokio's) so that the
//! synchronous `take_diagnostics` method can call `.lock()` without `blocking_lock`,
//! which would panic inside a tokio runtime.  The read-loop holds the std
//! mutex only while doing a quick `.extend()` — no async ops inside the
//! critical section.

use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, AtomicI64, Ordering},
        Arc, Mutex as StdMutex,
    },
    time::Duration,
};

use anyhow::Context as _;
use dashmap::DashMap;
use serde_json::{json, Value};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::{oneshot, Mutex as TokioMutex},
};

use super::{
    framing::{encode_message, try_decode},
    jsonrpc::{notification, parse_incoming, request, response, Incoming, RequestId},
};

// ── type aliases ──────────────────────────────────────────────────────────────

/// Async-mutex-guarded boxed writer, shared between caller and read-loop.
type SharedWriter = Arc<TokioMutex<Box<dyn AsyncWrite + Unpin + Send>>>;

/// In-flight requests: id → reply channel.
type PendingMap = Arc<DashMap<RequestId, oneshot::Sender<anyhow::Result<Value>>>>;

/// Buffered diagnostics: uri → list of diagnostic objects.
type DiagMap = Arc<StdMutex<HashMap<String, Vec<Value>>>>;

// ── LspClient ─────────────────────────────────────────────────────────────────

/// An active connection to one LSP server process.
// Fields are consumed by Task 6/7/10 (LSP manager + tool handler). Allow until then.
#[allow(dead_code)]
pub struct LspClient {
    writer: SharedWriter,
    pending: PendingMap,
    diagnostics: DiagMap,
    next_id: Arc<AtomicI64>,
    alive: Arc<AtomicBool>,
    position_encoding: String,
    req_timeout: Duration,
    /// Tracks which document URIs have been opened in this session.
    opened: StdMutex<HashSet<String>>,
    /// Monotonically-increasing document version counter for didChange.
    open_version: AtomicI64,
    /// Keeps the read-loop running as long as this client exists.
    _read_task: tokio::task::JoinHandle<()>,
    /// Owns the language-server subprocess so it lives as long as this client.
    ///
    /// `None` for in-memory transports (tests using duplex).  When the
    /// `Arc<LspClient>` is dropped the `Child` drops too, and `kill_on_drop`
    /// cleanly terminates the server process.
    child: StdMutex<Option<tokio::process::Child>>,
}

#[allow(dead_code)]
impl LspClient {
    /// Connect to an LSP server over an arbitrary async reader/writer pair.
    ///
    /// 1. Spawns the background read-loop.
    /// 2. Sends `initialize` with UTF-8 position-encoding and
    ///    `workspace.configuration` capability advertised.
    /// 3. Awaits the response (bounded by `req_timeout`); stores negotiated
    ///    `positionEncoding` (defaults to `"utf-16"` if absent).
    /// 4. Sends `initialized` notification.
    pub async fn connect<R, W>(
        reader: R,
        writer: W,
        root_uri: &str,
        init_options: Value,
        req_timeout: Duration,
    ) -> anyhow::Result<LspClient>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let writer: SharedWriter = Arc::new(TokioMutex::new(Box::new(writer)));
        let pending: PendingMap = Arc::new(DashMap::new());
        let diagnostics: DiagMap = Arc::new(StdMutex::new(HashMap::new()));
        let next_id = Arc::new(AtomicI64::new(1));
        let alive = Arc::new(AtomicBool::new(true));

        // Spawn read-loop before sending initialize so that any server
        // messages that arrive early are not dropped.
        let read_task = tokio::spawn(read_loop(
            reader,
            Arc::clone(&writer),
            Arc::clone(&pending),
            Arc::clone(&diagnostics),
            Arc::clone(&alive),
        ));

        // ── initialize ─────────────────────────────────────────────────────────
        let init_id = next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        pending.insert(init_id, tx);

        let init_msg = request(
            init_id,
            "initialize",
            json!({
                "rootUri": root_uri,
                "capabilities": {
                    "general": { "positionEncodings": ["utf-8"] },
                    "workspace": { "configuration": true },
                    "textDocument": { "rename": { "dynamicRegistration": false } }
                },
                "initializationOptions": init_options
            }),
        );
        write_framed(&writer, &init_msg).await?;

        // Wait for the initialize response.
        let init_result = tokio::time::timeout(req_timeout, rx)
            .await
            .context("LSP initialize timed out")?
            .context("read-loop closed before initialize response")?
            .context("LSP initialize returned an error")?;

        let position_encoding = init_result
            .get("capabilities")
            .and_then(|c| c.get("positionEncoding"))
            .and_then(Value::as_str)
            .unwrap_or("utf-16")
            .to_owned();

        // ── initialized ────────────────────────────────────────────────────────
        write_framed(&writer, &notification("initialized", json!({}))).await?;

        Ok(LspClient {
            writer,
            pending,
            diagnostics,
            next_id,
            alive,
            position_encoding,
            req_timeout,
            opened: StdMutex::new(HashSet::new()),
            open_version: AtomicI64::new(2),
            _read_task: read_task,
            child: StdMutex::new(None),
        })
    }

    // ── public API ─────────────────────────────────────────────────────────────

    /// Send a request and await the response value.
    ///
    /// Fails with an error if the server returns an error object, the
    /// connection drops, or the timeout elapses.
    pub async fn request(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.insert(id, tx);

        let msg = request(id, method, params);
        if let Err(e) = write_framed(&self.writer, &msg).await {
            self.pending.remove(&id);
            return Err(e);
        }

        match tokio::time::timeout(self.req_timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => {
                self.pending.remove(&id);
                anyhow::bail!("LSP connection closed before response to '{}'", method)
            }
            Err(_) => {
                self.pending.remove(&id);
                anyhow::bail!(
                    "LSP request '{}' timed out after {:?}",
                    method,
                    self.req_timeout
                )
            }
        }
    }

    /// Send a notification (fire-and-forget, no response expected).
    #[allow(dead_code)]
    pub async fn notify(&self, method: &str, params: Value) -> anyhow::Result<()> {
        write_framed(&self.writer, &notification(method, params)).await
    }

    /// Drain and return all buffered diagnostics for `uri`.
    ///
    /// Returns an empty vec when there are none.
    pub fn take_diagnostics(&self, uri: &str) -> Vec<Value> {
        let mut guard = self.diagnostics.lock().expect("diagnostics lock poisoned");
        guard.remove(uri).unwrap_or_default()
    }

    /// `true` while the read-loop is running (server still connected).
    /// Flips to `false` on EOF or read error.
    #[allow(dead_code)]
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    /// The `positionEncoding` negotiated during `initialize`.
    /// Defaults to `"utf-16"` when the server omits the field.
    #[allow(dead_code)]
    pub fn position_encoding(&self) -> &str {
        &self.position_encoding
    }

    /// Store the language-server subprocess so its lifetime is tied to this
    /// client.  Call once after [`connect`] when using a real host process.
    ///
    /// The lock is taken and released synchronously — no await inside the
    /// critical section — so this is safe to call from any async context.
    pub fn attach_process(&self, child: tokio::process::Child) {
        let mut guard = self.child.lock().expect("child lock poisoned");
        *guard = Some(child);
    }

    // ── document tracking ──────────────────────────────────────────────────────

    /// Ensure `uri` is open in the server.
    ///
    /// * First time: sends `textDocument/didOpen` and records the URI.
    /// * Subsequent calls: sends `textDocument/didChange` (full-text, bumped version).
    ///
    /// The `StdMutex` is locked only synchronously to check/insert the URI,
    /// released before any `.await`, so the async invariant is upheld.
    async fn ensure_open(
        &self,
        uri: &str,
        text: &str,
        language_id: &str,
    ) -> anyhow::Result<()> {
        // Lock, check/insert, drop — no await inside the critical section.
        let is_new = {
            let mut guard = self
                .opened
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            guard.insert(uri.to_owned())
        };

        if is_new {
            self.notify(
                "textDocument/didOpen",
                json!({
                    "textDocument": {
                        "uri": uri,
                        "languageId": language_id,
                        "version": 1,
                        "text": text
                    }
                }),
            )
            .await
        } else {
            let version = self.open_version.fetch_add(1, Ordering::Relaxed);
            self.notify(
                "textDocument/didChange",
                json!({
                    "textDocument": { "uri": uri, "version": version },
                    "contentChanges": [{ "text": text }]
                }),
            )
            .await
        }
    }

    // ── high-level document operations ─────────────────────────────────────────

    /// Open/sync `uri`, wait `collect` for the server to publish diagnostics,
    /// then drain and return them.
    #[allow(dead_code)]
    pub async fn diagnostics(
        &self,
        uri: &str,
        text: &str,
        language_id: &str,
        collect: Duration,
    ) -> Vec<Value> {
        if self.ensure_open(uri, text, language_id).await.is_err() {
            return Vec::new();
        }
        tokio::time::sleep(collect).await;
        self.take_diagnostics(uri)
    }

    /// Go-to-definition for the symbol at `(line, character)`.
    #[allow(dead_code)]
    pub async fn definition(
        &self,
        uri: &str,
        text: &str,
        language_id: &str,
        line: u32,
        character: u32,
    ) -> anyhow::Result<Value> {
        self.ensure_open(uri, text, language_id).await?;
        self.request(
            "textDocument/definition",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character }
            }),
        )
        .await
    }

    /// Find all references for the symbol at `(line, character)`.
    #[allow(dead_code)]
    pub async fn references(
        &self,
        uri: &str,
        text: &str,
        language_id: &str,
        line: u32,
        character: u32,
    ) -> anyhow::Result<Value> {
        self.ensure_open(uri, text, language_id).await?;
        self.request(
            "textDocument/references",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character }
            }),
        )
        .await
    }

    /// Hover information for the symbol at `(line, character)`.
    #[allow(dead_code)]
    pub async fn hover(
        &self,
        uri: &str,
        text: &str,
        language_id: &str,
        line: u32,
        character: u32,
    ) -> anyhow::Result<Value> {
        self.ensure_open(uri, text, language_id).await?;
        self.request(
            "textDocument/hover",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character }
            }),
        )
        .await
    }

    /// List all symbols in `uri` (no position needed).
    #[allow(dead_code)]
    pub async fn document_symbols(
        &self,
        uri: &str,
        text: &str,
        language_id: &str,
    ) -> anyhow::Result<Value> {
        self.ensure_open(uri, text, language_id).await?;
        self.request(
            "textDocument/documentSymbol",
            json!({ "textDocument": { "uri": uri } }),
        )
        .await
    }

    /// Rename the symbol at `(line, character)` to `new_name`.
    ///
    /// Returns the `WorkspaceEdit` from the server.
    #[allow(dead_code)]
    pub async fn rename(
        &self,
        uri: &str,
        text: &str,
        language_id: &str,
        line: u32,
        character: u32,
        new_name: &str,
    ) -> anyhow::Result<Value> {
        self.ensure_open(uri, text, language_id).await?;
        self.request(
            "textDocument/rename",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
                "newName": new_name
            }),
        )
        .await
    }
}

// ── internal helpers ──────────────────────────────────────────────────────────

/// Encode `msg` as an LSP frame and write it through the shared writer.
///
/// The mutex is held only for the `write_all` call and released before any
/// other await point.
async fn write_framed(writer: &SharedWriter, msg: &str) -> anyhow::Result<()> {
    let frame = encode_message(msg);
    let mut w = writer.lock().await;
    w.write_all(&frame).await.context("LSP write error")
}

/// Background task: decode incoming frames and dispatch by message type.
///
/// * `Response`  → wake the matching oneshot in `pending`.
/// * `Notification` `textDocument/publishDiagnostics` → buffer diagnostics.
/// * `ServerRequest` → reply via the shared writer (must NOT hang).
///
/// Sets `alive = false` on EOF/error and drains `pending` with errors.
async fn read_loop<R: AsyncRead + Unpin>(
    mut reader: R,
    writer: SharedWriter,
    pending: PendingMap,
    diagnostics: DiagMap,
    alive: Arc<AtomicBool>,
) {
    let mut buf: Vec<u8> = Vec::new();
    let mut tmp = [0u8; 4096];

    'outer: loop {
        match reader.read(&mut tmp).await {
            Ok(0) | Err(_) => break 'outer,
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
        }

        // Process all complete frames in the buffer before reading again.
        while let Some(msg) = try_decode(&mut buf) {
            match parse_incoming(&msg) {
                Ok(Incoming::Response { id, result, error }) => {
                    if let Some((_, tx)) = pending.remove(&id) {
                        let payload = if let Some(e) = error {
                            Err(anyhow::anyhow!("LSP server error: {}", e))
                        } else {
                            Ok(result.unwrap_or(Value::Null))
                        };
                        let _ = tx.send(payload);
                    }
                }

                Ok(Incoming::Notification { method, params }) => {
                    if method == "textDocument/publishDiagnostics" {
                        let uri = params
                            .get("uri")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned();
                        let diags = params
                            .get("diagnostics")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default();
                        // std::sync::Mutex: lock, extend, drop — no await inside.
                        if let Ok(mut guard) = diagnostics.lock() {
                            guard.entry(uri).or_default().extend(diags);
                        }
                    }
                    // Unknown notifications are intentionally ignored.
                }

                Ok(Incoming::ServerRequest { id, method, params }) => {
                    // Reply so the server doesn't wait forever.
                    let result = if method == "workspace/configuration" {
                        let len = params
                            .get("items")
                            .and_then(Value::as_array)
                            .map(Vec::len)
                            .unwrap_or(0);
                        json!(vec![Value::Null; len])
                    } else {
                        Value::Null
                    };
                    // Best-effort: a write failure means the connection is
                    // dying; the next read iteration will hit EOF/error.
                    let _ = write_framed(&writer, &response(id, result)).await;
                }

                Err(e) => {
                    tracing::warn!("LSP: unparseable incoming message: {}", e);
                }
            }
        }
    }

    // Mark dead and wake any waiting callers.
    alive.store(false, Ordering::Relaxed);
    let keys: Vec<_> = pending.iter().map(|r| *r.key()).collect();
    for key in keys {
        if let Some((_, tx)) = pending.remove(&key) {
            let _ = tx.send(Err(anyhow::anyhow!("LSP connection closed")));
        }
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::lsp::framing;
    use serde_json::json;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Read one complete LSP frame from `r` into `buf`, returning the decoded JSON.
    async fn read_one_msg(
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

    /// Minimal mock LSP server that exercises the client's server-request reply
    /// path with **causal ordering**: the echo response is only sent *after* the
    /// mock has received and validated the client's reply to `workspace/configuration`.
    ///
    /// Protocol (sequential):
    /// 1. Receive `initialize` → reply with capabilities.
    /// 2. Send `workspace/configuration` (id 999) + `publishDiagnostics`.
    /// 3. Read the next client message and **assert** it is a response with
    ///    `id == 999` and `result == [null]`.
    /// 4. Receive `custom/echo` → reply only now (echo result).
    ///
    /// Because the echo reply is gated behind step 3, if the client fails to
    /// send a correct config reply the `client.request("custom/echo", …)` call
    /// will time out and the test will fail.
    async fn mock_server(mut r: tokio::io::DuplexStream, mut w: tokio::io::DuplexStream) {
        let mut buf = Vec::new();

        // ── Step 1: receive initialize ────────────────────────────────────────
        let init_msg = read_one_msg(&mut r, &mut buf).await.unwrap();
        assert_eq!(init_msg["method"], "initialize");
        let init_id = init_msg["id"].as_i64().unwrap();

        let _ = w
            .write_all(&framing::encode_message(
                &json!({
                    "jsonrpc": "2.0",
                    "id": init_id,
                    "result": {"capabilities": {}}
                })
                .to_string(),
            ))
            .await;

        // ── Step 2: fire server→client workspace/configuration + diagnostics ──
        // Send both back-to-back before reading the client's reply.
        let _ = w
            .write_all(&framing::encode_message(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 999,
                    "method": "workspace/configuration",
                    "params": {"items": [{}]}
                })
                .to_string(),
            ))
            .await;
        let _ = w
            .write_all(&framing::encode_message(
                &json!({
                    "jsonrpc": "2.0",
                    "method": "textDocument/publishDiagnostics",
                    "params": {
                        "uri": "file:///x",
                        "diagnostics": [{"message": "boom"}]
                    }
                })
                .to_string(),
            ))
            .await;

        // ── Steps 3 & 4: drain until we see the config reply ──────────────────
        // The client sends `initialized` (notification) at some point after
        // the init response is processed.  The read-loop's reply to
        // `workspace/configuration` may race with that notification.  We accept
        // them in either order, asserting on the config reply when we find it.
        //
        // This is the key assertion: the reply MUST be a Response with id==999
        // and result==[null].  If the client never sends it, read_one_msg
        // blocks until the duplex channel closes (echo reply never arrives →
        // client times out → test fails).
        let mut saw_config_reply = false;
        for _ in 0..3 {
            let msg = match read_one_msg(&mut r, &mut buf).await {
                Some(m) => m,
                None => break,
            };
            // `initialized` notification: no id, has method field — skip.
            if msg.get("id").is_none() {
                // notification — skip
                continue;
            }
            // A message with no "method" field is a Response.
            if msg.get("method").is_none() {
                assert_eq!(
                    msg["id"], 999,
                    "config reply id mismatch: {msg}"
                );
                assert_eq!(
                    msg["result"],
                    json!([null]),
                    "config reply result mismatch: {msg}"
                );
                saw_config_reply = true;
                break;
            }
            // Unexpected request — stop.
            panic!("unexpected server-bound request before config reply: {msg}");
        }
        assert!(saw_config_reply, "client never sent a reply to workspace/configuration");

        // ── Step 5: receive and dispatch further client requests ─────────────
        // Handle custom/echo, textDocument/definition, textDocument/rename, and
        // any stray notifications (e.g. `initialized`, `didOpen`).
        // Loop until the client side closes.
        loop {
            let msg = match read_one_msg(&mut r, &mut buf).await {
                Some(m) => m,
                None => break,
            };
            // Notifications have no id — skip silently.
            let id = match msg.get("id") {
                Some(v) => v.clone(),
                None => continue,
            };
            match msg["method"].as_str().unwrap_or("") {
                "custom/echo" => {
                    let _ = w
                        .write_all(&framing::encode_message(
                            &json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": {"echo": msg["params"]}
                            })
                            .to_string(),
                        ))
                        .await;
                }
                "textDocument/definition" => {
                    let _ = w
                        .write_all(&framing::encode_message(
                            &json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": [{"uri":"file:///x","range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}}}]
                            })
                            .to_string(),
                        ))
                        .await;
                }
                "textDocument/rename" => {
                    let _ = w
                        .write_all(&framing::encode_message(
                            &json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": {"changes":{"file:///x":[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":3}},"newText":"y"}]}}
                            })
                            .to_string(),
                        ))
                        .await;
                }
                _ => {
                    // Unknown requests get a null result.
                    let _ = w
                        .write_all(&framing::encode_message(
                            &json!({"jsonrpc":"2.0","id":id,"result":null}).to_string(),
                        ))
                        .await;
                }
            }
        }
    }

    #[tokio::test]
    async fn handshake_request_and_serverrequest_reply() {
        let (cr, sw) = tokio::io::duplex(8192);
        let (sr, cw) = tokio::io::duplex(8192);
        tokio::spawn(mock_server(sr, sw));

        let c = LspClient::connect(cr, cw, "file:///root", json!({}), Duration::from_secs(2))
            .await
            .unwrap();

        let res = c.request("custom/echo", json!({"a": 1})).await.unwrap();
        assert_eq!(res["echo"]["a"], 1);

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(c.take_diagnostics("file:///x")[0]["message"], "boom");
    }

    #[tokio::test]
    async fn connect_times_out_without_server() {
        let (cr, _sw) = tokio::io::duplex(8192);
        let (_sr, cw) = tokio::io::duplex(8192);
        assert!(
            LspClient::connect(cr, cw, "file:///root", json!({}), Duration::from_millis(200))
                .await
                .is_err()
        );
    }

    // ── Task-5 high-level operation tests ─────────────────────────────────────

    /// `definition()` must send `textDocument/didOpen` (first call) then
    /// `textDocument/definition` and return the server's location list.
    #[tokio::test]
    async fn definition_returns_location() {
        let (cr, sw) = tokio::io::duplex(65536);
        let (sr, cw) = tokio::io::duplex(65536);
        tokio::spawn(mock_server(sr, sw));

        let c = LspClient::connect(cr, cw, "file:///root", json!({}), Duration::from_secs(2))
            .await
            .unwrap();

        let locs = c
            .definition("file:///x", "fn foo(){}", "rust", 0, 3)
            .await
            .unwrap();

        // Server returns an array with one location object.
        assert!(locs.is_array(), "expected array, got {locs}");
        let arr = locs.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["uri"], "file:///x");
    }

    /// `rename()` must return the WorkspaceEdit the server replies with.
    #[tokio::test]
    async fn rename_returns_workspace_edit() {
        let (cr, sw) = tokio::io::duplex(65536);
        let (sr, cw) = tokio::io::duplex(65536);
        tokio::spawn(mock_server(sr, sw));

        let c = LspClient::connect(cr, cw, "file:///root", json!({}), Duration::from_secs(2))
            .await
            .unwrap();

        let edit = c
            .rename("file:///x", "fn foo(){}", "rust", 0, 3, "y")
            .await
            .unwrap();

        // WorkspaceEdit has a `changes` map.
        assert!(
            edit.get("changes").is_some(),
            "expected WorkspaceEdit with 'changes', got {edit}"
        );
        let new_text = edit["changes"]["file:///x"][0]["newText"].as_str().unwrap();
        assert_eq!(new_text, "y");
    }

    /// `diagnostics()` sends `didOpen` then returns buffered diagnostics.
    ///
    /// We pre-seed the diag buffer (the mock already pushed `boom` to `file:///x`
    /// during handshake), so `collect` can be very short.
    #[tokio::test]
    async fn diagnostics_returns_buffered_note() {
        let (cr, sw) = tokio::io::duplex(65536);
        let (sr, cw) = tokio::io::duplex(65536);
        tokio::spawn(mock_server(sr, sw));

        let c = LspClient::connect(cr, cw, "file:///root", json!({}), Duration::from_secs(2))
            .await
            .unwrap();

        // Give the read-loop a moment to buffer the diagnostic sent during handshake.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // diagnostics() sends didOpen, waits `collect`, then drains.
        let diags = c
            .diagnostics("file:///x", "fn foo(){}", "rust", Duration::from_millis(10))
            .await;

        assert!(!diags.is_empty(), "expected at least one diagnostic");
        assert_eq!(diags[0]["message"], "boom");
    }

    /// Calling `definition()` twice on the same URI: first → `didOpen`,
    /// second → `didChange` (no crash, version bumped).
    #[tokio::test]
    async fn second_call_sends_did_change() {
        let (cr, sw) = tokio::io::duplex(65536);
        let (sr, cw) = tokio::io::duplex(65536);
        tokio::spawn(mock_server(sr, sw));

        let c = LspClient::connect(cr, cw, "file:///root", json!({}), Duration::from_secs(2))
            .await
            .unwrap();

        // First call — didOpen + definition.
        c.definition("file:///x", "fn foo(){}", "rust", 0, 0)
            .await
            .unwrap();

        // Second call — didChange + definition.  Should not panic or error.
        let locs = c
            .definition("file:///x", "fn bar(){}", "rust", 0, 3)
            .await
            .unwrap();
        assert!(locs.is_array());
    }
}
