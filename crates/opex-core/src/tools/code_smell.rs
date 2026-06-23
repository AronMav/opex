//! Non-blocking dangerous-code pattern warnings for agent-written files.

/// (needle, optional excluder, label). If `excluder` is non-empty and present, skip.
type Rule = (&'static str, &'static str, &'static str);

const PY: &[Rule] = &[
    ("eval(", "", "eval"),
    ("exec(", "", "exec"),
    ("pickle.load", "", "pickle.load"),
    ("yaml.load(", "loader=", "yaml.load-unsafe"),
    ("os.system(", "", "os.system"),
    ("shell=true", "", "subprocess-shell"),
    ("verify=false", "", "tls-verify-off"),
];
const JS: &[Rule] = &[
    ("eval(", "", "eval"),
    ("dangerouslysetinnerhtml", "", "dangerouslySetInnerHTML"),
    ("child_process.exec(", "", "child_process.exec"),
    ("new function(", "", "new-Function"),
];
const SH: &[Rule] = &[
    ("| sh", "", "pipe-to-sh"),
    ("| bash", "", "pipe-to-sh"),
    ("rm -rf /", "", "rm-root"),
];

fn rules_for(filename: &str) -> &'static [Rule] {
    let lower = filename.to_lowercase();
    if lower.ends_with(".py") {
        PY
    } else if lower.ends_with(".js")
        || lower.ends_with(".ts")
        || lower.ends_with(".tsx")
        || lower.ends_with(".jsx")
    {
        JS
    } else if lower.ends_with(".sh") || lower.ends_with(".bash") {
        SH
    } else {
        &[]
    }
}

pub fn scan_written(filename: &str, content: &str) -> Vec<&'static str> {
    let lower = content.to_lowercase();
    let mut out: Vec<&'static str> = Vec::new();
    for &(needle, excl, label) in rules_for(filename) {
        if lower.contains(needle) && (excl.is_empty() || !lower.contains(excl)) && !out.contains(&label) {
            out.push(label);
        }
    }
    out
}

/// Formatted non-blocking note (empty when clean) to append to a write result.
pub fn warning_for(filename: &str, content: &str) -> String {
    let labels = scan_written(filename, content);
    if labels.is_empty() {
        String::new()
    } else {
        format!(
            "\n⚠ Security note: potentially unsafe patterns ({}). Review before relying on it.",
            labels.join(", ")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_python() {
        let v = scan_written("x.py", "import os\nos.system('rm')\neval(user_input)");
        assert!(v.contains(&"os.system") && v.contains(&"eval"));
    }

    #[test]
    fn flags_js() {
        let v = scan_written("a.tsx", "el.dangerouslySetInnerHTML = { __html: x }");
        assert!(v.contains(&"dangerouslySetInnerHTML"));
    }

    #[test]
    fn ignores_non_code_and_clean() {
        assert!(scan_written("notes.md", "eval(this) os.system").is_empty());
        assert!(scan_written("ok.py", "print('hello world')").is_empty());
    }

    #[test]
    fn warning_for_formats_or_empty() {
        assert!(warning_for("ok.py", "print(1)").is_empty());
        assert!(warning_for("x.py", "eval(x)").starts_with("\n⚠ Security note:"));
    }
}
