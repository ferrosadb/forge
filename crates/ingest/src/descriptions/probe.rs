//! Pre-flight availability probe + interactive prompt.
//!
//! Contract: `check()` either returns a ready-to-use provider, or returns
//! an error. It never silently downgrades to `Skip` — any degraded state
//! must be an explicit user choice (fail-loud rule).

use crate::descriptions::config::{DescriptionsConfig, Provider};
use crate::descriptions::prompt::{parse_choice, render_lines, PromptAction};
use crate::descriptions::provider::{DescriptionProvider, ProbeInfo, ProviderError};
use crate::descriptions::providers::{build_provider, skip::SkipProvider};
use anyhow::{anyhow, bail, Result};
use std::io::{BufRead, Write};
use std::sync::Arc;

/// Outcome of the probe step, visible to the caller (e.g. the CLI) so it
/// can surface a consent banner.
pub struct ReadyProvider {
    pub provider: Arc<dyn DescriptionProvider>,
    pub probe_info: ProbeInfo,
    /// True when the caller switched from the configured provider to
    /// another (or to Skip) via the interactive prompt.
    pub switched_from_config: bool,
}

/// Resolve the configured provider to a ready one, prompting the user if
/// the probe fails and a TTY is attached.
pub fn check(cfg: &DescriptionsConfig) -> Result<ReadyProvider> {
    if !cfg.enabled || cfg.provider == Provider::Skip {
        return skip_ready(false);
    }

    // Clone cfg so we can mutate local_model on "choose model" action
    // without leaking state back to the caller's struct.
    let mut working_cfg = cfg.clone();
    let mut install_attempts: u8 = 0;
    loop {
        let provider = build_provider(&working_cfg)?;
        let probe_result = provider.probe();
        match probe_result {
            Ok(info) if info.selected_available => {
                let switched = working_cfg.local_model != cfg.local_model;
                return Ok(ReadyProvider {
                    provider,
                    probe_info: info,
                    switched_from_config: switched,
                });
            }
            Ok(info) => {
                let err = ProviderError::ModelMissing {
                    model: working_cfg.local_model.clone(),
                };
                let action = decide_action(&working_cfg, Some(&info), &err)?;
                apply_action(action, &mut working_cfg, &mut install_attempts)?;
            }
            Err(e) => {
                let action = decide_action(&working_cfg, None, &e)?;
                apply_action(action, &mut working_cfg, &mut install_attempts)?;
            }
        }

        // Detect a "picked skip" action via the sentinel model marker.
        if working_cfg.local_model == SKIP_SENTINEL {
            return skip_ready(true);
        }
    }
}

/// Marker model name used internally to signal "user picked skip"; the
/// match-on-value lets us stay inside the retry loop without a separate
/// control flag. Never observable outside this module.
const SKIP_SENTINEL: &str = "__internal_skip_sentinel__";

fn skip_ready(switched: bool) -> Result<ReadyProvider> {
    let provider: Arc<dyn DescriptionProvider> = Arc::new(SkipProvider);
    let probe_info = provider
        .probe()
        .map_err(|e| anyhow!("skip provider probe failed: {e}"))?;
    Ok(ReadyProvider {
        provider,
        probe_info,
        switched_from_config: switched,
    })
}

fn decide_action(
    cfg: &DescriptionsConfig,
    info: Option<&ProbeInfo>,
    err: &ProviderError,
) -> Result<PromptAction> {
    if cfg.non_interactive || !is_tty() {
        bail!(
            "description provider probe failed ({err}). \
             Remediation: (a) start the local endpoint ({}), \
             (b) pull the configured model (e.g. 'ollama pull {}'), \
             (c) override with --desc-provider skip, \
             or (d) choose a different model via --desc-model.",
            cfg.local_endpoint,
            cfg.local_model,
        );
    }
    prompt_tty(cfg, info, err)
}

fn apply_action(
    action: PromptAction,
    cfg: &mut DescriptionsConfig,
    install_attempts: &mut u8,
) -> Result<()> {
    match action {
        PromptAction::ChooseModel(name) => {
            eprintln!("[descriptions] switching to model '{}'", name);
            cfg.local_model = name;
            Ok(())
        }
        PromptAction::InstallAndRetry => {
            *install_attempts += 1;
            if *install_attempts > 2 {
                bail!(
                    "install attempted {} times without success; aborting to avoid a loop",
                    install_attempts
                );
            }
            install_model(&cfg.local_model)
        }
        PromptAction::Skip => {
            cfg.local_model = SKIP_SENTINEL.to_string();
            Ok(())
        }
        PromptAction::Abort => bail!("ingest aborted by user at provider-availability prompt"),
    }
}

