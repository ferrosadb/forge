//! CI/CD tool wrappers: GitHub Actions via `gh` CLI — check, logs, list.

use crate::runner::*;
use serde::Serialize;

// ── ci check ───────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct CheckResult {
    pub status: String,
    pub workflow: String,
    pub run_id: String,
    pub branch: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_job: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_lines: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

pub fn check(owner_repo: &str, git_ref: Option<&str>) -> CheckResult {
    let mut args = vec![
        "run",
        "list",
        "--repo",
        owner_repo,
        "--json",
        "status,conclusion,name,databaseId,headBranch,url",
        "--limit",
        "1",
    ];
    let ref_str;
    if let Some(r) = git_ref {
        ref_str = format!("--branch={r}");
        args.push(&ref_str);
    }
    let r = run_cmd("gh", &args, ".", None);

    if r.exit_code == -1 {
        return CheckResult {
            status: "error".to_string(),
            workflow: String::new(),
            run_id: String::new(),
            branch: String::new(),
            url: String::new(),
            failed_job: None,
            error_lines: None,
            hint: Some(binary_missing_hint("gh")),
        };
    }

    if r.exit_code != 0 {
        return CheckResult {
            status: "error".to_string(),
            workflow: String::new(), run_id: String::new(),
            branch: String::new(), url: String::new(),
            failed_job: None,
            error_lines: Some(truncate(&r.output, 500)),
            hint: Some("gh CLI failed. Check that you're authenticated (`gh auth status`) and the repo exists.".to_string()),
        };
    }

    let runs: Vec<serde_json::Value> = serde_json::from_str(&r.output).unwrap_or_default();
    let run = match runs.first() {
        Some(r) => r,
        None => {
            return CheckResult {
                status: "no_runs".to_string(),
                workflow: String::new(),
                run_id: String::new(),
                branch: String::new(),
                url: String::new(),
                failed_job: None,
                error_lines: None,
                hint: Some("No CI runs found. Push a commit or check the branch name.".to_string()),
            }
        }
    };

    let status = run
        .get("conclusion")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| run.get("status").and_then(|v| v.as_str()))
        .unwrap_or("unknown")
        .to_string();
    let workflow = run
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let run_id = run
        .get("databaseId")
        .and_then(|v| v.as_u64())
        .map(|n| n.to_string())
        .unwrap_or_default();
    let branch = run
        .get("headBranch")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let url = run
        .get("url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let (failed_job, error_lines) = if status == "failure" && !run_id.is_empty() {
        enrich_failure(owner_repo, &run_id)
    } else {
        (None, None)
    };

    let hint = match status.as_str() {
        "failure" => Some(format!(
            "CI failed. Use ci_cd with command='logs' and run_id='{}' to get full failure logs, then fix the issue and push.",
            run_id
        )),
        "in_progress" | "queued" | "pending" => Some("CI is still running. Check again shortly.".to_string()),
        "success" => None,
        _ => Some(format!("CI status is '{}'. Check the run URL for details.", status)),
    };

    CheckResult {
        status,
        workflow,
        run_id,
        branch,
        url,
        failed_job,
        error_lines,
        hint,
    }
}

fn enrich_failure(owner_repo: &str, run_id: &str) -> (Option<String>, Option<String>) {
    let jobs_r = run_cmd(
        "gh",
        &[
            "run", "view", run_id, "--repo", owner_repo, "--json", "jobs",
        ],
        ".",
        None,
    );
    let failed_job = serde_json::from_str::<serde_json::Value>(&jobs_r.output)
        .ok()
        .and_then(|v| {
            v.get("jobs")?
                .as_array()?
                .iter()
                .find(|j| j.get("conclusion").and_then(|c| c.as_str()) == Some("failure"))
                .and_then(|j| j.get("name")?.as_str().map(|s| s.to_string()))
        });

    let log_r = run_cmd(
        "sh",
        &[
            "-c",
            &format!("gh run view {run_id} --repo {owner_repo} --log-failed 2>&1 | tail -5"),
        ],
        ".",
        None,
    );
    let error_lines = if log_r.output.trim().is_empty() {
        None
    } else {
        Some(truncate(log_r.output.trim(), 300))
    };

    (failed_job, error_lines)
}

// ── ci logs ────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct LogsResult {
    pub run_id: String,
    pub logs: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

pub fn logs(owner_repo: &str, run_id: &str) -> LogsResult {
    let r = run_cmd(
        "gh",
        &["run", "view", run_id, "--repo", owner_repo, "--log-failed"],
        ".",
        None,
    );

    if r.exit_code == -1 {
        return LogsResult {
            run_id: run_id.to_string(),
            logs: String::new(),
            hint: Some(binary_missing_hint("gh")),
        };
    }

    let output = &r.output;
    let logs = if output.len() > 4096 {
        let start = output.len() - 4096;
        let start = output.ceil_char_boundary(start);
        format!("… (showing last 4096 bytes)\n{}", &output[start..])
    } else {
        output.clone()
    };

    let hint = if r.exit_code != 0 {
        Some(
            "Failed to fetch logs. Check the run_id and ensure `gh auth status` is valid."
                .to_string(),
        )
    } else if logs.is_empty() {
        Some("No failed logs found. The run may have succeeded or logs expired.".to_string())
    } else {
        Some("Read the log output to identify the failure. Fix the issue in source, then push to trigger a new run.".to_string())
    };

    LogsResult {
        run_id: run_id.to_string(),
        logs,
        hint,
    }
}

// ── ci list ────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ListResult {
    pub runs: Vec<RunEntry>,
    pub count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RunEntry {
    pub id: String,
    pub status: String,
    pub workflow: String,
    pub branch: String,
    pub updated: String,
}

pub fn list(owner_repo: &str, git_ref: Option<&str>, limit: u32) -> ListResult {
    let limit_str = limit.to_string();
    let mut args = vec![
        "run",
        "list",
        "--repo",
        owner_repo,
        "--json",
        "status,conclusion,name,databaseId,headBranch,updatedAt",
        "--limit",
        &limit_str,
    ];
    let ref_str;
    if let Some(r) = git_ref {
        ref_str = format!("--branch={r}");
        args.push(&ref_str);
    }
    let r = run_cmd("gh", &args, ".", None);

    if r.exit_code == -1 {
        return ListResult {
            runs: vec![],
            count: 0,
            hint: Some(binary_missing_hint("gh")),
        };
    }

    let runs_json: Vec<serde_json::Value> = serde_json::from_str(&r.output).unwrap_or_default();
    let runs: Vec<RunEntry> = runs_json
        .iter()
        .map(|v| {
            let status = v
                .get("conclusion")
                .and_then(|c| c.as_str())
                .filter(|s| !s.is_empty())
                .or_else(|| v.get("status").and_then(|s| s.as_str()))
                .unwrap_or("unknown")
                .to_string();
            RunEntry {
                id: v
                    .get("databaseId")
                    .and_then(|n| n.as_u64())
                    .map(|n| n.to_string())
                    .unwrap_or_default(),
                status,
                workflow: v
                    .get("name")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string(),
                branch: v
                    .get("headBranch")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string(),
                updated: v
                    .get("updatedAt")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string(),
            }
        })
        .collect();

    let count = runs.len();
    let hint = if count == 0 {
        Some("No CI runs found. Push a commit or verify the repo name.".to_string())
    } else {
        None
    };

    ListResult { runs, count, hint }
}
