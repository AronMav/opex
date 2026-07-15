//! One-shot startup seed of provider profiles (m084).
//!
//! Migrates the legacy per-agent LLM config (`provider_connection`, `model`,
//! `fallback_provider`, `tts_provider`, `imagegen_provider`) and the global
//! `provider_active` capability map into the new `profiles` table:
//!
//!   * a single `Default` profile whose media slots (tts/stt/vision/imagegen/
//!     websearch/compaction) come from `provider_active`, and whose `text`
//!     slot comes from the first agent (base agent preferred, else the first
//!     alphabetically);
//!   * one profile per agent whose legacy config differs from `Default`
//!     (different text chain, or a `tts_provider`/`imagegen_provider` override).
//!
//! It then rewrites every agent TOML — setting `[agent].profile` and stripping
//! the six legacy keys — and clears the six migrated capabilities from
//! `provider_active` (embedding is intentionally left in place).
//!
//! Idempotent via `sys_flags['profiles_seed_v1']`. Ordering is deliberate:
//! DB profiles are created first (idempotently), THEN the TOMLs are rewritten,
//! THEN `provider_active` is cleared, and the flag is set LAST — so any failure
//! before the flag leaves the source-of-truth (agent TOMLs + provider_active)
//! intact enough for the next startup to re-run cleanly.

use anyhow::Context;
use sqlx::PgPool;
use std::path::{Path, PathBuf};

use super::profiles::{SlotEntry, Slots};

const FLAG: &str = "profiles_seed_v1";

/// Capabilities that move into the `Default` profile. `embedding` is NOT
/// migrated — it stays in `provider_active` and is left untouched.
const MIGRATED_CAPS: [&str; 6] = ["tts", "stt", "vision", "imagegen", "websearch", "compaction"];

/// The six legacy `[agent]` keys stripped from every TOML after migration.
const LEGACY_KEYS: [&str; 6] = [
    "provider",
    "model",
    "provider_connection",
    "fallback_provider",
    "tts_provider",
    "imagegen_provider",
];

/// Parsed legacy config for one agent TOML, retained so the document can be
/// rewritten in place after the DB profiles are created.
struct AgentToml {
    path: PathBuf,
    /// Agent name (from `[agent].name`, falling back to the file stem) — used
    /// as the per-agent profile name and for deterministic ordering.
    name: String,
    base: bool,
    /// text-slot chain `[{primary}, {fallback}?]` derived from legacy fields.
    text_chain: Vec<SlotEntry>,
    tts_provider: Option<String>,
    imagegen_provider: Option<String>,
    /// Non-empty `[agent].profile` already present in the TOML — set once the
    /// file has been migrated. Its presence makes the rewrite step a no-op for
    /// this agent (see Fix 2 in `run_profiles_seed`), so a late-stage failure
    /// re-run can't flatten an already-assigned per-agent pointer to `Default`.
    existing_profile: Option<String>,
    doc: toml_edit::DocumentMut,
}

/// Read a non-empty `[agent].<key>` string from a toml_edit table.
fn read_str(agent: Option<&toml_edit::Table>, key: &str) -> Option<String> {
    agent
        .and_then(|t| t.get(key))
        .and_then(|i| i.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

fn parse_agent_toml(path: &Path) -> anyhow::Result<AgentToml> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("read agent TOML {}", path.display()))?;
    let doc = content
        .parse::<toml_edit::DocumentMut>()
        .with_context(|| format!("parse agent TOML {}", path.display()))?;

    let file_stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();

    // Read phase — extract all owned values before we mutate `doc` later.
    let agent = doc.get("agent").and_then(|i| i.as_table());
    let name = read_str(agent, "name").unwrap_or_else(|| file_stem.clone());
    let base = agent
        .and_then(|t| t.get("base"))
        .and_then(|i| i.as_bool())
        .unwrap_or(false);

    let provider_connection = read_str(agent, "provider_connection");
    let model = read_str(agent, "model");
    let fallback_provider = read_str(agent, "fallback_provider");
    let tts_provider = read_str(agent, "tts_provider");
    let imagegen_provider = read_str(agent, "imagegen_provider");
    let existing_profile = read_str(agent, "profile");

    // Text chain: primary = provider_connection ONLY (carrying the agent's
    // `model`); then the fallback provider (provider only) if set. The bare
    // `provider` field is a provider_TYPE (e.g. "ollama"), NOT a `providers.name`
    // row, so seeding a text SlotEntry from it would create an unresolvable slot
    // (skipped by `effective_chain` → empty chain → UnconfiguredProvider) and
    // mint a spurious per-agent profile. An agent without `provider_connection`
    // gets an empty text chain and correctly folds into `Default`.
    let mut text_chain = Vec::new();
    if let Some(primary) = provider_connection {
        text_chain.push(SlotEntry {
            provider: primary,
            model,
            voice: None,
        });
    }
    if let Some(fb) = fallback_provider {
        text_chain.push(SlotEntry {
            provider: fb,
            model: None,
            voice: None,
        });
    }

    Ok(AgentToml {
        path: path.to_path_buf(),
        name,
        base,
        text_chain,
        tts_provider,
        imagegen_provider,
        existing_profile,
        doc,
    })
}

