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
/// plus the new `response_id` to pass as `previous_response_id` next time,
/// plus the token usage so callers can roll up cost telemetry.
pub struct ChainCall {
    pub text: String,
    pub response_id: String,
    pub usage: crate::state::CoachUsage,
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
}

/// Per-call constraints for `session_send`. Everything is optional /
/// defaulted so callers who don't care can pass `CallConstraints::default()`.
#[derive(Debug, Clone, Copy, Default)]
pub struct CallConstraints {
    /// Hard cap on the model's output length. OpenAI maps this to
    /// `max_output_tokens`; Anthropic maps it to `max_tokens`.
    pub max_output_tokens: Option<u32>,
    /// Force JSON output where the provider supports it (OpenAI's
    /// `response_format: json_object`). On providers without an
    /// equivalent flag, the caller is expected to prompt for JSON in
    /// the message itself.
    pub require_json: bool,
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

/// Ask the coach LLM whether to allow or block a stop request.
/// The caller is responsible for releasing any state locks before invoking
/// this — the LLM call may take seconds.
pub async fn evaluate_stop(
    state: &SharedState,
    ctx: &StopContext,
) -> Result<StopDecision, String> {
    let prompt = build_stop_prompt(ctx)?;
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
    let usage = to_coach_usage(resp.usage);

    Ok(ChainCall { text, response_id, usage })
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
) -> Result<(String, Vec<crate::state::CoachMessage>, crate::state::CoachUsage), String> {
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
    let usage = to_coach_usage(resp.usage);

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

    Ok((text, new_history, usage))
}

// ── Stateful chain via Google Gemini (pure history resend) ────────────

/// Low-level call into Google's Gemini API. Gemini has no server-side
/// conversation state (no `previous_response_id`) and rig 0.34 does not
/// surface the `cachedContent` API in a way that fits a growing
/// observer chain — so the only honest option is to resend the full
/// history each call and pay full input rate on every turn.
///
/// This is the same pattern Google's own Python SDK uses for
/// `genai.ChatSession.send_message`: it's a client-side convenience
/// that feels like a session but ships the accumulated history every
/// turn under the hood. We do the same in `session_send`'s google arm.
///
/// Caller maintains the history Vec (threaded through `CoachChain::Google`).
/// On return, the vec grows by two messages: the user turn and the
/// assistant reply.
pub async fn chain_gemini(
    state: &SharedState,
    system: Option<&str>,
    history: &[crate::state::CoachMessage],
    new_message: &str,
    max_output_tokens: Option<u32>,
) -> Result<(String, Vec<crate::state::CoachMessage>, crate::state::CoachUsage), String> {
    use rig::client::CompletionClient;
    use rig::completion::{AssistantContent, Completion};
    use rig::message::Message as RigMessage;

    let cfg = snapshot_config(state).await?;
    if cfg.primary.provider != "google" {
        return Err(format!(
            "chain_gemini requires the Google provider; current: {}",
            cfg.primary.provider
        ));
    }

    let client = gemini::Client::new(&cfg.primary_token).map_err(|e| fmt_err("google", e))?;
    let mut builder = client.agent(&cfg.primary.model);
    if let Some(s) = system {
        builder = builder.preamble(s);
    }
    let agent = builder.build();

    let rig_history: Vec<RigMessage> = history
        .iter()
        .map(|m| match m.role {
            crate::state::CoachRole::User => RigMessage::user(&m.content),
            crate::state::CoachRole::Assistant => RigMessage::assistant(&m.content),
        })
        .collect();

    // Gemini accepts max_tokens via the standard CompletionRequestBuilder,
    // same shape as the Anthropic call. Default to 200 when unset.
    let max = max_output_tokens.unwrap_or(200) as u64;
    let resp = agent
        .completion(new_message, rig_history)
        .await
        .map_err(|e| fmt_err("google", e))?
        .max_tokens(max)
        .send()
        .await
        .map_err(|e| fmt_err("google", e))?;

    let text = resp
        .choice
        .iter()
        .filter_map(|c| match c {
            AssistantContent::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    let usage = to_coach_usage(resp.usage);

    let mut new_history = history.to_vec();
    new_history.push(crate::state::CoachMessage {
        role: crate::state::CoachRole::User,
        content: new_message.to_string(),
    });
    new_history.push(crate::state::CoachMessage {
        role: crate::state::CoachRole::Assistant,
        content: text.clone(),
    });

    Ok((text, new_history, usage))
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
pub fn build_observer_event(
    tool_name: &str,
    tool_input: &serde_json::Value,
) -> Result<String, String> {
    let input_pretty = serde_json::to_string(tool_input).unwrap_or_else(|_| "{}".into());
    let template = crate::prompts::load("observer_event")?;
    Ok(crate::prompts::render(
        &template,
        &[("tool_name", tool_name), ("tool_input", &input_pretty)],
    ))
}

/// Read the active provider from state, releasing the lock immediately.
async fn read_provider(state: &SharedState) -> String {
    state.read().await.model.provider.clone()
}

/// Append a single message to a coach session and get back the model's
/// reply. This is the one primitive every caller should use — specific
/// use cases (observer, stop evaluation, future features) are thin
/// wrappers that just build the right message and constraints.
///
/// The session is represented by the opaque `CoachChain` handle that
/// callers thread through: `Empty` on the first call, provider-specific
/// afterward. The caller always supplies the system prompt; this
/// function decides whether to pass it downstream based on whether the
/// provider's server already remembers it (OpenAI Responses does after
/// the first call; everyone else needs it every time but gets it cached
/// cheap where supported).
///
/// Support matrix (rig 0.34):
///
/// | Provider | Mode | Mechanism |
/// |----------|------|-----------|
/// | `openai` | **native** | Responses API `previous_response_id`, O(1) per call. |
/// | `anthropic` | emulated | Client-side `Vec<CoachMessage>` + `with_automatic_caching()`. Cached prefix ~10% of full input rate. |
/// | `google` | emulated | Client-side `Vec<CoachMessage>`, full history resent every call. No prefix caching that fits a growing chain — use cheap Flash models to keep cost tolerable. |
/// | others | unsupported | Returns `Err`. |
///
/// Emulated providers emit a once-per-process stderr warning on first
/// use so the developer knows the cost model differs from native.
pub async fn session_send(
    state: &SharedState,
    chain: &crate::state::CoachChain,
    system_prompt: &str,
    message: &str,
    constraints: CallConstraints,
) -> Result<(String, crate::state::CoachChain, crate::state::CoachUsage), String> {
    use crate::state::CoachChain;

    match read_provider(state).await.as_str() {
        "openai" => {
            let prev_id = match chain {
                CoachChain::OpenAi { response_id } => Some(response_id.as_str()),
                _ => None,
            };
            // Responses API remembers the system prompt once it's been
            // sent — resending on every call would duplicate it.
            let system_for_call = if prev_id.is_none() { Some(system_prompt) } else { None };
            let call = chain_openai(
                state,
                message,
                system_for_call,
                prev_id,
                constraints.require_json,
                constraints.max_output_tokens,
            )
            .await?;
            Ok((
                call.text,
                CoachChain::OpenAi { response_id: call.response_id },
                call.usage,
            ))
        }
        "anthropic" => {
            warn_emulation_once("anthropic", "client-side history with prompt caching");
            let history = match chain {
                CoachChain::Anthropic { history } => history.clone(),
                _ => Vec::new(),
            };
            let (text, new_history, usage) = chain_anthropic(
                state,
                Some(system_prompt),
                &history,
                message,
                constraints.max_output_tokens,
            )
            .await?;
            Ok((text, CoachChain::Anthropic { history: new_history }, usage))
        }
        "google" => {
            warn_emulation_once("google", "client-side history, no prefix caching");
            let history = match chain {
                CoachChain::Google { history } => history.clone(),
                _ => Vec::new(),
            };
            let (text, new_history, usage) = chain_gemini(
                state,
                Some(system_prompt),
                &history,
                message,
                constraints.max_output_tokens,
            )
            .await?;
            Ok((text, CoachChain::Google { history: new_history }, usage))
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
        },
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
        },
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
        },
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
        let event = build_observer_event("Write", &input).unwrap();
        assert!(event.contains("Write"));
        assert!(event.contains("/a.py"));
        assert!(event.contains("print(1)"));
    }

    /// Observer event must serialize Null inputs without panicking
    /// (some tools have no input or send a literal null).
    #[test]
    fn build_observer_event_handles_null_input() {
        let event = build_observer_event("NoInput", &serde_json::Value::Null).unwrap();
        assert!(event.contains("NoInput"));
        assert!(event.contains("null"));
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
            auto_uninstall_hooks_on_exit: true,
            hooks_user_enabled: false,
            cursor_hooks_user_enabled: false,
            #[cfg(feature = "pycoach")]
            pycoach: None,
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
        )
        .unwrap();
        let (_text1, chain1, _u1) =
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
        )
        .unwrap();
        let (_text2, chain2, _u2) = observe_event(&state, &priorities, &chain1, &event2)
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
        )
        .unwrap();
        let (_text, observed_chain, _u) =
            observe_event(&state, &priorities, &crate::state::CoachChain::Empty, &event)
                .await
                .expect("observe failed");

