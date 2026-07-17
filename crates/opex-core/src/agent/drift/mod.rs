//! Self-baseline persona-drift detector (spec stage B §2): drift = 1 − cos(recent,
//! baseline_centroid), где baseline = центроид собственных ранних ответов агента в
//! этой сессии. Phase 1 = detect+log; Phase 2 = A-anchor коррекция: при drift >
//! threshold И `[agent.drift] correct=true` в системный промпт дописывается
//! компактный identity-якорь (build_anchor_block / correction_anchor — чистые
//! функции здесь). Обвязка (embed через cfg().embedder, кэш, запись в
//! session_timeline, возврат якоря) — в engine/context_builder.rs.

/// Absolute floor on σ (divide-by-≈0 guard for a near-identical baseline).
pub const SIGMA_FLOOR_ABS: f32 = 0.05;
/// Relative floor on σ (× μ) — stops a tight-but-narrow baseline from becoming
/// hypersensitive to ordinary topic movement (spec §2.2).
pub const SIGMA_FLOOR_REL: f32 = 0.2;
/// Logged z is clamped to ±Z_CAP (keeps aggregate stats sane; not the fire gate).
pub const Z_CAP: f32 = 20.0;
/// Min chars for an own-turn to count toward the baseline (drops "да"/"готово").
pub const MIN_BASELINE_CHARS: usize = 40;

/// Per-session drift cache entry. `centroid/mu/sigma` are frozen at baseline
/// establishment; `anchor_active` is the mutable Schmitt-hysteresis state.
#[derive(Clone)]
pub struct CachedDrift {
    pub centroid: Vec<f32>,
    pub mu: f32,
    pub sigma: f32,
    pub anchor_active: bool,
}

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

/// Leave-one-out baseline stats → (full_centroid, μ, σ). Each baseline turn's
/// distance uses the centroid of the OTHER turns (out-of-sample, like the recent
/// turn), cancelling the in-sample-centroid bias that would otherwise re-create
/// v1's always-fire (spec §2.1). σ is the Bessel-corrected sample std (÷(n−1)).
/// `None` if fewer than 2 usable embeddings or all degenerate.
pub fn baseline_stats(embeddings: &[Vec<f32>]) -> Option<(Vec<f32>, f32, f32)> {
    let n = embeddings.len();
    if n < 2 {
        return None;
    }
    let full = centroid(embeddings)?;
    let mut dists = Vec::with_capacity(n);
    for i in 0..n {
        let others: Vec<Vec<f32>> = embeddings
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .map(|(_, e)| e.clone())
            .collect();
        let c_loo = centroid(&others)?;
        dists.push(1.0 - cosine(&embeddings[i], &c_loo));
    }
    let nf = n as f32;
    let mu = dists.iter().sum::<f32>() / nf;
    let var = dists.iter().map(|d| (d - mu).powi(2)).sum::<f32>() / (nf - 1.0);
    let sigma = var.sqrt();
    Some((full, mu, sigma))
}

/// z = (recent_dist − μ) / σ_eff, where σ_eff floors σ both absolutely and
/// relative to μ (spec §2.2). Result clamped to ±Z_CAP.
pub fn drift_zscore(mu: f32, sigma: f32, recent_dist: f32) -> f32 {
    let sigma_eff = sigma.max(SIGMA_FLOOR_ABS).max(SIGMA_FLOOR_REL * mu);
    let z = (recent_dist - mu) / sigma_eff;
    z.clamp(-Z_CAP, Z_CAP)
}

/// Schmitt trigger: fire above `z_fire`, release below `z_release`, else hold
/// the current state (spec §3). Returns the new `active`.
pub fn hysteresis_decision(z: f32, active: bool, z_fire: f32, z_release: f32) -> bool {
    if !active && z > z_fire {
        true
    } else if active && z < z_release {
        false
    } else {
        active
    }
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

    // MessageRow НЕ выводит Default (только Debug/Serialize/FromRow) — заполняем ВСЕ 16 полей явно.
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
            bookmarked_at: None,
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
    fn baseline_stats_loo_is_unbiased_on_no_drift() {
        // 8 baseline turns iid around a direction + one "recent" from the SAME
        // distribution. With LOO, the recent z must be ≈ 0 (not systematically
        // positive) — this is the regression test for the in-sample bias.
        let dim = 64usize;
        let mk = |seed: u64| -> Vec<f32> {
            // deterministic pseudo-random unit-ish vector around e0 with noise
            let mut v = vec![0.0f32; dim];
            v[0] = 1.0;
            let mut s = seed.wrapping_mul(2654435761);
            for x in v.iter_mut() {
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1);
                *x += ((s >> 33) as f32 / u32::MAX as f32 - 0.5) * 0.6;
            }
            v
        };
        let base: Vec<Vec<f32>> = (0..8).map(mk).collect();
        let (_c, mu, sigma) = baseline_stats(&base).expect("stats");
        assert!(sigma > 0.0, "sigma must be positive on varied baseline");
        // A held-out turn from the same distribution:
        let recent = mk(999);
        let (c, _, _) = baseline_stats(&base).unwrap();
        let d_r = drift_score(&c, &recent);
        let z = drift_zscore(mu, sigma, d_r);
        // Unbiased: |z| should be small (well under z_fire=2.5), NOT ~+1..2.5.
        assert!(z.abs() < 2.0, "no-drift z should be near 0, got {z} (bias?)");
    }

    #[test]
    fn drift_zscore_relative_floor_tames_narrow_baseline() {
        // tiny sigma but non-trivial mu → relative floor 0.2*mu dominates,
        // so a modest shift does NOT explode z.
        let mu = 0.30;
        let sigma = 0.001; // near-degenerate
        let d_r = mu + 0.30; // a "modest" shift equal to mu
        let z = drift_zscore(mu, sigma, d_r);
        // sigma_eff = max(0.001, 0.05, 0.2*0.30=0.06) = 0.06 → z = 0.30/0.06 = 5, clamped ok, but NOT 300.
        assert!(z <= Z_CAP && z < 6.0, "relative floor must cap sensitivity, got {z}");
    }

    #[test]
    fn drift_zscore_clamps_to_zcap() {
        let z = drift_zscore(0.1, 0.0001, 5.0); // huge
        assert!((z - Z_CAP).abs() < 1e-3, "z clamps to Z_CAP, got {z}");
    }

    #[test]
    fn hysteresis_schmitt_fire_release_hold() {
        // inactive: only fires above z_fire
        assert!(!hysteresis_decision(2.4, false, 2.5, 1.0));
        assert!(hysteresis_decision(2.6, false, 2.5, 1.0));
        // active: only releases below z_release
        assert!(hysteresis_decision(1.1, true, 2.5, 1.0));   // hold (in band)
        assert!(!hysteresis_decision(0.9, true, 2.5, 1.0));  // release
        // band holds BOTH states
        assert!(hysteresis_decision(1.5, true, 2.5, 1.0));   // stays active
        assert!(!hysteresis_decision(1.5, false, 2.5, 1.0)); // stays inactive
    }

    #[test]
    fn baseline_stats_none_below_two() {
        assert!(baseline_stats(&[]).is_none());
        assert!(baseline_stats(&[vec![1.0, 0.0]]).is_none()); // n<2 → no dispersion
    }
}
