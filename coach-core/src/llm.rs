//! Coach LLM integration. All provider calls go through `session_send`.

use rig::client::CompletionClient;
use rig::providers::{anthropic, gemini, openai};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::llm_log::{LlmCallRecord, LogContext};
use crate::settings::ModelConfig;
use crate::state::SharedState;

// ── Types ──────────────────────────────────────────────────────────────

/// Structured LLM response for stop-hook evaluation.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct StopDecision {
    /// True to let the agent stop, false to block and keep it working.
    pub allow: bool,
    /// Directive for the agent when blocking. Ignored if allow is true.
    pub message: Option<String>,
}

/// Convert rig's per-call usage record to our minimal three-field shape.
fn to_coach_usage(u: rig::completion::Usage) -> crate::state::CoachUsage {
    crate::state::CoachUsage {
        input_tokens: u.input_tokens,
        output_tokens: u.output_tokens,
        cached_input_tokens: u.cached_input_tokens,
    }
}

/// Snapshot of session info passed to the coach LLM when evaluating a stop.
pub struct StopContext {
    pub priorities: Vec<String>,
    pub cwd: Option<String>,
    pub tool_counts: HashMap<String, usize>,
    pub stop_count: usize,
    pub stop_blocked_count: usize,
    pub stop_reason: Option<String>,
    /// Coding-session id (e.g. Claude Code session UUID). Used as a
    /// routing key when JSONL logging is enabled.
    pub session_id: Option<String>,
}

/// Per-call constraints for `session_send`. Everything is optional /
/// defaulted so callers who don't care can pass `CallConstraints::default()`.
#[derive(Debug, Clone, Default)]
pub struct CallConstraints {
    /// Hard cap on the model's output length. OpenAI maps this to
    /// `max_output_tokens`; Anthropic maps it to `max_tokens`.
    pub max_output_tokens: Option<u32>,
    /// Force JSON output where the provider supports it (OpenAI's
    /// `response_format: json_object`). On providers without an
    /// equivalent flag, the caller is expected to prompt for JSON in
    /// the message itself.
    pub require_json: bool,
    /// When set, use this model instead of the global `config.model`.
    /// Used to lock a session to the model that was active when the
    /// session's coach first started observing.
    pub model: Option<crate::settings::ModelConfig>,
}

// ── Provider helpers ───────────────────────────────────────────────────

fn fmt_err(provider: &str, e: impl std::fmt::Display) -> String {
    format!("{provider}: {e}")
}

/// Read model config + token from state, then release the lock.
struct ProviderConfig {
    primary: ModelConfig,
    primary_token: String,
}

async fn snapshot_config(
    state: &SharedState,
    model_override: Option<&ModelConfig>,
) -> Result<ProviderConfig, String> {
    let s = state.read().await;
    let primary = model_override.cloned().unwrap_or_else(|| s.config.model.clone());
    let primary_token = s
        .effective_token(&primary.provider)
        .ok_or("No API token for primary model")?
        .to_string();
    Ok(ProviderConfig { primary, primary_token })
}

// ── Stop evaluation ────────────────────────────────────────────────────

fn build_stop_prompt(ctx: &StopContext) -> Result<String, String> {
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
    let stop_count = ctx.stop_count.to_string();
    let stop_blocked_count = ctx.stop_blocked_count.to_string();

    let template = crate::prompts::load("stop_oneshot")?;
    Ok(crate::prompts::render(
        &template,
        &[
            ("dir", dir),
            ("tools", &tools),
            ("stop_count", &stop_count),
            ("stop_blocked_count", &stop_blocked_count),
            ("stop_reason", reason),
            ("priorities", &priorities),
        ],
    ))
}

/// One-shot stop evaluation via session_send (no prior chain context).
/// Used as a fallback when the provider doesn't support stateful chains.
pub async fn evaluate_stop(
    state: &SharedState,
    ctx: &StopContext,
    model: Option<ModelConfig>,
) -> Result<StopDecision, String> {
    let prompt = build_stop_prompt(ctx)?;
    let (text, _chain, _usage) = session_send(
        state,
        &crate::state::CoachChain::Empty,
        "You evaluate whether an AI agent should stop. Respond with JSON only.",
        &prompt,
        CallConstraints { max_output_tokens: Some(200), require_json: true, model },
        LogContext::new("stop_oneshot", ctx.session_id.as_deref()),
    )
    .await?;
    parse_stop_decision(&text)
}

// ── Observer + chained stop ────────────────────────────────────────────

/// System message established on the first call in a coach session.
/// On subsequent calls (with `previous_response_id`), the model already
/// remembers it, but resending is harmless.
pub fn coach_system_prompt(priorities: &[String]) -> Result<String, String> {
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
    let template = crate::prompts::load("coach_system")?;
    Ok(crate::prompts::render(&template, &[("priorities", &ptext)]))
}

/// Build the per-event message we send to the observer.
/// Tool input is included verbatim so the observer "sees what Claude saw."
/// When available, the user's last prompt is prepended so the observer
/// can compare user intent against agent behavior.
pub fn build_observer_event(
    tool_name: &str,
    tool_input: &serde_json::Value,
    user_prompt: Option<&str>,
) -> Result<String, String> {
    let input_pretty = serde_json::to_string(tool_input).unwrap_or_else(|_| "{}".into());
    let user_prompt = user_prompt.unwrap_or("(not available)");
    let template = crate::prompts::load("observer_event")?;
    Ok(crate::prompts::render(
        &template,
        &[
            ("tool_name", tool_name),
            ("tool_input", &input_pretty),
            ("user_prompt", user_prompt),
        ],
    ))
}

/// Build a History chain from a mock response, so tests see
/// conversation growth in the session state.
fn grow_mock_chain(
    chain: &crate::state::CoachChain,
    user_msg: &str,
    asst_msg: &str,
) -> crate::state::CoachChain {
    use crate::state::{CoachChain, CoachMessage, CoachRole};
    let mut messages = match chain {
        CoachChain::History { messages } => messages.clone(),
        _ => Vec::new(),
    };
    messages.push(CoachMessage { role: CoachRole::User, content: user_msg.to_string() });
    messages.push(CoachMessage { role: CoachRole::Assistant, content: asst_msg.to_string() });
    CoachChain::History { messages }
}

