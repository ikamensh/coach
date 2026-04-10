use crate::state::{CoachChain, CoachUsage, SharedState};

pub use crate::llm::{NameSessionInput, StopContext, StopDecision};

/// Domain-level observation request for one completed tool call.
pub struct ObserveToolUseInput {
    pub priorities: Vec<String>,
    pub chain: CoachChain,
    pub tool_name: String,
    pub tool_input: serde_json::Value,
    pub user_prompt: Option<String>,
}

pub struct ObserveToolUseOutput {
    pub assessment: String,
    pub chain: CoachChain,
    pub usage: CoachUsage,
    /// The system prompt sent to the LLM for this observation.
    pub system_prompt: String,
    /// The user message sent to the LLM for this observation.
    pub user_message: String,
}

/// Continue an existing coach conversation when evaluating a Stop hook.
pub struct ChainedStopInput {
    pub priorities: Vec<String>,
    pub chain: CoachChain,
    pub stop_reason: Option<String>,
}

pub struct ChainedStopOutput {
    pub decision: StopDecision,
    pub chain: CoachChain,
    pub usage: CoachUsage,
}

pub struct NameSessionOutput {
    pub title: String,
    pub usage: CoachUsage,
}

/// High-level LLM coach entity used by the rest of the app.
///
/// The low-level provider and prompt plumbing stays in `llm.rs`; callers
/// should depend on this type so the app has one clear coach boundary.
#[derive(Clone)]
pub struct LlmCoach {
    state: SharedState,
}

impl LlmCoach {
    pub fn new(state: SharedState) -> Self {
        Self { state }
    }

    pub async fn observe_tool_use(
        &self,
        input: ObserveToolUseInput,
    ) -> Result<ObserveToolUseOutput, String> {
        let event = crate::llm::build_observer_event(
            &input.tool_name,
            &input.tool_input,
            input.user_prompt.as_deref(),
        )?;
        let system = crate::llm::coach_system_prompt(&input.priorities)?;
        let (assessment, chain, usage) =
            crate::llm::observe_event(&self.state, &input.priorities, &input.chain, &event).await?;
        Ok(ObserveToolUseOutput {
            assessment,
            chain,
            usage,
            system_prompt: system,
            user_message: event,
        })
    }

    pub async fn evaluate_stop(&self, context: StopContext) -> Result<StopDecision, String> {
        crate::llm::evaluate_stop(&self.state, &context).await
    }

    pub async fn evaluate_stop_chained(
        &self,
        input: ChainedStopInput,
    ) -> Result<ChainedStopOutput, String> {
        let (decision, chain, usage) = crate::llm::evaluate_stop_chained(
            &self.state,
            &input.priorities,
            &input.chain,
            input.stop_reason.as_deref(),
        )
        .await?;
        Ok(ChainedStopOutput {
            decision,
            chain,
            usage,
        })
    }

    pub async fn name_session(
        &self,
        input: NameSessionInput,
    ) -> Result<NameSessionOutput, String> {
        let (title, usage) = crate::llm::name_session(&self.state, &input).await?;
        Ok(NameSessionOutput { title, usage })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    fn test_state() -> SharedState {
        Arc::new(RwLock::new(crate::state::test_state()))
    }

    #[tokio::test]
    async fn observe_tool_use_routes_through_the_coach_boundary() {
        let state = test_state();
        state.write().await.mock_session_send = Some(Arc::new(|_system, user| {
            assert!(user.contains("Edit"));
            assert!(user.contains("hello"));
            assert!(user.contains("keep the session list stable"));
            Ok((
                "Focused on a small edit".to_string(),
                crate::state::CoachUsage {
                    input_tokens: 11,
                    output_tokens: 7,
                    cached_input_tokens: 0,
                },
            ))
        }));

        let coach = LlmCoach::new(state);
        let out = coach
            .observe_tool_use(ObserveToolUseInput {
                priorities: vec!["Clean boundaries".into()],
                chain: CoachChain::Empty,
                tool_name: "Edit".into(),
                tool_input: serde_json::json!({ "new_string": "hello" }),
                user_prompt: Some("keep the session list stable".into()),
            })
            .await
            .unwrap();

        assert_eq!(out.assessment, "Focused on a small edit");
        assert_eq!(out.usage.input_tokens, 11);
    }

    #[tokio::test]
    async fn evaluate_stop_routes_through_the_coach_boundary() {
        let state = test_state();
        state.write().await.mock_session_send = Some(Arc::new(|_system, _user| {
            Ok((
                r#"{"allow":false,"message":"Keep going"}"#.to_string(),
                crate::state::CoachUsage::default(),
            ))
        }));

        let coach = LlmCoach::new(state);
        let decision = coach
            .evaluate_stop(StopContext {
                priorities: vec!["Simplicity".into()],
                cwd: Some("/tmp/project".into()),
                tool_counts: HashMap::from([("Read".into(), 2)]),
                stop_count: 1,
                stop_blocked_count: 0,
                stop_reason: Some("end_turn".into()),
            })
            .await
            .unwrap();

        assert!(!decision.allow);
        assert_eq!(decision.message.as_deref(), Some("Keep going"));
    }

    #[tokio::test]
    async fn name_session_routes_through_the_coach_boundary() {
        let state = test_state();
        state.write().await.mock_session_send = Some(Arc::new(|_system, _user| {
            Ok((
                "Title: Extract Coach Interface".to_string(),
                crate::state::CoachUsage::default(),
            ))
        }));

        let coach = LlmCoach::new(state);
        let out = coach
            .name_session(NameSessionInput {
                priorities: vec!["Maintainability".into()],
                cwd: Some("/tmp/project".into()),
                tool_counts: HashMap::from([("Edit".into(), 3)]),
                last_assessment: Some("Splitting the coach boundary.".into()),
            })
            .await
            .unwrap();

        assert_eq!(out.title, "Extract Coach Interface");
    }
}
