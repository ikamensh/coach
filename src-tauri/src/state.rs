use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;

use crate::settings::{ModelConfig, Settings};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum CoachMode {
    Present,
    Away,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    Light,
    Dark,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityEntry {
    pub timestamp: DateTime<Utc>,
    pub session_id: String,
    pub hook_event: String,
    pub action: String,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub session_id: String,
    pub mode: CoachMode,
    pub cwd: Option<String>,
    pub last_event: DateTime<Utc>,
    pub event_count: usize,
}

/// Per-provider token status sent to the frontend.
/// "source" is "user", "env", or "none".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenStatus {
    pub source: String,
    /// Which env var was matched (only set when source == "env").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_var: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoachSnapshot {
    pub sessions: Vec<SessionSnapshot>,
    pub priorities: Vec<String>,
    pub activity_log: Vec<ActivityEntry>,
    pub port: u16,
    pub theme: Theme,
    pub model: ModelConfig,
    pub token_status: HashMap<String, TokenStatus>,
}

pub struct SessionState {
    pub mode: CoachMode,
    pub cwd: Option<String>,
    pub last_event: Instant,
    pub last_event_time: DateTime<Utc>,
    pub event_count: usize,
    pub last_stop_blocked: Option<Instant>,
}

pub struct CoachState {
    pub sessions: HashMap<String, SessionState>,
    pub priorities: Vec<String>,
    pub activity_log: Vec<ActivityEntry>,
    pub port: u16,
    pub theme: Theme,
    pub default_mode: CoachMode,
    pub model: ModelConfig,
    /// User-configured tokens (from settings file).
    pub api_tokens: HashMap<String, String>,
    /// Tokens detected from environment at startup (read-only).
    pub env_tokens: HashMap<String, String>,
}

impl CoachState {
    /// Resolve the effective token for a provider: user override wins, then env.
    pub fn effective_token(&self, provider: &str) -> Option<&str> {
        self.api_tokens
            .get(provider)
            .filter(|v| !v.is_empty())
            .or_else(|| self.env_tokens.get(provider))
            .map(|s| s.as_str())
    }
}

const MAX_LOG_ENTRIES: usize = 200;
const SESSION_TTL_SECS: u64 = 3600;

impl CoachState {
    pub fn from_settings(settings: Settings) -> Self {
        Self {
            sessions: HashMap::new(),
            priorities: settings.priorities,
            activity_log: Vec::new(),
            port: settings.port,
            theme: settings.theme,
            default_mode: CoachMode::Present,
            model: settings.model,
            api_tokens: settings.api_tokens,
            env_tokens: crate::settings::env_tokens(),
        }
    }

    pub fn to_settings(&self) -> Settings {
        Settings {
            api_tokens: self.api_tokens.clone(),
            model: self.model.clone(),
            priorities: self.priorities.clone(),
            theme: self.theme.clone(),
            port: self.port,
        }
    }

    pub fn save(&self) {
        self.to_settings().save();
    }

    /// Get or create a session, returning a mutable reference.
    pub fn session(&mut self, session_id: &str, cwd: Option<&str>) -> &mut SessionState {
        self.prune_stale();
        let default_mode = self.default_mode.clone();
        self.sessions
            .entry(session_id.to_string())
            .and_modify(|s| {
                s.last_event = Instant::now();
                s.last_event_time = Utc::now();
                s.event_count += 1;
                if let Some(cwd) = cwd {
                    s.cwd = Some(cwd.to_string());
                }
            })
            .or_insert_with(|| SessionState {
                mode: default_mode,
                cwd: cwd.map(String::from),
                last_event: Instant::now(),
                last_event_time: Utc::now(),
                event_count: 1,
                last_stop_blocked: None,
            })
    }

    /// Snapshot for the frontend. Tokens are masked (true = set, false = empty).
    pub fn snapshot(&self) -> CoachSnapshot {
        let mut sessions: Vec<SessionSnapshot> = self
            .sessions
            .iter()
            .map(|(id, s)| SessionSnapshot {
                session_id: id.clone(),
                mode: s.mode.clone(),
                cwd: s.cwd.clone(),
                last_event: s.last_event_time,
                event_count: s.event_count,
            })
            .collect();
        sessions.sort_by(|a, b| b.last_event.cmp(&a.last_event));

        CoachSnapshot {
            sessions,
            priorities: self.priorities.clone(),
            activity_log: self.activity_log.clone(),
            port: self.port,
            theme: self.theme.clone(),
            model: self.model.clone(),
            token_status: {
                let mut status = HashMap::new();
                for (provider, vars) in crate::settings::PROVIDER_ENV_VARS {
                    let has_user = self.api_tokens.get(*provider).is_some_and(|v| !v.is_empty());
                    let env_var_found = if !has_user {
                        vars.iter().find(|var| {
                            std::env::var(var).ok().is_some_and(|v| !v.is_empty())
                        }).map(|v| v.to_string())
                    } else {
                        None
                    };
                    let (source, env_var) = if has_user {
                        ("user", None)
                    } else if let Some(var) = env_var_found {
                        ("env", Some(var))
                    } else {
                        ("none", None)
                    };
                    status.insert(provider.to_string(), TokenStatus {
                        source: source.into(),
                        env_var,
                    });
                }
                status
            },
        }
    }

    pub fn log(
        &mut self,
        session_id: &str,
        hook_event: &str,
        action: &str,
        detail: Option<String>,
    ) {
        self.activity_log.push(ActivityEntry {
            timestamp: Utc::now(),
            session_id: session_id.to_string(),
            hook_event: hook_event.to_string(),
            action: action.to_string(),
            detail,
        });
        if self.activity_log.len() > MAX_LOG_ENTRIES {
            self.activity_log
                .drain(..self.activity_log.len() - MAX_LOG_ENTRIES);
        }
    }

    fn prune_stale(&mut self) {
        self.sessions
            .retain(|_, s| s.last_event.elapsed().as_secs() < SESSION_TTL_SECS);
    }
}

pub type SharedState = Arc<RwLock<CoachState>>;