        let (decision, new_chain, _u2) = evaluate_stop_chained(
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
        let (_decision, new_chain, _u) = evaluate_stop_chained(
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
            auto_uninstall_hooks_on_exit: true,
            hooks_user_enabled: false,
            cursor_hooks_user_enabled: false,
            #[cfg(feature = "pycoach")]
            pycoach: None,
        };
        Some(Arc::new(RwLock::new(state)))
    }

    /// Smallest possible Anthropic round-trip — verifies the agent +
    /// caching builder + extraction path works end to end.
    #[tokio::test]
    #[ignore]
    async fn live_chain_anthropic_basic() {
        let Some(state) = live_state_anthropic() else { return };
        let (text, new_history, usage) = chain_anthropic(
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
        // Usage should report something — both fields should be > 0 for any
        // real call. This is the regression check for the rig field shape.
        assert!(usage.input_tokens > 0, "expected non-zero input_tokens");
        assert!(usage.output_tokens > 0, "expected non-zero output_tokens");
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

        let (_t1, h1, _u1) = chain_anthropic(
            &state,
            Some(system),
            &[],
            "Remember this token: PURPLE-OWL-42. Reply 'noted'.",
            Some(20),
        )
        .await
        .expect("first call failed");

        let (text, _h2, _u2) = chain_anthropic(
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
        )
        .unwrap();
        let (_t1, chain1, _u1) =
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
        )
        .unwrap();
        let (_t2, chain2, _u2) = observe_event(&state, &priorities, &chain1, &event2)
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
        )
        .unwrap();
        let (_t, observed_chain, _u_obs) =
            observe_event(&state, &priorities, &crate::state::CoachChain::Empty, &event)
                .await
                .expect("observe failed");

