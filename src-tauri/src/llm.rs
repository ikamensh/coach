//! Dual-model LLM queries with parallel verification.
//!
//! Public interface:
//!   `query(prompt, state)`   → free-form text response
//!   `extract(prompt, state)` → structured response parsed into T
//!
//! Both run the primary model and (when a second provider has a token)
//! a verifier model in parallel. If both succeed, the response is
//! marked as verified. All provider plumbing is handled by rig-core.

use rig::client::CompletionClient;
use rig::completion::Prompt;
use rig::providers::{anthropic, gemini, openai, openrouter};
use schemars::JsonSchema;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::collections::HashMap;

use crate::settings::ModelConfig;
use crate::state::SharedState;

// ── Response types ──────────────────────────────────────────────────────

pub struct LlmResponse {
    pub text: String,
    pub model: String,
    pub verified: bool,
    pub verifier: Option<String>,
}

pub struct ExtractResponse<T> {
    pub data: T,
    pub model: String,
    pub verified: bool,
    pub verifier: Option<String>,
}

// ── Stop evaluation types ──────────────────────────────────────────────

/// Structured LLM response for stop-hook evaluation.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct StopDecision {
    /// True to let the agent stop, false to block and keep it working.
    pub allow: bool,
    /// Directive for the agent when blocking. Ignored if allow is true.
    pub message: Option<String>,
}

/// Result of a chained call into OpenAI's Responses API: the assistant text
/// plus the new `response_id` to pass as `previous_response_id` next time.
pub struct ChainCall {
    pub text: String,
    pub response_id: String,
}

/// Snapshot of session info passed to the coach LLM when evaluating a stop.
pub struct StopContext {
    pub priorities: Vec<String>,
    pub cwd: Option<String>,
    pub tool_counts: HashMap<String, usize>,
    pub stop_count: usize,
    pub stop_blocked_count: usize,
    pub stop_reason: Option<String>,
}

// ── Provider dispatch ───────────────────────────────────────────────────

fn fmt_err(provider: &str, e: impl std::fmt::Display) -> String {
    format!("{provider}: {e}")
}

async fn chat(provider: &str, model: &str, token: &str, prompt: &str) -> Result<String, String> {
    match provider {
        "google" => {
            let c = gemini::Client::new(token).map_err(|e| fmt_err(provider, e))?;
            let resp: String = c.agent(model).build().prompt(prompt).await.map_err(|e| fmt_err(provider, e))?;
            Ok(resp)
        }
        "anthropic" => {
            let c = anthropic::Client::new(token).map_err(|e| fmt_err(provider, e))?;
            let resp: String = c.agent(model).build().prompt(prompt).await.map_err(|e| fmt_err(provider, e))?;
            Ok(resp)
        }
        "openai" => {
            let c = openai::Client::new(token).map_err(|e| fmt_err(provider, e))?;
            let resp: String = c.agent(model).build().prompt(prompt).await.map_err(|e| fmt_err(provider, e))?;
            Ok(resp)
        }
        "openrouter" => {
            let c = openrouter::Client::new(token).map_err(|e| fmt_err(provider, e))?;
            let resp: String = c.agent(model).build().prompt(prompt).await.map_err(|e| fmt_err(provider, e))?;
            Ok(resp)
        }
        _ => Err(format!("unknown provider: {provider}")),
    }
}

async fn extract_one<T>(
    provider: &str,
    model: &str,
    token: &str,
    prompt: &str,
) -> Result<T, String>
where
    T: DeserializeOwned + Serialize + JsonSchema + Send + Sync + 'static,
{
    match provider {
        "google" => {
            let c = gemini::Client::new(token).map_err(|e| fmt_err(provider, e))?;
            let data: T = c.extractor::<T>(model).build().extract(prompt).await.map_err(|e| fmt_err(provider, e))?;
            Ok(data)
        }
        "anthropic" => {
            let c = anthropic::Client::new(token).map_err(|e| fmt_err(provider, e))?;
            let data: T = c.extractor::<T>(model).build().extract(prompt).await.map_err(|e| fmt_err(provider, e))?;
            Ok(data)
        }
        "openai" => {
            let c = openai::Client::new(token).map_err(|e| fmt_err(provider, e))?;
            let data: T = c.extractor::<T>(model).build().extract(prompt).await.map_err(|e| fmt_err(provider, e))?;
            Ok(data)
        }
        "openrouter" => {
            let c = openrouter::Client::new(token).map_err(|e| fmt_err(provider, e))?;
            let data: T = c.extractor::<T>(model).build().extract(prompt).await.map_err(|e| fmt_err(provider, e))?;
            Ok(data)
        }
        _ => Err(format!("unknown provider: {provider}")),
    }
}

// ── Verifier selection ──────────────────────────────────────────────────

/// Cheapest fast model per provider, tried in order.
const VERIFIER_CANDIDATES: &[(&str, &str)] = &[
    ("google", "gemini-2.0-flash"),
    ("openai", "gpt-4.1-mini"),
    ("anthropic", "claude-haiku-4-5-20251001"),
    ("openrouter", "google/gemini-2.0-flash-exp"),
];

