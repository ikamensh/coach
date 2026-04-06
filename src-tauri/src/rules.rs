//! Rule engine for pattern-based interventions.
//!
//! Each rule inspects PostToolUse payloads and returns an optional message
//! to inject via `additionalContext`.

/// An outdated model identifier and what to use instead.
struct ModelMapping {
    outdated: &'static str,
    suggestion: &'static str,
}

const OUTDATED_MODELS: &[ModelMapping] = &[
    // Google Gemini
    ModelMapping { outdated: "gemini-2.0-flash",   suggestion: "gemini-2.5-flash (stable) or gemini-3.0-flash" },
    ModelMapping { outdated: "gemini-2-flash",      suggestion: "gemini-2.5-flash (stable) or gemini-3.0-flash" },
    ModelMapping { outdated: "gemini-1.5-flash",    suggestion: "gemini-2.5-flash" },
    ModelMapping { outdated: "gemini-1.5-pro",      suggestion: "gemini-2.5-pro" },
    ModelMapping { outdated: "gemini-1.0",          suggestion: "gemini-2.5-flash" },
    // Anthropic Claude
    ModelMapping { outdated: "claude-3-5-sonnet",   suggestion: "claude-sonnet-4-6" },
    ModelMapping { outdated: "claude-3.5-sonnet",   suggestion: "claude-sonnet-4-6" },
    ModelMapping { outdated: "claude-3-opus",       suggestion: "claude-opus-4-6" },
    ModelMapping { outdated: "claude-3-sonnet",     suggestion: "claude-sonnet-4-6" },
    ModelMapping { outdated: "claude-3-haiku",      suggestion: "claude-haiku-4-5-20251001" },
    // OpenAI
    ModelMapping { outdated: "gpt-4o",              suggestion: "gpt-4.1 or gpt-5.4" },
    ModelMapping { outdated: "gpt-4-turbo",         suggestion: "gpt-4.1" },
    ModelMapping { outdated: "gpt-3.5",             suggestion: "gpt-4.1-mini" },
];

/// Check text content for outdated LLM model identifiers.
/// Returns Some(message) if any are found.
pub fn check_outdated_models(content: &str) -> Option<String> {
    let mut found: Vec<(&str, &str)> = Vec::new();

    for mapping in OUTDATED_MODELS {
        if content.contains(mapping.outdated) {
            // Avoid duplicate suggestions for similar patterns (e.g. gemini-2-flash and gemini-2.0-flash)
            if !found.iter().any(|(_, s)| *s == mapping.suggestion) {
                found.push((mapping.outdated, mapping.suggestion));
            }
        }
    }

    if found.is_empty() {
        return None;
    }

    let details: Vec<String> = found
        .iter()
        .map(|(old, new)| format!("'{}' -> {}", old, new))
        .collect();

    Some(format!(
        "Outdated model identifier detected: {}. Update to current versions.",
        details.join("; ")
    ))
}

/// Extract text content from a PostToolUse tool_input payload
/// that is worth checking for rule violations.
///
/// Returns the relevant text for Write (content), Edit (new_string),
/// and Bash (command) tools.
pub fn extract_checkable_text(tool_name: &str, tool_input: &serde_json::Value) -> Option<String> {
    match tool_name {
        "Write" | "write" => {
            tool_input.get("content").and_then(|v| v.as_str()).map(String::from)
        }
        "Edit" | "edit" => {
            tool_input.get("new_string").and_then(|v| v.as_str()).map(String::from)
        }
        "Bash" | "bash" => {
            tool_input.get("command").and_then(|v| v.as_str()).map(String::from)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_outdated_gemini_model() {
        let code = r#"client = genai.Client(model="gemini-2.0-flash")"#;
        let msg = check_outdated_models(code);
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("gemini-2.0-flash"));
    }

    #[test]
    fn detects_outdated_claude_model() {
        let code = r#"model: "claude-3-5-sonnet-20241022""#;
        let msg = check_outdated_models(code);
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("claude-3-5-sonnet"));
    }

    #[test]
    fn detects_outdated_gpt_model() {
        let code = r#"model="gpt-4o""#;
        let msg = check_outdated_models(code);
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("gpt-4o"));
    }

    #[test]
    fn passes_current_models() {
        let code = r#"
            model = "gemini-2.5-flash"
            other = "claude-sonnet-4-6"
            gpt = "gpt-4.1"
        "#;
        assert!(check_outdated_models(code).is_none());
    }

    #[test]
    fn multiple_outdated_in_one_file() {
        let code = r#"
            primary = "gemini-2.0-flash"
            fallback = "gpt-4o"
        "#;
        let msg = check_outdated_models(code).unwrap();
        assert!(msg.contains("gemini-2.0-flash"));
        assert!(msg.contains("gpt-4o"));
    }

    #[test]
    fn extract_write_content() {
        let input = serde_json::json!({"file_path": "/a.py", "content": "x = 1"});
        assert_eq!(extract_checkable_text("Write", &input), Some("x = 1".into()));
    }

    #[test]
    fn extract_edit_new_string() {
        let input = serde_json::json!({"old_string": "a", "new_string": "b"});
        assert_eq!(extract_checkable_text("Edit", &input), Some("b".into()));
    }

    #[test]
    fn extract_bash_command() {
        let input = serde_json::json!({"command": "echo hi"});
        assert_eq!(extract_checkable_text("Bash", &input), Some("echo hi".into()));
    }

    #[test]
    fn extract_unknown_tool_returns_none() {
        let input = serde_json::json!({"query": "test"});
        assert!(extract_checkable_text("Grep", &input).is_none());
    }
}