        let (decision, new_chain, _u_stop) = evaluate_stop_chained(
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
        let state = CoachState {
            sessions: HashMap::new(),
            session_id_to_pid: HashMap::new(),
            priorities: vec!["Test priority".into()],
            port: 7700,
            theme: Theme::System,
            default_mode: CoachMode::Present,
            model: ModelConfig {
                provider: "google".into(),
                // Cheap fast default — pick the current Flash model that
                // rig 0.34 knows about. Override by editing locally if a
                // newer Flash ships.
                model: "gemini-2.5-flash".into(),
            },
            api_tokens: HashMap::from([("google".into(), token)]),
            env_tokens: HashMap::new(),
            http_client: reqwest::Client::new(),
            coach_mode: EngineMode::Llm,
            rules: vec![],
            auto_uninstall_hooks_on_exit: true,
            hooks_user_enabled: false,
            cursor_hooks_user_enabled: false,
            #[cfg(feature = "pycoach")]
            pycoach: None,
        };
        Some(Arc::new(RwLock::new(state)))
    }

    /// Smallest Gemini round-trip — exercises the request builder,
    /// role conversion, and max_tokens plumbing.
    #[tokio::test]
    #[ignore]
    async fn live_chain_gemini_basic() {
        let Some(state) = live_state_google() else { return };
        let (text, new_history, _usage) = chain_gemini(
            &state,
            Some("You are a test bot. Respond with one word."),
            &[],
            "Reply with the single word: hello",
            Some(20),
        )
        .await
        .expect("chain_gemini failed");
        assert!(!text.is_empty(), "expected non-empty text");
        assert_eq!(new_history.len(), 2, "history should grow by user+assistant");
        assert_eq!(new_history[0].role, crate::state::CoachRole::User);
        assert_eq!(new_history[1].role, crate::state::CoachRole::Assistant);
    }

    /// Gemini preserves context across turns the same way Anthropic does:
    /// by us resending the growing history. Model should recall a token
    /// planted on the previous turn.
    #[tokio::test]
    #[ignore]
    async fn live_chain_gemini_continues_context_via_history() {
        let Some(state) = live_state_google() else { return };
        let system = "You are a test bot. Reply tersely.";

        let (_t1, h1, _u1) = chain_gemini(
            &state,
            Some(system),
            &[],
            "Remember this token: PURPLE-OWL-42. Reply 'noted'.",
            Some(20),
        )
        .await
        .expect("first call failed");

        let (text, _h2, _u2) = chain_gemini(
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
        )
        .unwrap();
        let (_t1, chain1, _u1) =
            observe_event(&state, &priorities, &crate::state::CoachChain::Empty, &event1)
                .await
                .expect("first observe_event failed");
        let h1 = match &chain1 {
            crate::state::CoachChain::Google { history } => history.clone(),
            other => panic!("expected Google chain, got {other:?}"),
        };
        assert_eq!(h1.len(), 2, "first call should produce user+assistant pair");

        let event2 = build_observer_event(
            "Bash",
            &serde_json::json!({"command": "python -c 'from x import add; print(add(2,3))'"}),
        )
        .unwrap();
        let (_t2, chain2, _u2) = observe_event(&state, &priorities, &chain1, &event2)
            .await
            .expect("second observe_event failed");
        let h2 = match &chain2 {
            crate::state::CoachChain::Google { history } => history.clone(),
            other => panic!("expected Google chain, got {other:?}"),
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
        )
        .unwrap();
        let (_t, observed_chain, _u_obs) =
            observe_event(&state, &priorities, &crate::state::CoachChain::Empty, &event)
                .await
                .expect("observe failed");

        let (decision, new_chain, _u_stop) = evaluate_stop_chained(
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
        assert!(matches!(new_chain, crate::state::CoachChain::Google { .. }));
    }
}