fn pick_verifier(
    primary_provider: &str,
    effective_token: impl Fn(&str) -> Option<String>,
) -> Option<(ModelConfig, String)> {
    VERIFIER_CANDIDATES.iter().find_map(|(provider, model)| {
        if *provider == primary_provider {
            return None;
        }
        effective_token(provider).map(|token| {
            (
                ModelConfig {
                    provider: provider.to_string(),
                    model: model.to_string(),
                },
                token,
            )
        })
    })
}

// ── Snapshot helper ─────────────────────────────────────────────────────

struct QueryConfig {
    primary: ModelConfig,
    primary_token: String,
    verifier: Option<(ModelConfig, String)>,
}

fn model_label(m: &ModelConfig) -> String {
    format!("{}/{}", m.provider, m.model)
}

/// Read model config + tokens from state, then release the lock.
async fn snapshot_config(state: &SharedState) -> Result<QueryConfig, String> {
    let s = state.read().await;
    let primary = s.model.clone();
    let primary_token = s
        .effective_token(&primary.provider)
        .ok_or("No API token for primary model")?
        .to_string();
    let verifier = pick_verifier(&primary.provider, |p| {
        s.effective_token(p).map(String::from)
    });
    Ok(QueryConfig { primary, primary_token, verifier })
}

// ── Public API ──────────────────────────────────────────────────────────

/// Free-form text query with dual-model verification.
pub async fn query(prompt: &str, state: &SharedState) -> Result<LlmResponse, String> {
    let cfg = snapshot_config(state).await?;
    let primary_label = model_label(&cfg.primary);

    match cfg.verifier {
        Some((v_model, v_token)) => {
            let verifier_label = model_label(&v_model);
            let (p, v) = tokio::join!(
                chat(&cfg.primary.provider, &cfg.primary.model, &cfg.primary_token, prompt),
                chat(&v_model.provider, &v_model.model, &v_token, prompt),
            );
            match (p, v) {
                (Ok(text), Ok(_)) => Ok(LlmResponse {
                    text, model: primary_label, verified: true, verifier: Some(verifier_label),
                }),
                (Ok(text), Err(e)) => {
                    eprintln!("verifier {verifier_label} failed: {e}");
                    Ok(LlmResponse { text, model: primary_label, verified: false, verifier: None })
                }
                (Err(_), Ok(text)) => Ok(LlmResponse {
                    text, model: verifier_label.clone(), verified: false, verifier: Some(verifier_label),
                }),
                (Err(e1), Err(e2)) => Err(format!("primary: {e1}; verifier: {e2}")),
            }
        }
        None => {
            let text = chat(&cfg.primary.provider, &cfg.primary.model, &cfg.primary_token, prompt).await?;
            Ok(LlmResponse { text, model: primary_label, verified: false, verifier: None })
        }
    }
}

/// Structured extraction with dual-model verification.
/// Both models must successfully parse to T for `verified = true`.
pub async fn extract<T>(prompt: &str, state: &SharedState) -> Result<ExtractResponse<T>, String>
where
    T: DeserializeOwned + Serialize + JsonSchema + Send + Sync + 'static,
{
    let cfg = snapshot_config(state).await?;
    let primary_label = model_label(&cfg.primary);

    match cfg.verifier {
        Some((v_model, v_token)) => {
            let verifier_label = model_label(&v_model);
            let (p, v) = tokio::join!(
                extract_one::<T>(&cfg.primary.provider, &cfg.primary.model, &cfg.primary_token, prompt),
                extract_one::<T>(&v_model.provider, &v_model.model, &v_token, prompt),
            );
            match (p, v) {
                (Ok(data), Ok(_)) => Ok(ExtractResponse {
                    data, model: primary_label, verified: true, verifier: Some(verifier_label),
                }),
                (Ok(data), Err(e)) => {
                    eprintln!("verifier {verifier_label} extract failed: {e}");
                    Ok(ExtractResponse { data, model: primary_label, verified: false, verifier: None })
                }
                (Err(_), Ok(data)) => Ok(ExtractResponse {
                    data, model: verifier_label.clone(), verified: false, verifier: Some(verifier_label),
                }),
                (Err(e1), Err(e2)) => Err(format!("primary: {e1}; verifier: {e2}")),
            }
        }
        None => {
            let data = extract_one::<T>(
                &cfg.primary.provider, &cfg.primary.model, &cfg.primary_token, prompt,
            ).await?;
            Ok(ExtractResponse { data, model: primary_label, verified: false, verifier: None })
        }
    }
}

// ── Stop evaluation ────────────────────────────────────────────────────

