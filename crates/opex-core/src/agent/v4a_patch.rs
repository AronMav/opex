//! V4A patch format — parser + in-memory application (pure, no IO).
//!
//! The V4A envelope (as used by OpenAI's `apply_patch`) describes edits by
//! CONTEXT rather than line numbers, which LLMs produce far more reliably than
//! unified diffs:
//!
//! ```text
//! *** Begin Patch
//! *** Update File: notes/todo.md
//! @@ optional anchor
//!  context line
//! -removed line
//! +added line
//!  context line
//! *** Add File: notes/new.md
//! +first line of the new file
//! +second line
//! *** End Patch
//! ```
//!
//! This module only parses and applies hunks against in-memory strings. All IO,
//! path validation, read-only checks and checkpointing live in the handler
//! (`pipeline::handlers::handle_apply_patch`). Scope: Update + Add only — no
//! Delete/Move (those stay with the dedicated, deny-listed tools).

const BEGIN: &str = "*** Begin Patch";
const END: &str = "*** End Patch";
const UPDATE: &str = "*** Update File:";
const ADD: &str = "*** Add File:";

/// One file operation parsed from a patch envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileOp {
    Update { path: String, hunks: Vec<Hunk> },
    Add { path: String, content: String },
}

/// A single hunk: the old block (context + removed lines, in file order) and
/// the new block (context + added lines). Application locates `old_lines` in the
/// current content and replaces that span with `new_lines`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Hunk {
    pub old_lines: Vec<String>,
    pub new_lines: Vec<String>,
}

/// Parse a V4A patch envelope into file operations.
///
/// Returns a human-readable error for: missing Begin/End, empty path, unknown
/// marker, a hunk with no additions or removals, or an Add section with a
/// malformed (non-`+`) line.
pub fn parse_patch(patch: &str) -> Result<Vec<FileOp>, String> {
    let lines: Vec<&str> = patch.split('\n').collect();

    // Locate the Begin/End envelope (tolerate leading/trailing blank lines).
    let begin = lines
        .iter()
        .position(|l| l.trim_end() == BEGIN)
        .ok_or_else(|| format!("missing '{BEGIN}'"))?;
    let end = lines
        .iter()
        .rposition(|l| l.trim_end() == END)
        .ok_or_else(|| format!("missing '{END}'"))?;
    if end <= begin {
        return Err(format!("'{END}' must come after '{BEGIN}'"));
    }

    let mut ops: Vec<FileOp> = Vec::new();
    // State for the file section currently being accumulated.
    enum Section {
        None,
        Update { path: String, hunks: Vec<Hunk>, cur: Hunk },
        Add { path: String, lines: Vec<String> },
    }
    let mut section = Section::None;

    // Finalize the current hunk into `hunks` if it carries any change.
    fn flush_hunk(hunks: &mut Vec<Hunk>, cur: &mut Hunk) -> Result<(), String> {
        if cur.old_lines.is_empty() && cur.new_lines.is_empty() {
            *cur = Hunk::default();
            return Ok(());
        }
        if cur.old_lines == cur.new_lines {
            return Err("hunk has no additions or removals".to_string());
        }
        hunks.push(std::mem::take(cur));
        Ok(())
    }

    fn close_section(ops: &mut Vec<FileOp>, section: Section) -> Result<(), String> {
        match section {
            Section::None => {}
            Section::Update { path, mut hunks, mut cur } => {
                flush_hunk(&mut hunks, &mut cur)?;
                if hunks.is_empty() {
                    return Err(format!("'{UPDATE} {path}' has no hunks"));
                }
                ops.push(FileOp::Update { path, hunks });
            }
            Section::Add { path, lines } => {
                ops.push(FileOp::Add { path, content: lines.join("\n") });
            }
        }
        Ok(())
    }

    for &raw in &lines[begin + 1..end] {
        let line = raw;
        if let Some(rest) = marker(line, UPDATE) {
            close_section(&mut ops, std::mem::replace(&mut section, Section::None))?;
            let path = rest.trim().to_string();
            if path.is_empty() {
                return Err(format!("'{UPDATE}' with empty path"));
            }
            section = Section::Update { path, hunks: Vec::new(), cur: Hunk::default() };
        } else if let Some(rest) = marker(line, ADD) {
            close_section(&mut ops, std::mem::replace(&mut section, Section::None))?;
            let path = rest.trim().to_string();
            if path.is_empty() {
                return Err(format!("'{ADD}' with empty path"));
            }
            section = Section::Add { path, lines: Vec::new() };
        } else if line.trim_end().starts_with("***") {
            return Err(format!("unknown marker: {}", line.trim_end()));
        } else {
            match &mut section {
                Section::None => {
                    // Stray non-marker content before the first file section.
                    if !line.trim().is_empty() {
                        return Err(format!("content outside a file section: {line}"));
                    }
                }
                Section::Update { hunks, cur, .. } => {
                    if line.starts_with("@@") {
                        flush_hunk(hunks, cur)?;
                    } else {
                        push_hunk_line(cur, line)?;
                    }
                }
                Section::Add { lines, .. } => {
                    if let Some(content) = line.strip_prefix('+') {
                        lines.push(content.to_string());
                    } else if line.is_empty() {
                        lines.push(String::new());
                    } else {
                        return Err(format!("Add File line must start with '+': {line}"));
                    }
                }
            }
        }
    }
    close_section(&mut ops, section)?;

    if ops.is_empty() {
        return Err("patch contains no file operations".to_string());
    }
    Ok(ops)
}

