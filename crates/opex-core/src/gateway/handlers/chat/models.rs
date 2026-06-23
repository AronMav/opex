//! `GET /v1/models` — list configured agents as OpenAI-compatible models.

use axum::{extract::State, response::Json};
use serde_json::{Value, json};

use crate::gateway::clusters::AgentCore;

pub(crate) async fn list_models(State(agents): State<AgentCore>) -> Json<Value> {
    let agents_map = agents.map.read().await;
    let data: Vec<Value> = agents_map
        .keys()
        .map(|name| {
            json!({
                "id": name,
                "object": "model",
                "created": 0,
                "owned_by": "hydeclaw"
            })
        })
        .collect();
    Json(json!({ "object": "list", "data": data }))
}
