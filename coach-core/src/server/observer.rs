use std::sync::Arc;

use crate::coach::{LlmCoach, NameSessionInput, ObserveToolUseInput};
use crate::state::{SessionId, SharedState};
use crate::EventEmitter;

/// Sequential observer consumer for one session. Reads chain from
/// session state before each LLM call, so each observation builds on
/// the previous one. Exits when the sender is dropped (session end or
/// `/clear`).
///
/// On the first call, locks the model from the global config so that
/// later settings changes don't affect this session's chain.
pub(crate) async fn observer_consumer(
    coach: SharedState,
    emitter: Arc<dyn EventEmitter>,
    session_id: SessionId,
    mut rx: tokio::sync::mpsc::Receiver<crate::state::ObserverQueueItem>,
) {
    while let Some(item) = rx.recv().await {
        let (chain, model) = {
            let mut s = coach.write().await;
            let global_model = s.config.model.clone();
            let sess = match s.sessions.get_mut(&session_id) {
                Some(sess) => sess,
                None => continue,
            };
            // Lock model on first coach call for this session.
            if sess.coach.model.is_none() {
                sess.coach.model = Some(global_model);
            }
            (
                sess.coach.memory.chain.clone(),
                sess.coach.model.clone().unwrap(),
            )
        };

        let llm_coach = LlmCoach::with_model(coach.clone(), model.clone());
        let started = std::time::Instant::now();
        match llm_coach
            .observe_tool_use(ObserveToolUseInput {
                priorities: item.priorities,
                chain,
                tool_name: item.tool_name,
                tool_input: item.tool_input,
                tool_output: item.tool_output,
                user_prompt: item.user_prompt,
                session_id: Some(session_id.clone()),
            })
            .await
        {
            Ok(result) => {
                let latency_ms = started.elapsed().as_millis() as u64;
                let (assessment, intervention) = parse_intervention(&result.assessment);
                crate::state::mutate(&coach, &emitter, |s| {
                    if let Some(sess) = s.sessions.get_mut(&session_id) {
                        sess.coach.record_success(latency_ms, result.usage, Some(result.chain), model.clone());
                        sess.coach.memory.last_assessment = Some(assessment.clone());
                        sess.coach.memory.last_system_prompt = Some(result.system_prompt);
                        sess.coach.memory.last_user_message = Some(result.user_message);
                        if let Some(ref msg) = intervention {
                            sess.coach.memory.pending_intervention = Some(msg.clone());
                            sess.coach.telemetry.intervention_count += 1;
                        }
                    }
                    s.sessions
                        .log(&session_id, "Observer", "noted", Some(assessment));
                    if let Some(ref msg) = intervention {
                        s.sessions.log(
                            &session_id,
                            "Observer",
                            "intervention pending",
                            Some(msg.clone()),
                        );
                    }
                })
                .await;
            }
            Err(e) => {
                eprintln!("[coach] observer call failed: {e}");
                crate::state::mutate(&coach, &emitter, |s| {
                    if let Some(sess) = s.sessions.get_mut(&session_id) {
                        sess.coach.record_error(&e);
                    }
                    s.sessions.log(&session_id, "Observer", "error", Some(e));
                })
                .await;
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
    session_id: SessionId,
    input: NameSessionInput,
) {
    // Read the session's locked model (or lock it now).
    let model = {
        let mut s = coach.write().await;
        let global_model = s.config.model.clone();
        if let Some(sess) = s.sessions.get_mut(&session_id) {
            if sess.coach.model.is_none() {
                sess.coach.model = Some(global_model.clone());
            }
            sess.coach.model.clone().unwrap()
        } else {
            global_model
        }
    };

    let llm_coach = LlmCoach::with_model(coach.clone(), model.clone());
    match llm_coach.name_session(input).await {
        Ok(result) => {
            crate::state::mutate(&coach, &emitter, |s| {
                if let Some(sess) = s.sessions.get_mut(&session_id) {
                    // Namer doesn't update the chain — pass 0 latency since
                    // it's a stateless call and latency isn't worth tracking.
                    sess.coach.record_success(0, result.usage, None, model.clone());
                    sess.coach.memory.session_title = Some(result.title.clone());
                }
                s.sessions
                    .log(&session_id, "Namer", "renamed", Some(result.title));
            })
            .await;
        }
        Err(e) => {
            eprintln!("[coach] name_session failed: {e}");
            crate::state::mutate(&coach, &emitter, |s| {
                if let Some(sess) = s.sessions.get_mut(&session_id) {
                    sess.coach.record_error(&e);
                }
                s.sessions.log(&session_id, "Namer", "error", Some(e));
            })
            .await;
        }
    }
}
