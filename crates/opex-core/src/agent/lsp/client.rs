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
    collections::HashMap,
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
// Fields are consumed by Task 5/6 (LSP manager + tool handler). Allow until then.
#[allow(dead_code)]
pub struct LspClient {
    writer: SharedWriter,
    pending: PendingMap,
    diagnostics: DiagMap,
    next_id: Arc<AtomicI64>,
    alive: Arc<AtomicBool>,
    position_encoding: String,
    req_timeout: Duration,
    /// Keeps the read-loop running as long as this client exists.
    _read_task: tokio::task::JoinHandle<()>,
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
                    "workspace": { "configuration": true }
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
            _read_task: read_task,
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

    /// Minimal mock LSP server exercising:
    /// - `initialize` request/response
    /// - A server→client `workspace/configuration` request (client must reply)
    /// - A `textDocument/publishDiagnostics` notification
    /// - Any other request → echo params
    async fn mock_server(mut r: tokio::io::DuplexStream, mut w: tokio::io::DuplexStream) {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        let mut sent_extra = false;

        loop {
            let n = match r.read(&mut tmp).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            buf.extend_from_slice(&tmp[..n]);

            while let Some(msg) = framing::try_decode(&mut buf) {
                let v: serde_json::Value = serde_json::from_str(&msg).unwrap();
                let method = v["method"].as_str().unwrap_or("");

                if let Some(id) = v.get("id").and_then(|x| x.as_i64()) {
                    // Client reply to our server-request has id+result and no method.
                    if v.get("result").is_some() && method.is_empty() {
                        continue;
                    }

                    let result = if method == "initialize" {
                        json!({"capabilities": {}})
                    } else {
                        json!({"echo": v["params"]})
                    };

                    let _ = w
                        .write_all(&framing::encode_message(
                            &json!({"jsonrpc":"2.0","id":id,"result":result}).to_string(),
                        ))
                        .await;

                    if method == "initialize" && !sent_extra {
                        sent_extra = true;
                        // server→client REQUEST: client must reply or it'd hang
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
                        // diagnostics notification
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
                    }
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
}
