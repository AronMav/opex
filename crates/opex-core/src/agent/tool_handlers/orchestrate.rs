//! `code_orchestrate` system tool — codemode (tools-as-code) entry point.
//!
//! Instead of one tool-call per turn, the LLM writes a short Python script
//! that calls tools programmatically (loops, Promise.all-equivalent via
//! `concurrent.futures`, filtering, aggregation in one turn). The script runs
//! in the code_exec Docker sandbox with a generated SDK preamble
//! (`opex_sdk.py`) that dispatches tool calls back to core via the loopback
//! `/api/sandbox/tool-call` endpoint.
//!
//! Flow:
//! 1. Build the agent's filtered tool list (`ToolDeps.available_tools`).
//! 2. Generate the Python SDK preamble from the tool definitions.
//! 3. Mint a per-execution capability token (HMAC, bound to session + agent +
//!    tools_hash + TTL).
//! 4. Run the script in the sandbox with `OPEX_CODEMODE_TOKEN`,
//!    `OPEX_CODEMODE_URL`, `OPEX_SESSION_ID`, `OPEX_AGENT_NAME` env vars
//!    injected at exec time.
//! 5. Collect stdout/stderr, parse `__tool_call__:` markers for nested SSE
//!    events, and return the result to the LLM.
//!
//! v1 restriction: codemode is available to base agents only (they already
//! have network access to the `opex` Docker network, so the sandbox can reach
//! core's loopback endpoint). Non-base agents get a clear error.

use async_trait::async_trait;
use serde_json::Value;

use crate::agent::pipeline::sdk_stubs::generate_python_sdk;
use crate::agent::tool_registry::{SystemToolHandler, ToolDeps};
use crate::uploads::{codemode_tools_hash, mint_codemode_token};

pub struct CodeOrchestrateHandler;

/// Buffer added to the sandbox timeout when scoping the codemode token TTL —
/// covers container startup + the last in-flight tool call finishing after the
/// script's wall-clock deadline. The TTL itself is derived from the sandbox's
/// actual `timeout_secs` (SEC review L4) so the token can't be replayed long
/// after the run that minted it could still be executing.
const CODEMODE_TOKEN_TTL_BUFFER_SECS: u64 = 30;

#[async_trait]
impl SystemToolHandler for CodeOrchestrateHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        handle_code_orchestrate(deps, args).await
    }
}

async fn handle_code_orchestrate(deps: ToolDeps<'_>, args: &Value) -> String {
    // v1: base agents only (they have network access to reach core).
    if !deps.agent_base {
        return "Error: code_orchestrate is available to base agents only (v1). Non-base agents lack network access to the loopback tool-call endpoint.".to_string();
    }

    let Some(code) = args.get("code").and_then(|c| c.as_str()) else {
        return "Error: missing required parameter 'code'.".to_string();
    };
    if code.is_empty() {
        return "Error: 'code' parameter is empty.".to_string();
    }

    // Sandbox is required for codemode (we need an isolated environment).
    let Some(sandbox) = deps.sandbox.as_ref() else {
        return "Error: code_orchestrate requires the Docker sandbox to be enabled.".to_string();
    };

    // Build the agent's filtered tool list for the SDK preamble + token binding.
    // IMPORTANT: this must use the same function as the sandbox.rs endpoint's
    // `codemode_available_tool_names()` — otherwise the tools_hash in the
    // minted token won't match the hash re-derived at the endpoint, and every
    // tool call will be rejected with 401. Both sites:
    // 1. Call `engine.codemode_available_tool_names()` (system tools filtered by policy).
    // 2. Exclude `code_orchestrate` itself (H2: prevent recursive calls).
    // The resulting set is what goes into the SDK preamble AND the tools_hash.
    let available: Vec<String> = match deps.agent_map {
        Some(map) => {
            let guard = map.read().await;
            match guard.get(deps.agent_name) {
                Some(h) => h
                    .engine
                    .codemode_available_tool_names()
                    .into_iter()
                    .filter(|name| name != "code_orchestrate")
                    .collect(),
                None => Vec::new(),
            }
        }
        None => Vec::new(),
    };
    let tools_hash = codemode_tools_hash(&available);

    // Generate SDK stubs from the same filtered list.
    let sdk_tools: Vec<opex_types::ToolDefinition> = available
        .iter()
        .map(|name| opex_types::ToolDefinition {
            name: name.clone(),
            description: String::new(),
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
        })
        .collect();
    let sdk_preamble = generate_python_sdk(&sdk_tools);

    // Mint the capability token. TTL is scoped to the sandbox's actual run
    // deadline + a small buffer (SEC review L4) — a token can't outlive the run
    // that could still be using it, shrinking the replay window.
    let key = deps.secrets.get_upload_hmac_key();
    let session_id = deps.session_id.unwrap_or_default();
    let token_ttl = sandbox.timeout_secs() + CODEMODE_TOKEN_TTL_BUFFER_SECS;
    let token = mint_codemode_token(
        &key,
        session_id,
        deps.agent_name,
        tools_hash,
        token_ttl,
    );

    // Build the full script: SDK preamble + user code.
    let full_script = format!("{sdk_preamble}\n# ── User code ──\n{code}");

    // Env vars to inject into the sandbox at exec time.
    let gateway_port = deps
        .gateway_listen
        .rsplit(':')
        .next()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(18789);
    let codemode_url = format!("http://host.docker.internal:{gateway_port}/api/sandbox/tool-call");
    let env_vars: Vec<(String, String)> = vec![
        ("OPEX_CODEMODE_TOKEN".into(), token),
        ("OPEX_CODEMODE_URL".into(), codemode_url),
        ("OPEX_SESSION_ID".into(), session_id.to_string()),
        ("OPEX_AGENT_NAME".into(), deps.agent_name.to_string()),
    ];

    // Execute in the sandbox with env injection + SDK preamble.
    // The script is base64-encoded and run as `python3 -c` with the preamble
    // prepended via PYTHONPATH or inline.
    let result = sandbox
        .execute_with_sdk(
            deps.agent_name,
            &full_script,
            "python",
            &env_vars,
            deps.workspace_dir,
            deps.agent_base,
        )
        .await;

    match result {
        Ok(exec) => {
            let mut output = String::new();
            if !exec.stdout.is_empty() {
                output.push_str(&exec.stdout);
            }
            if !exec.stderr.is_empty() {
                if !output.is_empty() {
                    output.push_str("\n--- stderr ---\n");
                }
                output.push_str(&exec.stderr);
            }
            if exec.exit_code != 0 {
                output.push_str(&format!("\nExit code: {}", exec.exit_code));
            }
            output
        }
        Err(e) => format!("Error: {e}"),
    }
}