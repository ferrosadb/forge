//! CLI handler for `frg fmem-skill-ingest`.
//!
//! Glues the clap surface to the orchestrator in
//! `forge-ingest::skill_ingest::run`. Responsible for:
//!
//! - Parsing the `--server` command string into an argv vector
//! - Spawning the `StdioTransport` (skipped under `--dry-run`)
//! - Running `initialize` under Strict protocol-version mode
//! - Invoking `skill_ingest::run` with the assembled `RunConfig`
//! - Pretty-printing a summary (P27 verbose + buckets)
//! - Mapping the `Summary::exit_code()` to the process exit

use std::path::PathBuf;

use anyhow::Result;
use forge_fmem_client::transport::stdio::StdioConfig;
use forge_fmem_client::{initialize, ExpectedProtocolVersion, StdioTransport};
use forge_ingest::skill_ingest::{self, RunConfig, Summary, VerifyFailure, VerifyFailureReason};

/// MCP-tool entry point — same orchestration but returns the JSON
/// summary instead of writing to stdout + exiting. Intended for
/// callers that speak MCP to forge (e.g. Claude invoking the tool
/// directly).
#[allow(clippy::too_many_arguments)]
pub fn run_as_mcp_tool(
    root: PathBuf,
    filter: Option<String>,
    dry_run: bool,
    session: Option<String>,
    force: bool,
    server: Option<String>,
    verbose: bool,
) -> Result<serde_json::Value> {
    let summary = execute_run(root, filter, dry_run, session, force, server)?;
    Ok(summary_json(&summary, verbose))
}

/// Entry point called from the `Commands::FmemSkillIngest` match arm.
#[allow(clippy::too_many_arguments)]
pub fn run_fmem_skill_ingest(
    root: PathBuf,
    filter: Option<String>,
    dry_run: bool,
    session: Option<String>,
    force: bool,
    server: Option<String>,
    verbose: bool,
    pretty: bool,
) -> Result<i32> {
    let summary = execute_run(root, filter, dry_run, session, force, server)?;
    emit_summary(&summary, verbose, pretty)?;
    Ok(summary.exit_code())
}

/// Shared execution path — spawn (or fake) the transport, run the
/// four-phase pipeline, return the `Summary`. Callers decide how to
/// surface it (stderr + stdout for CLI vs. JSON for MCP).
fn execute_run(
    root: PathBuf,
    filter: Option<String>,
    dry_run: bool,
    session: Option<String>,
    force: bool,
    server: Option<String>,
) -> Result<Summary> {
    let config = RunConfig {
        root,
        filter,
        dry_run,
        force,
        session_id: session,
    };

    let summary = if dry_run {
        // FMEA F18 — never spawn the subprocess. Use a transport that
        // panics on any call so the orchestrator's dry-run short-circuit
        // is enforced at runtime, not just by convention.
        use forge_fmem_client::MockTransport;
        let dry = MockTransport::panicking();
        skill_ingest::run(config, &dry).map_err(|e| anyhow::anyhow!("{e}"))?
    } else {
        let argv = parse_server_command(server.as_deref());
        let stdio_config = StdioConfig {
            command: argv,
            ..Default::default()
        };
        let transport =
            StdioTransport::spawn(stdio_config).map_err(|e| anyhow::anyhow!("spawn fmem: {e}"))?;
        // Best-effort handshake — strict protocol version. Fail loud if
        // the server advertises a version forge doesn't recognize.
        initialize(&transport, ExpectedProtocolVersion::Strict)
            .map_err(|e| anyhow::anyhow!("fmem initialize: {e}"))?;
        skill_ingest::run(config, &transport).map_err(|e| anyhow::anyhow!("{e}"))?
    };

    Ok(summary)
}

/// Parse `--server "fmem --mcp"` into `["fmem", "--mcp"]`. A naive
/// whitespace split is sufficient here because the server command is
/// operator-controlled and doesn't need shell-grade quoting.
fn parse_server_command(spec: Option<&str>) -> Vec<String> {
    match spec {
        Some(s) if !s.trim().is_empty() => s.split_whitespace().map(str::to_string).collect(),
        _ => vec!["fmem".to_string(), "--mcp".to_string()],
    }
}

/// Pretty-print the summary to stderr and emit a JSON digest on stdout.
/// The JSON is what scripts consume; the stderr block is for humans.
fn emit_summary(summary: &Summary, verbose: bool, pretty: bool) -> Result<()> {
    eprintln!("[skill-ingest] --- summary ---");
    eprintln!(
        "[skill-ingest] taxonomy edges: {} created, {} skipped, {} failed",
        summary.taxonomy_edges_created,
        summary.taxonomy_edges_skipped,
        summary.taxonomy_edges_failed,
    );
    eprintln!(
        "[skill-ingest] skills: {} created, {} updated, {} skipped (unchanged), {} failed, {} filtered out",
        summary.skills_created,
        summary.skills_updated,
        summary.skills_skipped_unchanged,
        summary.skills_failed,
        summary.skills_filtered_out,
    );
    if summary.repass_attempts > 0 {
        eprintln!(
            "[skill-ingest] phase C re-pass: {}/{} completed",
            summary.repass_completed, summary.repass_attempts
        );
    }
    eprintln!(
        "[skill-ingest] verified: {}; verification failures: {}",
        summary.verified,
        summary.verification_failures.len(),
    );
    eprintln!("[skill-ingest] duration: {} ms", summary.duration_ms);

    if verbose {
        for w in &summary.empty_steps_warnings {
            eprintln!("[skill-ingest] warn: {w}");
        }
    }
    for e in &summary.parse_errors {
        eprintln!("[skill-ingest] parse error: {e}");
    }
    for f in &summary.verification_failures {
        eprintln!("[skill-ingest] verify fail: {}", format_verify_failure(f));
    }

    let json_summary = summary_json(summary, verbose);
    println!("{}", forge_shared::emit_json(&json_summary, pretty)?);
    Ok(())
}

