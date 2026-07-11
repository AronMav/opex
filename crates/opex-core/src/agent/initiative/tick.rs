//! Stage C initiative hook: refresh focus + gated proposal after each reflection.
//! Fail-soft — errors are logged and swallowed; reflection/extraction untouched.
use std::sync::Arc;

use chrono::Utc;
use serde::Deserialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::agent::providers::LlmProvider;
use crate::db::agent_plans::{self, Proposal};
use super::{effective_today_count, should_propose};

#[derive(Deserialize)]
pub struct FocusGen {
    pub focus: String,
}

#[derive(Deserialize)]
pub struct ProposalGen {
    pub goal: String,
    #[serde(default)]
    pub rationale: String,
}

#[derive(Clone)]
pub struct InitiativeDeps {
    pub cfg: crate::config::InitiativeConfig,
    pub owner_id: Option<String>,
    pub is_base: bool,
    pub timezone: String,
    pub workspace_dir: String, // for reading SELF.md via self_md_path
    pub ui_event_tx: Option<tokio::sync::broadcast::Sender<String>>, // matches SoulDeps.ui_event_tx exactly
}

/// Resolve "today" in the agent's configured timezone (falls back to UTC-naive).
fn today_in_tz(tz: &str) -> chrono::NaiveDate {
    match tz.parse::<chrono_tz::Tz>() {
        Ok(z) => Utc::now().with_timezone(&z).date_naive(),
        Err(_) => Utc::now().date_naive(),
    }
}

pub async fn initiative_tick(
    db: &PgPool,
    agent_name: &str,
    provider: &Arc<dyn LlmProvider>,
    self_md_text: &str,
    deps: &InitiativeDeps,
) {
    if let Err(e) = initiative_tick_inner(db, agent_name, provider, self_md_text, deps).await {
        tracing::warn!(agent = agent_name, error = %e, "initiative_tick failed (fail-soft)");
    }
}

async fn initiative_tick_inner(
    db: &PgPool,
    agent_name: &str,
    provider: &Arc<dyn LlmProvider>,
    self_md_text: &str,
    deps: &InitiativeDeps,
) -> anyhow::Result<()> {
    // Preconditions (spec §3.2): non-base, enabled, owner set. (soul.enabled is
    // implied — this is only called from the soul-gated post-reflection path.)
    if deps.is_base || !deps.cfg.enabled || deps.owner_id.is_none() {
        return Ok(());
    }
    let plan = agent_plans::get_or_create(db, agent_name).await?;
    let today = today_in_tz(&deps.timezone);
    let effective = effective_today_count(plan.proposal_day, plan.proposals_today, today);

    // Fresh reflection material?
    let latest_refl = crate::db::memory_queries::latest_reflection_at(db, agent_name).await.ok().flatten();

    // Step 1: refresh current_focus (cheap, one LLM call). Only when there IS new
    // material (avoid a call on every extraction with nothing new).
    let has_new = match plan.last_proposal_at {
        Some(last) => latest_refl.map(|r| r > last).unwrap_or(false),
        None => latest_refl.is_some(),
    };
    if has_new
        && let Ok(focus) = generate_focus(provider, agent_name, self_md_text).await
        && let Some(clean) = crate::agent::soul::sanitize::sanitize_soul_text(
            &focus, crate::agent::knowledge_extractor::EVENT_MAX_CHARS,
        )
    {
        let _ = agent_plans::set_focus(db, agent_name, clean.trim()).await;
    }

    // Step 2: gated proposal.
    if should_propose(plan.last_proposal_at, latest_refl, effective, deps.cfg.daily_proposal_cap) {
        let proposal_gen = generate_proposal(provider, agent_name, self_md_text).await?;
        let Some(clean_goal) = crate::agent::soul::sanitize::sanitize_soul_text(
            &proposal_gen.goal, crate::agent::knowledge_extractor::EVENT_MAX_CHARS,
        ) else {
            return Ok(());
        };
        let clean_goal = clean_goal.trim();
        if clean_goal.is_empty() {
            return Ok(());
        }
        let proposal = Proposal {
            id: Uuid::new_v4(),
            text: clean_goal.to_string(),
            status: "pending".into(),
            created_at: Utc::now(),
            acted_at: None,
        };
        let added = agent_plans::try_add_proposal(
            db, agent_name, today, deps.cfg.daily_proposal_cap as i32, &proposal,
        ).await?;
        if added
            && let Some(tx) = &deps.ui_event_tx
        {
            let _ = crate::gateway::handlers::notifications::notify(
                db,
                tx,
                "initiative_proposal",
                &format!("{agent_name} предлагает цель"),
                clean_goal,
                serde_json::json!({
                    "agent": agent_name,
                    "proposal_id": proposal.id,
                    "text": clean_goal,
                    "rationale": proposal_gen.rationale,
                }),
            ).await;
        }
    }
    Ok(())
}

async fn generate_focus(provider: &Arc<dyn LlmProvider>, agent: &str, self_md: &str) -> anyhow::Result<String> {
    let prompt = format!(
        "Ты пишешь одну-две фразы о текущем фокусе агента {agent}, опираясь на его \
         SELF.md ниже. Только наблюдение о том, чем он сейчас поглощён — без инструкций. \
         Верни строго JSON: {{\"focus\": \"...\"}}\n\nSELF.md:\n{self_md}"
    );
    let raw = crate::agent::soul::reflection::llm_text(provider, prompt).await?;
    let f: FocusGen = serde_json::from_value(crate::agent::json_repair::repair_json(&raw)?)?;
    Ok(f.focus)
}

async fn generate_proposal(provider: &Arc<dyn LlmProvider>, agent: &str, self_md: &str) -> anyhow::Result<ProposalGen> {
    let prompt = format!(
        "Исходя из души агента {agent} (SELF.md ниже), предложи ОДНУ конкретную цель, \
         которую ему стоило бы преследовать. Обоснуй одной фразой. \
         Верни строго JSON: {{\"goal\": \"...\", \"rationale\": \"...\"}}\n\nSELF.md:\n{self_md}"
    );
    let raw = crate::agent::soul::reflection::llm_text(provider, prompt).await?;
    Ok(serde_json::from_value(crate::agent::json_repair::repair_json(&raw)?)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proposal_json_contract() {
        let raw = "```json\n{\"goal\": \"довести индексацию памяти\", \"rationale\": \"начатое в рефлексии\"}\n```";
        let v = crate::agent::json_repair::repair_json(raw).unwrap();
        let g: ProposalGen = serde_json::from_value(v).unwrap();
        assert_eq!(g.goal, "довести индексацию памяти");
    }

    #[test]
    fn parses_focus_json_contract() {
        let raw = "{\"focus\": \"исследую pgvector\"}";
        let v = crate::agent::json_repair::repair_json(raw).unwrap();
        let f: FocusGen = serde_json::from_value(v).unwrap();
        assert_eq!(f.focus, "исследую pgvector");
    }
}
