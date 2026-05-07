//! `GET /api/chat/{id}/stream` — resume an active SSE stream by session id.
//!
//! AI SDK calls this on mount when `resume=true`. Returns 204 if no active
//! stream, or SSE with replay (from `StreamRegistry`'s seq-indexed buffer)
//! followed by live events (broadcast subscription).
//!
//! Honours the `Last-Event-ID` header (standard SSE) and the equivalent
//! `?last_event_id=<seq>` query string for fetch-based clients that can not
//! set custom headers easily — only events with seq > last_event_id are
//! replayed from the buffer, eliminating duplicates after reconnect.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{
        IntoResponse,
        sse::{Event, KeepAlive, Sse},
    },
};
use hydeclaw_types::sse::{SseEvent, SyncStatus};

use crate::gateway::clusters::ChannelBus;

pub(crate) async fn api_chat_resume_stream(
    Path(id): Path<String>,
    Query(params): Query<std::collections::HashMap<String, String>>,
    headers: axum::http::HeaderMap,
    State(bus): State<ChannelBus>,
) -> impl IntoResponse {
    use async_stream::stream;
    use tokio::sync::broadcast;

    let last_event_id: Option<u64> = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .or_else(|| params.get("last_event_id").and_then(|s| s.parse::<u64>().ok()));

    match bus.stream_registry.subscribe(&id).await {
        None => {
            // No in-memory stream — check DB for recently finished/interrupted job
            let session_uuid = uuid::Uuid::parse_str(&id).ok();
            if let Some(sid) = session_uuid
                && let Ok(Some(job)) = crate::gateway::stream_jobs::get_active_job(
                    bus.stream_registry.db(), sid
                ).await {
                    let status = match job.status.as_str() {
                        "finished" => SyncStatus::Finished,
                        "error" => SyncStatus::Error,
                        "running" => {
                            // Running in DB but not in memory = Core restarted mid-stream
                            if let Err(e) = crate::gateway::stream_jobs::error_job(
                                bus.stream_registry.db(), job.id, "stream lost: core restarted"
                            ).await {
                                tracing::warn!(error = %e, "failed to mark stream job as error on resume");
                            }
                            SyncStatus::Interrupted
                        }
                        _ => SyncStatus::Error,
                    };
                    // `StreamJob.tool_calls` is a `serde_json::Value`; coerce
                    // to `Vec<Value>` for the typed payload (any non-array
                    // shape — null, {}, etc. — falls back to empty Vec).
                    let tool_calls: Vec<serde_json::Value> = serde_json::from_value(
                        job.tool_calls.clone()
                    ).unwrap_or_default();
                    let sync_event = SseEvent::Sync {
                        content: job.aggregated_text.clone(),
                        tool_calls,
                        status,
                        error: job.error_text.clone(),
                    };
                    let sync_str = serde_json::to_string(&sync_event)
                        .expect("SseEvent::Sync must serialize");
                    let sse_stream = async_stream::stream! {
                        yield Ok::<_, std::convert::Infallible>(Event::default().data(sync_str));
                        yield Ok(Event::default().data("[DONE]"));
                    };
                    return Sse::new(sse_stream)
                        .keep_alive(KeepAlive::default())
                        .into_response();
                }
            StatusCode::NO_CONTENT.into_response()
        }
        Some((buffered_events, mut broadcast_rx, already_finished)) => {
            // Filter buffer by client's last seen seq before counting replays.
            let filtered: Vec<(u64, String)> = buffered_events
                .into_iter()
                .filter(|(seq, _)| match last_event_id {
                    Some(last) => *seq > last,
                    None => true,
                })
                .collect();
            let _replay_count = filtered.len();
            let mut highest_replayed: Option<u64> = filtered.last().map(|(seq, _)| *seq);

            let sse_stream = stream! {
                // Phase 1: Replay buffered events with SSE id field for the
                // client to track (Last-Event-ID on reconnect).
                for (seq, event_json) in filtered {
                    yield Ok::<_, std::convert::Infallible>(
                        Event::default().id(seq.to_string()).data(event_json)
                    );
                }

                if already_finished {
                    yield Ok(Event::default().data("[DONE]"));
                    return;
                }

                // Phase 2: Live events via broadcast subscription.
                // Events between subscribe() and here may overlap with our
                // filtered slice — skip everything <= the last replayed seq
                // (or last_event_id when nothing was replayed).
                let cutoff = highest_replayed.or(last_event_id);
                loop {
                    match broadcast_rx.recv().await {
                        Ok((seq, event_json)) => {
                            if let Some(c) = cutoff
                                && seq <= c {
                                    continue;
                                }
                            let _ = highest_replayed.replace(seq);
                            let is_terminal =
                                event_json.contains("\"type\":\"finish\"")
                                || event_json.contains("\"type\":\"error\"");
                            yield Ok(Event::default().id(seq.to_string()).data(event_json));
                            if is_terminal {
                                yield Ok(Event::default().data("[DONE]"));
                                break;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(
                                lagged = n,
                                session = %id,
                                "Resume stream lagged"
                            );
                            // With seq-based cutoff this branch needs no
                            // explicit skip — events with seq <= cutoff are
                            // skipped on the next match arm regardless.
                            continue;
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            break;
                        }
                    }
                }
            };

            (
                [(
                    axum::http::header::HeaderName::from_static(
                        "x-vercel-ai-ui-message-stream",
                    ),
                    "v1",
                )],
                Sse::new(sse_stream).keep_alive(KeepAlive::default()),
            )
                .into_response()
        }
    }
}
