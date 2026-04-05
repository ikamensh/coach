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
use serde::{de::DeserializeOwned, Serialize};

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
}
