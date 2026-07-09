//! Pipeline step: cron job management.
//! Extracted from engine_handlers.rs as a free function taking &CommandContext.

use super::CommandContext;
use std::sync::Weak;
use uuid::Uuid;
use crate::scheduler::{compute_next_run, Scheduler, ScheduledJob};

/// Internal tool: manage scheduled cron jobs.
/// Mutating actions (create/delete/run) require base agent.
pub async fn handle_cron(ctx: &CommandContext<'_>, args: &serde_json::Value) -> String {
    let cfg = ctx.cfg;
    let action = args
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // Only base agents can add/update/remove/run cron jobs.
    // list and history are read-only, allowed for all agents.
    if !cfg.agent.base && !matches!(action, "list" | "history" | "runs") {
        return format!("Error: cron '{}' requires a base agent. Only base agents can manage cron jobs.", action);
    }

    let scheduler = match &cfg.scheduler {
        Some(s) => s,
        None => return "Error: scheduler not available".to_string(),
    };

    match action {
        "list" => {
            let agent_filter = if cfg.agent.base { None } else { Some(cfg.agent.name.as_str()) };
            let jobs_result = Scheduler::list_jobs(&cfg.db, agent_filter).await;
            match jobs_result {
                Ok(jobs) => {
                    if jobs.is_empty() {
                        return "No scheduled jobs.".to_string();
                    }
                    let mut out = format!("Scheduled jobs ({}):\n", jobs.len());
                    for job in &jobs {
                        let next = if job.enabled {
                            compute_next_run(&job.cron_expr, &job.timezone)
                                .unwrap_or_else(|| "unknown".to_string())
                        } else {
                            "disabled".to_string()
                        };
                        let announce = job.announce_to.as_ref()
                            .map(|v| format!("  announce_to: {}\n", v))
                            .unwrap_or_default();
                        let agent_label = if cfg.agent.base && job.agent_id != cfg.agent.name {
                            format!("  agent: {}\n", job.agent_id)
                        } else {
                            String::new()
                        };
                        out.push_str(&format!(
                            "- **{}** (id: {})\n{}  cron: `{}` ({})\n  task: {}\n  enabled: {}, last run: {}\n  next run: {}\n{}",
                            job.name,
                            job.id,
                            agent_label,
                            job.cron_expr,
                            job.timezone,
                            job.task_message,
                            job.enabled,
                            job.last_run_at
                                .map(|t| t.to_string())
                                .unwrap_or_else(|| "never".to_string()),
                            next,
                            announce,
                        ));
                    }
                    out
                }
                Err(e) => format!("Error listing jobs: {}", e),
            }
        }
        "add" => {
            let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let cron_expr = args.get("cron").and_then(|v| v.as_str()).unwrap_or("");
            let timezone = args
                .get("timezone")
                .and_then(|v| v.as_str())
                .unwrap_or(cfg.default_timezone.as_str());
            let task_raw = args.get("task").and_then(|v| v.as_str()).unwrap_or("");
            // T10 побочный пункт B (hermes parity): strip invisible/zero-width
            // Unicode from the stored task prompt so it can't smuggle hidden
            // prompt-injection payloads past a human reviewing the cron job
            // list, or past any future text-based scanner.
            let task_sanitized = crate::redact::strip_invisible_unicode(task_raw);
            let task = task_sanitized.as_str();
            let announce_to = args.get("announce_to").cloned();
            let autonomous_goal = args.get("autonomous_goal").and_then(|v| v.as_str()).map(String::from);
            let target_agent = if cfg.agent.base {
                args.get("agent").and_then(|v| v.as_str()).unwrap_or(&cfg.agent.name).to_string()
            } else {
                cfg.agent.name.clone()
            };

            if name.is_empty() || cron_expr.is_empty() || task.is_empty() {
                return "Error: 'name', 'cron', and 'task' are required for add".to_string();
            }

            // Validate cron expression (5 fields)
            let fields: Vec<&str> = cron_expr.split_whitespace().collect();
            if fields.len() != 5 {
                return "Error: cron expression must have 5 fields (min hour dom mon dow)".to_string();
            }

            // Insert into DB
            let row = match sqlx::query_scalar::<_, Uuid>(
                "INSERT INTO scheduled_jobs (agent_id, name, cron_expr, timezone, task_message, announce_to, autonomous_goal) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
            )
            .bind(&target_agent)
            .bind(name)
            .bind(cron_expr)
            .bind(timezone)
            .bind(task)
            .bind(&announce_to)
            .bind(&autonomous_goal)
            .fetch_one(&cfg.db)
            .await
            {
                Ok(id) => id,
                Err(e) => return format!("Error saving job to DB: {}", e),
            };

            // Hot-schedule the job immediately (only for self -- other agents activate on restart)
            let is_self = target_agent == cfg.agent.name;
            let activated = if is_self {
                if let Some(arc) = ctx.state.self_ref.get().and_then(Weak::upgrade) {
                    match scheduler.add_dynamic_job(
                        row, cron_expr, timezone,
                        task.to_string(), target_agent.clone(),
                        arc, cfg.db.clone(), announce_to, false, 0, false, None, None,
                        autonomous_goal,
                    ).await {
                        Ok(()) => true,
                        Err(e) => {
                            tracing::warn!(error = %e, "failed to hot-schedule job, will load on restart");
                            false
                        }
                    }
                } else {
                    false
                }
            } else {
                false
            };

            let agent_note = if !is_self { format!(" for agent '{}'", target_agent) } else { String::new() };
            if activated {
                format!(
                    "Job '{}' created and activated{} (id: {}). Cron: `{}` ({}).",
                    name, agent_note, row, cron_expr, timezone
                )
            } else {
                format!(
                    "Job '{}' created{} (id: {}). Cron: `{}` ({}). \
                     It will be activated on next restart. Use action 'run' to execute immediately.",
                    name, agent_note, row, cron_expr, timezone
                )
            }
        }
        "update" => {
            let job_id = args.get("job_id").and_then(|v| v.as_str()).unwrap_or("");
            if job_id.is_empty() {
                return "Error: 'job_id' is required for update".to_string();
            }
            let uuid = match Uuid::parse_str(job_id) {
                Ok(u) => u,
                Err(_) => return "Error: invalid job_id format (expected UUID)".to_string(),
            };

            // Fetch current job (base can update any)
            let current = if cfg.agent.base {
                sqlx::query_as::<_, ScheduledJob>(
                    "SELECT id, agent_id, name, cron_expr, timezone, task_message, enabled, created_at, last_run_at, silent, announce_to, jitter_secs, run_once, run_at \
                     FROM scheduled_jobs WHERE id = $1",
                )
                .bind(uuid)
                .fetch_optional(&cfg.db)
                .await
            } else {
                sqlx::query_as::<_, ScheduledJob>(
                    "SELECT id, agent_id, name, cron_expr, timezone, task_message, enabled, created_at, last_run_at, silent, announce_to, jitter_secs, run_once, run_at \
                     FROM scheduled_jobs WHERE id = $1 AND agent_id = $2",
                )
                .bind(uuid)
                .bind(&cfg.agent.name)
                .fetch_optional(&cfg.db)
                .await
            };

            let current = match current {
                Ok(Some(j)) => j,
                Ok(None) => return "Error: job not found or belongs to another agent".to_string(),
                Err(e) => return format!("Error fetching job: {}", e),
            };

            // Merge: use provided values or keep current
            let name = args.get("name").and_then(|v| v.as_str()).unwrap_or(&current.name);
            let cron_expr = args.get("cron").and_then(|v| v.as_str()).unwrap_or(&current.cron_expr);
            let timezone = args.get("timezone").and_then(|v| v.as_str()).unwrap_or(&current.timezone);
            let task_raw = args.get("task").and_then(|v| v.as_str()).unwrap_or(&current.task_message);
            // Same invisible-unicode strip as the `add` path — `current.task_message`
            // is already clean (sanitized on insert/prior update), but a freshly
            // provided `task` arg needs the same treatment.
            let task_sanitized = crate::redact::strip_invisible_unicode(task_raw);
            let task = task_sanitized.as_str();
            let enabled = args.get("enabled").and_then(|v| v.as_bool()).unwrap_or(current.enabled);
            let announce_to = args.get("announce_to").cloned().or(current.announce_to);
            // `autonomous_goal`: a provided string sets it; absent keeps the
            // stored value. Resolve it now (reading the column on keep, since
            // ScheduledJob doesn't carry it) so the live reschedule below
            // preserves goal-driven behaviour — mirrors the API update handler.
            // A COALESCE-only UPDATE would leave the rescheduled in-memory job
            // goalless until the next restart, diverging DB from runtime.
            let autonomous_goal: Option<String> = match args.get("autonomous_goal").and_then(|v| v.as_str()) {
                Some(g) => Some(g.to_string()),
                // F060: distinguish a query ERROR from a genuine SQL NULL. The
                // old `.ok().flatten()` collapsed both to None, so a transient
                // DB blip during the keep-read caused the unconditional
                // `SET autonomous_goal = $8` below to bind NULL and permanently
                // erase the job's goal. On error, abort the update instead.
                None => match sqlx::query_scalar::<_, Option<String>>(
                    "SELECT autonomous_goal FROM scheduled_jobs WHERE id = $1",
                )
                .bind(uuid)
                .fetch_one(&cfg.db)
                .await
                {
                    Ok(g) => g,
                    Err(e) => {
                        return format!(
                            "Error reading current job goal (update aborted to avoid erasing it): {e}"
                        );
                    }
                },
            };

            // Validate cron if changed
            if args.get("cron").is_some() {
                let fields: Vec<&str> = cron_expr.split_whitespace().collect();
                if fields.len() != 5 {
                    return "Error: cron expression must have 5 fields (min hour dom mon dow)".to_string();
                }
            }

            match sqlx::query(
                "UPDATE scheduled_jobs SET name = $2, cron_expr = $3, timezone = $4, task_message = $5, \
                 enabled = $6, announce_to = $7, autonomous_goal = $8 WHERE id = $1",
            )
            .bind(uuid)
            .bind(name)
            .bind(cron_expr)
            .bind(timezone)
            .bind(task)
            .bind(enabled)
            .bind(&announce_to)
            .bind(&autonomous_goal)
            .execute(&cfg.db)
            .await
            {
                Ok(_) => {
                    // Reschedule
                    scheduler.remove_dynamic_job(uuid).await.ok();
                    if enabled
                        && let Some(arc) = ctx.state.self_ref.get().and_then(Weak::upgrade)
                            && current.agent_id == cfg.agent.name
                                && let Err(e) = scheduler.add_dynamic_job(
                                    uuid, cron_expr, timezone,
                                    task.to_string(), current.agent_id.clone(),
                                    arc, cfg.db.clone(), announce_to, current.silent,
                                    current.jitter_secs, current.run_once, current.run_at,
                                    current.tool_policy.clone(),
                                    autonomous_goal,
                                ).await {
                                    tracing::error!(job_id = %uuid, error = %e, "failed to reschedule cron job");
                                }
                    format!("Job '{}' updated (id: {}).", name, uuid)
                }
                Err(e) => format!("Error updating job: {}", e),
            }
        }
        "remove" => {
            let job_id = args.get("job_id").and_then(|v| v.as_str()).unwrap_or("");
            if job_id.is_empty() {
                return "Error: 'job_id' is required for remove".to_string();
            }

            let uuid = match Uuid::parse_str(job_id) {
                Ok(u) => u,
                Err(_) => return "Error: invalid job_id format (expected UUID)".to_string(),
            };

            // Remove from scheduler if running
            if let Err(e) = scheduler.remove_dynamic_job(uuid).await {
                tracing::warn!(error = %e, "job not in scheduler (may not have been loaded)");
            }

            // Remove from DB (base can remove any job)
            let delete_result = if cfg.agent.base {
                sqlx::query("DELETE FROM scheduled_jobs WHERE id = $1")
                    .bind(uuid)
                    .execute(&cfg.db)
                    .await
            } else {
                sqlx::query("DELETE FROM scheduled_jobs WHERE id = $1 AND agent_id = $2")
                    .bind(uuid)
                    .bind(&cfg.agent.name)
                    .execute(&cfg.db)
                    .await
            };
            match delete_result {
                Ok(result) => {
                    if result.rows_affected() == 0 {
                        "Error: job not found or belongs to another agent".to_string()
                    } else {
                        format!("Job {} removed successfully", uuid)
                    }
                }
                Err(e) => format!("Error removing job: {}", e),
            }
        }
        "history" | "runs" => {
            let job_id = args.get("job_id").and_then(|v| v.as_str()).unwrap_or("");
            let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(10).min(50);

            if job_id.is_empty() {
                // Show recent runs (base: all agents, regular: own only)
                let rows = if cfg.agent.base {
                    sqlx::query_as::<_, (String, chrono::DateTime<chrono::Utc>, Option<chrono::DateTime<chrono::Utc>>, String, Option<String>, Option<String>)>(
                        "SELECT COALESCE(j.name, 'unknown'), r.started_at, r.finished_at, r.status, r.error, r.response_preview \
                         FROM cron_runs r LEFT JOIN scheduled_jobs j ON r.job_id = j.id \
                         ORDER BY r.started_at DESC LIMIT $1",
                    )
                    .bind(limit)
                    .fetch_all(&cfg.db)
                    .await
                } else {
                    sqlx::query_as::<_, (String, chrono::DateTime<chrono::Utc>, Option<chrono::DateTime<chrono::Utc>>, String, Option<String>, Option<String>)>(
                        "SELECT COALESCE(j.name, 'unknown'), r.started_at, r.finished_at, r.status, r.error, r.response_preview \
                         FROM cron_runs r LEFT JOIN scheduled_jobs j ON r.job_id = j.id \
                         WHERE r.agent_id = $1 ORDER BY r.started_at DESC LIMIT $2",
                    )
                    .bind(&cfg.agent.name)
                    .bind(limit)
                    .fetch_all(&cfg.db)
                    .await
                };

                match rows {
                    Ok(runs) if runs.is_empty() => "No cron runs found.".to_string(),
                    Ok(runs) => {
                        let mut out = format!("Recent cron runs ({}):\n", runs.len());
                        for (name, started, finished, status, error, preview) in &runs {
                            let duration = finished
                                .map(|f| {
                                    let secs = (f - *started).num_seconds();
                                    format!("{}s", secs)
                                })
                                .unwrap_or_else(|| "running".to_string());
                            out.push_str(&format!("- **{}** [{}] {}\n  started: {}, duration: {}\n",
                                name, status,
                                error.as_deref().map(|e| format!("error: {}", e)).unwrap_or_default(),
                                started, duration,
                            ));
                            if let Some(p) = preview {
                                out.push_str(&format!("  preview: {}\n", p));
                            }
                        }
                        out
                    }
                    Err(e) => format!("Error fetching runs: {}", e),
                }
            } else {
                let uuid = match Uuid::parse_str(job_id) {
                    Ok(u) => u,
                    Err(_) => return "Error: invalid job_id format (expected UUID)".to_string(),
                };

                let rows = if cfg.agent.base {
                    sqlx::query_as::<_, (chrono::DateTime<chrono::Utc>, Option<chrono::DateTime<chrono::Utc>>, String, Option<String>, Option<String>)>(
                        "SELECT started_at, finished_at, status, error, response_preview \
                         FROM cron_runs WHERE job_id = $1 ORDER BY started_at DESC LIMIT $2",
                    )
                    .bind(uuid)
                    .bind(limit)
                    .fetch_all(&cfg.db)
                    .await
                } else {
                    sqlx::query_as::<_, (chrono::DateTime<chrono::Utc>, Option<chrono::DateTime<chrono::Utc>>, String, Option<String>, Option<String>)>(
                        "SELECT started_at, finished_at, status, error, response_preview \
                         FROM cron_runs WHERE job_id = $1 AND agent_id = $2 ORDER BY started_at DESC LIMIT $3",
                    )
                    .bind(uuid)
                    .bind(&cfg.agent.name)
                    .bind(limit)
                    .fetch_all(&cfg.db)
                    .await
                };

                match rows {
                    Ok(runs) if runs.is_empty() => format!("No runs found for job {}", uuid),
                    Ok(runs) => {
                        let mut out = format!("Runs for job {} ({}):\n", uuid, runs.len());
                        for (started, finished, status, error, preview) in &runs {
                            let duration = finished
                                .map(|f| {
                                    let secs = (f - *started).num_seconds();
                                    format!("{}s", secs)
                                })
                                .unwrap_or_else(|| "running".to_string());
                            out.push_str(&format!("- [{}] {}\n  started: {}, duration: {}\n",
                                status,
                                error.as_deref().map(|e| format!("error: {}", e)).unwrap_or_default(),
                                started, duration,
                            ));
                            if let Some(p) = preview {
                                out.push_str(&format!("  preview: {}\n", p));
                            }
                        }
                        out
                    }
                    Err(e) => format!("Error fetching runs: {}", e),
                }
            }
        }
        "run" => {
            let task_arg = args.get("task").and_then(|v| v.as_str()).unwrap_or("");
            let job_id = args.get("job_id").and_then(|v| v.as_str()).unwrap_or("");

            // Resolve task text: from job_id (lookup DB) or direct task argument
            let task = if !job_id.is_empty() {
                let uuid = match Uuid::parse_str(job_id) {
                    Ok(u) => u,
                    Err(_) => return "Error: invalid job_id format (expected UUID)".to_string(),
                };
                let row = if cfg.agent.base {
                    sqlx::query_scalar::<_, String>(
                        "SELECT task_message FROM scheduled_jobs WHERE id = $1",
                    )
                    .bind(uuid)
                    .fetch_optional(&cfg.db)
                    .await
                } else {
                    sqlx::query_scalar::<_, String>(
                        "SELECT task_message FROM scheduled_jobs WHERE id = $1 AND agent_id = $2",
                    )
                    .bind(uuid)
                    .bind(&cfg.agent.name)
                    .fetch_optional(&cfg.db)
                    .await
                };
                match row {
                    Ok(Some(t)) => t,
                    Ok(None) => return "Error: job not found or belongs to another agent".to_string(),
                    Err(e) => return format!("Error looking up job: {}", e),
                }
            } else if !task_arg.is_empty() {
                task_arg.to_string()
            } else {
                return "Error: 'task' or 'job_id' is required for run".to_string();
            };

            // Execute the task immediately as a subagent via self_ref
            let engine = match ctx.state.self_ref.get().and_then(Weak::upgrade) {
                Some(arc) => arc,
                None => return "Error: engine reference not available for subagent execution".to_string(),
            };
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
            match tokio::time::timeout(
                std::time::Duration::from_secs(120),
                engine.run_subagent(&task, 5, Some(deadline), None, None, None),
            )
            .await
            {
                Ok(Ok(result)) => result,
                Ok(Err(e)) => format!("Error running task: {}", e),
                Err(_) => "Task timed out (120s limit).".to_string(),
            }
        }
        _ => format!("Error: unknown action '{}'. Use: list, history, add, update, remove, run", action),
    }
}
