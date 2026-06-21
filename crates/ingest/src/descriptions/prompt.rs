//! Pure prompt parsing + rendering for the probe-failure UI.
//!
//! The actual I/O (stdin/stdout) lives in `probe.rs`; this module is
//! pure so tests can exhaustively cover choice parsing and line
//! rendering without driving a terminal.

use crate::descriptions::provider::{ProbeInfo, ProviderError};

/// Action the user chose at the availability prompt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PromptAction {
    /// Use the named model instead of the configured one.
    ChooseModel(String),
    /// Attempt to install the configured model (e.g. via `ollama pull`)
    /// and re-probe.
    InstallAndRetry,
    /// Proceed without description extraction for this run.
    Skip,
    /// Abort ingest entirely.
    Abort,
}

/// Parse user input into a `PromptAction`. Returns `None` for unrecognized
/// input; the caller decides whether to re-prompt or fall back.
pub fn parse_choice(input: &str, available_models: &[String]) -> Option<PromptAction> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Letter commands (case-insensitive) take precedence.
    match trimmed.to_ascii_lowercase().as_str() {
        "i" | "install" => return Some(PromptAction::InstallAndRetry),
        "s" | "skip" => return Some(PromptAction::Skip),
        "a" | "abort" | "q" | "quit" => return Some(PromptAction::Abort),
        _ => {}
    }
    // Numeric choice indexing into available_models (1-based).
    if let Ok(n) = trimmed.parse::<usize>() {
        if n >= 1 && n <= available_models.len() {
            return Some(PromptAction::ChooseModel(available_models[n - 1].clone()));
        }
    }
    None
}

/// Render the prompt body lines (caller adds blank lines / colouring).
/// Returns `(body_lines, question)` where `question` is the last-line
/// prompt (e.g. "Choice [1-3/i/s/a]: ").
pub fn render_lines(
    endpoint: &str,
    model: &str,
    info: Option<&ProbeInfo>,
    err: &ProviderError,
) -> (Vec<String>, String) {
    let mut lines = Vec::new();
    lines.push("[frg ingest] description provider unavailable".to_string());
    lines.push(format!("  error: {err}"));
    lines.push(format!("  endpoint: {endpoint}"));
    lines.push(format!("  model: {model}"));

    let available: &[String] = info.map(|i| i.available_models.as_slice()).unwrap_or(&[]);
    lines.push(String::new());
    lines.push("Options:".to_string());
    if available.is_empty() {
        lines.push("  (no models reported by endpoint)".to_string());
    } else {
        for (idx, m) in available.iter().enumerate() {
            lines.push(format!("  [{}] use '{}'", idx + 1, m));
        }
    }
    lines.push(format!(
        "  [i] install '{model}' via 'ollama pull {model}' (runs now)"
    ));
    lines.push("  [s] skip description extraction for this run".to_string());
    lines.push("  [a] abort ingest".to_string());

    let question = if available.is_empty() {
        "Choice [i/s/a]: ".to_string()
    } else {
        format!("Choice [1-{}/i/s/a]: ", available.len())
    };
    (lines, question)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn models(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_numeric_picks_model() {
        let m = models(&["llama3:latest", "qwen:7b"]);
        assert_eq!(
            parse_choice("1", &m),
            Some(PromptAction::ChooseModel("llama3:latest".into()))
        );
        assert_eq!(
            parse_choice("2", &m),
            Some(PromptAction::ChooseModel("qwen:7b".into()))
        );
    }

    #[test]
    fn parse_numeric_out_of_range_is_none() {
        let m = models(&["a"]);
        assert_eq!(parse_choice("2", &m), None);
        assert_eq!(parse_choice("0", &m), None);
    }

    #[test]
    fn parse_install_case_insensitive() {
        assert_eq!(parse_choice("i", &[]), Some(PromptAction::InstallAndRetry));
        assert_eq!(parse_choice("I", &[]), Some(PromptAction::InstallAndRetry));
        assert_eq!(
            parse_choice("install", &[]),
            Some(PromptAction::InstallAndRetry)
        );
    }

    #[test]
    fn parse_skip_and_abort() {
        assert_eq!(parse_choice("s", &[]), Some(PromptAction::Skip));
        assert_eq!(parse_choice("SKIP", &[]), Some(PromptAction::Skip));
        assert_eq!(parse_choice("a", &[]), Some(PromptAction::Abort));
        assert_eq!(parse_choice("abort", &[]), Some(PromptAction::Abort));
        assert_eq!(parse_choice("q", &[]), Some(PromptAction::Abort));
    }

    #[test]
    fn parse_empty_and_garbage_returns_none() {
        assert_eq!(parse_choice("", &[]), None);
        assert_eq!(parse_choice("  ", &[]), None);
        assert_eq!(parse_choice("wat", &[]), None);
        assert_eq!(parse_choice("12345", &models(&["a"])), None);
    }

    #[test]
    fn letter_shortcut_beats_numeric_coincidence() {
        // "1install" is garbage — must not be interpreted as either.
        assert_eq!(parse_choice("1install", &models(&["a", "b"])), None);
    }

    #[test]
    fn render_lists_available_models_numbered() {
        let info = ProbeInfo {
            provider_label: "local-ollama".into(),
            model: "qwen:7b".into(),
            available_models: vec!["llama3:latest".into(), "qwen:7b-chat".into()],
            selected_available: false,
        };
        let err = ProviderError::ModelMissing {
            model: "qwen:7b".into(),
        };
        let (lines, q) = render_lines("http://localhost:11434", "qwen:7b", Some(&info), &err);
        let joined = lines.join("\n");
        assert!(joined.contains("[1] use 'llama3:latest'"), "{joined}");
        assert!(joined.contains("[2] use 'qwen:7b-chat'"), "{joined}");
        assert!(joined.contains("[i] install 'qwen:7b'"), "{joined}");
        assert!(joined.contains("[s] skip"), "{joined}");
        assert!(joined.contains("[a] abort"), "{joined}");
        assert_eq!(q, "Choice [1-2/i/s/a]: ");
    }

    #[test]
    fn render_when_no_available_models_hides_numeric() {
        let err = ProviderError::Unreachable("refused".into());
        let (lines, q) = render_lines("http://localhost:11434", "qwen:7b", None, &err);
        let joined = lines.join("\n");
        assert!(joined.contains("(no models reported"), "{joined}");
        assert!(joined.contains("[i] install"), "{joined}");
        assert!(joined.contains("[s] skip"), "{joined}");
        assert!(joined.contains("[a] abort"), "{joined}");
        assert_eq!(q, "Choice [i/s/a]: ");
    }

    #[test]
    fn render_surfaces_error_and_endpoint() {
        let err = ProviderError::ModelMissing {
            model: "qwen:7b".into(),
        };
        let (lines, _) = render_lines("http://127.0.0.1:11434", "qwen:7b", None, &err);
        let joined = lines.join("\n");
        assert!(joined.contains("127.0.0.1:11434"));
        assert!(joined.to_lowercase().contains("not available"));
    }
}