/// Match `*** Marker:` prefix, returning the remainder after the marker.
fn marker<'a>(line: &'a str, m: &str) -> Option<&'a str> {
    line.strip_prefix(m)
}

/// Classify and record one hunk body line (context / removed / added).
fn push_hunk_line(cur: &mut Hunk, line: &str) -> Result<(), String> {
    if let Some(c) = line.strip_prefix('+') {
        cur.new_lines.push(c.to_string());
    } else if let Some(c) = line.strip_prefix('-') {
        cur.old_lines.push(c.to_string());
    } else if let Some(c) = line.strip_prefix(' ') {
        cur.old_lines.push(c.to_string());
        cur.new_lines.push(c.to_string());
    } else if line.is_empty() {
        // Blank context line.
        cur.old_lines.push(String::new());
        cur.new_lines.push(String::new());
    } else {
        return Err(format!("hunk line must start with ' ', '+' or '-': {line}"));
    }
    Ok(())
}

/// Apply hunks to `original`, returning the new content.
///
/// Each hunk's `old_lines` must appear as a contiguous run of lines in the
/// (evolving) content — first an exact match, falling back to trailing-whitespace
/// tolerant comparison. Hunks are matched in order, each after the previous
/// match. A hunk that cannot be located fails the whole application (callers
/// rely on this for atomicity — nothing is written on error).
pub fn apply_hunks(original: &str, hunks: &[Hunk]) -> Result<String, String> {
    let mut lines: Vec<String> = original.split('\n').map(str::to_string).collect();
    let mut search_from = 0usize;

    for (idx, hunk) in hunks.iter().enumerate() {
        if hunk.old_lines.is_empty() {
            return Err(format!("hunk {} has no context to locate (need ' ' or '-' lines)", idx + 1));
        }
        let at = find_run(&lines, &hunk.old_lines, search_from)
            .ok_or_else(|| format!("hunk {}: context not found in file", idx + 1))?;
        let end = at + hunk.old_lines.len();
        lines.splice(at..end, hunk.new_lines.iter().cloned());
        search_from = at + hunk.new_lines.len();
    }
    Ok(lines.join("\n"))
}

