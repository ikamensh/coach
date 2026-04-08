//! Externalized model-facing prompts.
//!
//! Every prompt the coach sends to an LLM lives in `src-tauri/prompts/*.txt`.
//! Each template is also embedded into the binary via `include_str!` so the
//! shipped app is self-contained — there is no install-time prompts directory.
//!
//! ## Tight-loop iteration
//!
//! Set `COACH_PROMPTS_DIR=/path/to/coach/src-tauri/prompts` and every call
//! reads the matching `.txt` file fresh from disk. Edit the file, fire the
//! next hook, see the new prompt — no recompile. With the env var unset, the
//! embedded defaults are used.
//!
//! There is intentionally no fallback when the env var is set: a missing or
//! unreadable file surfaces as a clear error rather than silently reverting
//! to the embedded copy. This is the behavior you want during iteration —
//! you want to know your edit was actually picked up.
//!
//! ## Template syntax
//!
//! `{name}` placeholders are substituted by [`render`]. Substitution is
//! literal `String::replace`, applied once per (key, value) pair in the
//! order given. Unknown braces (e.g. JSON examples in the body of a prompt)
//! pass through untouched as long as no provided key matches.

use std::path::Path;

/// All embedded prompt templates, keyed by short name. The name is also the
/// `.txt` filename under `COACH_PROMPTS_DIR` when the override is active.
const EMBEDDED: &[(&str, &str)] = &[
    ("coach_system", include_str!("../prompts/coach_system.txt")),
    ("observer_event", include_str!("../prompts/observer_event.txt")),
    ("stop_oneshot", include_str!("../prompts/stop_oneshot.txt")),
    ("stop_chained", include_str!("../prompts/stop_chained.txt")),
    (
        "name_session_user",
        include_str!("../prompts/name_session_user.txt"),
    ),
    (
        "name_session_system",
        include_str!("../prompts/name_session_system.txt"),
    ),
];

/// Look up a prompt template by name. If `COACH_PROMPTS_DIR` is set, reads
/// `$COACH_PROMPTS_DIR/<name>.txt` fresh on every call (so editing the file
/// affects the very next request, no restart needed). Otherwise returns the
/// embedded copy.
///
/// Panics-by-design: an unknown name with no override is a programming error,
/// not a user error, so it crashes immediately. A missing file when the env
/// var IS set returns `Err` with the path so the caller can surface it.
pub fn load(name: &str) -> Result<String, String> {
    let dir = std::env::var("COACH_PROMPTS_DIR").ok();
    load_from(name, dir.as_deref().map(Path::new))
}

/// Pure version of [`load`] that takes the override directory explicitly.
/// Tests use this so they never have to mutate process-wide env state.
fn load_from(name: &str, override_dir: Option<&Path>) -> Result<String, String> {
    if let Some(dir) = override_dir {
        let path = dir.join(format!("{name}.txt"));
        return std::fs::read_to_string(&path)
            .map_err(|e| format!("prompts: failed to read {}: {e}", path.display()));
    }
    let embedded = EMBEDDED
        .iter()
        .find(|(k, _)| *k == name)
        .map(|(_, v)| *v)
        .unwrap_or_else(|| panic!("prompts: unknown template {name:?}"));
    Ok(embedded.to_string())
}

/// Substitute `{key}` placeholders in `template` with the given values.
/// Each `(key, value)` is applied as a literal `replace`, in order. Unknown
/// braces pass through untouched.
pub fn render(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    for (k, v) in vars {
        let needle = format!("{{{k}}}");
        out = out.replace(&needle, v);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every name listed in EMBEDDED must point at a non-empty string.
    /// Catches accidentally creating an entry for a file that doesn't exist
    /// (which would have been a compile error already, but also catches an
    /// empty file slipping into the repo).
    #[test]
    fn embedded_templates_are_non_empty() {
        for (name, body) in EMBEDDED {
            assert!(!body.trim().is_empty(), "{name} is empty");
        }
    }

    /// With no override directory, every embedded template loads cleanly
    /// and contains a recognizable substring of the original prompt body.
    /// Catches an EMBEDDED entry whose include_str path drifts away from
    /// the actual file content.
    #[test]
    fn load_from_returns_embedded_when_no_override() {
        let s = load_from("coach_system", None).unwrap();
        assert!(s.contains("Coach"));
        assert!(s.contains("{priorities}"));
    }

    /// Loading from a real on-disk override directory picks up the file's
    /// contents instead of the embedded default — this is the iteration
    /// loop in action.
    #[test]
    fn load_from_reads_override_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("coach_system.txt"), "OVERRIDE {priorities}").unwrap();
        let s = load_from("coach_system", Some(dir.path())).unwrap();
        assert_eq!(s, "OVERRIDE {priorities}");
    }

    /// A missing file under the override dir surfaces as a clear error
    /// containing the path — no silent fallback to the embedded copy, so
    /// you immediately see when an edit landed in the wrong place.
    #[test]
    fn load_from_errors_when_override_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let err = load_from("coach_system", Some(dir.path())).unwrap_err();
        assert!(err.contains("coach_system.txt"));
    }

    /// `render` substitutes every supplied key and leaves unknown braces
    /// (e.g. a JSON example) alone.
    #[test]
    fn render_substitutes_known_and_preserves_unknown() {
        let tpl = "Hi {name}, here is JSON: {\"k\": 1}";
        let out = render(tpl, &[("name", "world")]);
        assert_eq!(out, "Hi world, here is JSON: {\"k\": 1}");
    }

    /// Multiple substitutions in one template all apply.
    #[test]
    fn render_handles_multiple_keys() {
        let out = render("{a}-{b}-{a}", &[("a", "1"), ("b", "2")]);
        assert_eq!(out, "1-2-1");
    }
}
