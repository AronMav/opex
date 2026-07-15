//! `GET /api/chat/{id}/stream` — unified sync-envelope SSE stream.
//!
//! Every connection — fresh, mid-run, or after the turn already finished —
//! gets the SAME contract (spec §4.2): `sync_begin` → full replay of
//! buffered events (SSE `id`=seq) → `sync_end` → live events (if the run is
//! still in flight) → `finish`/`[DONE]`. There is no more `204 No Content`
//! and `Last-Event-ID` is ignored entirely — replay is always full; the
//! client (T6) rebuilds turn state idempotently from the envelope rather
//! than relying on the server to skip already-seen events.
//!
//! `?agent=<owner>` is REQUIRED (audit 2026-07-04, IDOR): the bearer token
//! is shared across the whole instance, so without an owner check any
//! token-holder could attach to any other agent's live stream by guessing
//! the session UUID and read the in-flight response in real time. Matches
//! the `verify_session_agent` gate already enforced on every session
//! endpoint in `sessions.rs`.

use axum::{
    extract::{Path, Query, State},
    response::{
        IntoResponse,
        sse::{Event, KeepAlive, Sse},
    },
};
use opex_types::sse::{SseEvent, SyncStatus};

use crate::gateway::ApiError;
use crate::gateway::clusters::{ChannelBus, InfraServices};
use crate::gateway::handlers::sessions::verify_session_agent;

const STREAM_HEADER_NAME: &str = "x-vercel-ai-ui-message-stream";

/// Serializes the envelope for the DB-only branch (no in-memory stream):
/// `sync_begin` → optional single `Sync` payload (the finished/interrupted
/// job snapshot from `stream_jobs`) → `sync_end`. Extracted as a pure
/// function so the ordering contract can be unit-tested without a DB or
/// broadcast harness (T4 brief step 2).
fn empty_envelope(status: SyncStatus, sync_payload: Option<SseEvent>) -> Vec<String> {
    let mut out = Vec::with_capacity(3);
    out.push(
        serde_json::to_string(&SseEvent::SyncBegin {
            boundary_message_id: None,
            run_status: status,
            truncated: false,
        })
        .expect("SseEvent::SyncBegin must serialize"),
    );
    if let Some(payload) = sync_payload {
        out.push(serde_json::to_string(&payload).expect("SseEvent::Sync must serialize"));
    }
    out.push(
        serde_json::to_string(&SseEvent::SyncEnd { last_seq: None })
            .expect("SseEvent::SyncEnd must serialize"),
    );
    out
}

