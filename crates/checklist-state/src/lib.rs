//! Module: Persist workflow checklists and schedule dependency-aware task DAGs.
//! Correctness: Correct when legacy checklists remain compatible, DAG and waiting-gate invariants validate, exact attempts are bounded, reviews update atomically, and caller-owned scoring remains explainable and auditable.
//! Last revised: 2026-07-12
//! Last changed: Added deterministic scored-ready selection and human waiting-gate resolution for CLI/MCP consumers.
//!
//! Stores named checklists as JSON files under `<project-root>/.forge/checklists/`.
//! Skills like `blueprint`, `compile-project`, and `performance-tuning` use these
//! to resume multi-step workflows across sessions, `/clear`, or compaction.
//!
//! All writes are atomic: write to a sibling temp file, then rename.

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

// ── Public types ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    Pending,
    InProgress,
    Completed,
    Blocked,
    Waiting,
}

impl ItemStatus {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "pending" => Ok(ItemStatus::Pending),
            "in_progress" => Ok(ItemStatus::InProgress),
            "completed" => Ok(ItemStatus::Completed),
            "blocked" => Ok(ItemStatus::Blocked),
            "waiting" => Ok(ItemStatus::Waiting),
            other => Err(anyhow!(
                "unknown status '{}'; expected one of pending|in_progress|completed|blocked|waiting",
                other
            )),
        }
    }
}