fn format_verify_failure(f: &VerifyFailure) -> String {
    match &f.reason {
        VerifyFailureReason::DoesNotExist => {
            format!("{}: skill not in fmem", f.skill)
        }
        VerifyFailureReason::MissingPrerequisites(names) => {
            format!(
                "{}: missing {} prereq edge(s) [{}]",
                f.skill,
                names.len(),
                names.join(", ")
            )
        }
        VerifyFailureReason::MissingTags { expected, actual } => {
            format!(
                "{}: expected tags {:?} but fmem has {:?}",
                f.skill, expected, actual
            )
        }
        VerifyFailureReason::TransportError(msg) => {
            format!("{}: transport error during verify: {}", f.skill, msg)
        }
    }
}

/// Serializable summary for stdout JSON. Separate from the core
/// `Summary` struct so we can compact it for script consumption
/// without leaking Rust-shaped internals.
fn summary_json(s: &Summary, verbose: bool) -> serde_json::Value {
    let failures: Vec<serde_json::Value> = s
        .verification_failures
        .iter()
        .map(|f| {
            serde_json::json!({
                "skill": f.skill,
                "reason": format_verify_failure(f),
            })
        })
        .collect();

    let mut out = serde_json::json!({
        "taxonomy": {
            "created": s.taxonomy_edges_created,
            "skipped": s.taxonomy_edges_skipped,
            "failed": s.taxonomy_edges_failed,
        },
        "skills": {
            "created": s.skills_created,
            "updated": s.skills_updated,
            "skipped_unchanged": s.skills_skipped_unchanged,
            "failed": s.skills_failed,
            "filtered_out": s.skills_filtered_out,
        },
        "repass": {
            "attempted": s.repass_attempts,
            "completed": s.repass_completed,
        },
        "verified": s.verified,
        "verification_failures": failures,
        "duration_ms": s.duration_ms,
        "exit_code": s.exit_code(),
    });

    if verbose {
        if let Some(obj) = out.as_object_mut() {
            obj.insert(
                "empty_steps_warnings".to_string(),
                serde_json::Value::from(s.empty_steps_warnings.clone()),
            );
            obj.insert(
                "parse_errors".to_string(),
                serde_json::Value::from(s.parse_errors.clone()),
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_server_none_defaults_to_fmem_mcp() {
        assert_eq!(
            parse_server_command(None),
            vec!["fmem".to_string(), "--mcp".to_string()]
        );
    }

    #[test]
    fn parse_server_empty_string_defaults() {
        assert_eq!(
            parse_server_command(Some("   ")),
            vec!["fmem".to_string(), "--mcp".to_string()]
        );
    }

    #[test]
    fn parse_server_splits_whitespace() {
        let argv = parse_server_command(Some("fmem --mcp --cluster test"));
        assert_eq!(argv, vec!["fmem", "--mcp", "--cluster", "test"]);
    }

    #[test]
    fn summary_json_shape_is_stable() {
        let s = Summary {
            skills_created: 3,
            skills_updated: 1,
            verified: 4,
            duration_ms: 123,
            ..Summary::default()
        };
        let j = summary_json(&s, false);
        assert_eq!(j["skills"]["created"], 3);
        assert_eq!(j["skills"]["updated"], 1);
        assert_eq!(j["verified"], 4);
        assert_eq!(j["duration_ms"], 123);
        assert_eq!(j["exit_code"], 0);
    }

    #[test]
    fn summary_json_verbose_adds_diagnostic_fields() {
        let s = Summary {
            empty_steps_warnings: vec!["x/SKILL.md".into()],
            parse_errors: vec!["y/SKILL.md: boom".into()],
            ..Summary::default()
        };
        let plain = summary_json(&s, false);
        assert!(plain.get("empty_steps_warnings").is_none());
        let verbose = summary_json(&s, true);
        assert!(verbose["empty_steps_warnings"].is_array());
        assert!(verbose["parse_errors"].is_array());
    }

    #[test]
    fn format_verify_failure_names_skill() {
        let f = VerifyFailure {
            skill: "tdd".into(),
            reason: VerifyFailureReason::DoesNotExist,
        };
        let s = format_verify_failure(&f);
        assert!(s.contains("tdd"));
        assert!(s.contains("not in fmem"));
    }

    #[test]
    fn format_verify_failure_lists_missing_prereqs() {
        let f = VerifyFailure {
            skill: "tdd".into(),
            reason: VerifyFailureReason::MissingPrerequisites(vec!["a".into(), "b".into()]),
        };
        let s = format_verify_failure(&f);
        assert!(s.contains("2 prereq"));
        assert!(s.contains("a, b"));
    }
}
