//! Minimal JSON-RPC 2.0 for the LSP client.
use serde_json::{json, Value};

#[allow(dead_code)]
pub type RequestId = i64;

#[allow(dead_code)]
pub fn request(id: RequestId, method: &str, params: Value) -> String {
    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}).to_string()
}

#[allow(dead_code)]
pub fn notification(method: &str, params: Value) -> String {
    json!({"jsonrpc":"2.0","method":method,"params":params}).to_string()
}

#[allow(dead_code)]
pub fn response(id: RequestId, result: Value) -> String {
    json!({"jsonrpc":"2.0","id":id,"result":result}).to_string()
}

#[allow(dead_code)]
#[derive(Debug)]
pub enum Incoming {
    Response {
        id: RequestId,
        result: Option<Value>,
        error: Option<Value>,
    },
    Notification {
        method: String,
        #[allow(dead_code)]
        params: Value,
    },
    ServerRequest {
        id: RequestId,
        method: String,
        #[allow(dead_code)]
        params: Value,
    },
}

#[allow(dead_code)]
pub fn parse_incoming(s: &str) -> anyhow::Result<Incoming> {
    let v: Value = serde_json::from_str(s)?;
    let id = v.get("id").and_then(Value::as_i64);
    let method = v.get("method").and_then(Value::as_str).map(str::to_string);
    match (id, method) {
        (Some(id), Some(method)) => Ok(Incoming::ServerRequest {
            id,
            method,
            params: v.get("params").cloned().unwrap_or(Value::Null),
        }),
        (Some(id), None) => Ok(Incoming::Response {
            id,
            result: v.get("result").cloned(),
            error: v.get("error").cloned(),
        }),
        (None, Some(method)) => Ok(Incoming::Notification {
            method,
            params: v.get("params").cloned().unwrap_or(Value::Null),
        }),
        (None, None) => anyhow::bail!("invalid JSON-RPC message: no id and no method"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_shape() {
        let v: serde_json::Value =
            serde_json::from_str(&request(7, "initialize", json!({"x": 1}))).unwrap();
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["id"], 7);
        assert_eq!(v["method"], "initialize");
        assert_eq!(v["params"]["x"], 1);
    }

    #[test]
    fn notification_has_no_id() {
        let v: serde_json::Value =
            serde_json::from_str(&notification("initialized", json!({}))).unwrap();
        assert!(v.get("id").is_none());
    }

    #[test]
    fn response_shape() {
        let v: serde_json::Value =
            serde_json::from_str(&response(7, json!(null))).unwrap();
        assert_eq!(v["id"], 7);
        assert!(v.get("result").is_some());
    }

    #[test]
    fn parses_response() {
        match parse_incoming(r#"{"jsonrpc":"2.0","id":7,"result":{"ok":true}}"#).unwrap() {
            Incoming::Response {
                id,
                result,
                error,
            } => {
                assert_eq!(id, 7);
                assert_eq!(result.unwrap()["ok"], true);
                assert!(error.is_none());
            }
            _ => panic!(),
        }
    }

    #[test]
    fn parses_notification() {
        match parse_incoming(r#"{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{}}"#)
            .unwrap()
        {
            Incoming::Notification { method, .. } => {
                assert_eq!(method, "textDocument/publishDiagnostics")
            }
            _ => panic!(),
        }
    }

    #[test]
    fn id_plus_method_is_server_request_not_response() {
        match parse_incoming(
            r#"{"jsonrpc":"2.0","id":3,"method":"workspace/configuration","params":{"items":[{}]}}"#,
        )
        .unwrap()
        {
            Incoming::ServerRequest { id, method, .. } => {
                assert_eq!(id, 3);
                assert_eq!(method, "workspace/configuration");
            }
            _ => panic!("must be ServerRequest, not Response"),
        }
    }
}