/// Create a profile only if a profile of that name does not already exist —
/// makes the create step re-runnable after a partial-failure restart.
async fn ensure_profile(db: &PgPool, name: &str, slots: &Slots) -> anyhow::Result<()> {
    if super::profiles::get_profile_by_name(db, name)
        .await
        .with_context(|| format!("lookup profile {name}"))?
        .is_none()
    {
        super::profiles::create_profile(db, name, slots)
            .await
            .with_context(|| format!("create profile {name}"))?;
    }
    Ok(())
}

pub async fn run_profiles_seed(db: &PgPool, agents_dir: &Path) -> anyhow::Result<()> {
    // 1. Gate — bail out early if we already ran.
    if opex_db::sys_flags::try_get(db, FLAG)
        .await
        .context("profiles seed gate-check")?
        .is_some()
    {
        return Ok(());
    }

    // 2. Default media slots from provider_active (priority order preserved).
    let mut default_slots = Slots::new();
    for cap in MIGRATED_CAPS {
        let rows = super::providers::get_active_providers(db, cap)
            .await
            .with_context(|| format!("read active providers for {cap}"))?;
        if rows.is_empty() {
            continue;
        }
        default_slots.insert(
            cap.to_string(),
            rows.into_iter()
                .map(|(provider, _)| SlotEntry {
                    provider,
                    model: None,
                    voice: None,
                })
                .collect(),
        );
    }

    // 3. Parse every agent TOML (alphabetical by filename for a deterministic
    //    "first agent").
    let mut paths: Vec<PathBuf> = std::fs::read_dir(agents_dir)
        .with_context(|| format!("read agents dir {}", agents_dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("toml"))
        .collect();
    paths.sort();

    // A single malformed TOML must NOT abort the seed — that would leave
    // `Default` uncreated → every agent resolves empty slots → all capabilities
    // disabled fleet-wide. Log + skip the bad file; the skipped agent keeps its
    // old TOML and folds into the serde-default `profile = "Default"` at load.
    let mut agents: Vec<AgentToml> = Vec::with_capacity(paths.len());
    for path in &paths {
        match parse_agent_toml(path) {
            Ok(a) => agents.push(a),
            Err(e) => tracing::warn!(
                path = %path.display(),
                error = %format!("{e:#}"),
                "skipping unparseable agent TOML during profiles seed"
            ),
        }
    }

    // 3b. Default text slot: base agent preferred, else first alphabetically.
    let default_idx = agents.iter().position(|a| a.base).unwrap_or(0);
    let default_text_chain: Vec<SlotEntry> = agents
        .get(default_idx)
        .map(|a| a.text_chain.clone())
        .unwrap_or_default();
    if !default_text_chain.is_empty() {
        default_slots.insert("text".to_string(), default_text_chain.clone());
    }

    // 4. Decide per agent: Default vs a dedicated profile, and build its slots.
    //    (profile name to assign, overlaid slots if dedicated)
    let mut assignments: Vec<(usize, String)> = Vec::with_capacity(agents.len());
    let mut per_agent_profiles: Vec<(String, Slots)> = Vec::new();
    for (idx, a) in agents.iter().enumerate() {
        let differs = a.text_chain != default_text_chain
            || a.tts_provider.is_some()
            || a.imagegen_provider.is_some();

        if differs {
            let mut slots = default_slots.clone();
            if !a.text_chain.is_empty() {
                slots.insert("text".to_string(), a.text_chain.clone());
            }
            if let Some(tts) = &a.tts_provider {
                slots.insert(
                    "tts".to_string(),
                    vec![SlotEntry {
                        provider: tts.clone(),
                        model: None,
                        voice: None,
                    }],
                );
            }
            if let Some(ig) = &a.imagegen_provider {
                slots.insert(
                    "imagegen".to_string(),
                    vec![SlotEntry {
                        provider: ig.clone(),
                        model: None,
                        voice: None,
                    }],
                );
            }
            per_agent_profiles.push((a.name.clone(), slots));
            assignments.push((idx, a.name.clone()));
        } else {
            assignments.push((idx, super::profiles::DEFAULT_PROFILE.to_string()));
        }
    }

    // 5. Create DB profiles first (idempotent) — Default, then per-agent.
    ensure_profile(db, super::profiles::DEFAULT_PROFILE, &default_slots).await?;
    for (name, slots) in &per_agent_profiles {
        ensure_profile(db, name, slots).await?;
    }

    // 6. Rewrite each agent TOML: set profile, strip legacy keys.
    //    Fix 2 (per-TOML idempotency): if the file ALREADY carries a non-empty
    //    `[agent].profile`, it was migrated on a prior run — leave it untouched.
    //    Without this, a late-stage failure (provider_active clear or flag-set)
    //    would re-run with all legacy fields already stripped → every agent
    //    computes `differs == false` → its `profile = "Arty"` gets clobbered
    //    back to `profile = "Default"`.
    for (idx, profile_name) in &assignments {
        let a = &mut agents[*idx];
        if a.existing_profile.is_some() {
            continue;
        }
        a.doc["agent"]["profile"] = toml_edit::value(profile_name.as_str());
        if let Some(table) = a.doc["agent"].as_table_mut() {
            for key in LEGACY_KEYS {
                table.remove(key);
            }
        }
        std::fs::write(&a.path, a.doc.to_string())
            .with_context(|| format!("write agent TOML {}", a.path.display()))?;
    }

    // 7. Clear the migrated capabilities from provider_active (embedding stays).
    for cap in MIGRATED_CAPS {
        super::providers::set_provider_active_list(db, cap, &[])
            .await
            .with_context(|| format!("clear provider_active for {cap}"))?;
    }

    // 8. Mark done — only reached if every step above succeeded.
    opex_db::sys_flags::upsert(db, FLAG, serde_json::json!(true))
        .await
        .context("set profiles seed flag")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test(migrations = "../../migrations")]
    async fn seed_builds_default_from_provider_active_and_cleans_up(pool: sqlx::PgPool) {
        sqlx::query("INSERT INTO providers (name, type, provider_type, enabled) VALUES \
            ('mm','tts','minimax',true),('sx','websearch','searxng',true),('emb','embedding','ollama',true),('llm1','llm','openai_compat',true)")
            .execute(&pool).await.unwrap();
        crate::db::providers::set_provider_active_list(&pool, "tts", &[("mm".into(), 1)]).await.unwrap();
        crate::db::providers::set_provider_active_list(&pool, "websearch", &[("sx".into(), 1)]).await.unwrap();
        crate::db::providers::set_provider_active_list(&pool, "embedding", &[("emb".into(), 1)]).await.unwrap();

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Arty.toml"),
            "[agent]\nname = \"Arty\"\ntemperature = 1.0\nprovider = \"ollama\"\nmodel = \"kimi\"\nprovider_connection = \"llm1\"\nfallback_provider = \"llm1\"\n").unwrap();

        run_profiles_seed(&pool, dir.path()).await.unwrap();

        // Default создан из active
        let d = crate::db::profiles::get_profile_by_name(&pool, "Default").await.unwrap().unwrap();
        let slots = d.parsed_slots();
        assert_eq!(slots["tts"][0].provider, "mm");
        assert_eq!(slots["websearch"][0].provider, "sx");
        assert!(!slots.contains_key("embedding"));
        // text-слот Default взят из первого агента
        assert_eq!(slots["text"][0].provider, "llm1");
        assert_eq!(slots["text"][0].model.as_deref(), Some("kimi"));

        // TOML переписан
        let rewritten = std::fs::read_to_string(dir.path().join("Arty.toml")).unwrap();
        assert!(rewritten.contains("profile = "));
        assert!(!rewritten.contains("provider_connection"));
        assert!(!rewritten.contains("fallback_provider"));

        // provider_active: остался только embedding
        let rows = crate::db::providers::list_provider_active(&pool).await.unwrap();
        assert!(rows.iter().all(|r| r.capability == "embedding"));

        // идемпотентность
        run_profiles_seed(&pool, dir.path()).await.unwrap();
        assert_eq!(crate::db::profiles::list_profiles(&pool).await.unwrap().len(), 1);
    }

    /// Fix 2: a late-stage failure (flag never set) must NOT flatten an
    /// already-migrated per-agent `profile = "Arty"` back to `Default` on re-run.
    #[sqlx::test(migrations = "../../migrations")]
    async fn rerun_after_flag_loss_preserves_per_agent_profile(pool: sqlx::PgPool) {
        sqlx::query("INSERT INTO providers (name, type, provider_type, enabled) VALUES \
            ('mm','tts','minimax',true),('llm1','llm','openai_compat',true)")
            .execute(&pool).await.unwrap();

        let dir = tempfile::tempdir().unwrap();
        // Base agent → source of Default text slot.
        std::fs::write(dir.path().join("Base.toml"),
            "[agent]\nname = \"Base\"\nbase = true\nprovider_connection = \"llm1\"\nmodel = \"kimi\"\n").unwrap();
        // Arty has a tts override → differs from Default → gets its OWN profile.
        std::fs::write(dir.path().join("Arty.toml"),
            "[agent]\nname = \"Arty\"\nprovider_connection = \"llm1\"\nmodel = \"kimi\"\ntts_provider = \"mm\"\n").unwrap();

        run_profiles_seed(&pool, dir.path()).await.unwrap();

        // After the first run Arty.toml points to its own profile.
        let arty1 = std::fs::read_to_string(dir.path().join("Arty.toml")).unwrap();
        assert!(arty1.contains("profile = \"Arty\""), "first run: {arty1}");
        assert!(crate::db::profiles::get_profile_by_name(&pool, "Arty").await.unwrap().is_some());

        // Simulate a late-stage failure: the flag was never persisted.
        opex_db::sys_flags::delete(&pool, FLAG).await.unwrap();

        // Re-run. The already-rewritten TOML has NO legacy fields, so a naive
        // re-decision would compute differs==false and clobber it to Default.
        run_profiles_seed(&pool, dir.path()).await.unwrap();

        let arty2 = std::fs::read_to_string(dir.path().join("Arty.toml")).unwrap();
        assert!(arty2.contains("profile = \"Arty\""),
            "re-run must preserve per-agent pointer, got: {arty2}");
        assert!(!arty2.contains("profile = \"Default\""), "must not flatten to Default: {arty2}");
    }

    /// Fix 3: one malformed TOML alongside a good one still creates `Default`
    /// and migrates the parseable agent — the seed does not error out.
    #[sqlx::test(migrations = "../../migrations")]
    async fn malformed_toml_is_skipped_not_fatal(pool: sqlx::PgPool) {
        sqlx::query("INSERT INTO providers (name, type, provider_type, enabled) VALUES \
            ('llm1','llm','openai_compat',true)")
            .execute(&pool).await.unwrap();

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Good.toml"),
            "[agent]\nname = \"Good\"\nbase = true\nprovider_connection = \"llm1\"\nmodel = \"kimi\"\n").unwrap();
        // Unparseable TOML — must be skipped, not abort the whole seed.
        std::fs::write(dir.path().join("Bad.toml"), "[agent]\nname = \"Bad\nthis = is not : valid ]]}").unwrap();

        run_profiles_seed(&pool, dir.path()).await.unwrap();

        // Default was still created and the good agent's text slot migrated.
        let d = crate::db::profiles::get_profile_by_name(&pool, "Default").await.unwrap().unwrap();
        assert_eq!(d.parsed_slots()["text"][0].provider, "llm1");
        // The good TOML was rewritten; the bad one was left untouched.
        let good = std::fs::read_to_string(dir.path().join("Good.toml")).unwrap();
        assert!(good.contains("profile = "));
        let bad = std::fs::read_to_string(dir.path().join("Bad.toml")).unwrap();
        assert!(!bad.contains("profile = "));
    }
}
