//! Stage C batch B: pure logic for plan-decompose-react within an approved
//! initiative goal. Prompts + advance decision. IO (LLM, DB, sanitize) lives in
//! the driver ([`super::driver`]).
use crate::db::session_goals::GoalRow;

pub const MAX_CHUNKS: usize = 8;
pub const CHUNK_MAX_CHARS: usize = 300;

pub fn decompose_prompt(goal: &str) -> String {
    format!(
        "Разбей цель на не более {MAX_CHUNKS} упорядоченных конкретных шагов. \
         Верни строго JSON: {{\"chunks\": [\"...\", ...]}}.\n\nЦель: {goal}"
    )
}

pub fn chunk_continuation_prompt(goal: &str, chunks: &[String], current: usize) -> String {
    let len = chunks.len();
    let cur_text = chunks.get(current).map(String::as_str).unwrap_or("");
    let done: Vec<String> = chunks.iter().take(current).enumerate()
        .map(|(i, c)| format!("{}. {c}", i + 1)).collect();
    let done_block = if done.is_empty() { String::new() } else { format!("\nСделано ранее:\n{}", done.join("\n")) };
    format!(
        "Цель: {goal}.\nТекущий шаг {}/{len}: {cur_text}.{done_block}\n\
         Работай над ТЕКУЩИМ шагом; когда шаг выполнен — заяви явно.",
        current + 1
    )
}

pub fn replan_prompt(goal: &str, done: &[String], remaining: &[String], last_outcome: &str) -> String {
    let last: String = last_outcome.chars().take(4000).collect();
    format!(
        "Неизменная цель (одобрена владельцем): {goal}.\nСделанные шаги: {done:?}.\n\
         Оставшиеся: {remaining:?}.\nПоследний результат: {last}.\n\
         Пересмотри ОСТАВШИЕСЯ шаги — они ДОЛЖНЫ выводиться из неизменной цели, не расширять её. \
         Верни строго JSON: {{\"remaining\": [\"...\", ...]}}."
    )
}

pub fn chunk_judge_prompt(goal: &str, current_chunk: &str, last: &str) -> String {
    let last_slice: String = last.chars().take(4000).collect();
    format!(
        "Цель: {goal}.\nТекущий шаг: {current_chunk}.\nПоследний ответ агента:\n{last_slice}\n\n\
         Выполнен ли ТЕКУЩИЙ шаг? Изменил ли результат план так, что оставшиеся шаги нужно пересмотреть? \
         Верни строго JSON: {{\"chunk_done\": <true|false>, \"replan\": <true|false>}}."
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkVerdict { pub chunk_done: bool, pub replan: bool, pub parse_ok: bool }

pub fn parse_chunk_verdict(raw: &str) -> ChunkVerdict {
    match crate::agent::json_repair::repair_json(raw) {
        Ok(v) => ChunkVerdict {
            chunk_done: v.get("chunk_done").and_then(|x| x.as_bool()).unwrap_or(false),
            replan: v.get("replan").and_then(|x| x.as_bool()).unwrap_or(false),
            parse_ok: true,
        },
        Err(_) => ChunkVerdict { chunk_done: false, replan: false, parse_ok: false },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecomposeAction { Continue, Advance, AdvanceAndReplan, Done, Pause(&'static str) }

/// Order mirrors `next_action`: Done checked FIRST (before budget). `current` = row.current_chunk.
pub fn advance_decision(row: &GoalRow, verdict: ChunkVerdict, chunks_len: usize) -> DecomposeAction {
    let current = row.current_chunk.max(0) as usize;
    if verdict.chunk_done && current + 1 >= chunks_len {
        return DecomposeAction::Done;
    }
    if !row.budget_left() {
        return DecomposeAction::Pause("budget");
    }
    if verdict.chunk_done && verdict.replan {
        return DecomposeAction::AdvanceAndReplan;
    }
    if verdict.chunk_done {
        return DecomposeAction::Advance;
    }
    if !verdict.parse_ok && row.consecutive_judge_failures + 1 >= 3 {
        return DecomposeAction::Pause("judge");
    }
    DecomposeAction::Continue
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::session_goals::GoalRow;

    fn row(current_chunk: i32, turn_count: i32, max_turns: i32, cjf: i32) -> GoalRow {
        GoalRow { session_id: uuid::Uuid::nil(), goal_text: "G".into(), status: "active".into(),
            turn_count, max_turns, subgoals: vec![], last_verdict: None,
            consecutive_judge_failures: cjf, origin: "initiative".into(), current_chunk,
            decompose_failed: false }
    }

    #[test]
    fn advance_done_before_budget() {
        // last chunk done + budget exhausted → Done (not Pause budget)
        let r = row(2, 20, 20, 0);
        assert!(matches!(advance_decision(&r, ChunkVerdict{chunk_done:true,replan:false,parse_ok:true}, 3), DecomposeAction::Done));
    }
    #[test]
    fn advance_pause_budget_midway() {
        let r = row(0, 20, 20, 0);
        assert!(matches!(advance_decision(&r, ChunkVerdict{chunk_done:false,replan:false,parse_ok:true}, 3), DecomposeAction::Pause("budget")));
    }
    #[test]
    fn advance_and_replan() {
        let r = row(0, 1, 20, 0);
        assert!(matches!(advance_decision(&r, ChunkVerdict{chunk_done:true,replan:true,parse_ok:true}, 3), DecomposeAction::AdvanceAndReplan));
    }
    #[test]
    fn advance_plain() {
        let r = row(0, 1, 20, 0);
        assert!(matches!(advance_decision(&r, ChunkVerdict{chunk_done:true,replan:false,parse_ok:true}, 3), DecomposeAction::Advance));
    }
    #[test]
    fn pause_judge_after_three_parse_fails() {
        let r = row(0, 1, 20, 2); // cjf=2, +1 = 3 → Pause judge
        assert!(matches!(advance_decision(&r, ChunkVerdict{chunk_done:false,replan:false,parse_ok:false}, 3), DecomposeAction::Pause("judge")));
    }
    #[test]
    fn continue_default() {
        let r = row(0, 1, 20, 0);
        assert!(matches!(advance_decision(&r, ChunkVerdict{chunk_done:false,replan:false,parse_ok:true}, 3), DecomposeAction::Continue));
    }
    #[test]
    fn parse_verdict_tolerant() {
        let v = parse_chunk_verdict("```json\n{\"chunk_done\": true, \"replan\": false}\n```");
        assert!(v.chunk_done && !v.replan && v.parse_ok);
        let bad = parse_chunk_verdict("garbage");
        assert!(!bad.chunk_done && !bad.parse_ok);
    }
    #[test]
    fn chunk_prompt_focuses_current() {
        let p = chunk_continuation_prompt("goalX", &["a".into(),"b".into(),"c".into()], 1);
        assert!(p.contains("goalX") && p.contains("b")); // current chunk
        assert!(p.contains("2/3") || p.contains("2 / 3") || p.to_lowercase().contains("шаг 2"));
    }
}