/// Convert a `Vec<CoachMessage>` into rig's message type.
fn to_rig_history(history: &[crate::state::CoachMessage]) -> Vec<rig::message::Message> {
    history
        .iter()
        .map(|m| match m.role {
            crate::state::CoachRole::User => rig::message::Message::user(&m.content),
            crate::state::CoachRole::Assistant => rig::message::Message::assistant(&m.content),
        })
        .collect()
}

/// Append user + assistant messages to a history vec, returning the
/// extended history and the assistant text.
fn extend_history(
    history: &[crate::state::CoachMessage],
    user_msg: &str,
    asst_msg: &str,
) -> Vec<crate::state::CoachMessage> {
    let mut new = history.to_vec();
    new.push(crate::state::CoachMessage {
        role: crate::state::CoachRole::User,
        content: user_msg.to_string(),
    });
    new.push(crate::state::CoachMessage {
        role: crate::state::CoachRole::Assistant,
        content: asst_msg.to_string(),
    });
    new
}

/// Extract text from a rig completion response.
fn extract_text(choice: &rig::one_or_many::OneOrMany<rig::completion::AssistantContent>) -> String {
    choice
        .iter()
        .filter_map(|c| match c {
            rig::completion::AssistantContent::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Append a single message to a coach session and get back the model's
/// reply. This is the one primitive every caller should use -- specific
/// use cases (observer, stop evaluation, future features) are thin
/// wrappers that just build the right message and constraints.
///
/// The session is represented by the opaque `CoachChain` handle that
/// callers thread through: `Empty` on the first call, shape-specific
/// afterward. The caller always supplies the system prompt; this
/// function decides whether to pass it downstream based on whether the
/// provider's server already remembers it.
///
/// Support matrix (rig 0.34):
///
/// | Provider | Chain shape | Mechanism |
/// |----------|-------------|-----------|
/// | `openai` | `ServerId` | Responses API `previous_response_id`, O(1) per call. |
/// | `anthropic` | `History` | Client-side history + `with_automatic_caching()`. ~10% input rate for cached prefix. |
/// | `google` | `History` | Client-side history, full resend every call. |
/// | others | - | Returns `Err`. |
pub async fn session_send(
    state: &SharedState,
    chain: &crate::state::CoachChain,
    system_prompt: &str,
    message: &str,
    constraints: CallConstraints,
    log_ctx: LogContext<'_>,
) -> Result<(String, crate::state::CoachChain, crate::state::CoachUsage), String> {
    use crate::state::CoachChain;

    // Snapshot logger, provider, and mock once so we can record the
    // request and outcome without re-acquiring the lock around the await.
    let (logger, provider_name, model_name, mock) = {
        let s = state.read().await;
        let model = constraints.model.as_ref().unwrap_or(&s.config.model);
        (
            s.services.llm_logger.clone(),
            model.provider.clone(),
            model.model.clone(),
            s.services.mock_session_send.clone(),
        )
    };
    let started = std::time::Instant::now();

    // Mock interception: lets tests exercise the full pipeline without a
    // real LLM provider. Mock calls are logged too so tests can assert
    // on the logging path without needing real providers.
    let result: Result<(String, CoachChain, crate::state::CoachUsage), String> =
        if let Some(mock) = mock {
            mock(system_prompt, message).map(|(text, usage)| {
                let new_chain = grow_mock_chain(chain, message, &text);
                (text, new_chain, usage)
            })
        } else {
            dispatch_real(state, chain, system_prompt, message, constraints.clone()).await
        };

    if let Some(logger) = logger {
        let latency_ms = started.elapsed().as_millis() as u64;
        let record = LlmCallRecord {
            ts: chrono::Utc::now(),
            caller: log_ctx.caller.to_string(),
            session_id: log_ctx.session_id.map(str::to_string),
            provider: provider_name,
            model: model_name,
            system_prompt: system_prompt.to_string(),
            user_message: message.to_string(),
            chain_in: chain.clone(),
            require_json: constraints.require_json,
            max_output_tokens: constraints.max_output_tokens,
            response_text: result.as_ref().ok().map(|(t, _, _)| t.clone()),
            error: result.as_ref().err().cloned(),
            latency_ms,
            usage: result.as_ref().ok().map(|(_, _, u)| *u),
            chain_out: result.as_ref().ok().map(|(_, c, _)| c.clone()),
        };
        logger.append(&record);
    }

    result
}

/// Real-provider dispatch extracted from `session_send` so the logging
/// shell can own the error plumbing. Mirrors the old `match provider`
/// body 1-for-1; the only behavior change is that we no longer read the
/// mock here (the caller already intercepted).
async fn dispatch_real(
    state: &SharedState,
    chain: &crate::state::CoachChain,
    system_prompt: &str,
    message: &str,
    constraints: CallConstraints,
) -> Result<(String, crate::state::CoachChain, crate::state::CoachUsage), String> {
    use crate::state::CoachChain;
    use rig::completion::Completion;

    let cfg = snapshot_config(state, constraints.model.as_ref()).await?;
    let provider = cfg.primary.provider.as_str();

    match provider {
        "openai" => {
            let prev_id = match chain {
                CoachChain::ServerId { id } => Some(id.as_str()),
                _ => None,
            };
            let client = openai::Client::new(&cfg.primary_token)
                .map_err(|e| fmt_err("openai", e))?;
            let mut builder = client.agent(&cfg.primary.model);
            // Responses API remembers the system prompt after the first call.
            if prev_id.is_none() {
                builder = builder.preamble(system_prompt);
            }

            let mut extra = serde_json::Map::new();
            if let Some(prev) = prev_id {
                extra.insert(
                    "previous_response_id".into(),
                    serde_json::Value::String(prev.to_string()),
                );
            }
            if constraints.require_json {
                extra.insert(
                    "response_format".into(),
                    serde_json::json!({"type": "json_object"}),
                );
            }
            if let Some(max) = constraints.max_output_tokens {
                extra.insert(
                    "max_output_tokens".into(),
                    serde_json::Value::Number(max.into()),
                );
            }
            if !extra.is_empty() {
                builder = builder.additional_params(serde_json::Value::Object(extra));
            }

            let resp = builder
                .build()
                .completion(message, Vec::<rig::message::Message>::new())
                .await
                .map_err(|e| fmt_err("openai", e))?
                .send()
                .await
                .map_err(|e| fmt_err("openai", e))?;

            let text = extract_text(&resp.choice);
            let usage = to_coach_usage(resp.usage);
            Ok((text, CoachChain::ServerId { id: resp.raw_response.id.clone() }, usage))
        }
        "anthropic" => {
            warn_emulation_once("anthropic", "client-side history with prompt caching");
            let history = match chain {
                CoachChain::History { messages } => messages.as_slice(),
                _ => &[],
            };

            let client = anthropic::Client::new(&cfg.primary_token)
                .map_err(|e| fmt_err("anthropic", e))?;
            let model = client
                .completion_model(&cfg.primary.model)
                .with_automatic_caching();
            let agent = rig::agent::AgentBuilder::new(model)
                .preamble(system_prompt)
                .build();

            let max = constraints.max_output_tokens.unwrap_or(200) as u64;
            let resp = agent
                .completion(message, to_rig_history(history))
                .await
                .map_err(|e| fmt_err("anthropic", e))?
                .max_tokens(max)
                .send()
                .await
                .map_err(|e| fmt_err("anthropic", e))?;

            let text = extract_text(&resp.choice);
            let usage = to_coach_usage(resp.usage);
            let new_messages = extend_history(history, message, &text);
            Ok((text, CoachChain::History { messages: new_messages }, usage))
        }
        "google" => {
            warn_emulation_once("google", "client-side history, no prefix caching");
            let history = match chain {
                CoachChain::History { messages } => messages.as_slice(),
                _ => &[],
            };

            let client = gemini::Client::new(&cfg.primary_token)
                .map_err(|e| fmt_err("google", e))?;
            let agent = client
                .agent(&cfg.primary.model)
                .preamble(system_prompt)
                .build();

            let max = constraints.max_output_tokens.unwrap_or(200) as u64;
            let resp = agent
                .completion(message, to_rig_history(history))
                .await
                .map_err(|e| fmt_err("google", e))?
                .max_tokens(max)
                .send()
                .await
                .map_err(|e| fmt_err("google", e))?;

            let text = extract_text(&resp.choice);
            let usage = to_coach_usage(resp.usage);
            let new_messages = extend_history(history, message, &text);
            Ok((text, CoachChain::History { messages: new_messages }, usage))
        }
        other => Err(format!(
            "session_send: provider {other} has no session support (native or emulated)"
        )),
    }
}

/// Emit a one-time stderr warning the first time a given provider is
/// used under emulation. Idempotent across the process — subsequent
/// calls for the same provider are silent.
fn warn_emulation_once(provider: &str, mechanism: &str) {
    use std::collections::HashSet;
    use std::sync::Mutex;
    static WARNED: Mutex<Option<HashSet<String>>> = Mutex::new(None);
    let mut guard = WARNED.lock().unwrap();
    let set = guard.get_or_insert_with(HashSet::new);
    if set.insert(provider.to_string()) {
        eprintln!(
            "[coach] {provider}: no native session API; emulating via {mechanism}. \
             Cost scales with conversation length — see TODO.md."
        );
    }
}

/// Fire one observer call. Thin wrapper around `session_send` — builds
/// the system prompt from priorities and caps output at 80 tokens.
pub async fn observe_event(
    state: &SharedState,
    priorities: &[String],
    chain: &crate::state::CoachChain,
    event: &str,
    session_id: Option<&str>,
    model: Option<ModelConfig>,
) -> Result<(String, crate::state::CoachChain, crate::state::CoachUsage), String> {
    let system = coach_system_prompt(priorities)?;
    session_send(
        state,
        chain,
        &system,
        event,
        CallConstraints {
            max_output_tokens: Some(80),
            require_json: false,
            model,
        },
        LogContext::new("observer", session_id),
    )
    .await
}

/// Synchronous stop evaluation that continues the observer's chain.
/// Returns the parsed decision and the new chain handle.
pub async fn evaluate_stop_chained(
    state: &SharedState,
    priorities: &[String],
    chain: &crate::state::CoachChain,
    stop_reason: Option<&str>,
    session_id: Option<&str>,
    model: Option<ModelConfig>,
) -> Result<(StopDecision, crate::state::CoachChain, crate::state::CoachUsage), String> {
    let reason = stop_reason.unwrap_or("not specified");
    let stop_chained_template = crate::prompts::load("stop_chained")?;
    let prompt = crate::prompts::render(&stop_chained_template, &[("stop_reason", reason)]);
    let system = coach_system_prompt(priorities)?;
    let (text, new_chain, usage) = session_send(
        state,
        chain,
        &system,
        &prompt,
        CallConstraints {
            max_output_tokens: Some(200),
            require_json: true,
            model,
        },
        LogContext::new("stop_chained", session_id),
    )
    .await?;
    let decision = parse_stop_decision(&text)?;
    Ok((decision, new_chain, usage))
}

// ── Session title (periodic) ────────────────────────────────────────────

/// Snapshot of session signal we feed the namer. Kept tiny on purpose:
/// the namer's job is to compress, not to read code. Built by the caller
/// from the live `SessionState` so this module doesn't have to know about
/// it.
pub struct NameSessionInput {
    pub priorities: Vec<String>,
    pub cwd: Option<String>,
    pub tool_counts: HashMap<String, usize>,
    /// The most recent observer assessment, if any. The single best signal
    /// for "what is this conversation actually about" because it already
    /// encodes the observer's running interpretation.
    pub last_assessment: Option<String>,
    /// Coding-session id (e.g. Claude Code session UUID). Used as a
    /// routing key when JSONL logging is enabled.
    pub session_id: Option<String>,
}

/// Build the prompt for `name_session`. Pure function so the test can
/// pin its shape without making a real LLM call.
pub fn build_name_session_prompt(input: &NameSessionInput) -> Result<String, String> {
    let cwd = input.cwd.as_deref().unwrap_or("unknown");
    let priorities = if input.priorities.is_empty() {
        "(none set)".to_string()
    } else {
        input.priorities.join(", ")
    };
    let tools = if input.tool_counts.is_empty() {
        "none yet".to_string()
    } else {
        let mut counts: Vec<(&String, &usize)> = input.tool_counts.iter().collect();
        counts.sort_by(|a, b| b.1.cmp(a.1));
        counts
            .iter()
            .take(5)
            .map(|(name, n)| format!("{name}×{n}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let assessment = input
        .last_assessment
        .as_deref()
        .unwrap_or("(no assessment yet)");
    let template = crate::prompts::load("name_session_user")?;
    Ok(crate::prompts::render(
        &template,
        &[
            ("cwd", cwd),
            ("priorities", &priorities),
            ("tools", &tools),
            ("assessment", assessment),
        ],
    ))
}

/// Post-process the model's reply into a clean title: trim whitespace,
/// strip surrounding quotes and trailing punctuation, drop a leading
/// "Title:" label some models add, and cap at 4 words.
///
/// Returns `None` if the cleaned result is empty so the caller can leave
/// the previous title in place rather than overwriting with garbage.
pub fn clean_session_title(raw: &str) -> Option<String> {
    // Strip code fences first — some models wrap short answers in ```.
    let unfenced = strip_code_fence(raw.trim()).unwrap_or(raw.trim());

    // Drop a "Title:" / "title -" prefix if present.
    let after_label = unfenced
        .strip_prefix("Title:")
        .or_else(|| unfenced.strip_prefix("title:"))
        .unwrap_or(unfenced)
        .trim();

    // Trim surrounding quotes / brackets, then trailing punctuation.
    let trimmed = after_label
        .trim_matches(|c: char| c == '"' || c == '\'' || c == '`' || c == '*')
        .trim_end_matches(['.', ',', ';', ':'])
        .trim();

    if trimmed.is_empty() {
        return None;
    }

    let words: Vec<&str> = trimmed.split_whitespace().take(4).collect();
    if words.is_empty() {
        None
    } else {
        Some(words.join(" "))
    }
}

/// Ask the coach to name the session in <=4 words. Stateless: uses a
/// fresh `CoachChain::Empty` so the title turn never pollutes the
/// observer chain that `evaluate_stop_chained` reads. Returns the cleaned
/// title plus token usage.
pub async fn name_session(
    state: &SharedState,
    input: &NameSessionInput,
    model: Option<ModelConfig>,
) -> Result<(String, crate::state::CoachUsage), String> {
    let prompt = build_name_session_prompt(input)?;
    // Tiny system prompt — the namer doesn't need the full coach preamble.
    let system = crate::prompts::load("name_session_system")?;
    let (text, _chain, usage) = session_send(
        state,
        &crate::state::CoachChain::Empty,
        &system,
        &prompt,
        CallConstraints {
            max_output_tokens: Some(20),
            require_json: false,
            model,
        },
        LogContext::new("namer", input.session_id.as_deref()),
    )
    .await?;
    let cleaned = clean_session_title(&text)
        .ok_or_else(|| format!("name_session: empty title after cleaning: {text:?}"))?;
    Ok((cleaned, usage))
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

    // ── Stop prompt + decision ──────────────────────────────────────────

    fn ctx_with(priorities: Vec<&str>) -> StopContext {
        StopContext {
            priorities: priorities.into_iter().map(String::from).collect(),
            cwd: Some("/projects/foo".into()),
            tool_counts: HashMap::from([("Read".into(), 5), ("Edit".into(), 2)]),
            stop_count: 2,
            stop_blocked_count: 1,
            stop_reason: Some("end_turn".into()),
            session_id: None,
        }
    }

    /// Prompt should mention every concrete piece of context the LLM needs.
    /// This is the only invariant — wording is free to evolve.
    #[test]
    fn build_stop_prompt_includes_context() {
        let ctx = ctx_with(vec!["Speed", "Quality"]);
        let p = build_stop_prompt(&ctx).unwrap();
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
            session_id: None,
        };
        let p = build_stop_prompt(&ctx).unwrap();
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
        let p = coach_system_prompt(&["Speed".into(), "Quality".into()]).unwrap();
        assert!(p.contains("Speed"));
        assert!(p.contains("Quality"));
        assert!(p.contains("1. Speed"));
        assert!(p.contains("2. Quality"));
    }

    /// Empty priorities should produce a sensible placeholder, not an
    /// awkward "highest first:\n\n" with nothing under it.
    #[test]
    fn coach_system_prompt_handles_no_priorities() {
        let p = coach_system_prompt(&[]).unwrap();
        assert!(p.contains("none set"));
    }

    /// Observer event must contain both the tool name and the input,
    /// so the LLM truly "sees what Claude saw."
    #[test]
    fn build_observer_event_includes_tool_and_input() {
        let input = serde_json::json!({"file_path": "/a.py", "content": "print(1)"});
        let event = build_observer_event("Write", &input, Some("stabilize the UI")).unwrap();
        assert!(event.contains("Write"));
        assert!(event.contains("/a.py"));
        assert!(event.contains("print(1)"));
        assert!(event.contains("stabilize the UI"));
    }

    /// Observer event must serialize Null inputs without panicking
    /// (some tools have no input or send a literal null).
    #[test]
    fn build_observer_event_handles_null_input() {
        let event = build_observer_event("NoInput", &serde_json::Value::Null, None).unwrap();
        assert!(event.contains("NoInput"));
        assert!(event.contains("null"));
        assert!(event.contains("(not available)"));
    }

    // ── Session abstraction ─────────────────────────────────────────────

    /// CallConstraints::default() should give the cheapest possible call:
    /// no cap, no structured-output flag. Callers opt into stronger
    /// constraints explicitly.
    #[test]
    fn call_constraints_default_is_minimal() {
        let c = CallConstraints::default();
        assert!(c.max_output_tokens.is_none());
        assert!(!c.require_json);
    }

    /// parse_stop_decision must accept raw JSON, JSON wrapped in a
    /// ```json fence, and JSON wrapped in a bare triple-backtick fence.
    /// All three shapes come out of Anthropic in the wild.
    #[test]
    fn parse_stop_decision_accepts_fenced_and_plain_json() {
        let plain = r#"{"allow": true, "message": null}"#;
        let fenced_json = "```json\n{\"allow\": false, \"message\": \"keep going\"}\n```";
        let fenced_plain = "```\n{\"allow\": true, \"message\": null}\n```";

        let d = parse_stop_decision(plain).unwrap();
        assert!(d.allow);

        let d = parse_stop_decision(fenced_json).unwrap();
        assert!(!d.allow);
        assert_eq!(d.message.unwrap(), "keep going");

        let d = parse_stop_decision(fenced_plain).unwrap();
        assert!(d.allow);
    }

    /// Malformed JSON must surface the raw text in the error so callers
    /// can see what the model actually said.
    #[test]
    fn parse_stop_decision_error_includes_raw_text() {
        let garbage = "not json at all";
        let err = parse_stop_decision(garbage).unwrap_err();
        assert!(err.contains("not json at all"));
    }

    /// strip_code_fence is purely syntactic — the content-type tag after
    /// ``` is ignored and the trailing fence must close the block.
    #[test]
    fn strip_code_fence_variants() {
        assert_eq!(strip_code_fence("```json\nx\n```"), Some("x"));
        assert_eq!(strip_code_fence("```\nx\n```"), Some("x"));
        // Not a fence at all → None, caller falls back to raw text.
        assert_eq!(strip_code_fence("x"), None);
        // Missing closing fence → None (caller can still try to parse as-is).
        assert_eq!(strip_code_fence("```json\nx"), None);
    }

    // ── Session title (name_session) ────────────────────────────────────

    fn name_input(
        priorities: Vec<&str>,
        cwd: Option<&str>,
        tools: &[(&str, usize)],
        last_assessment: Option<&str>,
    ) -> NameSessionInput {
        NameSessionInput {
            priorities: priorities.into_iter().map(String::from).collect(),
            cwd: cwd.map(String::from),
            tool_counts: tools.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
            last_assessment: last_assessment.map(String::from),
            session_id: None,
        }
    }

    /// The namer prompt must surface every signal the model is supposed
    /// to use: cwd, priorities, top tools, and the latest assessment.
    /// Wording is free to evolve; this only pins the contents.
    #[test]
    fn build_name_session_prompt_includes_signal() {
        let input = name_input(
            vec!["Simplicity"],
            Some("/projects/coach"),
            &[("Edit", 12), ("Read", 5)],
            Some("investigating the auth bug"),
        );
        let p = build_name_session_prompt(&input).unwrap();
        for needle in [
            "/projects/coach",
            "Simplicity",
            "Edit",
            "12",
            "investigating the auth bug",
            "4 words",
        ] {
            assert!(p.contains(needle), "prompt missing {needle:?}: {p}");
        }
    }

    /// Empty inputs must produce sensible placeholders, not blank lines
    /// the model might interpret as missing fields. The namer should
    /// never receive "tools used: \n".
    #[test]
    fn build_name_session_prompt_handles_empty_input() {
        let input = name_input(vec![], None, &[], None);
        let p = build_name_session_prompt(&input).unwrap();
        assert!(p.contains("none set"));
        assert!(p.contains("none yet"));
        assert!(p.contains("unknown"));
        assert!(p.contains("(no assessment yet)"));
    }

    /// Property: clean_session_title is idempotent on already-clean input.
    /// Once cleaned, re-cleaning must not change the title further.
    #[test]
    fn clean_session_title_is_idempotent() {
        for raw in ["auth refactor", "fix login bug", "one"] {
            let once = clean_session_title(raw).unwrap();
            let twice = clean_session_title(&once).unwrap();
            assert_eq!(once, twice, "second clean changed: {raw:?}");
        }
    }

    /// Property: clean_session_title never returns more than 4 words.
    /// This is the hard cap the user asked for.
    #[test]
    fn clean_session_title_caps_at_four_words() {
        let raw = "refactoring the authentication middleware to satisfy compliance";
        let cleaned = clean_session_title(raw).unwrap();
        assert_eq!(cleaned.split_whitespace().count(), 4);
        assert_eq!(cleaned, "refactoring the authentication middleware");
    }

    /// Models love to wrap short answers — strip quotes, fences, "Title:"
    /// labels, and trailing punctuation regardless of which combo arrives.
    #[test]
    fn clean_session_title_strips_common_wrappers() {
        let cases = [
            ("\"auth refactor\"", "auth refactor"),
            ("'auth refactor'", "auth refactor"),
            ("Title: auth refactor", "auth refactor"),
            ("title: auth refactor.", "auth refactor"),
            ("```\nauth refactor\n```", "auth refactor"),
            ("```text\nauth refactor\n```", "auth refactor"),
            ("**auth refactor**", "auth refactor"),
            ("auth refactor.", "auth refactor"),
        ];
        for (raw, expected) in cases {
            assert_eq!(
                clean_session_title(raw).as_deref(),
                Some(expected),
                "raw: {raw:?}",
            );
        }
    }

    /// Empty / whitespace-only / pure-punctuation replies must come back
    /// as `None` so the caller leaves the previous title untouched.
    #[test]
    fn clean_session_title_rejects_empty_payloads() {
        for raw in ["", "   ", "\"\"", "...", "**"] {
            assert_eq!(clean_session_title(raw), None, "raw should be empty: {raw:?}");
        }
    }

    /// warn_emulation_once must only fire once per provider per process.
    /// We can't easily assert on stderr, but we can verify the function
    /// itself doesn't panic on repeat calls and that HashSet insertion
    /// is idempotent. This is a smoke test, not a strict assertion.
    #[test]
    fn warn_emulation_once_is_idempotent() {
        // Fire multiple times — shouldn't panic or deadlock.
        for _ in 0..5 {
            warn_emulation_once("anthropic", "test mechanism");
        }
        // Different providers should coexist.
        warn_emulation_once("google", "test mechanism");
        warn_emulation_once("anthropic", "test mechanism");
    }

    // ── session_send ↔ llm_log wiring ───────────────────────────────────
    //
    // These tests drive `session_send` through the mock path so we can
    // verify the logger hook without touching a real provider. They set
    // a mock closure on state + install a tempdir-backed logger, then
    // assert on the on-disk JSONL.

    use crate::llm_log::LlmLogger;
    use crate::state::{CoachUsage, MockSessionSend};
    use std::sync::Arc;
    use tokio::sync::RwLock;

    /// State wired with both a mock and a tempdir-backed logger.
    fn mocked_state_with_logger(
        tmpdir: &tempfile::TempDir,
        mock: MockSessionSend,
    ) -> crate::state::SharedState {
        let mut cs = crate::state::test_state();
        cs.services.mock_session_send = Some(mock);
        cs.services.llm_logger =
            Some(LlmLogger::at(tmpdir.path().to_path_buf()).unwrap());
        Arc::new(RwLock::new(cs))
    }

    /// Property: every successful `session_send` call writes exactly one
    /// JSONL line with the full request + response + usage.
    #[tokio::test]
    async fn session_send_writes_one_line_per_call() {
        let tmp = tempfile::tempdir().unwrap();
        let mock: MockSessionSend = Arc::new(|_system, _user| {
            Ok((
                "mock response".to_string(),
                CoachUsage {
                    input_tokens: 11,
                    output_tokens: 3,
                    cached_input_tokens: 0,
                },
            ))
        });
        let state = mocked_state_with_logger(&tmp, mock);

        let (text, _chain, _usage) = session_send(
            &state,
            &crate::state::CoachChain::Empty,
            "sys prompt",
            "user msg",
            CallConstraints {
                max_output_tokens: Some(40),
                require_json: false,
                ..Default::default()
            },
            crate::llm_log::LogContext::new("observer", Some("sess-xyz")),
        )
        .await
        .unwrap();
        assert_eq!(text, "mock response");

        let contents =
            std::fs::read_to_string(tmp.path().join("sess-xyz.jsonl")).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 1);
        let v: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(v["caller"], "observer");
        assert_eq!(v["session_id"], "sess-xyz");
        assert_eq!(v["system_prompt"], "sys prompt");
        assert_eq!(v["user_message"], "user msg");
        assert_eq!(v["response_text"], "mock response");
        assert!(v["error"].is_null());
        assert_eq!(v["usage"]["input_tokens"], 11);
        assert_eq!(v["usage"]["output_tokens"], 3);
        assert_eq!(v["max_output_tokens"], 40);
        assert_eq!(v["require_json"], false);
    }

    /// Property: mock errors are logged too — error field carries the
    /// failure text, response_text and usage are null.
    #[tokio::test]
    async fn session_send_logs_errors_too() {
        let tmp = tempfile::tempdir().unwrap();
        let mock: MockSessionSend = Arc::new(|_, _| Err("boom".to_string()));
        let state = mocked_state_with_logger(&tmp, mock);

        let result = session_send(
            &state,
            &crate::state::CoachChain::Empty,
            "sys",
            "msg",
            CallConstraints::default(),
            crate::llm_log::LogContext::new("namer", Some("sess-err")),
        )
        .await;
        assert!(result.is_err());

        let contents =
            std::fs::read_to_string(tmp.path().join("sess-err.jsonl")).unwrap();
        let v: serde_json::Value = serde_json::from_str(contents.trim()).unwrap();
        assert_eq!(v["caller"], "namer");
        assert_eq!(v["error"], "boom");
        assert!(v["response_text"].is_null());
        assert!(v["usage"].is_null());
        assert!(v["chain_out"].is_null());
    }

    /// Property: without a logger attached, `session_send` produces no
    /// files at all — logging is opt-in and stays off by default.
    #[tokio::test]
    async fn session_send_writes_nothing_when_logger_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let mock: MockSessionSend = Arc::new(|_, _| {
            Ok(("ok".into(), CoachUsage::default()))
        });
        let mut cs = crate::state::test_state();
        cs.services.mock_session_send = Some(mock);
        // No llm_logger installed.
        let state = Arc::new(RwLock::new(cs));

        session_send(
            &state,
            &crate::state::CoachChain::Empty,
            "sys",
            "msg",
            CallConstraints::default(),
            crate::llm_log::LogContext::new("observer", Some("sess-nolog")),
        )
        .await
        .unwrap();

        let entries: Vec<_> = std::fs::read_dir(tmp.path()).unwrap().collect();
        assert!(entries.is_empty(), "no logger → no files");
    }

    /// Property: each wrapper tags its caller correctly so downstream
    /// analysis can filter by call type. Uses `observe_event` (observer)
    /// and `evaluate_stop_chained` (stop_chained) routed through the
    /// mock so the call shape is checked, not the LLM behavior.
    #[tokio::test]
    async fn wrappers_tag_their_caller_in_the_log() {
        let tmp = tempfile::tempdir().unwrap();
        let mock: MockSessionSend = Arc::new(|_, _| {
            Ok((
                r#"{"allow": true, "message": null}"#.to_string(),
                CoachUsage::default(),
            ))
        });
        let state = mocked_state_with_logger(&tmp, mock);
        let priorities = vec!["Simplicity".into()];
        let chain = crate::state::CoachChain::Empty;

        observe_event(&state, &priorities, &chain, "event", Some("sess-q"), None)
            .await
            .unwrap();
        evaluate_stop_chained(&state, &priorities, &chain, Some("end_turn"), Some("sess-q"), None)
            .await
            .unwrap();

        let contents =
            std::fs::read_to_string(tmp.path().join("sess-q.jsonl")).unwrap();
        let callers: Vec<String> = contents
            .lines()
            .map(|l| {
                let v: serde_json::Value = serde_json::from_str(l).unwrap();
                v["caller"].as_str().unwrap().to_string()
            })
            .collect();
        assert_eq!(callers, vec!["observer", "stop_chained"]);
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
    use crate::state::SharedState;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    /// Build an AppState that uses the live `OPENAI_API_KEY`.
    /// Returns None when the key is missing so tests no-op cleanly.
    fn live_state() -> Option<SharedState> {
        let token = std::env::var("OPENAI_API_KEY").ok().filter(|v| !v.is_empty())?;
        let mut state = crate::state::test_state();
        state.config.priorities = vec!["Test priority".into()];
        state.config.coach_mode = EngineMode::Llm;
        state.config.model = ModelConfig {
            provider: "openai".into(),
            model: "gpt-5.4-mini".into(),
        };
        state.config.api_tokens.insert("openai".into(), token);
        Some(Arc::new(RwLock::new(state)))
    }

    /// Smallest possible round-trip via session_send. Verifies the
    /// OpenAI Responses API path returns text and a ServerId chain.
    #[tokio::test]
    #[ignore]
    async fn live_session_send_openai_basic() {
        let Some(state) = live_state() else { return };
        let (text, chain, usage) = session_send(
            &state,
            &crate::state::CoachChain::Empty,
            "You are a test bot.",
            "Reply with the single word: hello",
            CallConstraints { max_output_tokens: Some(20), require_json: false, ..Default::default() },
            LogContext::new("test", None),
        )
        .await
        .expect("session_send failed");
        assert!(!text.is_empty(), "expected non-empty text");
        assert!(matches!(chain, crate::state::CoachChain::ServerId { .. }));
        assert!(usage.input_tokens > 0);
    }

    /// Server-side memory via session_send: tell the model a fact, then
    /// ask about it on the next turn through the chain.
    #[tokio::test]
    #[ignore]
    async fn live_session_send_openai_continues_context() {
        let Some(state) = live_state() else { return };
        let system = "You are a test bot. Reply tersely.";
        let constraints = CallConstraints { max_output_tokens: Some(30), require_json: false, ..Default::default() };

        let (_t1, chain1, _u1) = session_send(
            &state,
            &crate::state::CoachChain::Empty,
            system,
            "Remember this token: PURPLE-OWL-42. Reply 'noted'.",
            constraints.clone(),
            LogContext::new("test", None),
        )
        .await
        .expect("first call failed");

        let (text, _chain2, _u2) = session_send(
            &state,
            &chain1,
            system,
            "What was the token I told you to remember? Reply with just the token.",
            constraints,
            LogContext::new("test", None),
        )
        .await
        .expect("second call failed");

        assert!(
            text.contains("PURPLE-OWL-42"),
            "model didn't remember across turns; got: {text}"
        );
    }

    /// json_object response format must produce parseable JSON.
    #[tokio::test]
    #[ignore]
    async fn live_session_send_openai_json_mode() {
        let Some(state) = live_state() else { return };
        let (text, _chain, _usage) = session_send(
            &state,
            &crate::state::CoachChain::Empty,
            "You return JSON only.",
            "Return JSON of the form {\"answer\": <number>}. The number is 7.",
            CallConstraints { max_output_tokens: Some(60), require_json: true, ..Default::default() },
            LogContext::new("test", None),
        )
        .await
        .expect("session_send (json) failed");

        let parsed: serde_json::Value = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("json parse failed ({e}) on: {text}"));
        assert!(parsed.get("answer").is_some(), "missing 'answer' field: {text}");
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
            None,
        )
        .unwrap();
        let (_text1, chain1, _u1) =
            observe_event(&state, &priorities, &crate::state::CoachChain::Empty, &event1, None, None)
                .await
                .expect("first observe_event failed");
        let id1 = match &chain1 {
            crate::state::CoachChain::ServerId { id: response_id } => response_id.clone(),
            other => panic!("expected ServerId chain, got {other:?}"),
        };
        assert!(id1.starts_with("resp_"));

        let event2 = build_observer_event(
            "Bash",
            &serde_json::json!({"command": "python -c 'from x import add; print(add(2,3))'"}),
            None,
        )
        .unwrap();
        let (_text2, chain2, _u2) = observe_event(&state, &priorities, &chain1, &event2, None, None)
            .await
            .expect("second observe_event failed");
        let id2 = match &chain2 {
            crate::state::CoachChain::ServerId { id: response_id } => response_id.clone(),
            other => panic!("expected ServerId chain, got {other:?}"),
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
            None,
        )
        .unwrap();
        let (_text, observed_chain, _u) =
            observe_event(&state, &priorities, &crate::state::CoachChain::Empty, &event, None, None)
                .await
                .expect("observe failed");

        let (decision, new_chain, _u2) = evaluate_stop_chained(
            &state,
            &priorities,
            &observed_chain,
            Some("end_turn"),
            None,
            None,
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
        assert!(matches!(new_chain, crate::state::CoachChain::ServerId { .. }));
    }

    /// First-call evaluate_stop_chained (no prior chain) should also work,
    /// since the system preamble is set when chain is Empty.
    #[tokio::test]
    #[ignore]
    async fn live_evaluate_stop_chained_no_prior_context() {
        let Some(state) = live_state() else { return };
        let priorities = vec!["Do good work".into()];
        let (_decision, new_chain, _u) = evaluate_stop_chained(
            &state,
            &priorities,
            &crate::state::CoachChain::Empty,
            Some("end_turn"),
            None,
            None,
        )
        .await
        .expect("first-turn evaluate_stop_chained failed");
        assert!(matches!(new_chain, crate::state::CoachChain::ServerId { .. }));
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
        let mut state = crate::state::test_state();
        state.config.priorities = vec!["Test priority".into()];
        state.config.coach_mode = EngineMode::Llm;
        state.config.model = ModelConfig {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5-20251001".into(),
        };
        state.config.api_tokens.insert("anthropic".into(), token);
        Some(Arc::new(RwLock::new(state)))
    }

    /// Smallest possible Anthropic round-trip via session_send.
    #[tokio::test]
    #[ignore]
    async fn live_session_send_anthropic_basic() {
        let Some(state) = live_state_anthropic() else { return };
        let (text, chain, usage) = session_send(
            &state,
            &crate::state::CoachChain::Empty,
            "You are a test bot. Respond with one word.",
            "Reply with the single word: hello",
            CallConstraints { max_output_tokens: Some(20), require_json: false, ..Default::default() },
            LogContext::new("test", None),
        )
        .await
        .expect("session_send failed");
        assert!(!text.is_empty(), "expected non-empty text");
        let messages = match &chain {
            crate::state::CoachChain::History { messages } => messages,
            other => panic!("expected History chain, got {other:?}"),
        };
        assert_eq!(messages.len(), 2, "history should grow by user+assistant");
        assert!(usage.input_tokens > 0, "expected non-zero input_tokens");
        assert!(usage.output_tokens > 0, "expected non-zero output_tokens");
    }

    /// Anthropic preserves context across turns via client-side history
    /// threaded through CoachChain::History.
    #[tokio::test]
    #[ignore]
    async fn live_session_send_anthropic_continues_context() {
        let Some(state) = live_state_anthropic() else { return };
        let system = "You are a test bot. Reply tersely.";
        let constraints = CallConstraints { max_output_tokens: Some(30), require_json: false, ..Default::default() };

        let (_t1, chain1, _u1) = session_send(
            &state,
            &crate::state::CoachChain::Empty,
            system,
            "Remember this token: PURPLE-OWL-42. Reply 'noted'.",
            constraints.clone(),
            LogContext::new("test", None),
        )
        .await
        .expect("first call failed");

        let (text, _chain2, _u2) = session_send(
            &state,
            &chain1,
            system,
            "What was the token I told you to remember? Reply with just the token.",
            constraints,
            LogContext::new("test", None),
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
            None,
        )
        .unwrap();
        let (_t1, chain1, _u1) =
            observe_event(&state, &priorities, &crate::state::CoachChain::Empty, &event1, None, None)
                .await
                .expect("first observe_event failed");
        let h1 = match &chain1 {
            crate::state::CoachChain::History { messages: history } => history.clone(),
            other => panic!("expected History chain, got {other:?}"),
        };
        assert_eq!(h1.len(), 2, "first call should produce user+assistant pair");

        let event2 = build_observer_event(
            "Bash",
            &serde_json::json!({"command": "python -c 'from x import add; print(add(2,3))'"}),
            None,
        )
        .unwrap();
        let (_t2, chain2, _u2) = observe_event(&state, &priorities, &chain1, &event2, None, None)
            .await
            .expect("second observe_event failed");
        let h2 = match &chain2 {
            crate::state::CoachChain::History { messages: history } => history.clone(),
            other => panic!("expected History chain, got {other:?}"),
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
            None,
        )
        .unwrap();
        let (_t, observed_chain, _u_obs) =
            observe_event(&state, &priorities, &crate::state::CoachChain::Empty, &event, None, None)
                .await
                .expect("observe failed");

        let (decision, new_chain, _u_stop) = evaluate_stop_chained(
            &state,
            &priorities,
            &observed_chain,
            Some("end_turn"),
            None,
            None,
        )
        .await
        .expect("evaluate_stop_chained failed");

        let _ = decision.allow;
        if !decision.allow {
            assert!(decision.message.is_some(), "block decision should carry a message");
        }
        assert!(matches!(new_chain, crate::state::CoachChain::History { .. }));
    }

    // ── Gemini live tests ───────────────────────────────────────────────
    //
    // Same shape as Anthropic: client-side history, gated on
    // GOOGLE_API_KEY + #[ignore]. The point of these tests is to catch
    // request-shape regressions (wrong role mapping, missing max_tokens,
    // etc.) and confirm continuity actually works when we resend the
    // accumulated history.

    fn live_state_google() -> Option<SharedState> {
        let token = std::env::var("GOOGLE_API_KEY")
            .ok()
            .or_else(|| std::env::var("GEMINI_API_KEY").ok())
            .filter(|v| !v.is_empty())?;
        let mut state = crate::state::test_state();
        state.config.priorities = vec!["Test priority".into()];
        state.config.coach_mode = EngineMode::Llm;
        state.config.model = ModelConfig {
            provider: "google".into(),
            // Cheap fast default — pick the current Flash model that
            // rig 0.34 knows about. Override by editing locally if a
            // newer Flash ships.
            model: "gemini-2.5-flash".into(),
        };
        state.config.api_tokens.insert("google".into(), token);
        Some(Arc::new(RwLock::new(state)))
    }

    /// Smallest Gemini round-trip via session_send.
    #[tokio::test]
    #[ignore]
    async fn live_session_send_gemini_basic() {
        let Some(state) = live_state_google() else { return };
        let (text, chain, _usage) = session_send(
            &state,
            &crate::state::CoachChain::Empty,
            "You are a test bot. Respond with one word.",
            "Reply with the single word: hello",
            CallConstraints { max_output_tokens: Some(20), require_json: false, ..Default::default() },
            LogContext::new("test", None),
        )
        .await
        .expect("session_send failed");
        assert!(!text.is_empty(), "expected non-empty text");
        let messages = match &chain {
            crate::state::CoachChain::History { messages } => messages,
            other => panic!("expected History chain, got {other:?}"),
        };
        assert_eq!(messages.len(), 2, "history should grow by user+assistant");
    }

    /// Gemini preserves context via client-side history resend.
    #[tokio::test]
    #[ignore]
    async fn live_session_send_gemini_continues_context() {
        let Some(state) = live_state_google() else { return };
        let system = "You are a test bot. Reply tersely.";
        let constraints = CallConstraints { max_output_tokens: Some(30), require_json: false, ..Default::default() };

        let (_t1, chain1, _u1) = session_send(
            &state,
            &crate::state::CoachChain::Empty,
            system,
            "Remember this token: PURPLE-OWL-42. Reply 'noted'.",
            constraints.clone(),
            LogContext::new("test", None),
        )
        .await
        .expect("first call failed");

        let (text, _chain2, _u2) = session_send(
            &state,
            &chain1,
            system,
            "What was the token I told you to remember? Reply with just the token.",
            constraints,
            LogContext::new("test", None),
        )
        .await
        .expect("second call failed");

        assert!(
            text.contains("PURPLE-OWL-42"),
            "model didn't remember across turns; got: {text}"
        );
    }

    /// observe_event chain accumulation for Gemini via session_send.
    /// First call seeds the history; second call extends it. Same
    /// invariants as the Anthropic version.
    #[tokio::test]
    #[ignore]
    async fn live_observe_event_chain_gemini() {
        let Some(state) = live_state_google() else { return };
        let priorities = vec!["Code quality".into()];

        let event1 = build_observer_event(
            "Edit",
            &serde_json::json!({
                "file_path": "/tmp/x.py",
                "new_string": "def add(a, b):\n    return a + b\n"
            }),
            None,
        )
        .unwrap();
        let (_t1, chain1, _u1) =
            observe_event(&state, &priorities, &crate::state::CoachChain::Empty, &event1, None, None)
                .await
                .expect("first observe_event failed");
        let h1 = match &chain1 {
            crate::state::CoachChain::History { messages: history } => history.clone(),
            other => panic!("expected History chain, got {other:?}"),
        };
        assert_eq!(h1.len(), 2, "first call should produce user+assistant pair");

        let event2 = build_observer_event(
            "Bash",
            &serde_json::json!({"command": "python -c 'from x import add; print(add(2,3))'"}),
            None,
        )
        .unwrap();
        let (_t2, chain2, _u2) = observe_event(&state, &priorities, &chain1, &event2, None, None)
            .await
            .expect("second observe_event failed");
        let h2 = match &chain2 {
            crate::state::CoachChain::History { messages: history } => history.clone(),
            other => panic!("expected History chain, got {other:?}"),
        };
        assert_eq!(h2.len(), 4, "history should grow to 4 messages");
    }

    /// evaluate_stop_chained over Gemini must produce a parseable
    /// StopDecision. Gemini doesn't honor `response_format: json_object`,
    /// so we rely on prompt-level instructions + strip_code_fence.
    #[tokio::test]
    #[ignore]
    async fn live_evaluate_stop_chained_gemini() {
        let Some(state) = live_state_google() else { return };
        let priorities = vec!["Finish the task".into()];

        let event = build_observer_event(
            "Edit",
            &serde_json::json!({"file_path": "/tmp/done.py", "new_string": "print('done')"}),
            None,
        )
        .unwrap();
        let (_t, observed_chain, _u_obs) =
            observe_event(&state, &priorities, &crate::state::CoachChain::Empty, &event, None, None)
                .await
                .expect("observe failed");

        let (decision, new_chain, _u_stop) = evaluate_stop_chained(
            &state,
            &priorities,
            &observed_chain,
            Some("end_turn"),
            None,
            None,
        )
        .await
        .expect("evaluate_stop_chained failed");

        let _ = decision.allow;
        if !decision.allow {
            assert!(decision.message.is_some(), "block decision should carry a message");
        }
        assert!(matches!(new_chain, crate::state::CoachChain::History { .. }));
    }
}
