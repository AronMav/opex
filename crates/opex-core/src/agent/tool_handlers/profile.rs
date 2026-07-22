use serde_json::{json, Value};

use crate::agent::tool_registry::{SystemToolHandler, ToolDeps};

pub struct ProfileHandler;

#[async_trait::async_trait]
impl SystemToolHandler for ProfileHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("show");

        match action {
            "show" => handle_show(deps).await,
            "switch" => handle_switch(deps, args).await,
            _ => json!({
                "error": "unknown action. Use: show, switch"
            })
            .to_string(),
        }
    }
}

async fn handle_show(deps: ToolDeps<'_>) -> String {
    let slots = &deps.cfg.profile_slots;
    let agent_name = &deps.cfg.agent.name;
    let current_model = deps.cfg.provider.current_model();

    let mut out = format!(
        "# Profile: {} (agent: {})\n\n## Current model: {}\n\n## Slots:\n\n",
        deps.cfg.agent.profile, agent_name, current_model
    );

    if slots.is_empty() {
        out.push_str("_No slots configured._\n");
        return out;
    }

    // Sort slots in a stable order
    let mut keys: Vec<&String> = slots.keys().collect();
    keys.sort();

    for slot_name in keys {
        let entries = &slots[slot_name];
        out.push_str(&format!("### {} ({} provider{})\n", slot_name, entries.len(), if entries.len() != 1 { "s" } else { "" }));
        for (i, entry) in entries.iter().enumerate() {
            let marker = if i == 0 { " **[active]**" } else { "" };
            let model = entry.model.as_deref().unwrap_or("default");
            out.push_str(&format!(
                "  {}. `{}{}` — model: `{}`{}\n",
                i + 1,
                entry.provider,
                marker,
                model,
                entry.voice.as_deref().map(|v| format!(", voice: `{}`", v)).unwrap_or_default()
            ));
        }
        out.push('\n');
    }

    out.push_str("---\n");
    out.push_str("Use `profile(action=\"switch\", slot=\"text\", provider=\"name\")` to switch to a different provider for this turn.\n");

    out
}

async fn handle_switch(deps: ToolDeps<'_>, args: &Value) -> String {
    let slot = match args.get("slot").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s,
        _ => return json!({"error": "'slot' is required for switch. Use action=\"show\" to see available slots."}).to_string(),
    };

    let provider_name = match args.get("provider").and_then(|v| v.as_str()) {
        Some(p) if !p.is_empty() => p,
        _ => return json!({"error": "'provider' is required for switch. Use action=\"show\" to see available providers in each slot."}).to_string(),
    };

    let model_override = args.get("model").and_then(|v| v.as_str()).filter(|s| !s.is_empty());

    let slots = &deps.cfg.profile_slots;

    // Validate the slot exists
    let entries = match slots.get(slot) {
        Some(e) if !e.is_empty() => e,
        _ => return json!({"error": format!("slot '{}' not found in profile. Available: {}", slot, slots.keys().cloned().collect::<Vec<_>>().join(", "))}).to_string(),
    };

    // Validate the provider is in the slot
    if !entries.iter().any(|e| e.provider == provider_name) {
        let available: Vec<&str> = entries.iter().map(|e| e.provider.as_str()).collect();
        return json!({
            "error": format!("provider '{}' not found in slot '{}'. Available: {}", provider_name, slot, available.join(", "))
        }).to_string();
    }

    // Only the text slot supports per-turn model switching via set_model_override.
    // Other slots (vision, tts, stt, imagegen) are resolved at capability-tool
    // dispatch time via the profile_slots chain — the active provider is always
    // entries[0], so switching requires reordering the slot (which is a profile
    // edit, not a per-turn override).
    if slot != "text" {
        return json!({
            "error": format!("switch is only supported for the 'text' slot. Other slots (vision, tts, stt, imagegen) are resolved at dispatch time — use the UI Profiles page to reorder providers."),
            "hint": "For text: profile(action=\"switch\", slot=\"text\", provider=\"...\")"
        }).to_string();
    }

    // For the text slot: set the model override on the provider.
    // The provider is the RoutingProvider — set_model_override propagates
    // to all routes. The model name must match what the target provider
    // expects (we trust the agent to pass a valid model from the list).
    let model_to_set = match model_override {
        Some(m) => m.to_string(),
        None => {
            // Find the default model from the slot entry
            entries
                .iter()
                .find(|e| e.provider == provider_name)
                .and_then(|e| e.model.as_deref())
                .unwrap_or("")
                .to_string()
        }
    };

    if model_to_set.is_empty() {
        deps.cfg.provider.set_model_override(None);
    } else {
        deps.cfg.provider.set_model_override(Some(model_to_set.clone()));
    }

    json!({
        "ok": true,
        "message": format!("Switched text provider to '{}' with model '{}'. This applies to the current turn only.", provider_name, model_to_set),
        "provider": provider_name,
        "model": model_to_set,
        "slot": slot
    })
    .to_string()
}