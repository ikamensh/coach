//! Externalized model-facing prompts.
//!
//! Every prompt the coach sends to an LLM lives in `coach-core/prompts/*.txt`.
//! Each template is also embedded into the binary via `include_str!` so the
//! shipped app is self-contained.
//!
//! ## Tight-loop iteration
//!
//! Debug builds read each template fresh from disk on every call, from the
//! same path the release build `include_str!`s from. Edit a file, fire the
//! next hook, see the new prompt — no recompile. Release builds always use
//! the embedded copies.
//!
//! A missing or unreadable file in debug mode surfaces as a clear error
//! rather than silently reverting to the embedded copy: during iteration
//! you want to know your edit was actually picked up.
//!
//! ## Template syntax
//!
//! `{name}` placeholders are substituted by [`render`]. Substitution is
//! literal `String::replace`, applied once per (key, value) pair in the
//! order given. Unknown braces (e.g. JSON examples in the body of a prompt)
//! pass through untouched as long as no provided key matches.

use std::path::Path;
#[cfg(debug_assertions)]
use std::path::PathBuf;

/// All embedded prompt templates, keyed by short name. The name matches the
/// `.txt` filename under `coach-core/prompts/`.
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

/// Canonical on-disk location of the prompts, resolved at compile time from
/// `CARGO_MANIFEST_DIR`. Only used in debug builds — release ships the
/// embedded copies.
#[cfg(debug_assertions)]
fn prompts_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("prompts")
}

/// Look up a prompt template by name. In debug builds reads
/// `<crate>/prompts/<name>.txt` fresh on every call so prompt edits take
/// effect on the next request. In release builds returns the embedded copy.
///
/// Panics-by-design: an unknown name in a release build is a programming
/// error, not a user error, so it crashes immediately. A missing file in a
/// debug build returns `Err` with the path so the caller can surface it.
pub fn load(name: &str) -> Result<String, String> {
    #[cfg(debug_assertions)]
    {
        load_from(name, Some(&prompts_dir()))
    }
    #[cfg(not(debug_assertions))]
    {
        load_from(name, None)
    }
}

/// Pure version of [`load`] parameterized on the override directory. Tests
/// use this to exercise both branches without touching `cfg` or the real
/// on-disk path.
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
        assert!(s.contains("observe"));
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

    /// In debug builds, `load` should resolve the canonical on-disk path
    /// and return non-empty content for every embedded template name. This
    /// is the "does the debug path actually work" smoke test.
    #[cfg(debug_assertions)]
    #[test]
    fn load_resolves_on_disk_for_every_name_in_debug() {
        for (name, _) in EMBEDDED {
            let s = load(name).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(!s.trim().is_empty(), "{name} came back empty from disk");
        }
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