fn build_stop_prompt(ctx: &StopContext) -> String {
    let priorities = if ctx.priorities.is_empty() {
        "none set".to_string()
    } else {
        ctx.priorities
            .iter()
            .enumerate()
            .map(|(i, p)| format!("{}. {}", i + 1, p))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let tools = if ctx.tool_counts.is_empty() {
        "none yet".to_string()
    } else {
        let mut items: Vec<_> = ctx.tool_counts.iter().collect();
        items.sort_by(|a, b| b.1.cmp(a.1));
        items
            .iter()
            .map(|(k, v)| format!("{k}: {v}"))
            .collect::<Vec<_>>()
            .join(", ")
    };

    let reason = ctx.stop_reason.as_deref().unwrap_or("not specified");
    let dir = ctx.cwd.as_deref().unwrap_or("unknown");

    format!(
        "An autonomous coding agent wants to stop. The user is away.\n\
         Directory: {dir}\n\
         Tools used this session: {tools}\n\
         Stop attempts: {stop_count} ({blocked} previously blocked by coach)\n\
         Agent's stop reason: {reason}\n\n\
         User priorities (highest first):\n{priorities}\n\n\
         Allow stopping if the agent completed meaningful work or is stuck with no clear next step.\n\
         Block if it is pausing to ask a question or stopping prematurely — it should proceed autonomously.\n\
         When blocking, write a brief directive (1-2 sentences) about what to focus on next, referencing the priorities.",
        stop_count = ctx.stop_count,
        blocked = ctx.stop_blocked_count,
    )
}

/// Ask the coach LLM whether to allow or block a stop request.
/// The caller is responsible for releasing any state locks before invoking
/// this — the LLM call may take seconds.
pub async fn evaluate_stop(
    state: &SharedState,
    ctx: &StopContext,
) -> Result<StopDecision, String> {
    let prompt = build_stop_prompt(ctx);
    let resp = extract::<StopDecision>(&prompt, state).await?;
    Ok(resp.data)
}

// ── Stateful chain via OpenAI Responses API ────────────────────────────

/// Low-level call into OpenAI's Responses API. Returns assistant text plus
/// the new response_id so the caller can chain the next call.
///
/// rig 0.34's OpenAI client uses the Responses API by default. Server-side
/// state is referenced via `previous_response_id`, passed through
/// `additional_params`. We bypass rig's `extractor` here because the
/// extractor doesn't surface `raw_response` (no way to read the new id).
pub async fn chain_openai(
    state: &SharedState,
    prompt: &str,
    system: Option<&str>,
    previous_response_id: Option<&str>,
    require_json: bool,
    max_output_tokens: Option<u32>,
) -> Result<ChainCall, String> {
    use rig::completion::{AssistantContent, Completion};

    let cfg = snapshot_config(state).await?;
    if cfg.primary.provider != "openai" {
        return Err(format!(
            "stateful coach requires the OpenAI provider; current: {}",
            cfg.primary.provider
        ));
    }

    let client = openai::Client::new(&cfg.primary_token).map_err(|e| fmt_err("openai", e))?;
    let mut builder = client.agent(&cfg.primary.model);
    if let Some(s) = system {
        builder = builder.preamble(s);
    }

    let mut extra = serde_json::Map::new();
    if let Some(prev) = previous_response_id {
        extra.insert(
            "previous_response_id".into(),
            serde_json::Value::String(prev.to_string()),
        );
    }
    if require_json {
        extra.insert(
            "response_format".into(),
            serde_json::json!({"type": "json_object"}),
        );
    }
    if let Some(max) = max_output_tokens {
        extra.insert(
            "max_output_tokens".into(),
            serde_json::Value::Number(max.into()),
        );
    }
    if !extra.is_empty() {
        builder = builder.additional_params(serde_json::Value::Object(extra));
    }

    let agent = builder.build();
    let history: Vec<rig::message::Message> = vec![];
    let resp = agent
        .completion(prompt, history)
        .await
        .map_err(|e| fmt_err("openai", e))?
        .send()
        .await
        .map_err(|e| fmt_err("openai", e))?;

    let text = resp
        .choice
        .iter()
        .filter_map(|c| match c {
            AssistantContent::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    let response_id = resp.raw_response.id.clone();

    Ok(ChainCall { text, response_id })
}

// ── Stateful chain via Anthropic + prompt caching ──────────────────────

/// Low-level call into Anthropic's messages API. Anthropic has no
/// server-side conversation state, so the caller maintains the message
/// history and passes the running list in. The first call writes the
/// cache breakpoint via `with_automatic_caching()`; subsequent calls
/// hit the cache and pay ~10% of full input rate for the prefix.
///
/// Returns the assistant text and a NEW history vec with the user
/// message and the assistant response appended.
pub async fn chain_anthropic(
    state: &SharedState,
    system: Option<&str>,
    history: &[crate::state::CoachMessage],
    new_message: &str,
    max_output_tokens: Option<u32>,
) -> Result<(String, Vec<crate::state::CoachMessage>), String> {
    use rig::agent::AgentBuilder;
    use rig::client::CompletionClient;
    use rig::completion::{AssistantContent, Completion};
    use rig::message::Message as RigMessage;

    let cfg = snapshot_config(state).await?;
    if cfg.primary.provider != "anthropic" {
        return Err(format!(
            "chain_anthropic requires the Anthropic provider; current: {}",
            cfg.primary.provider
        ));
    }

    let client = anthropic::Client::new(&cfg.primary_token).map_err(|e| fmt_err("anthropic", e))?;
    let model = client
        .completion_model(&cfg.primary.model)
        .with_automatic_caching();

    let mut builder = AgentBuilder::new(model);
    if let Some(s) = system {
        builder = builder.preamble(s);
    }

    let agent = builder.build();

    // Convert our role-typed history into rig messages.
    let rig_history: Vec<RigMessage> = history
        .iter()
        .map(|m| match m.role {
            crate::state::CoachRole::User => RigMessage::user(&m.content),
            crate::state::CoachRole::Assistant => RigMessage::assistant(&m.content),
        })
        .collect();

    // Anthropic requires max_tokens at the request level — additional_params
    // is the wrong layer. CompletionRequestBuilder::max_tokens(u64) sets it.
    // rig 0.34's `calculate_max_tokens` only recognizes claude-3-* and
    // claude-sonnet-4-*; newer Haiku names like claude-haiku-4-5-* aren't
    // matched, so we always pass it explicitly.
    let max = max_output_tokens.unwrap_or(200) as u64;
    let resp = agent
        .completion(new_message, rig_history)
        .await
        .map_err(|e| fmt_err("anthropic", e))?
        .max_tokens(max)
        .send()
        .await
        .map_err(|e| fmt_err("anthropic", e))?;

    let text = resp
        .choice
        .iter()
        .filter_map(|c| match c {
            AssistantContent::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");

    // Build the updated history: existing + new user msg + assistant reply.
    let mut new_history = history.to_vec();
    new_history.push(crate::state::CoachMessage {
        role: crate::state::CoachRole::User,
        content: new_message.to_string(),
    });
    new_history.push(crate::state::CoachMessage {
        role: crate::state::CoachRole::Assistant,
        content: text.clone(),
    });

    Ok((text, new_history))
}

// ── Observer + chained stop ────────────────────────────────────────────

/// System message established on the first call in a coach session.
/// On subsequent calls (with `previous_response_id`), the model already
/// remembers it, but resending is harmless.
pub fn coach_system_prompt(priorities: &[String]) -> String {
    let ptext = if priorities.is_empty() {
        "(none set)".to_string()
    } else {
        priorities
            .iter()
            .enumerate()
            .map(|(i, p)| format!("{}. {}", i + 1, p))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "You are Coach, an observer watching an autonomous coding agent work for an away user.\n\
         After each tool the agent uses, I will send you a brief description of what just happened. \
         Your job is to maintain context. A one-line acknowledgment is enough — don't write essays.\n\n\
         When I later ask you to evaluate a stop request, use everything you've observed to decide:\n\
         • Allow the stop if the agent has completed meaningful work or is genuinely stuck.\n\
         • Block it if the agent is pausing for confirmation or stopping prematurely. \
         When blocking, write a brief directive (1-2 sentences) about what to focus on next, anchored to the priorities.\n\n\
         User priorities (highest first):\n{ptext}"
    )
}

/// Build the per-event message we send to the observer.
/// Tool input is included verbatim so the observer "sees what Claude saw."
pub fn build_observer_event(tool_name: &str, tool_input: &serde_json::Value) -> String {
    let input_pretty = serde_json::to_string(tool_input).unwrap_or_else(|_| "{}".into());
    format!("Tool: {tool_name}\nInput: {input_pretty}")
}

/// Read the active provider from state, releasing the lock immediately.
async fn read_provider(state: &SharedState) -> String {
    state.read().await.model.provider.clone()
}

/// Fire one observer call. Dispatches on the active provider:
///   • OpenAI Responses API: chains via `previous_response_id`. System
///     prompt is sent only on the first call.
///   • Anthropic messages API: maintains client-side history. System
///     prompt is sent every call (free with prompt caching).
///
/// Returns the assistant text and the updated chain handle. If the
/// active provider doesn't support stateful coach sessions, returns Err.
pub async fn observe_event(
    state: &SharedState,
    priorities: &[String],
    chain: &crate::state::CoachChain,
    event: &str,
) -> Result<(String, crate::state::CoachChain), String> {
    use crate::state::CoachChain;

    match read_provider(state).await.as_str() {
        "openai" => {
            let prev_id = match chain {
                CoachChain::OpenAi { response_id } => Some(response_id.as_str()),
                _ => None,
            };
            let system = if prev_id.is_none() {
                Some(coach_system_prompt(priorities))
            } else {
                None
            };
            let call =
                chain_openai(state, event, system.as_deref(), prev_id, false, Some(80)).await?;
            Ok((
                call.text,
                CoachChain::OpenAi {
                    response_id: call.response_id,
                },
            ))
        }
        "anthropic" => {
            let history = match chain {
                CoachChain::Anthropic { history } => history.clone(),
                _ => Vec::new(),
            };
            // Anthropic resends system every call — caching makes it cheap.
            let system = coach_system_prompt(priorities);
            let (text, new_history) =
                chain_anthropic(state, Some(&system), &history, event, Some(80)).await?;
            Ok((text, CoachChain::Anthropic { history: new_history }))
        }
        other => Err(format!(
            "stateful coach not supported for provider: {other}"
        )),
    }
}

/// Synchronous stop evaluation that continues the observer's chain.
/// Returns the parsed decision and the new chain handle (the caller may
/// keep it for UI/debugging even though the chain ends here).
pub async fn evaluate_stop_chained(
    state: &SharedState,
    priorities: &[String],
    chain: &crate::state::CoachChain,
    stop_reason: Option<&str>,
) -> Result<(StopDecision, crate::state::CoachChain), String> {
    use crate::state::CoachChain;

    let reason = stop_reason.unwrap_or("not specified");
    let prompt = format!(
        "The agent is requesting to stop. Stop reason from Claude: {reason}.\n\n\
         Decide whether to allow or block. Respond with ONLY a JSON object:\n\
         {{\"allow\": true|false, \"message\": \"directive if blocking, null if allowing\"}}"
    );

    match read_provider(state).await.as_str() {
        "openai" => {
            let prev_id = match chain {
                CoachChain::OpenAi { response_id } => Some(response_id.as_str()),
                _ => None,
            };
            let system = if prev_id.is_none() {
                Some(coach_system_prompt(priorities))
            } else {
                None
            };
            let call =
                chain_openai(state, &prompt, system.as_deref(), prev_id, true, Some(200)).await?;
            let decision = parse_stop_decision(&call.text)?;
            Ok((
                decision,
                CoachChain::OpenAi {
                    response_id: call.response_id,
                },
            ))
        }
        "anthropic" => {
            let history = match chain {
                CoachChain::Anthropic { history } => history.clone(),
                _ => Vec::new(),
            };
            let system = coach_system_prompt(priorities);
            let (text, new_history) =
                chain_anthropic(state, Some(&system), &history, &prompt, Some(200)).await?;
            let decision = parse_stop_decision(&text)?;
            Ok((decision, CoachChain::Anthropic { history: new_history }))
        }
        other => Err(format!(
            "stateful coach not supported for provider: {other}"
        )),
    }
}

/// Parse a StopDecision from a model response, tolerating models that
/// wrap JSON in ```json … ``` fences (Anthropic does this sometimes).
fn parse_stop_decision(text: &str) -> Result<StopDecision, String> {
    let trimmed = text.trim();
    let json_str = strip_code_fence(trimmed).unwrap_or(trimmed);
    serde_json::from_str(json_str)
        .map_err(|e| format!("stop decision JSON parse failed ({e}): {text}"))
}

/// If the text is wrapped in a triple-backtick code fence (with or
/// without a language tag), return just the inner content. Otherwise None.
fn strip_code_fence(text: &str) -> Option<&str> {
    let s = text.strip_prefix("```")?;
    let after_lang = match s.find('\n') {
        Some(i) => &s[i + 1..],
        None => s,
    };
    after_lang.strip_suffix("```").map(str::trim)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_verifier_skips_primary_provider() {
        let v = pick_verifier("google", |p| match p {
            "openai" => Some("sk".into()),
            _ => None,
        });
        assert_eq!(v.unwrap().0.provider, "openai");
    }

    #[test]
    fn pick_verifier_returns_none_when_only_primary_has_token() {
        let v = pick_verifier("google", |_| None);
        assert!(v.is_none());
    }

    #[test]
    fn pick_verifier_prefers_cheapest_candidate() {
        let v = pick_verifier("anthropic", |p| match p {
            "google" => Some("gk".into()),
            "openai" => Some("sk".into()),
            _ => None,
        });
        assert_eq!(v.unwrap().0.provider, "google");
    }

    // ── Stop prompt + decision ──────────────────────────────────────────

    fn ctx_with(priorities: Vec<&str>) -> StopContext {
        StopContext {
            priorities: priorities.into_iter().map(String::from).collect(),
            cwd: Some("/projects/foo".into()),
            tool_counts: HashMap::from([("Read".into(), 5), ("Edit".into(), 2)]),
            stop_count: 2,
            stop_blocked_count: 1,
            stop_reason: Some("end_turn".into()),
        }
    }

    /// Prompt should mention every concrete piece of context the LLM needs.
    /// This is the only invariant — wording is free to evolve.
    #[test]
    fn build_stop_prompt_includes_context() {
        let ctx = ctx_with(vec!["Speed", "Quality"]);
        let p = build_stop_prompt(&ctx);
        for needle in [
            "Speed", "Quality", "/projects/foo", "Read: 5", "Edit: 2",
            "2", "1", "end_turn",
        ] {
            assert!(p.contains(needle), "prompt missing {needle:?}: {p}");
        }
    }

    /// Empty fields should produce sensible placeholders, not empty strings
    /// or panics. The LLM should never see "Tools used: \n".
    #[test]
    fn build_stop_prompt_handles_empty_context() {
        let ctx = StopContext {
            priorities: vec![],
            cwd: None,
            tool_counts: HashMap::new(),
            stop_count: 1,
            stop_blocked_count: 0,
            stop_reason: None,
        };
        let p = build_stop_prompt(&ctx);
        assert!(p.contains("none set"));
        assert!(p.contains("none yet"));
        assert!(p.contains("unknown"));
        assert!(p.contains("not specified"));
    }

    /// StopDecision must roundtrip through JSON — both rig's extractor
    /// and our fallback paths depend on this.
    #[test]
    fn stop_decision_serde_roundtrip() {
        let cases = [
            StopDecision { allow: true, message: None },
            StopDecision { allow: false, message: Some("Continue with tests".into()) },
        ];
        for original in cases {
            let json = serde_json::to_string(&original).unwrap();
            let restored: StopDecision = serde_json::from_str(&json).unwrap();
            assert_eq!(restored.allow, original.allow);
            assert_eq!(restored.message, original.message);
        }
    }

    // ── Observer prompts ────────────────────────────────────────────────

    /// System prompt must include the user's priorities so the LLM has
    /// the right value frame from the very first call in the chain.
    #[test]
    fn coach_system_prompt_includes_priorities() {
        let p = coach_system_prompt(&["Speed".into(), "Quality".into()]);
        assert!(p.contains("Speed"));
        assert!(p.contains("Quality"));
        assert!(p.contains("1. Speed"));
        assert!(p.contains("2. Quality"));
    }

    /// Empty priorities should produce a sensible placeholder, not an
    /// awkward "highest first:\n\n" with nothing under it.
    #[test]
    fn coach_system_prompt_handles_no_priorities() {
        let p = coach_system_prompt(&[]);
        assert!(p.contains("none set"));
    }

    /// Observer event must contain both the tool name and the input,
    /// so the LLM truly "sees what Claude saw."
    #[test]
    fn build_observer_event_includes_tool_and_input() {
        let input = serde_json::json!({"file_path": "/a.py", "content": "print(1)"});
        let event = build_observer_event("Write", &input);
        assert!(event.contains("Write"));
        assert!(event.contains("/a.py"));
        assert!(event.contains("print(1)"));
    }

    /// Observer event must serialize Null inputs without panicking
    /// (some tools have no input or send a literal null).
    #[test]
    fn build_observer_event_handles_null_input() {
        let event = build_observer_event("NoInput", &serde_json::Value::Null);
        assert!(event.contains("NoInput"));
        assert!(event.contains("null"));
    }
}

// ── Live tests ─────────────────────────────────────────────────────────
//
// Real OpenAI API calls. Marked `#[ignore]` so a normal `cargo test` skips
// them; run with `cargo test --lib live_ -- --ignored --nocapture`. Each
// test also no-ops gracefully if `OPENAI_API_KEY` is unset, so they can
// stay enabled in CI without leaking 404s.
#[cfg(test)]
mod live_tests {
    use super::*;
    use crate::settings::{EngineMode, ModelConfig};
    use crate::state::{CoachMode, CoachState, SharedState, Theme};
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    /// Build a CoachState that uses the live `OPENAI_API_KEY`.
    /// Returns None when the key is missing so tests no-op cleanly.
    fn live_state() -> Option<SharedState> {
        let token = std::env::var("OPENAI_API_KEY").ok().filter(|v| !v.is_empty())?;
        let state = CoachState {
            sessions: HashMap::new(),
            session_id_to_pid: HashMap::new(),
            priorities: vec!["Test priority".into()],
            port: 7700,
            theme: Theme::System,
            default_mode: CoachMode::Present,
            model: ModelConfig {
                provider: "openai".into(),
                model: "gpt-5.4-mini".into(),
            },
            api_tokens: HashMap::from([("openai".into(), token)]),
            env_tokens: HashMap::new(),
            http_client: reqwest::Client::new(),
            coach_mode: EngineMode::Llm,
            rules: vec![],
        };
        Some(Arc::new(RwLock::new(state)))
    }

    /// Smallest possible round-trip. Verifies the agent + Responses API
    /// path returns text and a `resp_…` id we can chain with.
    #[tokio::test]
    #[ignore]
    async fn live_chain_openai_basic() {
        let Some(state) = live_state() else { return };
        let call = chain_openai(
            &state,
            "Reply with the single word: hello",
            None,
            None,
            false,
            Some(20),
        )
        .await
        .expect("chain_openai failed");
        assert!(!call.text.is_empty(), "expected non-empty text");
        assert!(
            call.response_id.starts_with("resp_"),
            "expected resp_ prefix, got: {}",
            call.response_id
        );
    }

    /// The whole reason we're using Responses API: server-side memory.
    /// Tell the model a fact, then ask about it on the next turn using
    /// only `previous_response_id` — if context is preserved, it knows.
    #[tokio::test]
    #[ignore]
    async fn live_chain_continues_context_via_response_id() {
        let Some(state) = live_state() else { return };
        let r1 = chain_openai(
            &state,
            "Remember this token: PURPLE-OWL-42. Reply 'noted'.",
            None,
            None,
            false,
            Some(20),
        )
        .await
        .expect("first call failed");

        let r2 = chain_openai(
            &state,
            "What was the token I told you to remember? Reply with just the token.",
            None,
            Some(&r1.response_id),
            false,
            Some(30),
        )
        .await
        .expect("second call failed");

        assert!(
            r2.text.contains("PURPLE-OWL-42"),
            "model didn't remember across turns; got: {}",
            r2.text
        );
        assert_ne!(r1.response_id, r2.response_id, "response_ids should differ");
    }

    /// json_object response format must produce parseable JSON.
    /// This is the path evaluate_stop_chained relies on.
    #[tokio::test]
    #[ignore]
    async fn live_chain_json_mode_returns_parseable_json() {
        let Some(state) = live_state() else { return };
        let call = chain_openai(
            &state,
            "Return JSON of the form {\"answer\": <number>}. The number is 7.",
            None,
            None,
            true,
            Some(60),
        )
        .await
        .expect("chain_openai (json) failed");

        let parsed: serde_json::Value = serde_json::from_str(&call.text)
            .unwrap_or_else(|e| panic!("json parse failed ({e}) on: {}", call.text));
        assert!(parsed.get("answer").is_some(), "missing 'answer' field: {}", call.text);
    }

    /// observe_event chain: two events, each producing a new response_id,
    /// each acknowledging the system preamble (set on first call only).
    #[tokio::test]
    #[ignore]
    async fn live_observe_event_chain() {
        let Some(state) = live_state() else { return };
        let priorities = vec!["Code quality".into()];

        let event1 = build_observer_event(
            "Edit",
            &serde_json::json!({
                "file_path": "/tmp/x.py",
                "old_string": "",
                "new_string": "def add(a, b):\n    return a + b\n"
            }),
        );
        let (_text1, chain1) =
            observe_event(&state, &priorities, &crate::state::CoachChain::Empty, &event1)
                .await
                .expect("first observe_event failed");
        let id1 = match &chain1 {
            crate::state::CoachChain::OpenAi { response_id } => response_id.clone(),
            other => panic!("expected OpenAi chain, got {other:?}"),
        };
        assert!(id1.starts_with("resp_"));

        let event2 = build_observer_event(
            "Bash",
            &serde_json::json!({"command": "python -c 'from x import add; print(add(2,3))'"}),
        );
        let (_text2, chain2) = observe_event(&state, &priorities, &chain1, &event2)
            .await
            .expect("second observe_event failed");
        let id2 = match &chain2 {
            crate::state::CoachChain::OpenAi { response_id } => response_id.clone(),
            other => panic!("expected OpenAi chain, got {other:?}"),
        };
        assert!(id2.starts_with("resp_"));
        assert_ne!(id1, id2);
    }

    /// evaluate_stop_chained must always return a parseable StopDecision.
    /// We don't care which way the model decides — we care that the JSON
    /// path is mechanically reliable.
    #[tokio::test]
    #[ignore]
    async fn live_evaluate_stop_chained_returns_parseable_decision() {
        let Some(state) = live_state() else { return };
        let priorities = vec!["Finish the task".into()];

        // Plant some context first.
        let event = build_observer_event(
            "Edit",
            &serde_json::json!({"file_path": "/tmp/done.py", "new_string": "print('done')"}),
        );
        let (_text, observed_chain) =
            observe_event(&state, &priorities, &crate::state::CoachChain::Empty, &event)
                .await
                .expect("observe failed");

        let (decision, new_chain) = evaluate_stop_chained(
            &state,
            &priorities,
            &observed_chain,
            Some("end_turn"),
        )
        .await
        .expect("evaluate_stop_chained failed");

        // Mechanical assertions only — value is up to the model.
        let _ = decision.allow;
        if !decision.allow {
            assert!(
                decision.message.is_some(),
                "blocking decision should carry a message"
            );
        }
        assert!(matches!(new_chain, crate::state::CoachChain::OpenAi { .. }));
    }

    /// First-call evaluate_stop_chained (no prior chain) should also work,
    /// since the system preamble is set when chain is Empty.
    #[tokio::test]
    #[ignore]
    async fn live_evaluate_stop_chained_no_prior_context() {
        let Some(state) = live_state() else { return };
        let priorities = vec!["Do good work".into()];
        let (_decision, new_chain) = evaluate_stop_chained(
            &state,
            &priorities,
            &crate::state::CoachChain::Empty,
            Some("end_turn"),
        )
        .await
        .expect("first-turn evaluate_stop_chained failed");
        assert!(matches!(new_chain, crate::state::CoachChain::OpenAi { .. }));
    }

    // ── Anthropic live tests ────────────────────────────────────────────
    //
    // Same shape as the OpenAI tests but the chain is client-side history
    // instead of a server-side response_id. Gated on ANTHROPIC_API_KEY +
    // #[ignore], no-ops cleanly when the key is missing.

    fn live_state_anthropic() -> Option<SharedState> {
        let token = std::env::var("ANTHROPIC_API_KEY")
            .ok()
            .filter(|v| !v.is_empty())?;
        let state = CoachState {
            sessions: HashMap::new(),
            session_id_to_pid: HashMap::new(),
            priorities: vec!["Test priority".into()],
            port: 7700,
            theme: Theme::System,
            default_mode: CoachMode::Present,
            model: ModelConfig {
                provider: "anthropic".into(),
                model: "claude-haiku-4-5-20251001".into(),
            },
            api_tokens: HashMap::from([("anthropic".into(), token)]),
            env_tokens: HashMap::new(),
            http_client: reqwest::Client::new(),
            coach_mode: EngineMode::Llm,
            rules: vec![],
        };
        Some(Arc::new(RwLock::new(state)))
    }

    /// Smallest possible Anthropic round-trip — verifies the agent +
    /// caching builder + extraction path works end to end.
    #[tokio::test]
    #[ignore]
    async fn live_chain_anthropic_basic() {
        let Some(state) = live_state_anthropic() else { return };
        let (text, new_history) = chain_anthropic(
            &state,
            Some("You are a test bot. Respond with one word."),
            &[],
            "Reply with the single word: hello",
            Some(20),
        )
        .await
        .expect("chain_anthropic failed");
        assert!(!text.is_empty(), "expected non-empty text");
        assert_eq!(new_history.len(), 2, "history should grow by user+assistant");
        assert_eq!(new_history[0].role, crate::state::CoachRole::User);
        assert_eq!(new_history[1].role, crate::state::CoachRole::Assistant);
    }

    /// Anthropic preserves context across turns when client passes the
    /// growing history back. The "test" of statefulness is whether the
    /// model can recall a token from an earlier turn.
    #[tokio::test]
    #[ignore]
    async fn live_chain_anthropic_continues_context_via_history() {
        let Some(state) = live_state_anthropic() else { return };
        let system = "You are a test bot. Reply tersely.";

        let (_t1, h1) = chain_anthropic(
            &state,
            Some(system),
            &[],
            "Remember this token: PURPLE-OWL-42. Reply 'noted'.",
            Some(20),
        )
        .await
        .expect("first call failed");

        let (text, _h2) = chain_anthropic(
            &state,
            Some(system),
            &h1,
            "What was the token I told you to remember? Reply with just the token.",
            Some(30),
        )
        .await
        .expect("second call failed");

        assert!(
            text.contains("PURPLE-OWL-42"),
            "model didn't remember across turns; got: {text}"
        );
    }

    /// observe_event chain accumulation for Anthropic. First call seeds
    /// the system prompt and history with one turn; second call extends
    /// the history.
    #[tokio::test]
    #[ignore]
    async fn live_observe_event_chain_anthropic() {
        let Some(state) = live_state_anthropic() else { return };
        let priorities = vec!["Code quality".into()];

        let event1 = build_observer_event(
            "Edit",
            &serde_json::json!({
                "file_path": "/tmp/x.py",
                "new_string": "def add(a, b):\n    return a + b\n"
            }),
        );
        let (_t1, chain1) =
            observe_event(&state, &priorities, &crate::state::CoachChain::Empty, &event1)
                .await
                .expect("first observe_event failed");
        let h1 = match &chain1 {
            crate::state::CoachChain::Anthropic { history } => history.clone(),
            other => panic!("expected Anthropic chain, got {other:?}"),
        };
        assert_eq!(h1.len(), 2, "first call should produce user+assistant pair");

        let event2 = build_observer_event(
            "Bash",
            &serde_json::json!({"command": "python -c 'from x import add; print(add(2,3))'"}),
        );
        let (_t2, chain2) = observe_event(&state, &priorities, &chain1, &event2)
            .await
            .expect("second observe_event failed");
        let h2 = match &chain2 {
            crate::state::CoachChain::Anthropic { history } => history.clone(),
            other => panic!("expected Anthropic chain, got {other:?}"),
        };
        assert_eq!(h2.len(), 4, "history should grow to 4 messages");
    }

    /// evaluate_stop_chained over Anthropic must produce a parseable
    /// StopDecision. Tolerant of code-fenced JSON output via
    /// strip_code_fence in parse_stop_decision.
    #[tokio::test]
    #[ignore]
    async fn live_evaluate_stop_chained_anthropic() {
        let Some(state) = live_state_anthropic() else { return };
        let priorities = vec!["Finish the task".into()];

        let event = build_observer_event(
            "Edit",
            &serde_json::json!({"file_path": "/tmp/done.py", "new_string": "print('done')"}),
        );
        let (_t, observed_chain) =
            observe_event(&state, &priorities, &crate::state::CoachChain::Empty, &event)
                .await
                .expect("observe failed");

        let (decision, new_chain) = evaluate_stop_chained(
            &state,
            &priorities,
            &observed_chain,
            Some("end_turn"),
        )
        .await
        .expect("evaluate_stop_chained failed");

        let _ = decision.allow;
        if !decision.allow {
            assert!(decision.message.is_some(), "block decision should carry a message");
        }
        assert!(matches!(new_chain, crate::state::CoachChain::Anthropic { .. }));
    }
}
