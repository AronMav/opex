use serde_json::Value;

/// Parsed GitHub webhook event with human-readable summary.
/// Fields beyond `summary` are available for future filtering and audit logging.
#[allow(dead_code)]
pub struct GitHubEvent {
    pub event_type: String,
    pub action: Option<String>,
    pub summary: String,
    pub repo: String,
    pub sender: String,
    pub url: Option<String>,
}

/// Parse a GitHub webhook event into a structured summary.
/// `event_type` comes from the `X-GitHub-Event` header.
pub fn parse_github_event(event_type: &str, payload: &Value) -> GitHubEvent {
    let repo = payload
        .pointer("/repository/full_name")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown/repo")
        .to_string();
    let sender = payload
        .pointer("/sender/login")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let action = payload
        .get("action")
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string);

    let (summary, url) = match event_type {
        "push" => parse_push(payload, &repo, &sender),
        "pull_request" => parse_pull_request(payload, &repo, &sender, action.as_deref()),
        "issues" => parse_issues(payload, &repo, &sender, action.as_deref()),
        "issue_comment" => parse_issue_comment(payload, &repo, &sender),
        "pull_request_review" => parse_pr_review(payload, &repo, &sender),
        "check_run" | "check_suite" => parse_ci(event_type, payload, &repo),
        "release" => parse_release(payload, &repo, &sender, action.as_deref()),
        _ => (
            format!("GitHub event '{event_type}' in {repo}"),
            None,
        ),
    };

    GitHubEvent {
        event_type: event_type.to_string(),
        action,
        summary,
        repo,
        sender,
        url,
    }
}

fn parse_push(payload: &Value, repo: &str, sender: &str) -> (String, Option<String>) {
    let branch = payload
        .get("ref")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .strip_prefix("refs/heads/")
        .unwrap_or("unknown");

    let commits = payload.get("commits").and_then(|v| v.as_array());
    let count = commits.map_or(0, std::vec::Vec::len);

    let messages: Vec<&str> = commits
        .map(|arr| {
            arr.iter()
                .take(3)
                .filter_map(|c| c.get("message").and_then(|m| m.as_str()))
                .collect()
        })
        .unwrap_or_default();

    let msgs = if messages.is_empty() {
        String::new()
    } else {
        let quoted: Vec<String> = messages.iter().map(|m| {
            // Take first line only
            let first_line = m.lines().next().unwrap_or(m);
            format!("'{first_line}'")
        }).collect();
        format!(": {}", quoted.join(", "))
    };

    let url = payload
        .get("compare")
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string);

    (
        format!(
            "User @{} pushed {} commit{} to {} in {}{}",
            sender,
            count,
            if count == 1 { "" } else { "s" },
            branch,
            repo,
            msgs
        ),
        url,
    )
}

fn parse_pull_request(
    payload: &Value,
    repo: &str,
    sender: &str,
    action: Option<&str>,
) -> (String, Option<String>) {
    let pr = payload.get("pull_request");
    let number = pr
        .and_then(|p| p.get("number"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let title = pr
        .and_then(|p| p.get("title"))
        .and_then(|t| t.as_str())
        .unwrap_or("untitled");
    let url = pr
        .and_then(|p| p.get("html_url"))
        .and_then(|u| u.as_str())
        .map(std::string::ToString::to_string);
    let action_str = action.unwrap_or("unknown");

    (
        format!(
            "PR #{number} {action_str} by @{sender} in {repo}: '{title}'"
        ),
        url,
    )
}

fn parse_issues(
    payload: &Value,
    repo: &str,
    sender: &str,
    action: Option<&str>,
) -> (String, Option<String>) {
    let issue = payload.get("issue");
    let number = issue
        .and_then(|i| i.get("number"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let title = issue
        .and_then(|i| i.get("title"))
        .and_then(|t| t.as_str())
        .unwrap_or("untitled");
    let url = issue
        .and_then(|i| i.get("html_url"))
        .and_then(|u| u.as_str())
        .map(std::string::ToString::to_string);
    let action_str = action.unwrap_or("unknown");

    (
        format!(
            "Issue #{number} {action_str} by @{sender} in {repo}: '{title}'"
        ),
        url,
    )
}

fn parse_issue_comment(
    payload: &Value,
    repo: &str,
    sender: &str,
) -> (String, Option<String>) {
    let issue_number = payload
        .pointer("/issue/number")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let body = payload
        .pointer("/comment/body")
        .and_then(|b| b.as_str())
        .unwrap_or("");
    let body_preview = if body.len() > 100 {
        format!("{}...", &body[..body.floor_char_boundary(100)])
    } else {
        body.to_string()
    };
    let url = payload
        .pointer("/comment/html_url")
        .and_then(|u| u.as_str())
        .map(std::string::ToString::to_string);

    (
        format!(
            "Comment by @{sender} on issue #{issue_number} in {repo}: '{body_preview}'"
        ),
        url,
    )
}

fn parse_pr_review(
    payload: &Value,
    repo: &str,
    sender: &str,
) -> (String, Option<String>) {
    let pr_number = payload
        .pointer("/pull_request/number")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let state = payload
        .pointer("/review/state")
        .and_then(|s| s.as_str())
        .unwrap_or("unknown");
    let url = payload
        .pointer("/review/html_url")
        .and_then(|u| u.as_str())
        .map(std::string::ToString::to_string);

    (
        format!(
            "Review {state} by @{sender} on PR #{pr_number} in {repo}"
        ),
        url,
    )
}

fn parse_ci(
    event_type: &str,
    payload: &Value,
    repo: &str,
) -> (String, Option<String>) {
    let key = if event_type == "check_run" {
        "check_run"
    } else {
        "check_suite"
    };
    let obj = payload.get(key);
    let name = obj
        .and_then(|o| o.get("name").or_else(|| o.get("app").and_then(|a| a.get("name"))))
        .and_then(|n| n.as_str())
        .unwrap_or("unknown");
    let conclusion = obj
        .and_then(|o| o.get("conclusion"))
        .and_then(|c| c.as_str())
        .unwrap_or("pending");
    let sha = obj
        .and_then(|o| o.get("head_sha"))
        .and_then(|s| s.as_str())
        .unwrap_or("unknown");
    let sha_short = if sha.len() >= 7 { &sha[..7] } else { sha };

    (
        format!(
            "CI '{name}' {conclusion} on {sha_short} in {repo}"
        ),
        None,
    )
}

fn parse_release(
    payload: &Value,
    repo: &str,
    sender: &str,
    action: Option<&str>,
) -> (String, Option<String>) {
    let release = payload.get("release");
    let tag_name = release
        .and_then(|r| r.get("tag_name"))
        .and_then(|t| t.as_str())
        .unwrap_or("unknown");
    let url = release
        .and_then(|r| r.get("html_url"))
        .and_then(|u| u.as_str())
        .map(std::string::ToString::to_string);
    let action_str = action.unwrap_or("unknown");

    (
        format!(
            "Release {tag_name} {action_str} in {repo} by @{sender}"
        ),
        url,
    )
}
