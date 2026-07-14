//! Self-baseline persona-drift detector (spec stage B §2): drift = 1 − cos(recent,
//! baseline_centroid), где baseline = центроид собственных ранних ответов агента в
//! этой сессии. Embedding-only, detect+log v1 (никаких инъекций). Чистые функции;
//! обвязка (embed через cfg().embedder, кэш, запись в session_timeline) — в
//! engine/context_builder.rs.

/// L2-нормализация. Вырожденный/нулевой вектор → None.
fn normalize(v: &[f32]) -> Option<Vec<f32>> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if !norm.is_finite() || norm < f32::EPSILON {
        return None;
    }
    Some(v.iter().map(|x| x / norm).collect())
}

/// Центроид нормированных эмбеддингов (среднее единичных векторов).
/// None, если пусто или все вырождены.
pub fn centroid(embeddings: &[Vec<f32>]) -> Option<Vec<f32>> {
    let normed: Vec<Vec<f32>> = embeddings.iter().filter_map(|e| normalize(e)).collect();
    if normed.is_empty() {
        return None;
    }
    let dim = normed[0].len();
    let mut acc = vec![0.0f32; dim];
    for v in &normed {
        for (i, x) in v.iter().enumerate() {
            if i < dim {
                acc[i] += x;
            }
        }
    }
    let n = normed.len() as f32;
    Some(acc.iter().map(|x| x / n).collect())
}

/// Косинус (0 при вырожденном векторе).
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na < f32::EPSILON || nb < f32::EPSILON {
        return 0.0;
    }
    dot / (na * nb)
}

/// drift = 1 − cos(recent, baseline_centroid) ∈ [0, 2]. Выше = дальше от раннего себя.
pub fn drift_score(baseline_centroid: &[f32], recent: &[f32]) -> f32 {
    1.0 - cosine(recent, baseline_centroid)
}

/// Compact identity-reminder block appended to the system prompt on an
/// over-threshold turn. Operator's `anchor` when set/non-blank, else a generic
/// name-based fallback. Trusted input (operator config / agent name) — not sanitized.
pub fn build_anchor_block(anchor: Option<&str>, agent_name: &str) -> String {
    let body = match anchor {
        Some(a) if !a.trim().is_empty() => a.trim().to_string(),
        _ => format!("Ты — {agent_name}. Сохраняй свой характер, тон и манеру речи."),
    };
    format!("\n\n[Идентичность — напоминание]\n{body}\n")
}

/// Correction decision: the anchor block to inject, or None. Some iff correction
/// is enabled AND the score is strictly over threshold (mirrors drift_probe's
/// `over = score > threshold`).
pub fn correction_anchor(
    score: f32,
    threshold: f32,
    correct: bool,
    anchor: Option<&str>,
    agent_name: &str,
) -> Option<String> {
    if correct && score > threshold {
        Some(build_anchor_block(anchor, agent_name))
    } else {
        None
    }
}

/// Тексты СОБСТВЕННЫХ assistant-ответов агента с натуральным содержимым,
/// хронологически. Фильтр: role=assistant, agent_id == свой ИЛИ None (untagged —
/// считаем своим; чужие peer-агенты в пуле тегируются своим id и исключаются),
/// непустой trim. Пропускает tool-call-only / пустые (спека §2, ревью F10/F12).
pub fn own_assistant_texts(
    history: &[opex_db::sessions::MessageRow],
    agent_name: &str,
) -> Vec<String> {
    history
        .iter()
        .filter(|m| {
            let own = m.agent_id.as_deref();
            m.role == "assistant"
                && (own.is_none() || own == Some(agent_name))
                && !m.content.trim().is_empty()
        })
        .map(|m| m.content.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // MessageRow НЕ выводит Default (только Debug/Serialize/FromRow) — заполняем ВСЕ 15 полей явно.
    fn row(role: &str, agent: Option<&str>, content: &str) -> opex_db::sessions::MessageRow {
        opex_db::sessions::MessageRow {
            id: uuid::Uuid::new_v4(),
            role: role.to_string(),
            content: content.to_string(),
            tool_calls: None,
            tool_call_id: None,
            created_at: chrono::Utc::now(),
            agent_id: agent.map(String::from),
            feedback: None,
            edited_at: None,
            status: "done".to_string(),
            thinking_blocks: None,
            parent_message_id: None,
            branch_from_message_id: None,
            abort_reason: None,
            is_mirror: false,
        }
    }

    #[test]
    fn drift_zero_when_recent_equals_baseline() {
        let base = centroid(&[vec![1.0, 0.0, 0.0]]).unwrap();
        let s = drift_score(&base, &[1.0, 0.0, 0.0]);
        assert!(s.abs() < 1e-5, "identical → ~0, got {s}");
    }

    #[test]
    fn drift_one_when_orthogonal() {
        let base = centroid(&[vec![1.0, 0.0, 0.0]]).unwrap();
        let s = drift_score(&base, &[0.0, 1.0, 0.0]);
        assert!((s - 1.0).abs() < 1e-5, "orthogonal → 1, got {s}");
    }

    #[test]
    fn centroid_normalizes_before_averaging() {
        // два ортогональных, разной магнитуды → центроид указывает в биссектрису
        let c = centroid(&[vec![10.0, 0.0], vec![0.0, 0.1]]).unwrap();
        // после нормализации оба единичные → среднее (0.5, 0.5)
        assert!((c[0] - 0.5).abs() < 1e-5 && (c[1] - 0.5).abs() < 1e-5, "got {c:?}");
    }

    #[test]
    fn centroid_empty_and_zero_vectors_are_none() {
        assert!(centroid(&[]).is_none());
        assert!(centroid(&[vec![0.0, 0.0, 0.0]]).is_none());
    }

    #[test]
    fn own_texts_filters_role_agent_and_empty() {
        let hist = vec![
            row("user", Some("A"), "привет"),
            row("assistant", Some("A"), "ответ A1"),
            row("assistant", Some("B"), "ответ чужого агента"),  // peer — исключить
            row("assistant", None, "ответ без тега"),            // None → считаем своим
            row("assistant", Some("A"), "   "),                  // пустой — исключить
            row("assistant", Some("A"), "ответ A2"),
        ];
        let texts = own_assistant_texts(&hist, "A");
        assert_eq!(texts, vec!["ответ A1", "ответ без тега", "ответ A2"]);
    }

    #[test]
    fn anchor_uses_operator_string_or_falls_back() {
        let a = build_anchor_block(Some("Ты — Опекс, инфра-ассистент."), "Opex");
        assert!(a.contains("Опекс, инфра-ассистент"));
        assert!(a.contains("[Идентичность — напоминание]"));
        // blank/None → generic fallback naming the agent
        let f = build_anchor_block(None, "Arty");
        assert!(f.contains("Arty"));
        assert!(f.contains("[Идентичность — напоминание]"));
        let b = build_anchor_block(Some("   "), "Arty");
        assert!(b.contains("Arty"), "blank anchor → fallback");
    }

    #[test]
    fn correction_anchor_gates_on_correct_and_threshold() {
        // over threshold + correct → Some(block)
        assert!(correction_anchor(0.5, 0.15, true, None, "A").is_some());
        // over threshold but correct off → None (detect-only)
        assert!(correction_anchor(0.5, 0.15, false, None, "A").is_none());
        // under threshold + correct → None
        assert!(correction_anchor(0.10, 0.15, true, None, "A").is_none());
        // exactly at threshold → None (strict >, matches drift_probe's `score > threshold`)
        assert!(correction_anchor(0.15, 0.15, true, None, "A").is_none());
    }
}
