use std::sync::Arc;

use crate::coach::{LlmCoach, NameSessionInput, ObserveToolUseInput};
use crate::state::SharedState;
use crate::EventEmitter;

use super::emit_update;

/// Sequential observer consumer for one session. Reads chain from
/// session state before each LLM call, so each observation builds on
/// the previous one. Exits when the sender is dropped (session end or
/// `/clear`).
pub(crate) async fn observer_consumer(
    coach: SharedState,
    emitter: Arc<dyn EventEmitter>,
    pid: u32,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<crate::state::ObserverQueueItem>,
) {
    let llm_coach = LlmCoach::new(coach.clone());
    while let Some(item) = rx.recv().await {
        // Read the current chain — includes all previous observations.
        let chain = {
            let s = coach.read().await;
            s.sessions.get(&pid)
                .map(|sess| sess.coach.memory.chain.clone())
                .unwrap_or_default()
        };

        let started = std::time::Instant::now();
        match llm_coach
            .observe_tool_use(ObserveToolUseInput {
                priorities: item.priorities,
                chain,
                tool_name: item.tool_name,
                tool_input: item.tool_input,
                user_prompt: item.user_prompt,
            })
            .await
        {
            Ok(result) => {
                let latency_ms = started.elapsed().as_millis() as u64;
                let (assessment, intervention) = parse_intervention(&result.assessment);
                let mut s = coach.write().await;
                if let Some(sess) = s.sessions.get_mut(&pid) {
                    sess.coach.record_success(latency_ms, result.usage, Some(result.chain));
                    sess.coach.memory.last_assessment = Some(assessment.clone());
                    sess.coach.memory.last_system_prompt = Some(result.system_prompt);
                    sess.coach.memory.last_user_message = Some(result.user_message);
                    if let Some(ref msg) = intervention {
                        sess.coach.memory.pending_intervention = Some(msg.clone());
                        sess.coach.telemetry.intervention_count += 1;
                    }
                }
                s.log(pid, "Observer", "noted", Some(assessment));
                if let Some(ref msg) = intervention {
                    s.log(pid, "Observer", "intervention pending", Some(msg.clone()));
                }
                emit_update(&*emitter, &s);
            }
            Err(e) => {
                eprintln!("[coach] observer call failed: {e}");
                let mut s = coach.write().await;
                if let Some(sess) = s.sessions.get_mut(&pid) {
                    sess.coach.record_error(&e);
                }
                s.log(pid, "Observer", "error", Some(e));
                emit_update(&*emitter, &s);
            }
        }
    }
}

/// Parse an observer response for the INTERVENE: prefix.
/// Returns (full assessment text, optional intervention message).
pub(crate) fn parse_intervention(text: &str) -> (String, Option<String>) {
    if let Some(rest) = text.strip_prefix("INTERVENE:") {
        (text.to_string(), Some(rest.trim().to_string()))
    } else {
        (text.to_string(), None)
    }
}

/// Periodic session-title generation. Stateless LLM call (fresh chain),
/// fire-and-forget like the observer. On success, writes the cleaned
/// title into coach memory. On failure, records the error so the
/// telemetry panel reflects it — same shape as `observer_consumer`.
pub(crate) async fn run_session_namer(
    coach: SharedState,
    emitter: Arc<dyn EventEmitter>,
    pid: u32,
    input: NameSessionInput,
) {
    let llm_coach = LlmCoach::new(coach.clone());
    match llm_coach.name_session(input).await {
        Ok(result) => {
            let mut s = coach.write().await;
            if let Some(sess) = s.sessions.get_mut(&pid) {
                // Namer doesn't update the chain — pass 0 latency since
                // it's a stateless call and latency isn't worth tracking.
                sess.coach.record_success(0, result.usage, None);
                sess.coach.memory.session_title = Some(result.title.clone());
            }
            s.log(pid, "Namer", "renamed", Some(result.title));
            emit_update(&*emitter, &s);
        }
        Err(e) => {
            eprintln!("[coach] name_session failed: {e}");
            let mut s = coach.write().await;
            if let Some(sess) = s.sessions.get_mut(&pid) {
                sess.coach.record_error(&e);
            }
            s.log(pid, "Namer", "error", Some(e));
            emit_update(&*emitter, &s);
        }
    }
}
