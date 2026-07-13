//! `/goal` autonomous-loop pure logic: command/verdict parsing, prompt building,
//! and the driver's per-turn decision. No IO — fully unit-tested.

use crate::db::session_goals::GoalRow;

pub mod decompose;
pub mod driver;
pub mod pool;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Done,
    Continue,
    ParseFail,
}

/// Tolerant judge-output parser: strip ``` fences, take the first {...}, read `done`.
// reviewed: offsets from find('{')/rfind('}') (ASCII) — char boundaries
#[allow(clippy::string_slice)]
pub fn parse_judge_verdict(raw: &str) -> Verdict {
    let cleaned = raw.replace("```json", "").replace("```", "");
    let (start, end) = (cleaned.find('{'), cleaned.rfind('}'));
    let Some((s, e)) = start.zip(end) else {
        return Verdict::ParseFail;
    };
    if s > e {
        return Verdict::ParseFail;
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&cleaned[s..=e]) else {
        return Verdict::ParseFail;
    };
    match v.get("done").and_then(|d| d.as_bool()) {
        Some(true) => Verdict::Done,
        Some(false) => Verdict::Continue,
        None => Verdict::ParseFail,
    }
}

/// The user-role message re-injected each autonomous turn.
pub fn continuation_prompt(goal: &str, subgoals: &[String]) -> String {
    let mut s = format!("[autonomous continuation] Keep working toward this goal:\n{goal}\n");
    if !subgoals.is_empty() {
        s.push_str("\nRanked criteria:\n");
        for (i, sg) in subgoals.iter().enumerate() {
            s.push_str(&format!("{}. {sg}\n", i + 1));
        }
    }
    s.push_str("\nWhen the goal is fully achieved, state that explicitly. Otherwise take the next concrete step.");
    s
}

pub enum GoalCmd {
    Set(String),
    Status,
    Pause,
    Resume,
    Clear,
}

pub fn parse_goal_command(arg: &str) -> GoalCmd {
    let a = arg.trim();
    match a.to_lowercase().as_str() {
        "" | "status" => GoalCmd::Status,
        "pause" => GoalCmd::Pause,
        "resume" => GoalCmd::Resume,
        "clear" => GoalCmd::Clear,
        _ => GoalCmd::Set(a.to_string()),
    }
}

pub enum SubgoalCmd {
    Add(String),
    List,
    Remove(usize),
}

pub fn parse_subgoal_command(arg: &str) -> SubgoalCmd {
    let a = arg.trim();
    if a.eq_ignore_ascii_case("list") {
        return SubgoalCmd::List;
    }
    if let Some(rest) = a.strip_prefix("remove ").or_else(|| a.strip_prefix("remove\t"))
        && let Ok(n) = rest.trim().parse::<usize>()
    {
        return SubgoalCmd::Remove(n);
    }
    SubgoalCmd::Add(a.to_string())
}

pub enum DriverAction {
    Continue,
    Done,
    Pause(&'static str),
}

/// Decide what the driver does after a turn, given the (just-reloaded) row and the judge verdict.
/// `consecutive_judge_failures` in `row` is the value BEFORE this verdict is recorded.
/// `Done` is checked BEFORE the budget so a goal completed on the budget-exhausting turn
/// is reported as done, not paused.
pub fn next_action(row: &GoalRow, verdict: Verdict) -> DriverAction {
    match verdict {
        Verdict::Done => DriverAction::Done,
        _ if !row.budget_left() => DriverAction::Pause("budget"),
        Verdict::Continue => DriverAction::Continue,
        Verdict::ParseFail => {
            if row.consecutive_judge_failures + 1 >= 3 {
                DriverAction::Pause("judge")
            } else {
                DriverAction::Continue
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_verdict() {
        assert_eq!(parse_judge_verdict(r#"{"done": true, "reason": "ok"}"#), Verdict::Done);
        assert_eq!(parse_judge_verdict(r#"{"done": false, "reason": "more"}"#), Verdict::Continue);
        assert_eq!(parse_judge_verdict("```json\n{\"done\": true}\n```"), Verdict::Done);
        assert_eq!(parse_judge_verdict("garbage"), Verdict::ParseFail);
        assert_eq!(parse_judge_verdict(""), Verdict::ParseFail);
    }

    #[test]
    fn parse_goal_cmd() {
        assert!(matches!(parse_goal_command("pause"), GoalCmd::Pause));
        assert!(matches!(parse_goal_command("resume"), GoalCmd::Resume));
        assert!(matches!(parse_goal_command("clear"), GoalCmd::Clear));
        assert!(matches!(parse_goal_command(""), GoalCmd::Status));
        assert!(matches!(parse_goal_command("status"), GoalCmd::Status));
        match parse_goal_command("refactor the api") {
            GoalCmd::Set(t) => assert_eq!(t, "refactor the api"),
            _ => panic!(),
        }
    }

    #[test]
    fn parse_subgoal_cmd() {
        assert!(matches!(parse_subgoal_command("list"), SubgoalCmd::List));
        assert!(matches!(parse_subgoal_command("remove 2"), SubgoalCmd::Remove(2)));
        match parse_subgoal_command("tests pass") {
            SubgoalCmd::Add(t) => assert_eq!(t, "tests pass"),
            _ => panic!(),
        }
    }

    #[test]
    fn continuation_includes_goal_and_subgoals() {
        let p = continuation_prompt("ship it", &["tests green".into(), "docs updated".into()]);
        assert!(p.contains("ship it"));
        assert!(p.contains("tests green") && p.contains("docs updated"));
    }

    fn row(status: &str, turns: i32, max: i32, cjf: i32) -> GoalRow {
        GoalRow {
            session_id: uuid::Uuid::nil(),
            goal_text: "g".into(),
            status: status.into(),
            turn_count: turns,
            max_turns: max,
            subgoals: vec![],
            last_verdict: None,
            consecutive_judge_failures: cjf,
            origin: "goal".into(),
            current_chunk: 0,
            decompose_failed: false,
        }
    }

    #[test]
    fn decision_table() {
        assert!(matches!(next_action(&row("active", 1, 20, 0), Verdict::Done), DriverAction::Done));
        assert!(matches!(next_action(&row("active", 1, 20, 0), Verdict::Continue), DriverAction::Continue));
        assert!(matches!(next_action(&row("active", 1, 20, 2), Verdict::ParseFail), DriverAction::Pause(_)));
        assert!(matches!(next_action(&row("active", 20, 20, 0), Verdict::Continue), DriverAction::Pause(_)));
        assert!(matches!(next_action(&row("active", 20, 20, 0), Verdict::Done), DriverAction::Done));
    }
}
