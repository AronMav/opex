//! FSE Phase 9 regression + retirement guards.
use std::path::Path;

fn repo_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .to_path_buf()
}

/// No agent config or scaffold may still instruct an agent to honor the retired
/// image/audio/document arms. The video arm lives only in media-processing.md,
/// which is a skill, not an agent TOML.
#[test]
fn no_config_references_retired_media_skill() {
    let root = repo_root();
    let mut offenders = Vec::new();
    for dir in ["crates/hydeclaw-core/scaffold", "crates/hydeclaw-core/tests/fixtures/agents"] {
        let p = root.join(dir);
        if !p.exists() {
            continue;
        }
        for entry in walk(&p) {
            let txt = std::fs::read_to_string(&entry).unwrap_or_default();
            // The skill name may appear; the retired *YAML tools* must not be
            // mandated by an agent config/scaffold.
            if txt.contains("transcribe_audio") || txt.contains("auto-describe") {
                offenders.push(entry.display().to_string());
            }
        }
    }
    assert!(offenders.is_empty(), "retired media arms referenced in: {offenders:?}");
}

fn walk(p: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(p) {
        for e in rd.flatten() {
            let path = e.path();
            if path.is_dir() {
                out.extend(walk(&path));
            } else {
                out.push(path);
            }
        }
    }
    out
}