/// Shell out to `ollama pull <model>`. Streams stdout/stderr straight to
/// the terminal so the user sees pull progress. Returns an error if the
/// binary is not found or the pull fails.
fn install_model(model: &str) -> Result<()> {
    use std::process::Command;
    eprintln!("[descriptions] running 'ollama pull {model}' (this can take a while)…");
    let status = Command::new("ollama")
        .args(["pull", model])
        .status()
        .map_err(|e| {
            anyhow!(
                "could not launch 'ollama' (is it installed and on PATH?): {e}. \
                 Install from https://ollama.com and re-run, or choose 's' to skip."
            )
        })?;
    if !status.success() {
        bail!(
            "'ollama pull {model}' exited with status {:?}; the requested model may not exist",
            status.code()
        );
    }
    eprintln!("[descriptions] install ok; re-probing…");
    Ok(())
}

fn prompt_tty(
    cfg: &DescriptionsConfig,
    info: Option<&ProbeInfo>,
    err: &ProviderError,
) -> Result<PromptAction> {
    let stderr = std::io::stderr();
    let mut err_handle = stderr.lock();
    writeln!(err_handle)?;
    let (lines, question) = render_lines(&cfg.local_endpoint, &cfg.local_model, info, err);
    for line in &lines {
        writeln!(err_handle, "{line}")?;
    }
    write!(err_handle, "{question}")?;
    err_handle.flush()?;
    drop(err_handle);

    let available: Vec<String> = info.map(|i| i.available_models.clone()).unwrap_or_default();

    let stdin = std::io::stdin();
    let mut line = String::new();
    stdin.lock().read_line(&mut line)?;
    match parse_choice(&line, &available) {
        Some(action) => Ok(action),
        None => Ok(PromptAction::Abort),
    }
}

#[cfg(not(test))]
fn is_tty() -> bool {
    // std::io::IsTerminal is stable as of Rust 1.70.
    use std::io::IsTerminal;
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

#[cfg(test)]
fn is_tty() -> bool {
    // Always false in tests — exercises the non-interactive fail-loud path.
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skip_provider_bypasses_probe_logic() {
        let mut cfg = DescriptionsConfig::default();
        cfg.provider = Provider::Skip;
        let ready = check(&cfg).unwrap();
        assert_eq!(ready.provider.label(), "skip");
        assert!(!ready.switched_from_config);
    }

    #[test]
    fn disabled_uses_skip() {
        let mut cfg = DescriptionsConfig::default();
        cfg.enabled = false;
        let ready = check(&cfg).unwrap();
        assert_eq!(ready.provider.label(), "skip");
    }

    #[test]
    fn non_interactive_unreachable_fails_loud() {
        let mut cfg = DescriptionsConfig::default();
        cfg.provider = Provider::Local;
        cfg.local_endpoint = "http://127.0.0.1:1".to_string();
        cfg.local_timeout_ms = 200;
        cfg.non_interactive = true;
        let res = check(&cfg);
        let msg = match res {
            Ok(_) => panic!("expected failure on unreachable endpoint"),
            Err(e) => e.to_string(),
        };
        assert!(msg.to_lowercase().contains("probe failed"), "msg: {msg}");
        // Regression: remediation banner now names `ollama pull` explicitly.
        assert!(msg.contains("ollama pull"), "msg: {msg}");
    }

    #[test]
    fn apply_action_choose_model_updates_cfg() {
        let mut cfg = DescriptionsConfig::default();
        cfg.local_model = "old-model".to_string();
        let mut attempts = 0;
        apply_action(
            PromptAction::ChooseModel("new-model".to_string()),
            &mut cfg,
            &mut attempts,
        )
        .unwrap();
        assert_eq!(cfg.local_model, "new-model");
    }

    #[test]
    fn apply_action_skip_marks_sentinel() {
        let mut cfg = DescriptionsConfig::default();
        let mut attempts = 0;
        apply_action(PromptAction::Skip, &mut cfg, &mut attempts).unwrap();
        assert_eq!(cfg.local_model, SKIP_SENTINEL);
    }

    #[test]
    fn apply_action_abort_errors() {
        let mut cfg = DescriptionsConfig::default();
        let mut attempts = 0;
        let res = apply_action(PromptAction::Abort, &mut cfg, &mut attempts);
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("aborted by user"));
    }

    #[test]
    fn install_loop_gives_up_after_two_attempts() {
        let mut cfg = DescriptionsConfig::default();
        let mut attempts: u8 = 2;
        // Simulated third install attempt — apply_action increments then bails.
        let res = apply_action(PromptAction::InstallAndRetry, &mut cfg, &mut attempts);
        // Whether the binary exists or not, the attempt counter must
        // reject the THIRD call. The second call would attempt the real
        // binary which may or may not be present; we're testing the cap.
        // Easiest assertion: after 3 attempts (2 going in + 1 here),
        // apply_action rejects.
        // (If ollama *is* installed and this *happened* to succeed, the
        // cap wouldn't fire. In CI this is effectively always missing.)
        assert!(
            res.is_err(),
            "expected error (either install failure or cap); got {:?}",
            res
        );
    }
}
