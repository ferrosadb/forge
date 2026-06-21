//! Module: Persist workflow checklists and schedule dependency-aware task DAGs.
//! Correctness: Correct when flat v1 checklists still round-trip, DAG validation rejects invalid dependencies, ready items respect completed prerequisites, and claim leases prevent duplicate work.
//! Last revised: 2026-04-27
//! Last changed: Added optional dependency metadata, DAG validation, ready-set calculation, and claim/release scheduling APIs.
//!
//! Stores named checklists as JSON files under `<project-root>/.forge/checklists/`.
//! Skills like `blueprint`, `compile-project`, and `performance-tuning` use these
//! to resume multi-step workflows across sessions, `/clear`, or compaction.
//!
//! All writes are atomic: write to a sibling temp file, then rename.

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
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
}

impl ItemStatus {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "pending" => Ok(ItemStatus::Pending),
            "in_progress" => Ok(ItemStatus::InProgress),
            "completed" => Ok(ItemStatus::Completed),
            "blocked" => Ok(ItemStatus::Blocked),
            other => Err(anyhow!(
                "unknown status '{}'; expected one of pending|in_progress|completed|blocked",
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
            item.status = status;
            item.completed_at = if status == ItemStatus::Completed {
                Some(now)
            } else {
                None
            };
            item.claimed_by = None;
            item.lease_expires_at = None;
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
}
