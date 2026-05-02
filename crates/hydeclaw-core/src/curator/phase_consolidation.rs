//! Phase 3 — Analyst / Verifier / Executor skill consolidation.

use serde::Deserialize;
use crate::gateway::clusters::AgentCore;

// ── Public result type ────────────────────────────────────────────────────────

pub struct ConsolidationResult {
    pub commands_executed: i32,
    pub log: Vec<String>,
}

// ── Proposal data types ───────────────────────────────────────────────────────

#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub(crate) struct CapabilityEntry {
    pub capability: String,
    pub from_quote: String,
    pub covered_in: String,
    pub covering_quote: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "lowercase")]
pub(crate) enum Proposal {
    Archive {
        skill: String,
        replacement: String,
        #[allow(dead_code)]
        reason: String,
        capability_map: Vec<CapabilityEntry>,
    },
    Merge {
        sources: Vec<String>,
        into: String,
        reason: String,
    },
    Fix {
        skill: String,
        description: String,
    },
}

#[derive(Debug, Deserialize)]
pub(crate) struct ProposalsFile {
    pub proposals: Vec<Proposal>,
}

// ── Entry point stub (filled in Task 6) ──────────────────────────────────────

pub async fn run(
    _workspace_dir: &str,
    _agents: &AgentCore,
    _agent_name: &str,
) -> anyhow::Result<ConsolidationResult> {
    Ok(ConsolidationResult { commands_executed: 0, log: vec!["not yet implemented".into()] })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proposals_valid_json_parses() {
        let json = r##"{
            "proposals": [
                {
                    "action": "archive",
                    "skill": "daily-reflection",
                    "replacement": "self-improvement",
                    "reason": "covered",
                    "capability_map": [
                        {
                            "capability": "journal format",
                            "from_quote": "Journal: YYYY-MM-DD",
                            "covered_in": "self-improvement Section 1",
                            "covering_quote": "Journal: YYYY-MM-DD"
                        }
                    ]
                },
                {
                    "action": "fix",
                    "skill": "research-strategy",
                    "description": "add section on source validation"
                }
            ]
        }"##;
        let parsed: ProposalsFile = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.proposals.len(), 2);
        match &parsed.proposals[0] {
            Proposal::Archive { skill, capability_map, .. } => {
                assert_eq!(skill, "daily-reflection");
                assert_eq!(capability_map.len(), 1);
            }
            _ => panic!("expected Archive"),
        }
    }

    #[test]
    fn proposals_invalid_json_returns_err() {
        let result: Result<ProposalsFile, _> = serde_json::from_str("not json {{");
        assert!(result.is_err());
    }

    #[test]
    fn proposals_unknown_action_returns_err() {
        let json = r#"{"proposals": [{"action": "delete", "skill": "x"}]}"#;
        let result: Result<ProposalsFile, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }
}