fn default_status() -> ItemStatus {
    ItemStatus::Pending
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParallelPolicy {
    SerialCode,
    SameTreeReadonly,
    WorktreeRequired,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BatchVerification {
    pub batch: u32,
    pub command: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateKind {
    Review,
    Decision,
    External,
    LoopDetected,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WaitingGate {
    pub kind: GateKind,
    pub created_at: DateTime<Utc>,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub question: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attempt_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effort {
    Small,
    Medium,
    Large,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct AttemptState {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub last_fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub last_execution_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub same_attempt_count: u32,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub similar_low_progress_count: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub last_event_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "is_zero_i32")]
    pub retry_penalty: i32,
    #[serde(default, skip_serializing_if = "is_zero_i32")]
    pub goal_retry_penalty: i32,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub post_pivot_return_count: u32,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub last_progress_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub next_attempt_number: u32,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub active_attempt: Option<ActiveAttempt>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exact_attempts: Vec<ExactAttemptParticipant>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub last_attempt: Option<FinishedAttempt>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub exact_retry_limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub exact_retry_execution_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptRole {
    Implementer,
    Verifier,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ScopedInputDigest {
    pub path: String,
    pub digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AttemptFingerprintInput {
    pub acceptance_criterion: String,
    pub relevant_inputs: Vec<ScopedInputDigest>,
    pub normalized_command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ActiveAttempt {
    pub attempt_id: String,
    pub execution_fingerprint: String,
    pub agent_id: String,
    pub role: AttemptRole,
    pub started_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExactAttemptParticipant {
    pub attempt_id: String,
    pub agent_id: String,
    pub role: AttemptRole,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FinishedAttempt {
    pub attempt_id: String,
    pub fingerprint: String,
    pub execution_fingerprint: String,
    pub agent_id: String,
    pub role: AttemptRole,
    pub finished_at: DateTime<Utc>,
    pub result_signature: String,
    pub progress: String,
    pub new_information: String,
    pub next_action: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AttemptFinish {
    pub result_signature: String,
    pub progress: String,
    pub new_information: String,
    pub next_action: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttemptDecision {
    Accepted,
    LoopDetected,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AttemptStartReport {
    pub attempt_id: String,
    pub execution_fingerprint: String,
    pub decision: AttemptDecision,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewOutcome {
    Approved,
    Disapproved,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewSeverity {
    Required,
    Optional,
    Informational,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReviewFeedbackInput {
    pub severity: ReviewSeverity,
    pub feedback: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FollowUpInput {
    pub id: String,
    pub title: String,
    pub severity: ReviewSeverity,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReviewInput {
    pub review_id: String,
    pub outcome: ReviewOutcome,
    pub reviewer_id: String,
    pub reviewed_at: DateTime<Utc>,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub feedback: Vec<ReviewFeedbackInput>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub follow_ups: Vec<FollowUpInput>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReviewFeedback {
    pub source_review_id: String,
    pub severity: ReviewSeverity,
    pub feedback: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReviewRecord {
    pub review_id: String,
    pub outcome: ReviewOutcome,
    pub reviewer_id: String,
    pub reviewed_at: DateTime<Utc>,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub feedback: Vec<ReviewFeedback>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub follow_up_item_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FollowUpLink {
    pub source_review_id: String,
    pub severity: ReviewSeverity,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HumanPreferenceAction {
    Set,
    Replace,
    Clear,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HumanPreferenceOperation {
    Set(i32),
    Replace(i32),
    Clear,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HumanPreferenceChange {
    pub operation: HumanPreferenceOperation,
    pub actor: String,
    pub reason: String,
    pub changed_at: DateTime<Utc>,
    pub restore_priority_state: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HumanPreferenceAudit {
    pub action: HumanPreferenceAction,
    pub previous_value: Option<i32>,
    pub new_value: Option<i32>,
    pub actor: String,
    pub reason: String,
    pub changed_at: DateTime<Utc>,
    pub restored_priority_state: bool,
}

fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

fn is_zero_i32(value: &i32) -> bool {
    *value == 0
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChecklistItem {
    pub id: String,
    pub title: String,
    #[serde(default = "default_status")]
    pub status: ItemStatus,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub completed_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub batch: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verification: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_refs: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub claimed_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub lease_expires_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub gate: Option<WaitingGate>,
    #[serde(rename = "goalRef", skip_serializing_if = "Option::is_none", default)]
    pub goal_ref: Option<String>,
    #[serde(
        rename = "goalSummary",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub goal_summary: Option<String>,
    #[serde(
        rename = "itemContribution",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub item_contribution: Option<String>,
    #[serde(
        rename = "basePriority",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub base_priority: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub effort: Option<Effort>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub critical: Option<bool>,
    #[serde(
        rename = "humanPriorityOverride",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub human_priority_override: Option<i32>,
    #[serde(
        rename = "humanPreferenceAudit",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub human_preference_audit: Vec<HumanPreferenceAudit>,
    #[serde(
        rename = "priorityStateRestored",
        default,
        skip_serializing_if = "is_false"
    )]
    pub priority_state_restored: bool,
    #[serde(
        rename = "parentItemId",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub parent_item_id: Option<String>,
    #[serde(
        rename = "attemptState",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub attempt_state: Option<AttemptState>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub review: Option<ReviewRecord>,
    #[serde(rename = "followUp", skip_serializing_if = "Option::is_none", default)]
    pub follow_up: Option<FollowUpLink>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checklist {
    pub name: String,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source_skill: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub schema_version: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source_plan: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub parallel_policy: Option<ParallelPolicy>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub batch_verification: Vec<BatchVerification>,
    pub items: Vec<ChecklistItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ValidationReport {
    pub valid: bool,
    pub errors: Vec<String>,
    pub batches: Vec<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadyReport {
    pub checklist: String,
    pub ready_count: usize,
    pub items: Vec<ChecklistItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimReport {
    pub checklist: String,
    pub agent_id: String,
    pub claimed_count: usize,
    pub claimed: Vec<ChecklistItem>,
    pub remaining_ready_count: usize,
}

/// Caller-owned effort values used by scoring. Forge supplies no defaults so
/// product policy cannot be introduced accidentally inside the state crate.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EffortScorePolicy {
    pub small: i64,
    pub medium: i64,
    pub large: i64,
    pub unspecified: i64,
}

impl EffortScorePolicy {
    fn value(self, effort: Option<Effort>) -> i64 {
        match effort {
            Some(Effort::Small) => self.small,
            Some(Effort::Medium) => self.medium,
            Some(Effort::Large) => self.large,
            None => self.unspecified,
        }
    }
}

/// Policy for valuing every transitive downstream item in the checklist DAG.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DagUnlockPolicy {
    pub priority_divisor: i64,
    pub effort_weight: EffortScorePolicy,
}

/// Time-based recovery applied only up to the item's active penalties.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PenaltyDecayPolicy {
    pub interval_seconds: i64,
    pub recovery_per_interval: i64,
}

/// Bonuses for work with a dependency path to penalized work.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UnblockBonusPolicy {
    pub penalized_item: i64,
    pub penalized_goal: i64,
}

/// Complete scoring policy. Every numeric product choice is required from the
/// caller; this crate intentionally provides no `Default` implementation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ScorePolicy {
    pub default_base_priority: i64,
    pub ease_bonus: EffortScorePolicy,
    pub dag_unlock: DagUnlockPolicy,
    pub goal_progress_max_bonus: i64,
    pub exact_retry_penalty_per_unit: i64,
    pub semantic_fixation_penalty_per_unit: i64,
    pub parent_goal_retry_penalty_per_unit: i64,
    pub minimum_fixated_items_for_goal_penalty: usize,
    pub minimum_post_pivot_returns_for_goal_penalty: u32,
    pub decay: PenaltyDecayPolicy,
    pub unblock: UnblockBonusPolicy,
    pub critical_visibility_floor: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ScoreComponents {
    pub base_priority: i64,
    pub ease_bonus: i64,
    pub dag_unlock_bonus: i64,
    pub goal_progress_bonus: i64,
    pub human_preference: i64,
    pub exact_retry_penalty: i64,
    pub semantic_fixation_penalty: i64,
    pub parent_goal_retry_penalty: i64,
    pub decay_recovery: i64,
    pub unblock_bonus: i64,
    pub critical_visibility_floor: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ScoreHelp {
    pub gate_kind: GateKind,
    pub reason: String,
    pub question: Option<String>,
    pub why_it_matters: String,
    pub unlocks: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ItemScore {
    pub item_id: String,
    pub status: ItemStatus,
    pub effective_score: i64,
    pub components: ScoreComponents,
    pub explanation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<ScoreHelp>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ScoreReport {
    pub checklist: String,
    /// Scores retain checklist order. Ordering policy belongs to T-005.
    pub items: Vec<ItemScore>,
}

/// One dependency-ready pending item paired with its complete named score.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoredReadyItem {
    pub item: ChecklistItem,
    pub score: ItemScore,
}

/// Deterministically ordered scored-ready output for schedulers and agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoredReadyReport {
    pub checklist: String,
    pub ready_count: usize,
    pub items: Vec<ScoredReadyItem>,
}

/// Result of resolving a non-review gate. Identity and reason are returned to
/// the caller but are not persisted as an override incident; T-008 owns that
/// hook-token and incident-lifecycle contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveReport {
    pub state: Checklist,
    pub item_id: String,
    pub resolved_by: String,
    pub reason: String,
    pub prior_gate: WaitingGate,
    pub prior_attempt_ids: Vec<String>,
    pub event_refs: Vec<String>,
    pub recovery_hints: Vec<String>,
}

// ── Path helpers ────────────────────────────────────────────────────────────

fn checklists_dir(project_root: &Path) -> PathBuf {
    project_root.join(".forge").join("checklists")
}

fn checklist_path(project_root: &Path, name: &str) -> PathBuf {
    checklists_dir(project_root).join(format!("{}.json", name))
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("checklist name must not be empty");
    }
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        bail!("checklist name must not contain path separators or '..'");
    }
    Ok(())
}

// ── Slug derivation ─────────────────────────────────────────────────────────

/// Convert a title to a slug-like id: lowercase, non-alnum runs become `-`,
/// trimmed at the ends.
pub fn slugify(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut prev_dash = true; // treat leading position as if preceded by dash to avoid leading dash
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

// ── Atomic JSON I/O ─────────────────────────────────────────────────────────

fn read_checklist(path: &Path) -> Result<Checklist> {
    let bytes = fs::read(path)
        .with_context(|| format!("failed to read checklist at {}", path.display()))?;
    let cl: Checklist = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse checklist JSON at {}", path.display()))?;
    Ok(cl)
}

fn write_checklist_atomic(path: &Path, cl: &Checklist) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create dir {}", parent.display()))?;
    }
    let json = serde_json::to_vec_pretty(cl).context("failed to serialize checklist")?;

    // tempfile sibling + rename for atomicity.
    let dir = path
        .parent()
        .ok_or_else(|| anyhow!("checklist path has no parent"))?;
    let tmp_name = format!(
        ".{}.tmp.{}",
        path.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("checklist"),
        std::process::id()
    );
    let tmp_path = dir.join(tmp_name);

    {
        let mut f = fs::File::create(&tmp_path)
            .with_context(|| format!("failed to create temp file {}", tmp_path.display()))?;
        f.write_all(&json)
            .context("failed to write checklist bytes")?;
        f.sync_all().ok();
    }
    fs::rename(&tmp_path, path).with_context(|| {
        format!(
            "failed to rename {} -> {}",
            tmp_path.display(),
            path.display()
        )
    })?;
    Ok(())
}

// ── DAG validation and scheduling ───────────────────────────────────────────

fn item_index(cl: &Checklist) -> BTreeMap<String, usize> {
    cl.items
        .iter()
        .enumerate()
        .map(|(i, item)| (item.id.clone(), i))
        .collect()
}

fn topo_batches_or_errors(cl: &Checklist) -> (Vec<Vec<String>>, Vec<String>) {
    let mut errors = Vec::new();
    let mut ids = BTreeSet::new();
    for item in &cl.items {
        if item.id.trim().is_empty() {
            errors.push(format!(
                "checklist '{}' contains an item with an empty id",
                cl.name
            ));
        }
        if !ids.insert(item.id.clone()) {
            errors.push(format!("duplicate checklist item id '{}'", item.id));
        }
        match (item.status, item.gate.is_some()) {
            (ItemStatus::Waiting, false) => {
                errors.push(format!("waiting item '{}' requires a gate", item.id));
            }
            (ItemStatus::Waiting, true) | (_, false) => {}
            (_, true) => errors.push(format!(
                "gate on item '{}' is only valid for waiting status",
                item.id
            )),
        }
    }

    let index = item_index(cl);
    for item in &cl.items {
        let mut seen_deps = BTreeSet::new();
        for dep in &item.depends_on {
            if dep == &item.id {
                errors.push(format!("item '{}' depends on itself", item.id));
            }
            if !seen_deps.insert(dep) {
                errors.push(format!("item '{}' repeats dependency '{}'", item.id, dep));
            }
            if !index.contains_key(dep) {
                errors.push(format!(
                    "item '{}' depends on missing item '{}'",
                    item.id, dep
                ));
            }
        }
    }
    if !errors.is_empty() {
        return (Vec::new(), errors);
    }

    let mut indegree: BTreeMap<String, usize> = BTreeMap::new();
    let mut dependents: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for item in &cl.items {
        indegree.insert(item.id.clone(), item.depends_on.len());
        for dep in &item.depends_on {
            dependents
                .entry(dep.clone())
                .or_default()
                .push(item.id.clone());
        }
    }

    let original_order: Vec<String> = cl.items.iter().map(|i| i.id.clone()).collect();
    let mut completed = BTreeSet::new();
    let mut batches = Vec::new();

    loop {
        let batch: Vec<String> = original_order
            .iter()
            .filter(|id| !completed.contains(*id))
            .filter(|id| indegree.get(*id).copied().unwrap_or(0) == 0)
            .cloned()
            .collect();

        if batch.is_empty() {
            break;
        }

        for id in &batch {
            completed.insert(id.clone());
            if let Some(children) = dependents.get(id) {
                for child in children {
                    if let Some(entry) = indegree.get_mut(child) {
                        *entry = entry.saturating_sub(1);
                    }
                }
            }
        }
        batches.push(batch);
    }

    if completed.len() != cl.items.len() {
        let remaining: Vec<String> = original_order
            .into_iter()
            .filter(|id| !completed.contains(id))
            .collect();
        errors.push(format!(
            "dependency cycle detected involving item(s): {}",
            remaining.join(", ")
        ));
    }

    (batches, errors)
}

/// Validate dependency references, duplicate ids, and cycles.
pub fn validate_dependencies(cl: &Checklist) -> ValidationReport {
    let (batches, errors) = topo_batches_or_errors(cl);
    ValidationReport {
        valid: errors.is_empty(),
        errors,
        batches,
    }
}

/// Return topological batches for a valid checklist.
pub fn derive_batches(cl: &Checklist) -> Result<Vec<Vec<String>>> {
    let report = validate_dependencies(cl);
    if !report.valid {
        bail!("invalid checklist DAG: {}", report.errors.join("; "));
    }
    Ok(report.batches)
}

fn deps_completed(cl: &Checklist, item: &ChecklistItem, index: &BTreeMap<String, usize>) -> bool {
    item.depends_on.iter().all(|dep| {
        index
            .get(dep)
            .and_then(|i| cl.items.get(*i))
            .map(|dep_item| dep_item.status == ItemStatus::Completed)
            .unwrap_or(false)
    })
}

fn is_expired(item: &ChecklistItem, now: DateTime<Utc>) -> bool {
    item.lease_expires_at
        .map(|lease| lease <= now)
        .unwrap_or(false)
}

fn ready_items_from(
    cl: &Checklist,
    now: DateTime<Utc>,
    include_expired_leases: bool,
    limit: Option<usize>,
) -> Result<Vec<ChecklistItem>> {
    let report = validate_dependencies(cl);
    if !report.valid {
        bail!("invalid checklist DAG: {}", report.errors.join("; "));
    }
    let index = item_index(cl);
    let mut out = Vec::new();
    for item in &cl.items {
        let status_ready = item.status == ItemStatus::Pending
            || (include_expired_leases
                && item.status == ItemStatus::InProgress
                && is_expired(item, now));
        if status_ready && deps_completed(cl, item, &index) {
            out.push(item.clone());
            if limit.is_some_and(|n| out.len() >= n) {
                break;
            }
        }
    }
    Ok(out)
}

// ── Explainable priority scoring ──────────────────────────────────────────

fn validate_score_policy(policy: &ScorePolicy) -> Result<()> {
    if policy.dag_unlock.priority_divisor <= 0 {
        bail!("score policy DAG priority divisor must be positive");
    }
    if policy.minimum_fixated_items_for_goal_penalty < 2 {
        bail!("score policy must require at least two fixated items for a parent-goal penalty");
    }
    if policy.minimum_post_pivot_returns_for_goal_penalty < 2 {
        bail!(
            "score policy must require at least two post-pivot returns for a parent-goal penalty"
        );
    }
    if policy.decay.interval_seconds <= 0 {
        bail!("score policy decay interval must be positive");
    }
    for (name, value) in [
        ("exact retry penalty", policy.exact_retry_penalty_per_unit),
        (
            "semantic fixation penalty",
            policy.semantic_fixation_penalty_per_unit,
        ),
        (
            "parent-goal retry penalty",
            policy.parent_goal_retry_penalty_per_unit,
        ),
        ("decay recovery", policy.decay.recovery_per_interval),
        (
            "penalized-item unblock bonus",
            policy.unblock.penalized_item,
        ),
        (
            "penalized-goal unblock bonus",
            policy.unblock.penalized_goal,
        ),
    ] {
        if value < 0 {
            bail!("score policy {name} must not be negative");
        }
    }
    Ok(())
}

fn scaled_score(left: i64, right: i64, divisor: i64) -> i64 {
    let value = i128::from(left) * i128::from(right) / i128::from(divisor);
    value.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

fn dependents_by_item(cl: &Checklist) -> BTreeMap<String, Vec<usize>> {
    let mut dependents = BTreeMap::<String, Vec<usize>>::new();
    for (index, item) in cl.items.iter().enumerate() {
        for dependency in &item.depends_on {
            dependents
                .entry(dependency.clone())
                .or_default()
                .push(index);
        }
    }
    dependents
}

fn descendant_indices(
    item_id: &str,
    dependents: &BTreeMap<String, Vec<usize>>,
    items: &[ChecklistItem],
) -> Vec<usize> {
    let mut pending = dependents.get(item_id).cloned().unwrap_or_default();
    let mut seen = BTreeSet::new();
    let mut descendants = Vec::new();
    while let Some(index) = pending.pop() {
        if !seen.insert(index) {
            continue;
        }
        descendants.push(index);
        if let Some(children) = dependents.get(&items[index].id) {
            pending.extend(children);
        }
    }
    descendants.sort_unstable();
    descendants
}

fn item_has_priority_penalty(item: &ChecklistItem) -> bool {
    !item.priority_state_restored
        && item.attempt_state.as_ref().is_some_and(|state| {
            state.retry_penalty > 0
                || state.similar_low_progress_count > 0
                || state.goal_retry_penalty > 0
                || state.post_pivot_return_count > 0
        })
}

fn goal_retry_units(cl: &Checklist, goal_ref: Option<&str>, policy: &ScorePolicy) -> i64 {
    let Some(goal_ref) = goal_ref else {
        return 0;
    };
    let members: Vec<&AttemptState> = cl
        .items
        .iter()
        .filter(|item| item.goal_ref.as_deref() == Some(goal_ref))
        .filter_map(|item| item.attempt_state.as_ref())
        .collect();
    let fixated_count = members
        .iter()
        .filter(|state| state.similar_low_progress_count > 0)
        .count();
    let cross_item_units = if fixated_count >= policy.minimum_fixated_items_for_goal_penalty {
        fixated_count - policy.minimum_fixated_items_for_goal_penalty + 1
    } else {
        0
    };
    let recorded_goal_units: i64 = if cross_item_units == 0 {
        0
    } else {
        members
            .iter()
            .map(|state| i64::from(state.goal_retry_penalty.max(0)))
            .sum()
    };
    let post_pivot_returns: u64 = members
        .iter()
        .map(|state| u64::from(state.post_pivot_return_count))
        .sum();
    let post_pivot_threshold = u64::from(policy.minimum_post_pivot_returns_for_goal_penalty);
    let post_pivot_units = if post_pivot_returns >= post_pivot_threshold {
        post_pivot_returns - post_pivot_threshold + 1
    } else {
        0
    };
    i64::try_from(cross_item_units)
        .unwrap_or(i64::MAX)
        .saturating_add(recorded_goal_units)
        .saturating_add(i64::try_from(post_pivot_units).unwrap_or(i64::MAX))
}

fn goal_progress_bonus(cl: &Checklist, item: &ChecklistItem, policy: &ScorePolicy) -> i64 {
    let Some(goal_ref) = item.goal_ref.as_deref() else {
        return 0;
    };
    let mut total = 0_i64;
    let mut completed = 0_i64;
    for member in &cl.items {
        if member.goal_ref.as_deref() == Some(goal_ref) {
            total = total.saturating_add(1);
            if member.status == ItemStatus::Completed {
                completed = completed.saturating_add(1);
            }
        }
    }
    if total == 0 {
        0
    } else {
        scaled_score(policy.goal_progress_max_bonus, completed, total)
    }
}

fn score_explanation(components: &ScoreComponents, effective_score: i64) -> String {
    let mut terms = vec![format!("base {}", components.base_priority)];
    for (name, value) in [
        ("ease", components.ease_bonus),
        ("DAG unlock", components.dag_unlock_bonus),
        ("goal progress", components.goal_progress_bonus),
        ("human preference", components.human_preference),
        ("exact retry", components.exact_retry_penalty),
        ("semantic fixation", components.semantic_fixation_penalty),
        ("parent-goal retry", components.parent_goal_retry_penalty),
        ("decay", components.decay_recovery),
        ("unblock", components.unblock_bonus),
        ("critical floor", components.critical_visibility_floor),
    ] {
        if value != 0 {
            terms.push(format!("{name} {value:+}"));
        }
    }
    format!("{} = {effective_score}", terms.join(", "))
}

/// Score every incomplete item in checklist order. This function calculates
/// value only; ready-item ordering and automatic selection are deferred to
/// T-005.
pub fn score_checklist(
    cl: &Checklist,
    policy: &ScorePolicy,
    now: DateTime<Utc>,
) -> Result<ScoreReport> {
    validate_score_policy(policy)?;
    let validation = validate_dependencies(cl);
    if !validation.valid {
        bail!("invalid checklist DAG: {}", validation.errors.join("; "));
    }

    let dependents = dependents_by_item(cl);
    let mut scores = Vec::new();
    for item in cl
        .items
        .iter()
        .filter(|item| item.status != ItemStatus::Completed)
    {
        let descendants = descendant_indices(&item.id, &dependents, &cl.items);
        let base_priority = item
            .base_priority
            .map(i64::from)
            .unwrap_or(policy.default_base_priority);
        let dag_unlock_bonus = descendants.iter().fold(0_i64, |score, index| {
            let downstream = &cl.items[*index];
            let priority = downstream
                .base_priority
                .map(i64::from)
                .unwrap_or(policy.default_base_priority);
            let effort = policy.dag_unlock.effort_weight.value(downstream.effort);
            score.saturating_add(scaled_score(
                priority,
                effort,
                policy.dag_unlock.priority_divisor,
            ))
        });
        let attempt_state = item.attempt_state.as_ref();
        let exact_units = if item.priority_state_restored {
            0
        } else {
            attempt_state
                .map(|state| i64::from(state.retry_penalty.max(0)))
                .unwrap_or(0)
        };
        let semantic_units = if item.priority_state_restored {
            0
        } else {
            attempt_state
                .map(|state| i64::from(state.similar_low_progress_count))
                .unwrap_or(0)
        };
        let parent_goal_units = if item.priority_state_restored {
            0
        } else {
            goal_retry_units(cl, item.goal_ref.as_deref(), policy)
        };
        let exact_retry_penalty = -policy
            .exact_retry_penalty_per_unit
            .saturating_mul(exact_units);
        let semantic_fixation_penalty = -policy
            .semantic_fixation_penalty_per_unit
            .saturating_mul(semantic_units);
        let parent_goal_retry_penalty = -policy
            .parent_goal_retry_penalty_per_unit
            .saturating_mul(parent_goal_units);
        let total_penalty = exact_retry_penalty
            .saturating_add(semantic_fixation_penalty)
            .saturating_add(parent_goal_retry_penalty)
            .saturating_neg();
        let elapsed_intervals = attempt_state
            .and_then(|state| state.last_progress_at)
            .map(|at| now.signed_duration_since(at).num_seconds().max(0))
            .unwrap_or(0)
            / policy.decay.interval_seconds;
        let decay_recovery = policy
            .decay
            .recovery_per_interval
            .saturating_mul(elapsed_intervals)
            .min(total_penalty);

        let penalized_descendants = descendants
            .iter()
            .filter(|index| item_has_priority_penalty(&cl.items[**index]))
            .count();
        let penalized_goals: BTreeSet<&str> = descendants
            .iter()
            .filter_map(|index| cl.items[*index].goal_ref.as_deref())
            .filter(|goal| goal_retry_units(cl, Some(goal), policy) > 0)
            .collect();
        let unblock_bonus = policy
            .unblock
            .penalized_item
            .saturating_mul(i64::try_from(penalized_descendants).unwrap_or(i64::MAX))
            .saturating_add(
                policy
                    .unblock
                    .penalized_goal
                    .saturating_mul(i64::try_from(penalized_goals.len()).unwrap_or(i64::MAX)),
            );

        let mut components = ScoreComponents {
            base_priority,
            ease_bonus: policy.ease_bonus.value(item.effort),
            dag_unlock_bonus,
            goal_progress_bonus: goal_progress_bonus(cl, item, policy),
            human_preference: item.human_priority_override.map(i64::from).unwrap_or(0),
            exact_retry_penalty,
            semantic_fixation_penalty,
            parent_goal_retry_penalty,
            decay_recovery,
            unblock_bonus,
            critical_visibility_floor: 0,
        };
        let subtotal = components
            .base_priority
            .saturating_add(components.ease_bonus)
            .saturating_add(components.dag_unlock_bonus)
            .saturating_add(components.goal_progress_bonus)
            .saturating_add(components.human_preference)
            .saturating_add(components.exact_retry_penalty)
            .saturating_add(components.semantic_fixation_penalty)
            .saturating_add(components.parent_goal_retry_penalty)
            .saturating_add(components.decay_recovery)
            .saturating_add(components.unblock_bonus);
        if item.critical.unwrap_or(false) && subtotal < policy.critical_visibility_floor {
            components.critical_visibility_floor =
                policy.critical_visibility_floor.saturating_sub(subtotal);
        }
        let effective_score = subtotal.saturating_add(components.critical_visibility_floor);
        let help = if item.critical.unwrap_or(false) && item.status == ItemStatus::Waiting {
            item.gate.as_ref().map(|gate| ScoreHelp {
                gate_kind: gate.kind,
                reason: gate.reason.clone(),
                question: gate.question.clone(),
                why_it_matters: item
                    .item_contribution
                    .clone()
                    .or_else(|| item.goal_summary.clone())
                    .unwrap_or_else(|| item.title.clone()),
                unlocks: descendants
                    .iter()
                    .map(|index| cl.items[*index].id.clone())
                    .collect(),
            })
        } else {
            None
        };
        scores.push(ItemScore {
            item_id: item.id.clone(),
            status: item.status,
            effective_score,
            explanation: score_explanation(&components, effective_score),
            components,
            help,
        });
    }
    Ok(ScoreReport {
        checklist: cl.name.clone(),
        items: scores,
    })
}

// ── Exact attempt fingerprints ─────────────────────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CanonicalDependencyState {
    id: String,
    status: ItemStatus,
    gate: Option<CanonicalGateState>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CanonicalGateState {
    kind: GateKind,
    reason: String,
    question: Option<String>,
    artifact_refs: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CanonicalExecutionFingerprint {
    item_id: String,
    acceptance_criterion: String,
    relevant_inputs: Vec<ScopedInputDigest>,
    normalized_command: String,
    dependencies: Vec<CanonicalDependencyState>,
    gate: Option<CanonicalGateState>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CanonicalCompletedFingerprint<'a> {
    execution_fingerprint: &'a str,
    result_signature: String,
}

fn normalize_prose(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn normalize_opaque_identity(value: &str) -> String {
    value.trim().to_string()
}

fn normalize_scoped_path(value: &str) -> String {
    let replaced = value.trim().replace('\\', "/");
    let absolute = replaced.starts_with('/');
    let mut components: Vec<&str> = Vec::new();
    for component in replaced.split('/') {
        match component {
            "" | "." => {}
            ".." if components.last().is_some_and(|last| *last != "..") => {
                components.pop();
            }
            other => components.push(other),
        }
    }
    let normalized = components.join("/");
    if absolute {
        format!("/{normalized}")
    } else {
        normalized
    }
}

fn canonical_gate_state(gate: &WaitingGate) -> CanonicalGateState {
    let mut artifact_refs: Vec<String> = gate
        .artifact_refs
        .iter()
        .map(|reference| normalize_prose(reference))
        .collect();
    artifact_refs.sort();
    CanonicalGateState {
        kind: gate.kind,
        reason: normalize_prose(&gate.reason),
        question: gate.question.as_deref().map(normalize_prose),
        artifact_refs,
    }
}

fn sha256_json<T: Serialize>(value: &T) -> Result<String> {
    let encoded = serde_json::to_vec(value).context("failed to serialize attempt fingerprint")?;
    let digest = Sha256::digest(encoded);
    Ok(format!("sha256:{digest:x}"))
}

fn canonical_execution_fingerprint(
    checklist: &Checklist,
    item_id: &str,
    input: &AttemptFingerprintInput,
) -> Result<CanonicalExecutionFingerprint> {
    let index = item_index(checklist);
    let item = index
        .get(item_id)
        .and_then(|position| checklist.items.get(*position))
        .ok_or_else(|| {
            anyhow!(
                "item id '{}' not found in checklist '{}'",
                item_id,
                checklist.name
            )
        })?;

    let acceptance_criterion = normalize_prose(&input.acceptance_criterion);
    let normalized_command = normalize_opaque_identity(&input.normalized_command);
    if acceptance_criterion.is_empty() {
        bail!("acceptance_criterion must not be empty");
    }
    if normalized_command.is_empty() {
        bail!("normalized_command must not be empty");
    }

    let mut relevant_inputs: Vec<ScopedInputDigest> = input
        .relevant_inputs
        .iter()
        .map(|entry| ScopedInputDigest {
            path: normalize_scoped_path(&entry.path),
            digest: entry.digest.trim().to_ascii_lowercase(),
        })
        .collect();
    if relevant_inputs
        .iter()
        .any(|entry| entry.path.is_empty() || entry.digest.is_empty())
    {
        bail!("relevant input paths and digests must not be empty");
    }
    relevant_inputs
        .sort_by(|left, right| (&left.path, &left.digest).cmp(&(&right.path, &right.digest)));

    let mut dependencies = Vec::with_capacity(item.depends_on.len());
    for dependency_id in &item.depends_on {
        let dependency = index
            .get(dependency_id)
            .and_then(|position| checklist.items.get(*position))
            .ok_or_else(|| {
                anyhow!(
                    "item '{}' depends on missing item '{}'",
                    item_id,
                    dependency_id
                )
            })?;
        dependencies.push(CanonicalDependencyState {
            id: dependency.id.clone(),
            status: dependency.status,
            gate: dependency.gate.as_ref().map(canonical_gate_state),
        });
    }
    dependencies.sort_by(|left, right| left.id.cmp(&right.id));

    Ok(CanonicalExecutionFingerprint {
        item_id: item.id.clone(),
        acceptance_criterion,
        relevant_inputs,
        normalized_command,
        dependencies,
        gate: item.gate.as_ref().map(canonical_gate_state),
    })
}

/// Hash the canonical pre-execution attempt inputs. Operational counters,
/// attempt identities, actors, and timestamps are intentionally excluded.
pub fn attempt_execution_fingerprint(
    checklist: &Checklist,
    item_id: &str,
    input: &AttemptFingerprintInput,
) -> Result<String> {
    sha256_json(&canonical_execution_fingerprint(checklist, item_id, input)?)
}

/// Hash a completed attempt by combining its canonical pre-execution identity
/// with the normalized result supplied only after execution.
pub fn completed_attempt_fingerprint(
    checklist: &Checklist,
    item_id: &str,
    input: &AttemptFingerprintInput,
    result_signature: &str,
) -> Result<String> {
    let execution_fingerprint = attempt_execution_fingerprint(checklist, item_id, input)?;
    completed_fingerprint_from_execution(&execution_fingerprint, result_signature)
}

fn completed_fingerprint_from_execution(
    execution_fingerprint: &str,
    result_signature: &str,
) -> Result<String> {
    let result_signature = normalize_opaque_identity(result_signature);
    if result_signature.is_empty() {
        bail!("result_signature must not be empty");
    }
    sha256_json(&CanonicalCompletedFingerprint {
        execution_fingerprint,
        result_signature,
    })
}

// ── Public API ──────────────────────────────────────────────────────────────

/// Create a new checklist with the given item titles. Fails if a checklist
/// with `name` already exists.
pub fn create(dir: &Path, name: &str, titles: &[String]) -> Result<Checklist> {
    validate_name(name)?;
    let path = checklist_path(dir, name);
    if path.exists() {
        bail!("checklist '{}' already exists at {}", name, path.display());
    }
    let now = Utc::now();
    let mut items = Vec::with_capacity(titles.len());
    let mut seen_ids: Vec<String> = Vec::new();
    for title in titles {
        let mut id = slugify(title);
        if id.is_empty() {
            id = format!("item-{}", items.len() + 1);
        }
        // Disambiguate collisions: append `-2`, `-3`, ...
        let mut candidate = id.clone();
        let mut n = 2usize;
        while seen_ids.iter().any(|s| s == &candidate) {
            candidate = format!("{}-{}", id, n);
            n += 1;
        }
        seen_ids.push(candidate.clone());
        items.push(ChecklistItem {
            id: candidate,
            title: title.clone(),
            status: ItemStatus::Pending,
            completed_at: None,
            notes: None,
            depends_on: Vec::new(),
            batch: None,
            verification: Vec::new(),
            source_refs: Vec::new(),
            claimed_by: None,
            lease_expires_at: None,
            gate: None,
            goal_ref: None,
            goal_summary: None,
            item_contribution: None,
            base_priority: None,
            effort: None,
            critical: None,
            human_priority_override: None,
            human_preference_audit: Vec::new(),
            priority_state_restored: false,
            parent_item_id: None,
            attempt_state: None,
            review: None,
            follow_up: None,
        });
    }
    let cl = Checklist {
        name: name.to_string(),
        created: now,
        updated: now,
        source_skill: None,
        schema_version: None,
        source_plan: None,
        parallel_policy: None,
        batch_verification: Vec::new(),
        items,
    };
    write_checklist_atomic(&path, &cl)?;
    Ok(cl)
}

/// Create a dependency-aware checklist from rich items. Fails if invalid.
pub fn create_dag_from_items(
    dir: &Path,
    name: &str,
    mut items: Vec<ChecklistItem>,
) -> Result<Checklist> {
    let now = Utc::now();
    for item in items.iter_mut() {
        item.completed_at = if item.status == ItemStatus::Completed {
            item.completed_at.or(Some(now))
        } else {
            None
        };
    }
    let cl = Checklist {
        name: name.to_string(),
        created: now,
        updated: now,
        source_skill: Some("compile-project".to_string()),
        schema_version: Some(2),
        source_plan: None,
        parallel_policy: None,
        batch_verification: Vec::new(),
        items,
    };
    create_dag(dir, name, cl)
}

/// Create a dependency-aware checklist from a full checklist value. Fails if invalid.
pub fn create_dag(dir: &Path, name: &str, mut cl: Checklist) -> Result<Checklist> {
    validate_name(name)?;
    cl.name = name.to_string();
    cl.schema_version = cl.schema_version.or(Some(2));
    let path = checklist_path(dir, name);
    if path.exists() {
        bail!("checklist '{}' already exists at {}", name, path.display());
    }
    let report = validate_dependencies(&cl);
    if !report.valid {
        bail!("invalid checklist DAG: {}", report.errors.join("; "));
    }
    write_checklist_atomic(&path, &cl)?;
    Ok(cl)
}

/// List all checklist names in `<dir>/.forge/checklists/`.
pub fn list(dir: &Path) -> Result<Vec<String>> {
    let cdir = checklists_dir(dir);
    if !cdir.exists() {
        return Ok(Vec::new());
    }
    let mut names = Vec::new();
    for entry in
        fs::read_dir(&cdir).with_context(|| format!("failed to read dir {}", cdir.display()))?
    {
        let entry = entry?;
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) == Some("json") {
            if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                // Skip our temp files.
                if !stem.starts_with('.') {
                    names.push(stem.to_string());
                }
            }
        }
    }
    names.sort();
    Ok(names)
}

/// Read and return an existing checklist.
pub fn show(dir: &Path, name: &str) -> Result<Checklist> {
    validate_name(name)?;
    let path = checklist_path(dir, name);
    if !path.exists() {
        bail!("checklist '{}' not found at {}", name, path.display());
    }
    read_checklist(&path)
}

/// Validate an existing checklist and return a structured report.
pub fn validate(dir: &Path, name: &str) -> Result<ValidationReport> {
    let cl = show(dir, name)?;
    Ok(validate_dependencies(&cl))
}

/// Return dependency-ready items from an existing checklist.
pub fn ready(
    dir: &Path,
    name: &str,
    limit: Option<usize>,
    include_expired_leases: bool,
) -> Result<ReadyReport> {
    let cl = show(dir, name)?;
    let items = ready_items_from(&cl, Utc::now(), include_expired_leases, limit)?;
    Ok(ReadyReport {
        checklist: cl.name,
        ready_count: items.len(),
        items,
    })
}

/// Return only dependency-ready pending items, ordered by descending effective
/// score and then ascending item ID. The limit is applied after scoring/sort so
/// checklist storage order cannot influence automatic scheduling.
pub fn scored_ready(
    dir: &Path,
    name: &str,
    policy: &ScorePolicy,
    limit: Option<usize>,
) -> Result<ScoredReadyReport> {
    let checklist = show(dir, name)?;
    let now = Utc::now();
    let ready_items = ready_items_from(&checklist, now, false, None)?;
    let scores = score_checklist(&checklist, policy, now)?
        .items
        .into_iter()
        .map(|score| (score.item_id.clone(), score))
        .collect::<BTreeMap<_, _>>();
    let mut items = ready_items
        .into_iter()
        .filter(|item| item.status == ItemStatus::Pending)
        .map(|item| {
            let score = scores
                .get(&item.id)
                .cloned()
                .ok_or_else(|| anyhow!("ready item '{}' has no score", item.id))?;
            Ok(ScoredReadyItem { item, score })
        })
        .collect::<Result<Vec<_>>>()?;
    items.sort_by(|left, right| {
        right
            .score
            .effective_score
            .cmp(&left.score.effective_score)
            .then_with(|| left.item.id.cmp(&right.item.id))
    });
    if let Some(limit) = limit {
        items.truncate(limit);
    }
    Ok(ScoredReadyReport {
        checklist: checklist.name,
        ready_count: items.len(),
        items,
    })
}

/// Load and score an existing checklist using the caller-supplied policy.
pub fn score(dir: &Path, name: &str, policy: &ScorePolicy) -> Result<ScoreReport> {
    let checklist = show(dir, name)?;
    score_checklist(&checklist, policy, Utc::now())
}

/// Atomically claim ready items for an agent. Same-agent expired/in-progress claims
/// can be renewed through `include_expired_leases`; active claims by another agent
/// are never returned by the ready-set calculation.
pub fn claim(
    dir: &Path,
    name: &str,
    agent_id: &str,
    limit: usize,
    lease_minutes: i64,
    include_expired_leases: bool,
) -> Result<ClaimReport> {
    validate_name(name)?;
    if agent_id.trim().is_empty() {
        bail!("agent_id must not be empty");
    }
    if limit == 0 {
        bail!("claim limit must be greater than zero");
    }
    if lease_minutes <= 0 {
        bail!("lease_minutes must be greater than zero");
    }

    let path = checklist_path(dir, name);
    if !path.exists() {
        bail!("checklist '{}' not found at {}", name, path.display());
    }
    let mut cl = read_checklist(&path)?;
    let now = Utc::now();
    let ready = ready_items_from(&cl, now, include_expired_leases, Some(limit))?;
    let ready_ids: BTreeSet<String> = ready.iter().map(|item| item.id.clone()).collect();
    let lease_expires_at = now + Duration::minutes(lease_minutes);
    let mut claimed = Vec::new();

    for item in cl.items.iter_mut() {
        if ready_ids.contains(&item.id) {
            item.status = ItemStatus::InProgress;
            item.completed_at = None;
            item.claimed_by = Some(agent_id.to_string());
            item.lease_expires_at = Some(lease_expires_at);
            claimed.push(item.clone());
        }
    }

    cl.updated = now;
    let remaining_ready_count = ready_items_from(&cl, now, include_expired_leases, None)?.len();
    write_checklist_atomic(&path, &cl)?;
    Ok(ClaimReport {
        checklist: cl.name,
        agent_id: agent_id.to_string(),
        claimed_count: claimed.len(),
        claimed,
        remaining_ready_count,
    })
}

/// Release a claim back to pending. If `agent_id` is provided, it must match the
/// current claimant.
pub fn release(dir: &Path, name: &str, item_id: &str, agent_id: Option<&str>) -> Result<Checklist> {
    validate_name(name)?;
    let path = checklist_path(dir, name);
    if !path.exists() {
        bail!("checklist '{}' not found at {}", name, path.display());
    }
    let mut cl = read_checklist(&path)?;
    let mut found = false;
    for item in cl.items.iter_mut() {
        if item.id == item_id {
            reject_active_attempt_transition(item, "release")?;
            if let Some(expected) = agent_id {
                if item.claimed_by.as_deref() != Some(expected) {
                    bail!(
                        "item '{}' is claimed by {:?}, not '{}'",
                        item_id,
                        item.claimed_by,
                        expected
                    );
                }
            }
            item.status = ItemStatus::Pending;
            item.completed_at = None;
            item.claimed_by = None;
            item.lease_expires_at = None;
            item.gate = None;
            found = true;
            break;
        }
    }
    if !found {
        bail!("item id '{}' not found in checklist '{}'", item_id, name);
    }
    cl.updated = Utc::now();
    write_checklist_atomic(&path, &cl)?;
    Ok(cl)
}

/// Update an item's status. Sets `completed_at` when transitioning into
/// `Completed`; clears it otherwise. Manual status changes clear claims/leases.
pub fn set(dir: &Path, name: &str, item_id: &str, status: ItemStatus) -> Result<Checklist> {
    if status == ItemStatus::Waiting {
        bail!("waiting status requires a typed gate; use set_waiting");
    }
    validate_name(name)?;
    let path = checklist_path(dir, name);
    if !path.exists() {
        bail!("checklist '{}' not found at {}", name, path.display());
    }
    let mut cl = read_checklist(&path)?;
    let now = Utc::now();
    let mut found = false;
    for item in cl.items.iter_mut() {
        if item.id == item_id {
            reject_active_attempt_transition(item, "manually set status on")?;
            item.status = status;
            item.completed_at = if status == ItemStatus::Completed {
                Some(now)
            } else {
                None
            };
            item.claimed_by = None;
            item.lease_expires_at = None;
            item.gate = None;
            found = true;
            break;
        }
    }
    if !found {
        bail!("item id '{}' not found in checklist '{}'", item_id, name);
    }
    cl.updated = now;
    write_checklist_atomic(&path, &cl)?;
    Ok(cl)
}

/// Put an item into `Waiting` with the gate that must be resolved before work
/// can resume. Entering waiting atomically releases any active claim and lease.
pub fn set_waiting(dir: &Path, name: &str, item_id: &str, gate: WaitingGate) -> Result<Checklist> {
    validate_name(name)?;
    let path = checklist_path(dir, name);
    if !path.exists() {
        bail!("checklist '{}' not found at {}", name, path.display());
    }
    let mut cl = read_checklist(&path)?;
    let mut found = false;
    for item in cl.items.iter_mut() {
        if item.id == item_id {
            reject_active_attempt_transition(item, "set waiting on")?;
            item.status = ItemStatus::Waiting;
            item.gate = Some(gate);
            item.completed_at = None;
            item.claimed_by = None;
            item.lease_expires_at = None;
            found = true;
            break;
        }
    }
    if !found {
        bail!("item id '{}' not found in checklist '{}'", item_id, name);
    }
    cl.updated = Utc::now();
    write_checklist_atomic(&path, &cl)?;
    Ok(cl)
}

/// Resolve a decision, external, or loop gate back to pending. Review gates
/// must use `apply_review` so their atomic outcome/follow-up rules cannot be
/// bypassed. Human identity and reason are mandatory even before T-008 adds
/// durable override incidents.
pub fn resolve_waiting(
    dir: &Path,
    name: &str,
    item_id: &str,
    resolved_by: &str,
    reason: &str,
) -> Result<ResolveReport> {
    validate_name(name)?;
    if resolved_by.trim().is_empty() {
        bail!("resolved_by must identify a human");
    }
    if reason.trim().is_empty() {
        bail!("resolution reason must not be empty");
    }
    let path = checklist_path(dir, name);
    if !path.exists() {
        bail!("checklist '{}' not found at {}", name, path.display());
    }
    let mut checklist = read_checklist(&path)?;
    let item = checklist
        .items
        .iter_mut()
        .find(|item| item.id == item_id)
        .ok_or_else(|| anyhow!("item id '{}' not found in checklist '{}'", item_id, name))?;
    reject_active_attempt_transition(item, "resolve waiting gate for")?;
    if item.status != ItemStatus::Waiting {
        bail!("item '{}' is not waiting", item_id);
    }
    let prior_gate = item
        .gate
        .clone()
        .ok_or_else(|| anyhow!("waiting item '{}' has no typed gate", item_id))?;
    if prior_gate.kind == GateKind::Review {
        bail!("review gates must be resolved with apply_review");
    }

    let mut prior_attempt_ids = item
        .attempt_state
        .as_ref()
        .map(|state| {
            state
                .exact_attempts
                .iter()
                .map(|attempt| attempt.attempt_id.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for attempt_id in &prior_gate.attempt_ids {
        if !prior_attempt_ids.contains(attempt_id) {
            prior_attempt_ids.push(attempt_id.clone());
        }
    }
    let event_refs = item
        .attempt_state
        .as_ref()
        .map(|state| state.last_event_refs.clone())
        .unwrap_or_default();
    let recovery_hints = match prior_gate.kind {
        GateKind::Decision => vec!["claim the item after applying the recorded human decision"],
        GateKind::External => vec!["claim the item after verifying the external condition"],
        GateKind::LoopDetected => vec!["claim the item and start a structurally novel attempt"],
        GateKind::Review => unreachable!("review gates returned above"),
    }
    .into_iter()
    .map(str::to_string)
    .collect();

    item.status = ItemStatus::Pending;
    item.gate = None;
    item.completed_at = None;
    item.claimed_by = None;
    item.lease_expires_at = None;
    checklist.updated = Utc::now();
    write_checklist_atomic(&path, &checklist)?;
    Ok(ResolveReport {
        state: checklist,
        item_id: item_id.to_string(),
        resolved_by: resolved_by.to_string(),
        reason: reason.to_string(),
        prior_gate,
        prior_attempt_ids,
        event_refs,
        recovery_hints,
    })
}

fn validate_human_preference_change(change: &HumanPreferenceChange) -> Result<()> {
    let actor = change.actor.trim();
    if actor
        .strip_prefix("human:")
        .is_none_or(|identity| identity.trim().is_empty())
    {
        bail!("human preference actor must be a non-empty 'human:' identity");
    }
    if change.reason.trim().is_empty() {
        bail!("human preference reason must not be empty");
    }
    Ok(())
}

/// Set, replace, or clear one human preference and append an immutable audit
/// entry. A requested restore resets score penalties and their decay anchor but
/// deliberately preserves exact-attempt fingerprints and participant history.
pub fn update_human_preference(
    dir: &Path,
    name: &str,
    item_id: &str,
    change: HumanPreferenceChange,
) -> Result<Checklist> {
    validate_name(name)?;
    validate_human_preference_change(&change)?;
    let path = checklist_path(dir, name);
    if !path.exists() {
        bail!("checklist '{}' not found at {}", name, path.display());
    }
    let mut cl = read_checklist(&path)?;
    let item = cl
        .items
        .iter_mut()
        .find(|item| item.id == item_id)
        .ok_or_else(|| anyhow!("item id '{}' not found in checklist '{}'", item_id, name))?;
    if item
        .human_preference_audit
        .last()
        .is_some_and(|audit| audit.changed_at > change.changed_at)
    {
        bail!("human preference timestamp precedes the latest audit entry");
    }

    let previous_value = item.human_priority_override;
    let (action, new_value) = match change.operation {
        HumanPreferenceOperation::Set(value) if previous_value.is_none() => {
            (HumanPreferenceAction::Set, Some(value))
        }
        HumanPreferenceOperation::Set(_) => {
            bail!("human preference is already set; replace or clear it")
        }
        HumanPreferenceOperation::Replace(value) if previous_value.is_some() => {
            (HumanPreferenceAction::Replace, Some(value))
        }
        HumanPreferenceOperation::Replace(_) => {
            bail!("human preference is not set; set it before replacing it")
        }
        HumanPreferenceOperation::Clear if previous_value.is_some() => {
            (HumanPreferenceAction::Clear, None)
        }
        HumanPreferenceOperation::Clear => bail!("human preference is already clear"),
    };

    item.human_priority_override = new_value;
    item.priority_state_restored = change.restore_priority_state;
    if change.restore_priority_state {
        if let Some(state) = item.attempt_state.as_mut() {
            state.retry_penalty = 0;
            state.similar_low_progress_count = 0;
            state.goal_retry_penalty = 0;
            state.post_pivot_return_count = 0;
            state.last_progress_at = Some(change.changed_at);
        }
    }
    item.human_preference_audit.push(HumanPreferenceAudit {
        action,
        previous_value,
        new_value,
        actor: change.actor,
        reason: change.reason,
        changed_at: change.changed_at,
        restored_priority_state: change.restore_priority_state,
    });
    cl.updated = change.changed_at;
    write_checklist_atomic(&path, &cl)?;
    Ok(cl)
}

fn validate_review_input(input: &ReviewInput) -> Result<()> {
    for (field, value) in [
        ("review_id", input.review_id.as_str()),
        ("reviewer_id", input.reviewer_id.as_str()),
        ("reason", input.reason.as_str()),
    ] {
        if value.trim().is_empty() {
            bail!("{field} must not be empty");
        }
    }
    if input
        .feedback
        .iter()
        .any(|feedback| feedback.feedback.trim().is_empty())
    {
        bail!("review feedback must not be empty");
    }
    let mut follow_up_ids = BTreeSet::new();
    for follow_up in &input.follow_ups {
        if follow_up.id.trim().is_empty() {
            bail!("follow-up id must not be empty");
        }
        if follow_up.title.trim().is_empty() {
            bail!("follow-up title must not be empty");
        }
        if !follow_up_ids.insert(follow_up.id.as_str()) {
            bail!("duplicate follow-up id '{}'", follow_up.id);
        }
    }
    Ok(())
}

fn validate_review_policy(input: &ReviewInput) -> Result<()> {
    match input.outcome {
        ReviewOutcome::Approved
            if input
                .feedback
                .iter()
                .any(|feedback| feedback.severity == ReviewSeverity::Required)
                || input
                    .follow_ups
                    .iter()
                    .any(|follow_up| follow_up.severity == ReviewSeverity::Required) =>
        {
            bail!("approved reviews may not contain required feedback or follow-ups");
        }
        ReviewOutcome::Disapproved
            if input.follow_ups.is_empty()
                || input
                    .follow_ups
                    .iter()
                    .any(|follow_up| follow_up.severity != ReviewSeverity::Required) =>
        {
            bail!("disapproved reviews require one or more required follow-ups");
        }
        ReviewOutcome::Approved | ReviewOutcome::Disapproved => Ok(()),
    }
}

fn review_follow_up_item(
    follow_up: FollowUpInput,
    review_id: &str,
    source: &ChecklistItem,
) -> ChecklistItem {
    let item_contribution = follow_up.title.clone();
    ChecklistItem {
        id: follow_up.id,
        title: follow_up.title,
        status: ItemStatus::Pending,
        completed_at: None,
        notes: None,
        depends_on: Vec::new(),
        batch: None,
        verification: Vec::new(),
        source_refs: Vec::new(),
        claimed_by: None,
        lease_expires_at: None,
        gate: None,
        goal_ref: source.goal_ref.clone(),
        goal_summary: source.goal_summary.clone(),
        item_contribution: Some(item_contribution),
        base_priority: source.base_priority,
        effort: source.effort,
        critical: source.critical,
        human_priority_override: None,
        human_preference_audit: Vec::new(),
        priority_state_restored: false,
        parent_item_id: Some(source.id.clone()),
        attempt_state: None,
        review: None,
        follow_up: Some(FollowUpLink {
            source_review_id: review_id.to_string(),
            severity: follow_up.severity,
        }),
    }
}

/// Apply one review transaction to a review-gated source item. The checklist
/// is validated in memory and written only after the source transition,
/// feedback linkage, and all follow-up creation succeed.
pub fn apply_review(
    dir: &Path,
    name: &str,
    item_id: &str,
    input: ReviewInput,
) -> Result<Checklist> {
    validate_name(name)?;
    validate_review_input(&input)?;
    validate_review_policy(&input)?;
    let path = checklist_path(dir, name);
    if !path.exists() {
        bail!("checklist '{}' not found at {}", name, path.display());
    }
    let mut checklist = read_checklist(&path)?;
    let source_position = checklist
        .items
        .iter()
        .position(|item| item.id == item_id)
        .ok_or_else(|| anyhow!("item id '{}' not found in checklist '{}'", item_id, name))?;
    let source = &checklist.items[source_position];
    if checklist.items.iter().any(|item| {
        item.review
            .as_ref()
            .is_some_and(|review| review.review_id == input.review_id)
            || item
                .follow_up
                .as_ref()
                .is_some_and(|follow_up| follow_up.source_review_id == input.review_id)
    }) {
        bail!("review_id '{}' is already in use", input.review_id);
    }
    reject_active_attempt_transition(source, "review")?;
    if source.status != ItemStatus::Waiting
        || source.gate.as_ref().map(|gate| gate.kind) != Some(GateKind::Review)
    {
        bail!("item '{}' must be waiting on a review gate", item_id);
    }
    let outcome = input.outcome;
    let reviewed_at = input.reviewed_at;
    let feedback = input
        .feedback
        .into_iter()
        .map(|entry| ReviewFeedback {
            source_review_id: input.review_id.clone(),
            severity: entry.severity,
            feedback: entry.feedback,
        })
        .collect();
    let follow_up_item_ids = input
        .follow_ups
        .iter()
        .map(|follow_up| follow_up.id.clone())
        .collect::<Vec<_>>();
    let follow_up_items = input
        .follow_ups
        .into_iter()
        .map(|follow_up| review_follow_up_item(follow_up, &input.review_id, source))
        .collect::<Vec<_>>();

    let source = &mut checklist.items[source_position];
    source.status = match outcome {
        ReviewOutcome::Approved => ItemStatus::Completed,
        ReviewOutcome::Disapproved => ItemStatus::Pending,
    };
    source.completed_at = (outcome == ReviewOutcome::Approved).then_some(reviewed_at);
    source.claimed_by = None;
    source.lease_expires_at = None;
    source.gate = None;
    if outcome == ReviewOutcome::Disapproved {
        source.depends_on.extend(follow_up_item_ids.iter().cloned());
    }
    source.review = Some(ReviewRecord {
        review_id: input.review_id,
        outcome,
        reviewer_id: input.reviewer_id,
        reviewed_at,
        reason: input.reason,
        feedback,
        follow_up_item_ids,
    });
    checklist.items.extend(follow_up_items);
    let report = validate_dependencies(&checklist);
    if !report.valid {
        bail!("invalid review transaction: {}", report.errors.join("; "));
    }
    checklist.updated = Utc::now();
    write_checklist_atomic(&path, &checklist)?;
    Ok(checklist)
}

/// Declare a higher finite retry count for one genuinely transient operation.
/// The declaration is immutable and must be persisted before its first attempt.
pub fn declare_transient_retry_limit(
    dir: &Path,
    name: &str,
    item_id: &str,
    input: &AttemptFingerprintInput,
    exact_retry_limit: u32,
) -> Result<Checklist> {
    validate_name(name)?;
    if exact_retry_limit <= 1 {
        bail!("a transient exact retry limit must be higher than the default of one");
    }
    let path = checklist_path(dir, name);
    if !path.exists() {
        bail!("checklist '{}' not found at {}", name, path.display());
    }
    let mut checklist = read_checklist(&path)?;
    let execution_fingerprint = attempt_execution_fingerprint(&checklist, item_id, input)?;
    let item = checklist
        .items
        .iter_mut()
        .find(|item| item.id == item_id)
        .ok_or_else(|| anyhow!("item id '{}' not found in checklist '{}'", item_id, name))?;
    let state = item.attempt_state.get_or_insert_with(AttemptState::default);
    let attempt_started = state.next_attempt_number != 0
        || state.active_attempt.is_some()
        || state.last_attempt.is_some()
        || state.last_fingerprint.is_some()
        || !state.exact_attempts.is_empty();
    if attempt_started
        || state.exact_retry_limit.is_some()
        || state.exact_retry_execution_fingerprint.is_some()
    {
        bail!("a transient retry limit may only be declared once before the first attempt");
    }
    state.exact_retry_limit = Some(exact_retry_limit);
    state.exact_retry_execution_fingerprint = Some(execution_fingerprint);
    checklist.updated = Utc::now();
    write_checklist_atomic(&path, &checklist)?;
    Ok(checklist)
}

fn exact_retry_limit(state: &AttemptState, execution_fingerprint: &str) -> u32 {
    if state.exact_retry_execution_fingerprint.as_deref() == Some(execution_fingerprint) {
        state.exact_retry_limit.unwrap_or(1)
    } else {
        1
    }
}

fn reject_active_attempt_transition(item: &ChecklistItem, transition: &str) -> Result<()> {
    if let Some(active) = item
        .attempt_state
        .as_ref()
        .and_then(|state| state.active_attempt.as_ref())
    {
        bail!(
            "cannot {transition} item '{}' while active attempt '{}' exists",
            item.id,
            active.attempt_id
        );
    }
    Ok(())
}

/// Start a bounded exact attempt. The first execution must be performed by an
/// implementer; unchanged retries require fresh verifier actors and stop at the
/// predeclared limit. A forbidden repetition becomes a loop gate, not execution.
pub fn attempt_start(
    dir: &Path,
    name: &str,
    item_id: &str,
    agent_id: &str,
    role: AttemptRole,
    input: AttemptFingerprintInput,
) -> Result<AttemptStartReport> {
    validate_name(name)?;
    if agent_id.trim().is_empty() {
        bail!("agent_id must not be empty");
    }
    let path = checklist_path(dir, name);
    if !path.exists() {
        bail!("checklist '{}' not found at {}", name, path.display());
    }

    let mut checklist = read_checklist(&path)?;
    let execution_fingerprint = attempt_execution_fingerprint(&checklist, item_id, &input)?;
    let position = checklist
        .items
        .iter()
        .position(|item| item.id == item_id)
        .ok_or_else(|| anyhow!("item id '{}' not found in checklist '{}'", item_id, name))?;
    let now = Utc::now();
    let item = &mut checklist.items[position];
    if item.status != ItemStatus::InProgress {
        bail!("item '{}' must be in_progress to start an attempt", item_id);
    }
    if role == AttemptRole::Implementer && item.claimed_by.as_deref() != Some(agent_id) {
        bail!(
            "implementer '{}' does not hold the claim for item '{}'",
            agent_id,
            item_id
        );
    }

    let state = item.attempt_state.get_or_insert_with(AttemptState::default);
    if let Some(active) = &state.active_attempt {
        bail!(
            "attempt '{}' is already active for item '{}'",
            active.attempt_id,
            item_id
        );
    }
    state.next_attempt_number = state
        .next_attempt_number
        .checked_add(1)
        .ok_or_else(|| anyhow!("attempt id counter exhausted for item '{}'", item_id))?;
    let attempt_id = format!("A-{}", state.next_attempt_number);
    let same_execution =
        state.last_execution_fingerprint.as_deref() == Some(execution_fingerprint.as_str());

    let loop_reason = if same_execution {
        if state
            .exact_attempts
            .iter()
            .any(|attempt| attempt.agent_id == agent_id)
        {
            Some("an actor may not repeat within an unchanged attempt sequence")
        } else if state.same_attempt_count > exact_retry_limit(state, &execution_fingerprint) {
            Some("the predeclared exact retry limit is exhausted")
        } else {
            (role != AttemptRole::Verifier)
                .then_some("an unchanged retry requires a fresh verifier")
        }
    } else {
        (role != AttemptRole::Implementer)
            .then_some("a new attempt fingerprint must start with an implementer")
    };

    let decision = if let Some(reason) = loop_reason {
        let mut attempt_ids: Vec<String> = state
            .exact_attempts
            .iter()
            .map(|attempt| attempt.attempt_id.clone())
            .collect();
        attempt_ids.push(attempt_id.clone());
        item.status = ItemStatus::Waiting;
        item.gate = Some(WaitingGate {
            kind: GateKind::LoopDetected,
            created_at: now,
            reason: reason.to_string(),
            question: Some(
                "What structurally novel action or human input should resolve this loop?"
                    .to_string(),
            ),
            attempt_ids,
            artifact_refs: Vec::new(),
        });
        item.completed_at = None;
        item.claimed_by = None;
        item.lease_expires_at = None;
        AttemptDecision::LoopDetected
    } else {
        state.active_attempt = Some(ActiveAttempt {
            attempt_id: attempt_id.clone(),
            execution_fingerprint: execution_fingerprint.clone(),
            agent_id: agent_id.to_string(),
            role,
            started_at: now,
        });
        AttemptDecision::Accepted
    };

    item.priority_state_restored = false;
    checklist.updated = now;
    write_checklist_atomic(&path, &checklist)?;
    Ok(AttemptStartReport {
        attempt_id,
        execution_fingerprint,
        decision,
    })
}

/// Finish the active attempt and persist its normalized completed fingerprint
/// plus the structured result, progress, new-information, and next-action data.
pub fn attempt_finish(
    dir: &Path,
    name: &str,
    item_id: &str,
    attempt_id: &str,
    finish: AttemptFinish,
) -> Result<Checklist> {
    validate_name(name)?;
    for (field, value) in [
        ("result_signature", finish.result_signature.as_str()),
        ("progress", finish.progress.as_str()),
        ("new_information", finish.new_information.as_str()),
        ("next_action", finish.next_action.as_str()),
    ] {
        if value.trim().is_empty() {
            bail!("{field} must not be empty");
        }
    }
    let path = checklist_path(dir, name);
    if !path.exists() {
        bail!("checklist '{}' not found at {}", name, path.display());
    }

    let mut checklist = read_checklist(&path)?;
    let item = checklist
        .items
        .iter_mut()
        .find(|item| item.id == item_id)
        .ok_or_else(|| anyhow!("item id '{}' not found in checklist '{}'", item_id, name))?;
    if item.status != ItemStatus::InProgress {
        bail!(
            "item '{}' must remain in_progress to finish an attempt",
            item_id
        );
    }
    let state = item
        .attempt_state
        .as_mut()
        .ok_or_else(|| anyhow!("item '{}' has no active attempt", item_id))?;
    let active = state
        .active_attempt
        .as_ref()
        .filter(|active| active.attempt_id == attempt_id)
        .cloned()
        .ok_or_else(|| {
            anyhow!(
                "attempt '{}' is not active for item '{}'",
                attempt_id,
                item_id
            )
        })?;
    let fingerprint = completed_fingerprint_from_execution(
        &active.execution_fingerprint,
        &finish.result_signature,
    )?;
    let now = Utc::now();

    if state.last_fingerprint.as_deref() == Some(fingerprint.as_str())
        && state.last_execution_fingerprint.as_deref()
            == Some(active.execution_fingerprint.as_str())
    {
        state.same_attempt_count = state.same_attempt_count.saturating_add(1);
    } else {
        state.same_attempt_count = 1;
        state.exact_attempts.clear();
    }
    state.exact_attempts.push(ExactAttemptParticipant {
        attempt_id: active.attempt_id.clone(),
        agent_id: active.agent_id.clone(),
        role: active.role,
    });
    state.last_fingerprint = Some(fingerprint.clone());
    state.last_execution_fingerprint = Some(active.execution_fingerprint.clone());
    state.last_progress_at = Some(now);
    state.last_attempt = Some(FinishedAttempt {
        attempt_id: active.attempt_id,
        fingerprint,
        execution_fingerprint: active.execution_fingerprint,
        agent_id: active.agent_id,
        role: active.role,
        finished_at: now,
        result_signature: finish.result_signature,
        progress: finish.progress,
        new_information: finish.new_information,
        next_action: finish.next_action,
    });
    state.active_attempt = None;
    checklist.updated = now;
    write_checklist_atomic(&path, &checklist)?;
    Ok(checklist)
}

/// Attach (replace) a free-text note on an item.
pub fn note(dir: &Path, name: &str, item_id: &str, note: &str) -> Result<Checklist> {
    validate_name(name)?;
    let path = checklist_path(dir, name);
    if !path.exists() {
        bail!("checklist '{}' not found at {}", name, path.display());
    }
    let mut cl = read_checklist(&path)?;
    let mut found = false;
    for item in cl.items.iter_mut() {
        if item.id == item_id {
            item.notes = Some(note.to_string());
            found = true;
            break;
        }
    }
    if !found {
        bail!("item id '{}' not found in checklist '{}'", item_id, name);
    }
    cl.updated = Utc::now();
    write_checklist_atomic(&path, &cl)?;
    Ok(cl)
}

/// Delete a checklist file.
pub fn delete(dir: &Path, name: &str) -> Result<()> {
    validate_name(name)?;
    let path = checklist_path(dir, name);
    if !path.exists() {
        bail!("checklist '{}' not found at {}", name, path.display());
    }
    fs::remove_file(&path)
        .with_context(|| format!("failed to remove checklist at {}", path.display()))?;
    Ok(())
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn titles(ts: &[&str]) -> Vec<String> {
        ts.iter().map(|s| s.to_string()).collect()
    }

    fn item(id: &str, deps: &[&str]) -> ChecklistItem {
        ChecklistItem {
            id: id.to_string(),
            title: id.to_string(),
            status: ItemStatus::Pending,
            completed_at: None,
            notes: None,
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            batch: None,
            verification: Vec::new(),
            source_refs: Vec::new(),
            claimed_by: None,
            lease_expires_at: None,
            gate: None,
            goal_ref: None,
            goal_summary: None,
            item_contribution: None,
            base_priority: None,
            effort: None,
            critical: None,
            human_priority_override: None,
            human_preference_audit: Vec::new(),
            priority_state_restored: false,
            parent_item_id: None,
            attempt_state: None,
            review: None,
            follow_up: None,
        }
    }

    fn dag(items: Vec<ChecklistItem>) -> Checklist {
        let now = Utc::now();
        Checklist {
            name: "dag".to_string(),
            created: now,
            updated: now,
            source_skill: Some("compile-project".to_string()),
            schema_version: Some(2),
            source_plan: Some("specs/compiled-project-plan.md".to_string()),
            parallel_policy: Some(ParallelPolicy::WorktreeRequired),
            batch_verification: Vec::new(),
            items,
        }
    }

    fn gate(kind: GateKind) -> WaitingGate {
        WaitingGate {
            kind,
            created_at: "2026-07-12T18:00:00Z".parse().unwrap(),
            reason: "Needs input".to_string(),
            question: Some("Which option?".to_string()),
            attempt_ids: vec!["A-17".to_string()],
            artifact_refs: vec!["specs/proposal.md#ties".to_string()],
        }
    }

    fn score_policy() -> ScorePolicy {
        ScorePolicy {
            default_base_priority: 0,
            ease_bonus: EffortScorePolicy {
                small: 7,
                medium: 3,
                large: 1,
                unspecified: 0,
            },
            dag_unlock: DagUnlockPolicy {
                priority_divisor: 10,
                effort_weight: EffortScorePolicy {
                    small: 3,
                    medium: 2,
                    large: 1,
                    unspecified: 1,
                },
            },
            goal_progress_max_bonus: 12,
            exact_retry_penalty_per_unit: 4,
            semantic_fixation_penalty_per_unit: 6,
            parent_goal_retry_penalty_per_unit: 8,
            minimum_fixated_items_for_goal_penalty: 2,
            minimum_post_pivot_returns_for_goal_penalty: 2,
            decay: PenaltyDecayPolicy {
                interval_seconds: 3_600,
                recovery_per_interval: 2,
            },
            unblock: UnblockBonusPolicy {
                penalized_item: 9,
                penalized_goal: 11,
            },
            critical_visibility_floor: 25,
        }
    }

    fn finish_unchanged_attempt(dir: &Path, name: &str, attempt_id: &str, actor: &str) {
        attempt_finish(
            dir,
            name,
            "T-002",
            attempt_id,
            AttemptFinish {
                result_signature: "temporary service unavailable".to_string(),
                progress: format!("{actor} completed the transient operation"),
                new_information: "the transient result is unchanged".to_string(),
                next_action: "retry within the predeclared bound".to_string(),
            },
        )
        .unwrap();
    }

    // T-004 test list:
    // - [x] Scores expose every named component using only caller-supplied policy values.
    // - [x] DAG unlock value accounts for downstream priority and effort.
    // - [x] A single fixated child does not create a parent-goal penalty.
    // - [x] Cross-item fixation and post-pivot returns can penalize a goal.
    // - [x] Penalties decay without recovering more than the applied penalty.
    // - [x] Work that unblocks penalized items or goals receives configured bonuses.
    // - [x] Critical waiting items remain visible at the floor with help metadata.
    // - [x] Human preference set/replace/clear is audited and can restore priority state.
    #[test]
    fn score_exposes_policy_driven_named_components() {
        let now: DateTime<Utc> = "2026-07-12T20:00:00Z".parse().unwrap();
        let mut prerequisite = item("A", &[]);
        prerequisite.base_priority = Some(40);
        prerequisite.effort = Some(Effort::Small);
        prerequisite.goal_ref = Some("G".to_string());
        prerequisite.human_priority_override = Some(5);
        prerequisite.attempt_state = Some(AttemptState {
            retry_penalty: 2,
            similar_low_progress_count: 1,
            last_progress_at: Some("2026-07-12T18:00:00Z".parse().unwrap()),
            ..AttemptState::default()
        });

        let mut downstream = item("B", &["A"]);
        downstream.base_priority = Some(30);
        downstream.effort = Some(Effort::Medium);
        downstream.goal_ref = Some("G".to_string());
        downstream.attempt_state = Some(AttemptState {
            retry_penalty: 1,
            ..AttemptState::default()
        });

        let mut completed = item("done", &[]);
        completed.status = ItemStatus::Completed;
        completed.base_priority = Some(10);
        completed.effort = Some(Effort::Large);
        completed.goal_ref = Some("G".to_string());

        let policy = score_policy();

        let report = score_checklist(
            &dag(vec![prerequisite, downstream, completed]),
            &policy,
            now,
        )
        .unwrap();
        let score = report
            .items
            .iter()
            .find(|score| score.item_id == "A")
            .unwrap();

        assert_eq!(score.components.base_priority, 40);
        assert_eq!(score.components.ease_bonus, 7);
        assert_eq!(score.components.dag_unlock_bonus, 6);
        assert_eq!(score.components.goal_progress_bonus, 4);
        assert_eq!(score.components.human_preference, 5);
        assert_eq!(score.components.exact_retry_penalty, -8);
        assert_eq!(score.components.semantic_fixation_penalty, -6);
        assert_eq!(score.components.parent_goal_retry_penalty, 0);
        assert_eq!(score.components.decay_recovery, 4);
        assert_eq!(score.components.unblock_bonus, 9);
        assert_eq!(score.components.critical_visibility_floor, 0);
        assert_eq!(score.effective_score, 61);
        assert!(score.explanation.contains("base 40"));
        assert!(score.explanation.contains("DAG unlock +6"));
    }

    #[test]
    fn priority_preference_set_replace_clear_is_audited_and_can_restore_state() {
        let tmp = TempDir::new().unwrap();
        let changed_at: DateTime<Utc> = "2026-07-12T21:00:00Z".parse().unwrap();
        let mut target = item("T-004", &[]);
        target.attempt_state = Some(AttemptState {
            same_attempt_count: 3,
            similar_low_progress_count: 2,
            retry_penalty: 3,
            goal_retry_penalty: 4,
            post_pivot_return_count: 1,
            last_progress_at: Some("2026-07-12T18:00:00Z".parse().unwrap()),
            ..AttemptState::default()
        });
        create_dag_from_items(tmp.path(), "priority-audit", vec![target]).unwrap();

        let checklist = update_human_preference(
            tmp.path(),
            "priority-audit",
            "T-004",
            HumanPreferenceChange {
                operation: HumanPreferenceOperation::Set(20),
                actor: "human:owner".to_string(),
                reason: "Restore release-blocking work".to_string(),
                changed_at,
                restore_priority_state: true,
            },
        )
        .unwrap();
        let target = &checklist.items[0];
        assert_eq!(target.human_priority_override, Some(20));
        assert!(target.priority_state_restored);
        let state = target.attempt_state.as_ref().unwrap();
        assert_eq!(
            state.same_attempt_count, 3,
            "exact guard history is preserved"
        );
        assert_eq!(state.retry_penalty, 0);
        assert_eq!(state.similar_low_progress_count, 0);
        assert_eq!(state.goal_retry_penalty, 0);
        assert_eq!(state.post_pivot_return_count, 0);
        assert_eq!(state.last_progress_at, Some(changed_at));
        assert_eq!(target.human_preference_audit.len(), 1);
        assert_eq!(
            target.human_preference_audit[0],
            HumanPreferenceAudit {
                action: HumanPreferenceAction::Set,
                previous_value: None,
                new_value: Some(20),
                actor: "human:owner".to_string(),
                reason: "Restore release-blocking work".to_string(),
                changed_at,
                restored_priority_state: true,
            }
        );

        let checklist = update_human_preference(
            tmp.path(),
            "priority-audit",
            "T-004",
            HumanPreferenceChange {
                operation: HumanPreferenceOperation::Replace(30),
                actor: "human:owner".to_string(),
                reason: "Increase preference after review".to_string(),
                changed_at: changed_at + Duration::minutes(1),
                restore_priority_state: false,
            },
        )
        .unwrap();
        assert_eq!(checklist.items[0].human_priority_override, Some(30));
        assert!(!checklist.items[0].priority_state_restored);
        assert_eq!(checklist.items[0].human_preference_audit.len(), 2);
        assert_eq!(
            checklist.items[0].human_preference_audit[1].action,
            HumanPreferenceAction::Replace
        );

        let checklist = update_human_preference(
            tmp.path(),
            "priority-audit",
            "T-004",
            HumanPreferenceChange {
                operation: HumanPreferenceOperation::Clear,
                actor: "human:owner".to_string(),
                reason: "Return to policy score".to_string(),
                changed_at: changed_at + Duration::minutes(2),
                restore_priority_state: false,
            },
        )
        .unwrap();
        assert_eq!(checklist.items[0].human_priority_override, None);
        assert!(!checklist.items[0].priority_state_restored);
        assert_eq!(checklist.items[0].human_preference_audit.len(), 3);
        assert_eq!(
            checklist.items[0].human_preference_audit[2].action,
            HumanPreferenceAction::Clear
        );
        let persisted = show(tmp.path(), "priority-audit").unwrap();
        assert_eq!(
            persisted.items[0].human_preference_audit,
            checklist.items[0].human_preference_audit
        );
        assert_eq!(
            persisted.items[0].human_preference_audit[1].previous_value,
            Some(20)
        );
        assert_eq!(
            persisted.items[0].human_preference_audit[1].new_value,
            Some(30)
        );
        assert_eq!(
            persisted.items[0].human_preference_audit[2].previous_value,
            Some(30)
        );
        assert_eq!(persisted.items[0].human_preference_audit[2].new_value, None);
    }

    #[test]
    fn priority_preference_rejects_agents_blank_reasons_and_invalid_lifecycle_atomically() {
        let tmp = TempDir::new().unwrap();
        create_dag_from_items(tmp.path(), "preference-policy", vec![item("T-004", &[])]).unwrap();
        let path = checklist_path(tmp.path(), "preference-policy");
        let before = fs::read(&path).unwrap();
        let at: DateTime<Utc> = "2026-07-12T21:00:00Z".parse().unwrap();

        for (operation, actor, reason) in [
            (
                HumanPreferenceOperation::Set(10),
                "agent:worker",
                "agent cannot override score",
            ),
            (HumanPreferenceOperation::Set(10), "human:owner", "  "),
            (
                HumanPreferenceOperation::Replace(10),
                "human:owner",
                "nothing exists to replace",
            ),
            (
                HumanPreferenceOperation::Clear,
                "human:owner",
                "nothing exists to clear",
            ),
        ] {
            assert!(update_human_preference(
                tmp.path(),
                "preference-policy",
                "T-004",
                HumanPreferenceChange {
                    operation,
                    actor: actor.to_string(),
                    reason: reason.to_string(),
                    changed_at: at,
                    restore_priority_state: false,
                },
            )
            .is_err());
            assert_eq!(fs::read(&path).unwrap(), before);
        }
    }

    #[test]
    fn priority_human_restore_suppresses_inherited_goal_penalty_for_that_item_only() {
        let tmp = TempDir::new().unwrap();
        let at: DateTime<Utc> = "2026-07-12T21:00:00Z".parse().unwrap();
        let mut restored = item("restored", &[]);
        restored.goal_ref = Some("G".to_string());
        let mut sibling = item("sibling", &[]);
        sibling.goal_ref = Some("G".to_string());
        sibling.attempt_state = Some(AttemptState {
            post_pivot_return_count: 2,
            ..AttemptState::default()
        });
        create_dag_from_items(tmp.path(), "goal-restore", vec![restored, sibling]).unwrap();

        let checklist = update_human_preference(
            tmp.path(),
            "goal-restore",
            "restored",
            HumanPreferenceChange {
                operation: HumanPreferenceOperation::Set(10),
                actor: "human:owner".to_string(),
                reason: "Restore this item after reviewing the pivot".to_string(),
                changed_at: at,
                restore_priority_state: true,
            },
        )
        .unwrap();
        let report = score_checklist(&checklist, &score_policy(), at).unwrap();

        assert_eq!(report.items[0].components.parent_goal_retry_penalty, 0);
        assert_eq!(
            report.items[1].components.parent_goal_retry_penalty, -8,
            "restoring one child does not erase goal history for its sibling"
        );
    }

    #[test]
    fn priority_goal_penalty_requires_cross_item_fixation_or_repeated_post_pivot_returns() {
        let now: DateTime<Utc> = "2026-07-12T20:00:00Z".parse().unwrap();
        let mut first = item("first", &[]);
        first.goal_ref = Some("G".to_string());
        first.attempt_state = Some(AttemptState {
            similar_low_progress_count: 3,
            goal_retry_penalty: 2,
            post_pivot_return_count: 1,
            ..AttemptState::default()
        });

        let single = score_checklist(&dag(vec![first.clone()]), &score_policy(), now).unwrap();
        assert_eq!(
            single.items[0].components.parent_goal_retry_penalty, 0,
            "one stuck child and one post-pivot return do not penalize its goal"
        );

        let mut second = item("second", &[]);
        second.goal_ref = Some("G".to_string());
        second.attempt_state = Some(AttemptState {
            similar_low_progress_count: 1,
            ..AttemptState::default()
        });
        let cross_item = score_checklist(&dag(vec![first, second]), &score_policy(), now).unwrap();
        assert!(cross_item
            .items
            .iter()
            .all(|score| score.components.parent_goal_retry_penalty < 0));

        let mut returned = item("returned", &[]);
        returned.goal_ref = Some("H".to_string());
        returned.attempt_state = Some(AttemptState {
            post_pivot_return_count: 2,
            ..AttemptState::default()
        });
        let repeated = score_checklist(&dag(vec![returned]), &score_policy(), now).unwrap();
        assert_eq!(repeated.items[0].components.parent_goal_retry_penalty, -8);
    }

    #[test]
    fn score_keeps_critical_waiting_item_at_floor_with_help_metadata() {
        let now: DateTime<Utc> = "2026-07-12T20:00:00Z".parse().unwrap();
        let mut waiting = item("critical", &[]);
        waiting.status = ItemStatus::Waiting;
        waiting.gate = Some(WaitingGate {
            kind: GateKind::Decision,
            created_at: now,
            reason: "Release choice is missing".to_string(),
            question: Some("Ship with the compatibility path?".to_string()),
            attempt_ids: Vec::new(),
            artifact_refs: Vec::new(),
        });
        waiting.critical = Some(true);
        waiting.item_contribution = Some("Unblock the release".to_string());
        waiting.attempt_state = Some(AttemptState {
            retry_penalty: 100,
            similar_low_progress_count: 100,
            last_progress_at: Some(now),
            ..AttemptState::default()
        });
        let dependent = item("ship", &["critical"]);

        let report = score_checklist(&dag(vec![waiting, dependent]), &score_policy(), now).unwrap();

        assert_eq!(report.items[0].item_id, "critical");
        assert_eq!(report.items[0].effective_score, 25);
        assert!(report.items[0].components.critical_visibility_floor > 0);
        let help = report.items[0].help.as_ref().unwrap();
        assert_eq!(help.gate_kind, GateKind::Decision);
        assert_eq!(help.reason, "Release choice is missing");
        assert_eq!(
            help.question.as_deref(),
            Some("Ship with the compatibility path?")
        );
        assert_eq!(help.why_it_matters, "Unblock the release");
        assert_eq!(help.unlocks, vec!["ship"]);
        assert_eq!(report.items[1].item_id, "ship", "scoring does not reorder");
    }

    #[test]
    fn score_caps_decay_and_rewards_paths_to_penalized_items_and_goals() {
        let now: DateTime<Utc> = "2026-07-12T20:00:00Z".parse().unwrap();
        let mut decayed = item("decayed", &[]);
        decayed.attempt_state = Some(AttemptState {
            retry_penalty: 1,
            last_progress_at: Some("2026-07-12T10:00:00Z".parse().unwrap()),
            ..AttemptState::default()
        });
        let decay_report = score_checklist(&dag(vec![decayed]), &score_policy(), now).unwrap();
        assert_eq!(decay_report.items[0].components.exact_retry_penalty, -4);
        assert_eq!(decay_report.items[0].components.decay_recovery, 4);

        let root = item("root", &[]);
        let mut first = item("first", &["root"]);
        first.goal_ref = Some("penalized-goal".to_string());
        first.attempt_state = Some(AttemptState {
            similar_low_progress_count: 1,
            ..AttemptState::default()
        });
        let mut second = item("second", &["first"]);
        second.goal_ref = Some("penalized-goal".to_string());
        second.attempt_state = Some(AttemptState {
            retry_penalty: 1,
            similar_low_progress_count: 1,
            ..AttemptState::default()
        });
        let unblock_report =
            score_checklist(&dag(vec![root, first, second]), &score_policy(), now).unwrap();
        assert_eq!(
            unblock_report.items[0].components.unblock_bonus, 29,
            "two penalized descendants and one distinct penalized goal"
        );
    }

    // T-003 test list:
    // - [x] Disapproval atomically records review/feedback and creates required dependencies.
    // - [x] Approval completes the source and permits only optional/informational follow-ups.
    // - [x] Malformed or policy-invalid reviews preserve the checklist bytes.
    // - [x] Review and follow-up linkage round-trips through checklist JSON.
    // - [x] An active attempt prevents review from changing checklist state.
    #[test]
    fn review_disapproved_atomically_creates_required_follow_up_dependency() {
        let tmp = TempDir::new().unwrap();
        let mut source = item("T-003", &[]);
        source.status = ItemStatus::Waiting;
        source.gate = Some(gate(GateKind::Review));
        create_dag_from_items(tmp.path(), "reviews", vec![source]).unwrap();
        let reviewed_at = "2026-07-12T19:00:00Z".parse().unwrap();

        apply_review(
            tmp.path(),
            "reviews",
            "T-003",
            ReviewInput {
                review_id: "R-003".to_string(),
                outcome: ReviewOutcome::Disapproved,
                reviewer_id: "human:reviewer".to_string(),
                reviewed_at,
                reason: "Atomic rollback needs explicit coverage".to_string(),
                feedback: vec![ReviewFeedbackInput {
                    severity: ReviewSeverity::Required,
                    feedback: "Add byte-preservation assertions".to_string(),
                }],
                follow_ups: vec![FollowUpInput {
                    id: "T-003-F1".to_string(),
                    title: "Cover review rollback".to_string(),
                    severity: ReviewSeverity::Required,
                }],
            },
        )
        .unwrap();
        let checklist = show(tmp.path(), "reviews").unwrap();

        let source = &checklist.items[0];
        assert_eq!(source.status, ItemStatus::Pending);
        assert_eq!(source.depends_on, vec!["T-003-F1"]);
        assert!(source.gate.is_none());
        assert!(source.claimed_by.is_none());
        assert!(source.lease_expires_at.is_none());
        let review = source.review.as_ref().unwrap();
        assert_eq!(review.review_id, "R-003");
        assert_eq!(review.outcome, ReviewOutcome::Disapproved);
        assert_eq!(review.reviewer_id, "human:reviewer");
        assert_eq!(review.reviewed_at, reviewed_at);
        assert_eq!(review.reason, "Atomic rollback needs explicit coverage");
        assert_eq!(review.follow_up_item_ids, vec!["T-003-F1"]);
        assert_eq!(review.feedback.len(), 1);
        assert_eq!(review.feedback[0].source_review_id, "R-003");
        assert_eq!(review.feedback[0].severity, ReviewSeverity::Required);

        let follow_up = &checklist.items[1];
        assert_eq!(follow_up.id, "T-003-F1");
        assert_eq!(follow_up.status, ItemStatus::Pending);
        assert_eq!(
            follow_up.follow_up.as_ref(),
            Some(&FollowUpLink {
                source_review_id: "R-003".to_string(),
                severity: ReviewSeverity::Required,
            })
        );
    }

    #[test]
    fn review_approved_completes_source_with_optional_and_informational_follow_ups() {
        let tmp = TempDir::new().unwrap();
        let mut source = item("T-003", &[]);
        source.status = ItemStatus::Waiting;
        source.gate = Some(gate(GateKind::Review));
        create_dag_from_items(tmp.path(), "approval", vec![source]).unwrap();
        let reviewed_at = "2026-07-12T20:00:00Z".parse().unwrap();

        let checklist = apply_review(
            tmp.path(),
            "approval",
            "T-003",
            ReviewInput {
                review_id: "R-004".to_string(),
                outcome: ReviewOutcome::Approved,
                reviewer_id: "human:reviewer".to_string(),
                reviewed_at,
                reason: "The required behavior is complete".to_string(),
                feedback: vec![ReviewFeedbackInput {
                    severity: ReviewSeverity::Informational,
                    feedback: "Keep the mutation API narrow".to_string(),
                }],
                follow_ups: vec![
                    FollowUpInput {
                        id: "T-003-F2".to_string(),
                        title: "Consider a convenience builder".to_string(),
                        severity: ReviewSeverity::Optional,
                    },
                    FollowUpInput {
                        id: "T-003-F3".to_string(),
                        title: "Document downstream CLI mapping".to_string(),
                        severity: ReviewSeverity::Informational,
                    },
                ],
            },
        )
        .unwrap();

        let source = &checklist.items[0];
        assert_eq!(source.status, ItemStatus::Completed);
        assert_eq!(source.completed_at, Some(reviewed_at));
        assert!(source.depends_on.is_empty());
        assert!(source.gate.is_none());
        assert_eq!(
            source.review.as_ref().unwrap().follow_up_item_ids,
            vec!["T-003-F2", "T-003-F3"]
        );
        assert_eq!(
            checklist.items[1].follow_up.as_ref().unwrap(),
            &FollowUpLink {
                source_review_id: "R-004".to_string(),
                severity: ReviewSeverity::Optional,
            }
        );
        assert_eq!(
            checklist.items[2].follow_up.as_ref().unwrap().severity,
            ReviewSeverity::Informational
        );
    }

    #[test]
    fn review_policy_invalid_follow_up_leaves_checklist_bytes_unchanged() {
        let tmp = TempDir::new().unwrap();
        let mut source = item("T-003", &[]);
        source.status = ItemStatus::Waiting;
        source.gate = Some(gate(GateKind::Review));
        create_dag_from_items(tmp.path(), "review-policy", vec![source]).unwrap();
        let path = checklist_path(tmp.path(), "review-policy");
        let before = fs::read(&path).unwrap();

        let error = apply_review(
            tmp.path(),
            "review-policy",
            "T-003",
            ReviewInput {
                review_id: "R-invalid".to_string(),
                outcome: ReviewOutcome::Approved,
                reviewer_id: "human:reviewer".to_string(),
                reviewed_at: "2026-07-12T20:30:00Z".parse().unwrap(),
                reason: "Approved with contradictory required work".to_string(),
                feedback: Vec::new(),
                follow_ups: vec![FollowUpInput {
                    id: "T-required".to_string(),
                    title: "Required correction".to_string(),
                    severity: ReviewSeverity::Required,
                }],
            },
        )
        .unwrap_err();

        assert!(error.to_string().contains("approved"));
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn follow_up_disapproved_requires_one_or_more_required_items_without_writes() {
        let tmp = TempDir::new().unwrap();
        let mut source = item("T-003", &[]);
        source.status = ItemStatus::Waiting;
        source.gate = Some(gate(GateKind::Review));
        create_dag_from_items(tmp.path(), "disapproval-policy", vec![source]).unwrap();
        let path = checklist_path(tmp.path(), "disapproval-policy");
        let before = fs::read(&path).unwrap();
        let base = ReviewInput {
            review_id: "R-invalid".to_string(),
            outcome: ReviewOutcome::Disapproved,
            reviewer_id: "human:reviewer".to_string(),
            reviewed_at: "2026-07-12T20:45:00Z".parse().unwrap(),
            reason: "Corrections are required".to_string(),
            feedback: Vec::new(),
            follow_ups: Vec::new(),
        };

        let no_follow_up =
            apply_review(tmp.path(), "disapproval-policy", "T-003", base.clone()).unwrap_err();
        assert!(no_follow_up.to_string().contains("required follow-up"));
        assert_eq!(fs::read(&path).unwrap(), before);

        let optional_follow_up = apply_review(
            tmp.path(),
            "disapproval-policy",
            "T-003",
            ReviewInput {
                follow_ups: vec![FollowUpInput {
                    id: "T-optional".to_string(),
                    title: "Optional work".to_string(),
                    severity: ReviewSeverity::Optional,
                }],
                ..base
            },
        )
        .unwrap_err();
        assert!(optional_follow_up
            .to_string()
            .contains("required follow-up"));
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn review_malformed_input_is_rejected_without_changing_checklist_bytes() {
        let base = ReviewInput {
            review_id: "R-valid".to_string(),
            outcome: ReviewOutcome::Approved,
            reviewer_id: "human:reviewer".to_string(),
            reviewed_at: "2026-07-12T21:00:00Z".parse().unwrap(),
            reason: "Review is complete".to_string(),
            feedback: vec![ReviewFeedbackInput {
                severity: ReviewSeverity::Informational,
                feedback: "Useful context".to_string(),
            }],
            follow_ups: vec![FollowUpInput {
                id: "T-follow-up".to_string(),
                title: "Optional cleanup".to_string(),
                severity: ReviewSeverity::Optional,
            }],
        };
        let mut cases = Vec::new();
        cases.push((
            "blank review id",
            ReviewInput {
                review_id: "  ".to_string(),
                ..base.clone()
            },
        ));
        cases.push((
            "blank reviewer id",
            ReviewInput {
                reviewer_id: "\t".to_string(),
                ..base.clone()
            },
        ));
        cases.push((
            "blank reason",
            ReviewInput {
                reason: "".to_string(),
                ..base.clone()
            },
        ));
        cases.push((
            "blank feedback",
            ReviewInput {
                feedback: vec![ReviewFeedbackInput {
                    severity: ReviewSeverity::Informational,
                    feedback: " ".to_string(),
                }],
                ..base.clone()
            },
        ));
        cases.push((
            "blank follow-up id",
            ReviewInput {
                follow_ups: vec![FollowUpInput {
                    id: "".to_string(),
                    title: "Optional cleanup".to_string(),
                    severity: ReviewSeverity::Optional,
                }],
                ..base.clone()
            },
        ));
        cases.push((
            "blank follow-up title",
            ReviewInput {
                follow_ups: vec![FollowUpInput {
                    id: "T-follow-up".to_string(),
                    title: " ".to_string(),
                    severity: ReviewSeverity::Optional,
                }],
                ..base.clone()
            },
        ));
        cases.push((
            "duplicate follow-up id",
            ReviewInput {
                follow_ups: vec![base.follow_ups[0].clone(), base.follow_ups[0].clone()],
                ..base.clone()
            },
        ));
        cases.push((
            "follow-up id collides with source",
            ReviewInput {
                follow_ups: vec![FollowUpInput {
                    id: "T-003".to_string(),
                    title: "Collision".to_string(),
                    severity: ReviewSeverity::Optional,
                }],
                ..base
            },
        ));

        for (case, input) in cases {
            let tmp = TempDir::new().unwrap();
            let mut source = item("T-003", &[]);
            source.status = ItemStatus::Waiting;
            source.gate = Some(gate(GateKind::Review));
            create_dag_from_items(tmp.path(), "malformed", vec![source]).unwrap();
            let path = checklist_path(tmp.path(), "malformed");
            let before = fs::read(&path).unwrap();

            let error = apply_review(tmp.path(), "malformed", "T-003", input).expect_err(case);
            assert!(!error.to_string().is_empty(), "{case}");
            assert_eq!(fs::read(&path).unwrap(), before, "{case}");
        }
    }

    #[test]
    fn review_cannot_bypass_an_active_attempt() {
        let tmp = TempDir::new().unwrap();
        create_dag_from_items(tmp.path(), "active-review", vec![item("T-003", &[])]).unwrap();
        claim(
            tmp.path(),
            "active-review",
            "agent:implementer",
            1,
            30,
            false,
        )
        .unwrap();
        attempt_start(
            tmp.path(),
            "active-review",
            "T-003",
            "agent:implementer",
            AttemptRole::Implementer,
            AttemptFingerprintInput {
                acceptance_criterion: "review cannot consume active attempt".to_string(),
                relevant_inputs: vec![ScopedInputDigest {
                    path: "src/lib.rs".to_string(),
                    digest: "sha256:review".to_string(),
                }],
                normalized_command: "cargo test review".to_string(),
            },
        )
        .unwrap();
        let path = checklist_path(tmp.path(), "active-review");
        let before = fs::read(&path).unwrap();

        let error = apply_review(
            tmp.path(),
            "active-review",
            "T-003",
            ReviewInput {
                review_id: "R-active".to_string(),
                outcome: ReviewOutcome::Disapproved,
                reviewer_id: "human:reviewer".to_string(),
                reviewed_at: "2026-07-12T21:15:00Z".parse().unwrap(),
                reason: "Attempt must finish first".to_string(),
                feedback: Vec::new(),
                follow_ups: vec![FollowUpInput {
                    id: "T-active-follow-up".to_string(),
                    title: "Finish active attempt".to_string(),
                    severity: ReviewSeverity::Required,
                }],
            },
        )
        .unwrap_err();

        assert!(error.to_string().contains("active attempt"));
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn review_correction_rejects_reused_provenance_and_inherits_follow_up_context() {
        let tmp = TempDir::new().unwrap();
        let reviewed_at = "2026-07-12T21:30:00Z".parse().unwrap();
        let mut source = item("T-003", &[]);
        source.status = ItemStatus::Waiting;
        source.gate = Some(gate(GateKind::Review));
        source.goal_ref = Some("G-anti-loop".to_string());
        source.goal_summary = Some("Ship bounded anti-loop control".to_string());
        source.base_priority = Some(80);
        source.effort = Some(Effort::Small);
        source.critical = Some(true);

        let mut prior_review = item("T-prior-review", &[]);
        prior_review.status = ItemStatus::Completed;
        prior_review.review = Some(ReviewRecord {
            review_id: "R-used-record".to_string(),
            outcome: ReviewOutcome::Approved,
            reviewer_id: "human:prior".to_string(),
            reviewed_at,
            reason: "Prior decision".to_string(),
            feedback: Vec::new(),
            follow_up_item_ids: Vec::new(),
        });
        let mut prior_follow_up = item("T-prior-follow-up", &[]);
        prior_follow_up.follow_up = Some(FollowUpLink {
            source_review_id: "R-used-link".to_string(),
            severity: ReviewSeverity::Informational,
        });
        create_dag_from_items(
            tmp.path(),
            "review-correction",
            vec![source, prior_review, prior_follow_up],
        )
        .unwrap();
        let path = checklist_path(tmp.path(), "review-correction");
        let before = fs::read(&path).unwrap();

        for review_id in ["R-used-record", "R-used-link"] {
            let error = apply_review(
                tmp.path(),
                "review-correction",
                "T-003",
                ReviewInput {
                    review_id: review_id.to_string(),
                    outcome: ReviewOutcome::Approved,
                    reviewer_id: "human:reviewer".to_string(),
                    reviewed_at,
                    reason: "Must use unique provenance".to_string(),
                    feedback: Vec::new(),
                    follow_ups: Vec::new(),
                },
            )
            .unwrap_err();
            assert!(error.to_string().contains("review_id"), "{review_id}");
            assert_eq!(fs::read(&path).unwrap(), before, "{review_id}");
        }

        let required_feedback = apply_review(
            tmp.path(),
            "review-correction",
            "T-003",
            ReviewInput {
                review_id: "R-required-feedback".to_string(),
                outcome: ReviewOutcome::Approved,
                reviewer_id: "human:reviewer".to_string(),
                reviewed_at,
                reason: "Required feedback contradicts approval".to_string(),
                feedback: vec![ReviewFeedbackInput {
                    severity: ReviewSeverity::Required,
                    feedback: "Correct this before approval".to_string(),
                }],
                follow_ups: Vec::new(),
            },
        )
        .unwrap_err();
        assert!(required_feedback.to_string().contains("required"));
        assert_eq!(fs::read(&path).unwrap(), before);

        let follow_up_title = "Document inherited scheduling context";
        let checklist = apply_review(
            tmp.path(),
            "review-correction",
            "T-003",
            ReviewInput {
                review_id: "R-unique".to_string(),
                outcome: ReviewOutcome::Approved,
                reviewer_id: "human:reviewer".to_string(),
                reviewed_at,
                reason: "Correction is complete".to_string(),
                feedback: Vec::new(),
                follow_ups: vec![FollowUpInput {
                    id: "T-context-follow-up".to_string(),
                    title: follow_up_title.to_string(),
                    severity: ReviewSeverity::Optional,
                }],
            },
        )
        .unwrap();

        let follow_up = checklist
            .items
            .iter()
            .find(|item| item.id == "T-context-follow-up")
            .unwrap();
        assert_eq!(follow_up.goal_ref.as_deref(), Some("G-anti-loop"));
        assert_eq!(
            follow_up.goal_summary.as_deref(),
            Some("Ship bounded anti-loop control")
        );
        assert_eq!(follow_up.base_priority, Some(80));
        assert_eq!(follow_up.effort, Some(Effort::Small));
        assert_eq!(follow_up.critical, Some(true));
        assert_eq!(follow_up.parent_item_id.as_deref(), Some("T-003"));
        assert_eq!(
            follow_up.item_contribution.as_deref(),
            Some(follow_up_title)
        );
    }

    #[test]
    fn transient_attempt_retry_limit_is_predeclared_bounded_and_uses_fresh_verifiers() {
        let tmp = TempDir::new().unwrap();
        let input = AttemptFingerprintInput {
            acceptance_criterion: "transient operation eventually succeeds".to_string(),
            relevant_inputs: vec![ScopedInputDigest {
                path: "service/status".to_string(),
                digest: "sha256:abc".to_string(),
            }],
            normalized_command: "check transient service".to_string(),
        };
        create_dag_from_items(tmp.path(), "transient", vec![item("T-002", &[])]).unwrap();
        claim(tmp.path(), "transient", "agent:implementer", 1, 30, false).unwrap();

        let before_declaration = show(tmp.path(), "transient").unwrap();
        let declared_fingerprint =
            attempt_execution_fingerprint(&before_declaration, "T-002", &input).unwrap();
        let declared =
            declare_transient_retry_limit(tmp.path(), "transient", "T-002", &input, 3).unwrap();
        let declared_state = declared.items[0].attempt_state.as_ref().unwrap();
        assert_eq!(declared_state.exact_retry_limit, Some(3));
        assert_eq!(
            declared_state.exact_retry_execution_fingerprint.as_deref(),
            Some(declared_fingerprint.as_str())
        );

        let implementation = attempt_start(
            tmp.path(),
            "transient",
            "T-002",
            "agent:implementer",
            AttemptRole::Implementer,
            input.clone(),
        )
        .unwrap();
        assert_eq!(implementation.decision, AttemptDecision::Accepted);
        finish_unchanged_attempt(
            tmp.path(),
            "transient",
            &implementation.attempt_id,
            "agent:implementer",
        );

        let late_raise =
            declare_transient_retry_limit(tmp.path(), "transient", "T-002", &input, 4).unwrap_err();
        assert!(late_raise.to_string().contains("before the first attempt"));

        for (actor, expected_id) in [
            ("agent:verifier-1", "A-2"),
            ("agent:verifier-2", "A-3"),
            ("agent:verifier-3", "A-4"),
        ] {
            let retry = attempt_start(
                tmp.path(),
                "transient",
                "T-002",
                actor,
                AttemptRole::Verifier,
                input.clone(),
            )
            .unwrap();
            assert_eq!(retry.attempt_id, expected_id);
            assert_eq!(retry.decision, AttemptDecision::Accepted);
            finish_unchanged_attempt(tmp.path(), "transient", &retry.attempt_id, actor);
        }

        let over_limit = attempt_start(
            tmp.path(),
            "transient",
            "T-002",
            "agent:verifier-4",
            AttemptRole::Verifier,
            input.clone(),
        )
        .unwrap();
        assert_eq!(over_limit.attempt_id, "A-5");
        assert_eq!(over_limit.decision, AttemptDecision::LoopDetected);

        create_dag_from_items(tmp.path(), "novel", vec![item("T-002", &[])]).unwrap();
        claim(tmp.path(), "novel", "agent:novel-implementer", 1, 30, false).unwrap();
        declare_transient_retry_limit(tmp.path(), "novel", "T-002", &input, 3).unwrap();
        let original = attempt_start(
            tmp.path(),
            "novel",
            "T-002",
            "agent:novel-implementer",
            AttemptRole::Implementer,
            input.clone(),
        )
        .unwrap();
        finish_unchanged_attempt(
            tmp.path(),
            "novel",
            &original.attempt_id,
            "agent:novel-implementer",
        );

        let mut novel_input = input.clone();
        novel_input.normalized_command =
            "check transient service through alternate endpoint".to_string();
        let novel_implementation = attempt_start(
            tmp.path(),
            "novel",
            "T-002",
            "agent:novel-implementer",
            AttemptRole::Implementer,
            novel_input.clone(),
        )
        .unwrap();
        assert_eq!(novel_implementation.decision, AttemptDecision::Accepted);
        finish_unchanged_attempt(
            tmp.path(),
            "novel",
            &novel_implementation.attempt_id,
            "agent:novel-implementer",
        );
        let novel_verification = attempt_start(
            tmp.path(),
            "novel",
            "T-002",
            "agent:novel-verifier-1",
            AttemptRole::Verifier,
            novel_input.clone(),
        )
        .unwrap();
        assert_eq!(novel_verification.decision, AttemptDecision::Accepted);
        finish_unchanged_attempt(
            tmp.path(),
            "novel",
            &novel_verification.attempt_id,
            "agent:novel-verifier-1",
        );
        let novel_over_default = attempt_start(
            tmp.path(),
            "novel",
            "T-002",
            "agent:novel-verifier-2",
            AttemptRole::Verifier,
            novel_input,
        )
        .unwrap();
        assert_eq!(novel_over_default.decision, AttemptDecision::LoopDetected);

        create_dag_from_items(tmp.path(), "transient-self", vec![item("T-002", &[])]).unwrap();
        claim(tmp.path(), "transient-self", "agent:same", 1, 30, false).unwrap();
        declare_transient_retry_limit(tmp.path(), "transient-self", "T-002", &input, 3).unwrap();
        let first = attempt_start(
            tmp.path(),
            "transient-self",
            "T-002",
            "agent:same",
            AttemptRole::Implementer,
            input.clone(),
        )
        .unwrap();
        finish_unchanged_attempt(
            tmp.path(),
            "transient-self",
            &first.attempt_id,
            "agent:same",
        );
        let self_repeat = attempt_start(
            tmp.path(),
            "transient-self",
            "T-002",
            "agent:same",
            AttemptRole::Verifier,
            input.clone(),
        )
        .unwrap();
        assert_eq!(self_repeat.decision, AttemptDecision::LoopDetected);
    }

    #[test]
    fn active_attempt_rejects_transitions_and_finish_requires_in_progress_without_consuming_token()
    {
        let tmp = TempDir::new().unwrap();
        create_dag_from_items(tmp.path(), "active", vec![item("T-002", &[])]).unwrap();
        claim(tmp.path(), "active", "agent:implementer", 1, 30, false).unwrap();
        let started = attempt_start(
            tmp.path(),
            "active",
            "T-002",
            "agent:implementer",
            AttemptRole::Implementer,
            AttemptFingerprintInput {
                acceptance_criterion: "active attempt remains owned".to_string(),
                relevant_inputs: vec![ScopedInputDigest {
                    path: "src/lib.rs".to_string(),
                    digest: "sha256:active".to_string(),
                }],
                normalized_command: "run active attempt".to_string(),
            },
        )
        .unwrap();
        let path = checklist_path(tmp.path(), "active");
        let active_bytes = fs::read(&path).unwrap();

        let release_error =
            release(tmp.path(), "active", "T-002", Some("agent:implementer")).unwrap_err();
        assert!(release_error.to_string().contains("active attempt"));
        assert_eq!(fs::read(&path).unwrap(), active_bytes);

        let set_error = set(tmp.path(), "active", "T-002", ItemStatus::Completed).unwrap_err();
        assert!(set_error.to_string().contains("active attempt"));
        assert_eq!(fs::read(&path).unwrap(), active_bytes);

        let waiting_error =
            set_waiting(tmp.path(), "active", "T-002", gate(GateKind::External)).unwrap_err();
        assert!(waiting_error.to_string().contains("active attempt"));
        assert_eq!(fs::read(&path).unwrap(), active_bytes);

        let finish = AttemptFinish {
            result_signature: "completed".to_string(),
            progress: "made progress".to_string(),
            new_information: "learned result".to_string(),
            next_action: "stop".to_string(),
        };
        let mut stale = show(tmp.path(), "active").unwrap();
        stale.items[0].status = ItemStatus::Pending;
        write_checklist_atomic(&path, &stale).unwrap();
        let stale_bytes = fs::read(&path).unwrap();

        let finish_error = attempt_finish(
            tmp.path(),
            "active",
            "T-002",
            &started.attempt_id,
            finish.clone(),
        )
        .unwrap_err();
        assert!(finish_error.to_string().contains("in_progress"));
        assert_eq!(fs::read(&path).unwrap(), stale_bytes);
        assert_eq!(
            show(tmp.path(), "active").unwrap().items[0]
                .attempt_state
                .as_ref()
                .unwrap()
                .active_attempt
                .as_ref()
                .unwrap()
                .attempt_id,
            started.attempt_id
        );

        stale.items[0].status = ItemStatus::InProgress;
        write_checklist_atomic(&path, &stale).unwrap();
        let finished = attempt_finish(
            tmp.path(),
            "active",
            "T-002",
            &started.attempt_id,
            finish.clone(),
        )
        .unwrap();
        assert!(finished.items[0]
            .attempt_state
            .as_ref()
            .unwrap()
            .active_attempt
            .is_none());

        let finished_bytes = fs::read(&path).unwrap();
        let consumed_error =
            attempt_finish(tmp.path(), "active", "T-002", &started.attempt_id, finish).unwrap_err();
        assert!(consumed_error.to_string().contains("not active"));
        assert_eq!(fs::read(&path).unwrap(), finished_bytes);
    }

    #[test]
    fn attempt_accepted_start_reactivates_penalties_without_erasing_human_preference() {
        let tmp = TempDir::new().unwrap();
        create_dag_from_items(tmp.path(), "accepted-restore", vec![item("T-002", &[])]).unwrap();
        update_human_preference(
            tmp.path(),
            "accepted-restore",
            "T-002",
            HumanPreferenceChange {
                operation: HumanPreferenceOperation::Set(20),
                actor: "human:owner".to_string(),
                reason: "Restore priority before a novel attempt".to_string(),
                changed_at: "2026-07-12T22:00:00Z".parse().unwrap(),
                restore_priority_state: true,
            },
        )
        .unwrap();
        claim(
            tmp.path(),
            "accepted-restore",
            "agent:implementer",
            1,
            30,
            false,
        )
        .unwrap();
        let before = show(tmp.path(), "accepted-restore").unwrap();
        let preference = before.items[0].human_priority_override;
        let audit = before.items[0].human_preference_audit.clone();
        assert!(before.items[0].priority_state_restored);

        let report = attempt_start(
            tmp.path(),
            "accepted-restore",
            "T-002",
            "agent:implementer",
            AttemptRole::Implementer,
            AttemptFingerprintInput {
                acceptance_criterion: "accepted start reactivates penalties".to_string(),
                relevant_inputs: vec![ScopedInputDigest {
                    path: "src/lib.rs".to_string(),
                    digest: "sha256:accepted".to_string(),
                }],
                normalized_command: "cargo test attempt accepted".to_string(),
            },
        )
        .unwrap();

        assert_eq!(report.decision, AttemptDecision::Accepted);
        let persisted = show(tmp.path(), "accepted-restore").unwrap();
        assert!(!persisted.items[0].priority_state_restored);
        assert_eq!(persisted.items[0].human_priority_override, preference);
        assert_eq!(persisted.items[0].human_preference_audit, audit);
    }

    #[test]
    fn attempt_loop_detected_start_reactivates_penalties_without_erasing_human_preference() {
        let tmp = TempDir::new().unwrap();
        create_dag_from_items(tmp.path(), "rejected-restore", vec![item("T-002", &[])]).unwrap();
        claim(
            tmp.path(),
            "rejected-restore",
            "agent:implementer",
            1,
            30,
            false,
        )
        .unwrap();
        let input = AttemptFingerprintInput {
            acceptance_criterion: "rejected start reactivates penalties".to_string(),
            relevant_inputs: vec![ScopedInputDigest {
                path: "src/lib.rs".to_string(),
                digest: "sha256:rejected".to_string(),
            }],
            normalized_command: "cargo test attempt rejected".to_string(),
        };
        let first = attempt_start(
            tmp.path(),
            "rejected-restore",
            "T-002",
            "agent:implementer",
            AttemptRole::Implementer,
            input.clone(),
        )
        .unwrap();
        finish_unchanged_attempt(
            tmp.path(),
            "rejected-restore",
            &first.attempt_id,
            "implementer",
        );
        let second = attempt_start(
            tmp.path(),
            "rejected-restore",
            "T-002",
            "agent:verifier",
            AttemptRole::Verifier,
            input.clone(),
        )
        .unwrap();
        finish_unchanged_attempt(
            tmp.path(),
            "rejected-restore",
            &second.attempt_id,
            "verifier",
        );
        update_human_preference(
            tmp.path(),
            "rejected-restore",
            "T-002",
            HumanPreferenceChange {
                operation: HumanPreferenceOperation::Set(30),
                actor: "human:owner".to_string(),
                reason: "Restore priority before checking the rejected start".to_string(),
                changed_at: "2026-07-12T22:05:00Z".parse().unwrap(),
                restore_priority_state: true,
            },
        )
        .unwrap();
        let before = show(tmp.path(), "rejected-restore").unwrap();
        let preference = before.items[0].human_priority_override;
        let audit = before.items[0].human_preference_audit.clone();
        assert!(before.items[0].priority_state_restored);

        let report = attempt_start(
            tmp.path(),
            "rejected-restore",
            "T-002",
            "agent:third",
            AttemptRole::Verifier,
            input,
        )
        .unwrap();

        assert_eq!(report.decision, AttemptDecision::LoopDetected);
        let persisted = show(tmp.path(), "rejected-restore").unwrap();
        assert!(!persisted.items[0].priority_state_restored);
        assert_eq!(persisted.items[0].human_priority_override, preference);
        assert_eq!(persisted.items[0].human_preference_audit, audit);
    }

    #[test]
    fn attempt_exact_repeat_allows_one_distinct_verifier_then_opens_loop_gate() {
        let tmp = TempDir::new().unwrap();
        let mut dependency = item("T-001", &[]);
        dependency.status = ItemStatus::Completed;
        create_dag_from_items(
            tmp.path(),
            "attempts",
            vec![dependency, item("T-002", &["T-001"])],
        )
        .unwrap();
        claim(tmp.path(), "attempts", "agent:implementer", 1, 30, false).unwrap();

        let canonical = AttemptFingerprintInput {
            acceptance_criterion: "exact attempt control".to_string(),
            relevant_inputs: vec![
                ScopedInputDigest {
                    path: "src/lib.rs".to_string(),
                    digest: "SHA256:ABC".to_string(),
                },
                ScopedInputDigest {
                    path: "tests/attempt.rs".to_string(),
                    digest: "sha256:def".to_string(),
                },
            ],
            normalized_command: "cargo test -p forge-checklist-state attempt".to_string(),
        };
        let equivalent = AttemptFingerprintInput {
            acceptance_criterion: "  exact   attempt control  ".to_string(),
            relevant_inputs: vec![
                ScopedInputDigest {
                    path: " tests\\attempt.rs ".to_string(),
                    digest: " SHA256:DEF ".to_string(),
                },
                ScopedInputDigest {
                    path: "./src//lib.rs".to_string(),
                    digest: " sha256:abc ".to_string(),
                },
            ],
            normalized_command: " cargo test -p forge-checklist-state attempt ".to_string(),
        };

        let checklist = show(tmp.path(), "attempts").unwrap();
        let execution_fingerprint =
            attempt_execution_fingerprint(&checklist, "T-002", &canonical).unwrap();
        assert_eq!(
            execution_fingerprint,
            attempt_execution_fingerprint(&checklist, "T-002", &equivalent).unwrap()
        );
        let mut internally_distinct_command = canonical.clone();
        internally_distinct_command.normalized_command =
            "cargo  test -p forge-checklist-state attempt".to_string();
        assert_ne!(
            execution_fingerprint,
            attempt_execution_fingerprint(&checklist, "T-002", &internally_distinct_command)
                .unwrap()
        );
        let mut changed_dependency = checklist.clone();
        changed_dependency.items[0].status = ItemStatus::Pending;
        assert_ne!(
            execution_fingerprint,
            attempt_execution_fingerprint(&changed_dependency, "T-002", &canonical).unwrap()
        );
        let mut changed_gate = checklist.clone();
        changed_gate.items[1].gate = Some(gate(GateKind::External));
        let changed_gate_fingerprint =
            attempt_execution_fingerprint(&changed_gate, "T-002", &canonical).unwrap();
        assert_ne!(execution_fingerprint, changed_gate_fingerprint);
        changed_gate.items[1]
            .gate
            .as_mut()
            .unwrap()
            .attempt_ids
            .push("A-999".to_string());
        assert_eq!(
            changed_gate_fingerprint,
            attempt_execution_fingerprint(&changed_gate, "T-002", &canonical).unwrap()
        );
        changed_gate.items[1].gate.as_mut().unwrap().reason = "Different input".to_string();
        assert_ne!(
            changed_gate_fingerprint,
            attempt_execution_fingerprint(&changed_gate, "T-002", &canonical).unwrap()
        );
        assert_eq!(
            completed_attempt_fingerprint(
                &checklist,
                "T-002",
                &canonical,
                "failed: verifier limit"
            )
            .unwrap(),
            completed_attempt_fingerprint(
                &checklist,
                "T-002",
                &equivalent,
                " failed: verifier limit "
            )
            .unwrap()
        );
        assert_ne!(
            completed_attempt_fingerprint(
                &checklist,
                "T-002",
                &canonical,
                "failed: verifier limit"
            )
            .unwrap(),
            completed_attempt_fingerprint(
                &checklist,
                "T-002",
                &canonical,
                "failed:  verifier limit"
            )
            .unwrap()
        );

        let implementation = attempt_start(
            tmp.path(),
            "attempts",
            "T-002",
            "agent:implementer",
            AttemptRole::Implementer,
            equivalent.clone(),
        )
        .unwrap();
        assert_eq!(implementation.attempt_id, "A-1");
        assert_eq!(implementation.decision, AttemptDecision::Accepted);
        attempt_finish(
            tmp.path(),
            "attempts",
            "T-002",
            "A-1",
            AttemptFinish {
                result_signature: "failed: verifier limit".to_string(),
                progress: "implemented deterministic guard".to_string(),
                new_information: "one exact verification is required".to_string(),
                next_action: "run independent verification".to_string(),
            },
        )
        .unwrap();

        let after_bookkeeping = show(tmp.path(), "attempts").unwrap();
        assert_eq!(
            execution_fingerprint,
            attempt_execution_fingerprint(&after_bookkeeping, "T-002", &canonical).unwrap()
        );

        let verification = attempt_start(
            tmp.path(),
            "attempts",
            "T-002",
            "agent:verifier",
            AttemptRole::Verifier,
            canonical.clone(),
        )
        .unwrap();
        assert_eq!(verification.attempt_id, "A-2");
        assert_eq!(verification.decision, AttemptDecision::Accepted);
        attempt_finish(
            tmp.path(),
            "attempts",
            "T-002",
            "A-2",
            AttemptFinish {
                result_signature: "failed: verifier limit".to_string(),
                progress: "independently reproduced".to_string(),
                new_information: "result is unchanged".to_string(),
                next_action: "pivot".to_string(),
            },
        )
        .unwrap();

        let finished = show(tmp.path(), "attempts").unwrap();
        let state = finished.items[1].attempt_state.as_ref().unwrap();
        assert_eq!(state.same_attempt_count, 2);
        assert!(state
            .last_fingerprint
            .as_deref()
            .unwrap()
            .starts_with("sha256:"));
        assert_eq!(state.last_attempt.as_ref().unwrap().attempt_id, "A-2");
        assert_eq!(
            state.last_attempt.as_ref().unwrap().new_information,
            "result is unchanged"
        );
        assert_eq!(state.last_attempt.as_ref().unwrap().next_action, "pivot");

        let third = attempt_start(
            tmp.path(),
            "attempts",
            "T-002",
            "agent:third",
            AttemptRole::Verifier,
            canonical,
        )
        .unwrap();
        assert_eq!(third.attempt_id, "A-3");
        assert_eq!(third.decision, AttemptDecision::LoopDetected);
        let gated = show(tmp.path(), "attempts").unwrap();
        assert_eq!(gated.items[1].status, ItemStatus::Waiting);
        let gate = gated.items[1].gate.as_ref().unwrap();
        assert_eq!(gate.kind, GateKind::LoopDetected);
        assert_eq!(gate.attempt_ids, vec!["A-1", "A-2", "A-3"]);

        create_dag_from_items(tmp.path(), "self-check", vec![item("T-002", &[])]).unwrap();
        claim(tmp.path(), "self-check", "agent:same", 1, 30, false).unwrap();
        let first = attempt_start(
            tmp.path(),
            "self-check",
            "T-002",
            "agent:same",
            AttemptRole::Implementer,
            equivalent.clone(),
        )
        .unwrap();
        attempt_finish(
            tmp.path(),
            "self-check",
            "T-002",
            &first.attempt_id,
            AttemptFinish {
                result_signature: "failed: verifier limit".to_string(),
                progress: "implemented".to_string(),
                new_information: "baseline".to_string(),
                next_action: "verify".to_string(),
            },
        )
        .unwrap();
        let self_verification = attempt_start(
            tmp.path(),
            "self-check",
            "T-002",
            "agent:same",
            AttemptRole::Verifier,
            equivalent,
        )
        .unwrap();
        assert_eq!(self_verification.decision, AttemptDecision::LoopDetected);
        let self_gated = show(tmp.path(), "self-check").unwrap();
        assert_eq!(self_gated.items[0].status, ItemStatus::Waiting);
        assert_eq!(
            self_gated.items[0].gate.as_ref().unwrap().kind,
            GateKind::LoopDetected
        );
    }

    #[test]
    fn schema_v3_waiting_item_with_typed_gate_and_operational_fields_round_trips() {
        let json = r#"{
          "name": "anti-loop",
          "created": "2026-07-12T17:00:00Z",
          "updated": "2026-07-12T18:00:00Z",
          "schema_version": 3,
          "items": [{
            "id": "T-001",
            "title": "Choose ownership",
            "status": "waiting",
            "gate": {
              "kind": "decision",
              "createdAt": "2026-07-12T18:00:00Z",
              "reason": "Canonical span ownership requires user preference",
              "question": "Use boundary-local or remote endpoints?",
              "attemptIds": ["A-17", "A-18"],
              "artifactRefs": ["specs/proposal.md#ties"]
            },
            "goalRef": "G-jsm-parity",
            "goalSummary": "Complete all 335 engraving cases",
            "itemContribution": "Prove one strict cross-measure tie case",
            "basePriority": 80,
            "effort": "small",
            "critical": false,
            "humanPriorityOverride": 95,
            "parentItemId": "T-000",
            "attemptState": {
              "lastFingerprint": "sha256:abc",
              "sameAttemptCount": 1,
              "similarLowProgressCount": 2,
              "lastEventRefs": ["fmem:event-1"],
              "retryPenalty": 3,
              "goalRetryPenalty": 4,
              "lastProgressAt": "2026-07-12T17:59:00Z"
            }
          }]
        }"#;

        let checklist: Checklist = serde_json::from_str(json).unwrap();
        let item = &checklist.items[0];
        assert_eq!(item.status, ItemStatus::Waiting);
        assert_eq!(item.gate.as_ref().unwrap().kind, GateKind::Decision);
        assert_eq!(item.goal_ref.as_deref(), Some("G-jsm-parity"));
        assert_eq!(item.effort, Some(Effort::Small));
        assert_eq!(item.attempt_state.as_ref().unwrap().same_attempt_count, 1);

        let serialized = serde_json::to_value(&checklist).unwrap();
        assert_eq!(serialized["items"][0]["gate"]["kind"], "decision");
        assert_eq!(serialized["items"][0]["goalRef"], "G-jsm-parity");
        assert_eq!(serialized["items"][0]["attemptState"]["retryPenalty"], 3);
    }

    #[test]
    fn schema_v3_accepts_all_closed_gate_kinds_and_rejects_unknown_kinds() {
        for (json_kind, expected) in [
            ("review", GateKind::Review),
            ("decision", GateKind::Decision),
            ("external", GateKind::External),
            ("loop_detected", GateKind::LoopDetected),
        ] {
            let json = format!(
                r#"{{"kind":"{json_kind}","createdAt":"2026-07-12T18:00:00Z","reason":"Needs input"}}"#
            );
            let parsed: WaitingGate = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed.kind, expected);
        }

        let unknown = r#"{"kind":"other","createdAt":"2026-07-12T18:00:00Z","reason":"No"}"#;
        assert!(serde_json::from_str::<WaitingGate>(unknown).is_err());
    }

    #[test]
    fn waiting_gate_validation_enforces_status_gate_pairing() {
        let mut waiting_without_gate = item("waiting", &[]);
        waiting_without_gate.status = ItemStatus::Waiting;
        let report = validate_dependencies(&dag(vec![waiting_without_gate]));
        assert!(!report.valid);
        assert!(report
            .errors
            .iter()
            .any(|error| error.contains("requires a gate")));

        let mut pending_with_gate = item("pending", &[]);
        pending_with_gate.gate = Some(gate(GateKind::Review));
        let report = validate_dependencies(&dag(vec![pending_with_gate]));
        assert!(!report.valid);
        assert!(report
            .errors
            .iter()
            .any(|error| error.contains("only valid for waiting")));
    }

    #[test]
    fn entering_waiting_through_public_transition_clears_claim_and_lease() {
        let tmp = TempDir::new().unwrap();
        create_dag_from_items(tmp.path(), "dag", vec![item("a", &[])]).unwrap();
        claim(tmp.path(), "dag", "agent-1", 1, 30, false).unwrap();

        let checklist = set_waiting(tmp.path(), "dag", "a", gate(GateKind::External)).unwrap();

        assert_eq!(checklist.items[0].status, ItemStatus::Waiting);
        assert_eq!(
            checklist.items[0].gate.as_ref().unwrap().kind,
            GateKind::External
        );
        assert!(checklist.items[0].claimed_by.is_none());
        assert!(checklist.items[0].lease_expires_at.is_none());
    }

    #[test]
    fn waiting_dependencies_are_not_ready() {
        let tmp = TempDir::new().unwrap();
        let mut dependency = item("dependency", &[]);
        dependency.status = ItemStatus::Waiting;
        dependency.gate = Some(gate(GateKind::Decision));
        create_dag_from_items(
            tmp.path(),
            "dag",
            vec![dependency, item("dependent", &["dependency"])],
        )
        .unwrap();

        let report = ready(tmp.path(), "dag", None, true).unwrap();
        assert!(report.items.is_empty());
    }

    #[test]
    fn old_v2_dag_checklist_json_loads_and_old_fields_stay_absent() {
        let json = r#"{
          "name": "old-dag",
          "created": "2026-04-27T14:00:00Z",
          "updated": "2026-04-27T15:00:00Z",
          "schema_version": 2,
          "items": [
            {"id": "a", "title": "A", "status": "completed"},
            {"id": "b", "title": "B", "status": "pending", "depends_on": ["a"]}
          ]
        }"#;

        let checklist: Checklist = serde_json::from_str(json).unwrap();
        assert_eq!(checklist.schema_version, Some(2));
        assert_eq!(checklist.items[1].depends_on, vec!["a"]);

        let serialized = serde_json::to_value(checklist).unwrap();
        let item = serialized["items"][0].as_object().unwrap();
        for field in [
            "gate",
            "goalRef",
            "goalSummary",
            "itemContribution",
            "basePriority",
            "effort",
            "critical",
            "humanPriorityOverride",
            "humanPreferenceAudit",
            "priorityStateRestored",
            "parentItemId",
            "attemptState",
            "review",
            "followUp",
        ] {
            assert!(!item.contains_key(field), "unexpected legacy field {field}");
        }
    }

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("Phase 1: Architect"), "phase-1-architect");
        assert_eq!(slugify("  Hello, World!  "), "hello-world");
        assert_eq!(slugify("already-slug"), "already-slug");
        assert_eq!(slugify("MiXeD CaSe"), "mixed-case");
    }

    #[test]
    fn round_trip_create_show() {
        let tmp = TempDir::new().unwrap();
        let ts = titles(&["Phase 1: Architect", "Phase 2: DSM"]);
        let cl = create(tmp.path(), "blueprint-init", &ts).unwrap();
        assert_eq!(cl.items.len(), 2);
        assert_eq!(cl.items[0].id, "phase-1-architect");
        assert_eq!(cl.items[0].status, ItemStatus::Pending);

        let loaded = show(tmp.path(), "blueprint-init").unwrap();
        assert_eq!(loaded.name, "blueprint-init");
        assert_eq!(loaded.items.len(), 2);
        assert_eq!(loaded.items[1].id, "phase-2-dsm");
        assert_eq!(loaded.schema_version, None);
    }

    #[test]
    fn old_flat_checklist_json_loads() {
        let json = r#"{
          "name": "old",
          "created": "2026-04-10T14:00:00Z",
          "updated": "2026-04-10T15:32:00Z",
          "items": [
            {"id": "a", "title": "A", "status": "pending"},
            {"id": "b", "title": "B", "status": "completed", "completed_at": "2026-04-10T15:32:00Z"}
          ]
        }"#;
        let cl: Checklist = serde_json::from_str(json).unwrap();
        assert_eq!(cl.schema_version, None);
        assert!(cl.items[0].depends_on.is_empty());
        assert_eq!(cl.items[0].status, ItemStatus::Pending);

        let serialized = serde_json::to_value(cl).unwrap();
        assert!(serialized["items"][0].get("gate").is_none());
        assert!(serialized["items"][0].get("attemptState").is_none());
    }

    #[test]
    fn list_returns_sorted_names() {
        let tmp = TempDir::new().unwrap();
        create(tmp.path(), "z-list", &titles(&["a"])).unwrap();
        create(tmp.path(), "a-list", &titles(&["a"])).unwrap();
        create(tmp.path(), "m-list", &titles(&["a"])).unwrap();

        let names = list(tmp.path()).unwrap();
        assert_eq!(names, vec!["a-list", "m-list", "z-list"]);
    }

    #[test]
    fn list_empty_dir_ok() {
        let tmp = TempDir::new().unwrap();
        let names = list(tmp.path()).unwrap();
        assert!(names.is_empty());
    }

    #[test]
    fn set_transitions() {
        let tmp = TempDir::new().unwrap();
        create(tmp.path(), "cl", &titles(&["Do a thing", "Do another"])).unwrap();

        let cl = set(tmp.path(), "cl", "do-a-thing", ItemStatus::InProgress).unwrap();
        assert_eq!(cl.items[0].status, ItemStatus::InProgress);
        assert!(cl.items[0].completed_at.is_none());

        let cl = set(tmp.path(), "cl", "do-a-thing", ItemStatus::Completed).unwrap();
        assert_eq!(cl.items[0].status, ItemStatus::Completed);
        assert!(cl.items[0].completed_at.is_some());

        let cl = set(tmp.path(), "cl", "do-a-thing", ItemStatus::Blocked).unwrap();
        assert_eq!(cl.items[0].status, ItemStatus::Blocked);
        assert!(cl.items[0].completed_at.is_none());
    }

    #[test]
    fn note_attaches_text() {
        let tmp = TempDir::new().unwrap();
        create(tmp.path(), "cl", &titles(&["Task"])).unwrap();
        let cl = note(tmp.path(), "cl", "task", "wrote spec.md").unwrap();
        assert_eq!(cl.items[0].notes.as_deref(), Some("wrote spec.md"));
    }

    #[test]
    fn error_on_missing_checklist() {
        let tmp = TempDir::new().unwrap();
        assert!(show(tmp.path(), "nope").is_err());
        assert!(set(tmp.path(), "nope", "x", ItemStatus::Pending).is_err());
        assert!(note(tmp.path(), "nope", "x", "hi").is_err());
        assert!(delete(tmp.path(), "nope").is_err());
    }

    #[test]
    fn error_on_missing_item() {
        let tmp = TempDir::new().unwrap();
        create(tmp.path(), "cl", &titles(&["One"])).unwrap();
        assert!(set(tmp.path(), "cl", "nope", ItemStatus::Completed).is_err());
        assert!(note(tmp.path(), "cl", "nope", "x").is_err());
    }

    #[test]
    fn create_rejects_duplicate() {
        let tmp = TempDir::new().unwrap();
        create(tmp.path(), "cl", &titles(&["A"])).unwrap();
        assert!(create(tmp.path(), "cl", &titles(&["A"])).is_err());
    }

    #[test]
    fn delete_removes_file() {
        let tmp = TempDir::new().unwrap();
        create(tmp.path(), "cl", &titles(&["A"])).unwrap();
        delete(tmp.path(), "cl").unwrap();
        assert!(show(tmp.path(), "cl").is_err());
    }

    #[test]
    fn duplicate_titles_get_unique_ids() {
        let tmp = TempDir::new().unwrap();
        let cl = create(
            tmp.path(),
            "cl",
            &titles(&["Same Title", "Same Title", "Same Title"]),
        )
        .unwrap();
        let ids: Vec<&str> = cl.items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, vec!["same-title", "same-title-2", "same-title-3"]);
    }

    #[test]
    fn name_validation_rejects_traversal() {
        let tmp = TempDir::new().unwrap();
        assert!(create(tmp.path(), "../evil", &titles(&["A"])).is_err());
        assert!(create(tmp.path(), "a/b", &titles(&["A"])).is_err());
        assert!(create(tmp.path(), "", &titles(&["A"])).is_err());
    }

    #[test]
    fn item_status_parse() {
        assert_eq!(ItemStatus::parse("pending").unwrap(), ItemStatus::Pending);
        assert_eq!(
            ItemStatus::parse("in_progress").unwrap(),
            ItemStatus::InProgress
        );
        assert_eq!(
            ItemStatus::parse("completed").unwrap(),
            ItemStatus::Completed
        );
        assert_eq!(ItemStatus::parse("blocked").unwrap(), ItemStatus::Blocked);
        assert_eq!(ItemStatus::parse("waiting").unwrap(), ItemStatus::Waiting);
        assert!(ItemStatus::parse("bogus").is_err());
    }

    #[test]
    fn validate_dependency_errors() {
        let report = validate_dependencies(&dag(vec![item("a", &["missing"])]));
        assert!(!report.valid);
        assert!(report.errors[0].contains("missing"));

        let report = validate_dependencies(&dag(vec![item("a", &["a"])]));
        assert!(!report.valid);
        assert!(report.errors[0].contains("depends on itself"));

        let report = validate_dependencies(&dag(vec![item("a", &["b"]), item("b", &["a"])]));
        assert!(!report.valid);
        assert!(report.errors[0].contains("cycle"));
    }

    #[test]
    fn derive_topological_batches() {
        let cl = dag(vec![
            item("a", &[]),
            item("b", &[]),
            item("c", &["a", "b"]),
            item("d", &["c"]),
        ]);
        let batches = derive_batches(&cl).unwrap();
        assert_eq!(batches, vec![vec!["a", "b"], vec!["c"], vec!["d"]]);
    }

    #[test]
    fn ready_respects_completed_dependencies() {
        let tmp = TempDir::new().unwrap();
        let cl = create_dag_from_items(
            tmp.path(),
            "dag",
            vec![item("a", &[]), item("b", &["a"]), item("c", &["b"])],
        )
        .unwrap();
        assert_eq!(cl.items.len(), 3);

        let r = ready(tmp.path(), "dag", None, false).unwrap();
        assert_eq!(
            r.items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
            vec!["a"]
        );

        set(tmp.path(), "dag", "a", ItemStatus::Completed).unwrap();
        let r = ready(tmp.path(), "dag", None, false).unwrap();
        assert_eq!(
            r.items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
            vec!["b"]
        );
    }

    #[test]
    fn claim_sets_lease_and_release_clears_it() {
        let tmp = TempDir::new().unwrap();
        create_dag_from_items(tmp.path(), "dag", vec![item("a", &[]), item("b", &[])]).unwrap();
        let report = claim(tmp.path(), "dag", "agent-1", 1, 30, false).unwrap();
        assert_eq!(report.claimed_count, 1);
        assert_eq!(report.claimed[0].id, "a");
        assert_eq!(report.remaining_ready_count, 1);

        let cl = show(tmp.path(), "dag").unwrap();
        assert_eq!(cl.items[0].status, ItemStatus::InProgress);
        assert_eq!(cl.items[0].claimed_by.as_deref(), Some("agent-1"));
        assert!(cl.items[0].lease_expires_at.is_some());

        let cl = release(tmp.path(), "dag", "a", Some("agent-1")).unwrap();
        assert_eq!(cl.items[0].status, ItemStatus::Pending);
        assert!(cl.items[0].claimed_by.is_none());
        assert!(cl.items[0].lease_expires_at.is_none());
    }

    #[test]
    fn release_rejects_wrong_agent() {
        let tmp = TempDir::new().unwrap();
        create_dag_from_items(tmp.path(), "dag", vec![item("a", &[])]).unwrap();
        claim(tmp.path(), "dag", "agent-1", 1, 30, false).unwrap();
        assert!(release(tmp.path(), "dag", "a", Some("agent-2")).is_err());
    }

    #[test]
    fn set_completion_clears_claim() {
        let tmp = TempDir::new().unwrap();
        create_dag_from_items(tmp.path(), "dag", vec![item("a", &[])]).unwrap();
        claim(tmp.path(), "dag", "agent-1", 1, 30, false).unwrap();
        let cl = set(tmp.path(), "dag", "a", ItemStatus::Completed).unwrap();
        assert_eq!(cl.items[0].status, ItemStatus::Completed);
        assert!(cl.items[0].claimed_by.is_none());
        assert!(cl.items[0].lease_expires_at.is_none());
    }

    #[test]
    fn scored_ready_keeps_only_dependency_ready_pending_items_and_sorts_stably() {
        let tmp = TempDir::new().unwrap();
        let mut low = item("z-low", &[]);
        low.base_priority = Some(10);
        let mut tie_b = item("b-tie", &[]);
        tie_b.base_priority = Some(30);
        let mut tie_a = item("a-tie", &[]);
        tie_a.base_priority = Some(30);
        let mut waiting = item("waiting-high", &[]);
        waiting.base_priority = Some(100);
        waiting.status = ItemStatus::Waiting;
        waiting.gate = Some(gate(GateKind::Decision));
        let mut active = item("active-high", &[]);
        active.base_priority = Some(100);
        active.status = ItemStatus::InProgress;
        let mut blocker = item("blocker", &[]);
        blocker.status = ItemStatus::Blocked;
        let dependent = item("dependent", &["blocker"]);
        create_dag_from_items(
            tmp.path(),
            "scored",
            vec![low, tie_b, tie_a, waiting, active, blocker, dependent],
        )
        .unwrap();

        let mut policy = score_policy();
        policy.ease_bonus = EffortScorePolicy {
            small: 0,
            medium: 0,
            large: 0,
            unspecified: 0,
        };
        policy.dag_unlock.effort_weight = policy.ease_bonus;
        policy.goal_progress_max_bonus = 0;
        let report = scored_ready(tmp.path(), "scored", &policy, None).unwrap();

        assert_eq!(report.ready_count, 3);
        assert_eq!(
            report
                .items
                .iter()
                .map(|entry| entry.item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a-tie", "b-tie", "z-low"]
        );
        assert!(report.items.iter().all(|entry| {
            entry.item.status == ItemStatus::Pending
                && !entry.score.explanation.is_empty()
                && entry.score.components.base_priority > 0
        }));
    }

    #[test]
    fn resolve_waiting_requires_human_context_and_returns_recovery_state() {
        let tmp = TempDir::new().unwrap();
        let mut target = item("target", &[]);
        target.attempt_state = Some(AttemptState {
            last_event_refs: vec!["fmem:event-7".to_string()],
            ..AttemptState::default()
        });
        create_dag_from_items(tmp.path(), "resolve", vec![target]).unwrap();
        set_waiting(
            tmp.path(),
            "resolve",
            "target",
            gate(GateKind::LoopDetected),
        )
        .unwrap();

        assert!(resolve_waiting(tmp.path(), "resolve", "target", "", "pivot").is_err());
        assert!(resolve_waiting(tmp.path(), "resolve", "target", "human:bkearns", "").is_err());

        let report = resolve_waiting(
            tmp.path(),
            "resolve",
            "target",
            "human:bkearns",
            "Approved a structurally novel pivot",
        )
        .unwrap();
        let target = report
            .state
            .items
            .iter()
            .find(|item| item.id == "target")
            .unwrap();
        assert_eq!(target.status, ItemStatus::Pending);
        assert!(target.gate.is_none());
        assert_eq!(report.prior_attempt_ids, vec!["A-17"]);
        assert_eq!(report.event_refs, vec!["fmem:event-7"]);
        assert!(!report.recovery_hints.is_empty());
        assert_eq!(report.resolved_by, "human:bkearns");
        assert_eq!(report.reason, "Approved a structurally novel pivot");
    }
}