/// Find the first index >= `from` where `needle` matches a contiguous run in
/// `haystack` (exact, then trailing-whitespace tolerant).
fn find_run(haystack: &[String], needle: &[String], from: usize) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    let last = haystack.len() - needle.len();
    // Exact pass first, then a tolerant pass — prefer exact matches.
    for tolerant in [false, true] {
        let mut i = from;
        while i <= last {
            let hit = needle.iter().zip(&haystack[i..i + needle.len()]).all(|(n, h)| {
                if tolerant { n.trim_end() == h.trim_end() } else { n == h }
            });
            if hit {
                return Some(i);
            }
            i += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hunk(old: &[&str], new: &[&str]) -> Hunk {
        Hunk {
            old_lines: old.iter().map(|s| s.to_string()).collect(),
            new_lines: new.iter().map(|s| s.to_string()).collect(),
        }
    }

    // ── parse ────────────────────────────────────────────────────────────────

    #[test]
    fn parse_update_single_hunk() {
        let p = "*** Begin Patch\n*** Update File: a.md\n x\n-old\n+new\n y\n*** End Patch";
        let ops = parse_patch(p).unwrap();
        assert_eq!(
            ops,
            vec![FileOp::Update {
                path: "a.md".into(),
                hunks: vec![hunk(&["x", "old", "y"], &["x", "new", "y"])],
            }]
        );
    }

    #[test]
    fn parse_add_file() {
        let p = "*** Begin Patch\n*** Add File: n.md\n+line1\n+line2\n*** End Patch";
        let ops = parse_patch(p).unwrap();
        assert_eq!(ops, vec![FileOp::Add { path: "n.md".into(), content: "line1\nline2".into() }]);
    }

    #[test]
    fn parse_multi_file_and_multi_hunk() {
        let p = "*** Begin Patch\n*** Update File: a.md\n a\n-b\n+B\n@@\n c\n-d\n+D\n*** Add File: n.md\n+x\n*** End Patch";
        let ops = parse_patch(p).unwrap();
        assert_eq!(ops.len(), 2);
        match &ops[0] {
            FileOp::Update { path, hunks } => {
                assert_eq!(path, "a.md");
                assert_eq!(hunks.len(), 2);
            }
            _ => panic!("expected Update"),
        }
        assert_eq!(ops[1], FileOp::Add { path: "n.md".into(), content: "x".into() });
    }

    #[test]
    fn parse_missing_begin_errors() {
        assert!(parse_patch("*** Update File: a\n+x\n*** End Patch").is_err());
    }

    #[test]
    fn parse_missing_end_errors() {
        assert!(parse_patch("*** Begin Patch\n*** Update File: a\n-x\n+y").is_err());
    }

    #[test]
    fn parse_empty_path_errors() {
        assert!(parse_patch("*** Begin Patch\n*** Update File: \n-x\n+y\n*** End Patch").is_err());
    }

    #[test]
    fn parse_hunk_no_change_errors() {
        // Only context, no +/- → not a real change.
        let p = "*** Begin Patch\n*** Update File: a.md\n x\n y\n*** End Patch";
        assert!(parse_patch(p).is_err());
    }

    #[test]
    fn parse_unknown_marker_errors() {
        let p = "*** Begin Patch\n*** Delete File: a.md\n*** End Patch";
        assert!(parse_patch(p).is_err());
    }

    // ── apply ──────────────────────────────────────────────────────────────────

    #[test]
    fn apply_replace_with_context() {
        let out = apply_hunks("a\nb\nc", &[hunk(&["a", "b"], &["a", "B"])]).unwrap();
        assert_eq!(out, "a\nB\nc");
    }

    #[test]
    fn apply_pure_replace_no_context() {
        let out = apply_hunks("a\nb\nc", &[hunk(&["b"], &["B"])]).unwrap();
        assert_eq!(out, "a\nB\nc");
    }

    #[test]
    fn apply_insertion() {
        let out = apply_hunks("a\nb", &[hunk(&["a", "b"], &["a", "X", "b"])]).unwrap();
        assert_eq!(out, "a\nX\nb");
    }

    #[test]
    fn apply_not_found_errors() {
        assert!(apply_hunks("a\nb\nc", &[hunk(&["zzz"], &["q"])]).is_err());
    }

    #[test]
    fn apply_multiple_hunks_in_order() {
        let out = apply_hunks(
            "a\nb\nc\nd",
            &[hunk(&["a"], &["A"]), hunk(&["d"], &["D"])],
        )
        .unwrap();
        assert_eq!(out, "A\nb\nc\nD");
    }

    #[test]
    fn apply_trailing_whitespace_tolerant() {
        // File line has a trailing space; hunk context does not.
        let out = apply_hunks("a\nb \nc", &[hunk(&["b"], &["B"])]).unwrap();
        assert_eq!(out, "a\nB\nc");
    }

    #[test]
    fn apply_preserves_trailing_newline() {
        let out = apply_hunks("a\nb\n", &[hunk(&["a"], &["A"])]).unwrap();
        assert_eq!(out, "A\nb\n");
    }

    #[test]
    fn parse_then_apply_end_to_end() {
        let p = "*** Begin Patch\n*** Update File: a.md\n keep\n-drop\n+add\n*** End Patch";
        let ops = parse_patch(p).unwrap();
        let FileOp::Update { hunks, .. } = &ops[0] else { panic!() };
        let out = apply_hunks("keep\ndrop\ntail", hunks).unwrap();
        assert_eq!(out, "keep\nadd\ntail");
    }
}