pub(crate) async fn api_chat_stream(
    Path(id): Path<String>,
    Query(params): Query<std::collections::HashMap<String, String>>,
    State(bus): State<ChannelBus>,
    State(infra): State<InfraServices>,
) -> impl IntoResponse {
    use async_stream::stream;
    use tokio::sync::broadcast;

    let agent = match params.get("agent").map(String::as_str) {
        Some(a) if !a.is_empty() => a,
        _ => return ApiError::BadRequest("agent parameter required".into()).into_response(),
    };

    let session_uuid = match uuid::Uuid::parse_str(&id) {
        Ok(u) => u,
        Err(_) => return ApiError::BadRequest("invalid session id".into()).into_response(),
    };
    if let Err(resp) = verify_session_agent(&infra.db, session_uuid, agent).await {
        return resp;
    }

    match bus.stream_registry.subscribe(&id).await {
        None => {
            // No in-memory stream — check DB for a recently finished/errored/
            // interrupted job and fold it into the envelope as the single
            // replay event. `session_uuid` was already validated +
            // ownership-checked above.
            let (status, sync_payload) = if let Ok(Some(job)) =
                crate::gateway::stream_jobs::get_active_job(bus.stream_registry.db(), session_uuid)
                    .await
            {
                let status = match job.status.as_str() {
                    "finished" => SyncStatus::Finished,
                    "error" => SyncStatus::Error,
                    "running" => {
                        // Running in DB but not in memory = Core restarted mid-stream.
                        if let Err(e) = crate::gateway::stream_jobs::error_job(
                            bus.stream_registry.db(),
                            job.id,
                            "stream lost: core restarted",
                        )
                        .await
                        {
                            tracing::warn!(error = %e, "failed to mark stream job as error on resume");
                        }
                        SyncStatus::Interrupted
                    }
                    _ => SyncStatus::Error,
                };
                // `StreamJob.tool_calls` is a `serde_json::Value`; coerce to
                // `Vec<Value>` for the typed payload (any non-array shape —
                // null, {}, etc. — falls back to empty Vec).
                let tool_calls: Vec<serde_json::Value> =
                    serde_json::from_value(job.tool_calls.clone()).unwrap_or_default();
                let sync_event = SseEvent::Sync {
                    content: job.aggregated_text.clone(),
                    tool_calls,
                    status,
                    error: job.error_text.clone(),
                };
                (status, Some(sync_event))
            } else {
                (SyncStatus::Finished, None)
            };

            let envelope = empty_envelope(status, sync_payload);
            let sse_stream = async_stream::stream! {
                for e in envelope {
                    yield Ok::<_, std::convert::Infallible>(Event::default().data(e));
                }
                yield Ok(Event::default().data("[DONE]"));
            };
            (
                [(
                    axum::http::header::HeaderName::from_static(STREAM_HEADER_NAME),
                    "v1",
                )],
                Sse::new(sse_stream).keep_alive(KeepAlive::default()),
            )
                .into_response()
        }
        Some(sub) => {
            let events = sub.events;
            let mut broadcast_rx = sub.rx;
            let already_finished = sub.finished;
            let boundary_message_id = sub.boundary_message_id;
            let truncated = sub.truncated;

            // Last seq of the buffered replay — becomes `sync_end.lastSeq`
            // and the initial live-loop cutoff (dedup against events pushed
            // between `subscribe()` and the first `recv()`).
            let mut highest_replayed: Option<u64> = events.last().map(|(seq, _)| *seq);
            let last_seq_for_end = highest_replayed;

            // Known simplification (documented, not a regression): this
            // collapses an in-memory stream that finished via ERROR into
            // `Finished` — the `finished` atomic is set by both
            // `mark_finished` and `mark_error` (stream_registry.rs), so we
            // can't distinguish them here. The actual `error` event is still
            // in the replay buffer and reaches the client regardless; the
            // error/interrupted distinction only survives in the DB branch
            // (`None` above), which reads `stream_jobs.status` directly.
            let run_status = if already_finished {
                SyncStatus::Finished
            } else {
                SyncStatus::Running
            };

            let sync_begin = serde_json::to_string(&SseEvent::SyncBegin {
                boundary_message_id: Some(boundary_message_id),
                run_status,
                truncated,
            })
            .expect("SseEvent::SyncBegin must serialize");
            let sync_end = serde_json::to_string(&SseEvent::SyncEnd {
                last_seq: last_seq_for_end,
            })
            .expect("SseEvent::SyncEnd must serialize");

            let sse_stream = stream! {
                yield Ok::<_, std::convert::Infallible>(Event::default().data(sync_begin));

                // Phase 1: full replay of buffered events with SSE id field
                // for the client to track (Last-Event-ID is otherwise unused
                // now, but the id is still useful for debugging/devtools).
                for (seq, event_json) in events {
                    yield Ok(Event::default().id(seq.to_string()).data(event_json));
                }

                yield Ok(Event::default().data(sync_end));

                if already_finished {
                    yield Ok(Event::default().data("[DONE]"));
                    return;
                }

                // Phase 2: live events via broadcast subscription. Events
                // between subscribe() and here may overlap with the replayed
                // slice — skip everything <= the last replayed seq.
                let cutoff = highest_replayed;
                loop {
                    match broadcast_rx.recv().await {
                        Ok((seq, event_json)) => {
                            if let Some(c) = cutoff
                                && seq <= c {
                                    continue;
                                }
                            let _ = highest_replayed.replace(seq);
                            // F068: parse only the TOP-LEVEL discriminator. A raw
                            // substring scan matched `"type":"error"` nested
                            // (unescaped) inside a tool-input `input` / rich-card
                            // `data` payload, prematurely ending the resumed
                            // stream mid-turn on ordinary tool arguments.
                            let is_terminal = serde_json::from_str::<serde_json::Value>(&event_json)
                                .ok()
                                .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(String::from))
                                .is_some_and(|t| t == "finish" || t == "error");
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
                                "stream lagged"
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
                    axum::http::header::HeaderName::from_static(STREAM_HEADER_NAME),
                    "v1",
                )],
                Sse::new(sse_stream).keep_alive(KeepAlive::default()),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_envelope_orders_begin_payload_end() {
        let ev = empty_envelope(SyncStatus::Finished, None);
        assert!(ev[0].contains("sync_begin") && ev.last().unwrap().contains("sync_end"));
        assert_eq!(ev.len(), 2);
    }

    #[test]
    fn empty_envelope_includes_interrupted_sync() {
        let s = SseEvent::Sync {
            content: "partial".into(),
            tool_calls: vec![],
            status: SyncStatus::Interrupted,
            error: None,
        };
        let ev = empty_envelope(SyncStatus::Interrupted, Some(s));
        assert_eq!(ev.len(), 3);
        assert!(ev[1].contains("partial"));
    }
}
