//! Four-phase skill-ingest orchestrator.
//!
//! Phases (blueprint `architecture.md`):
//!
//! | Phase | Work                                                        |
//! |-------|-------------------------------------------------------------|
//! | A     | Taxonomy seed — `ensure_parent_tag` per PARENT_TAG edge     |
//! | B     | Per skill: `ingest_skill` (fmem auto-creates tag entities)  |
//! | C     | Re-pass: any skill with `missing_prerequisites` after B     |
//! | D     | Verify every parsed skill — hard exit gate                  |
//!
//! All four phases respect the locked design choices from
//! `overview.md`:
//!
//! - Verification in phase D is a hard gate; on any missing edge or
//!   missing skill the run exits with code 4.
//! - Exit codes: 0 clean · 1 parse/schema · 2 transport · 3 precondition
//!   · 4 verification.
//! - Dry-run must never reach the transport (FMEA F18); it short-
//!   circuits after phase A's plan is built.
//! - Single in-flight request (P1-5); no concurrency in v1.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use forge_fmem_client::{
    ensure_parent_tag, ingest_skill, verify_skill, EnsureParentTagArgs, IngestSkillAction,
    Transport, VerifySkillArgs,
};

use super::build_args::build_ingest_args;
use super::collision;
use super::parse::{self, Skill};
use super::secret_check;
use super::supplementary::{self, ResolvedSupplementary};
use super::taxonomy::{self, TaxonomyPlan};
use super::walk::{self, SkillFile};

/// Caller-facing run configuration.
#[derive(Debug, Clone)]
pub struct RunConfig {
    /// Root directory to walk (must contain the skill categories).
    pub root: PathBuf,
    /// Optional skill name filter — only skills whose name matches
    /// are ingested. Matches simple glob via `name.contains(pat)`
    /// with `*` wildcards; empty string = match all.
    pub filter: Option<String>,
    /// Dry run: parse, validate, build the taxonomy plan, but do not
    /// call fmem.
    pub dry_run: bool,
    /// Force re-ingest even when content_hash matches — sent as the
    /// absence of `content_hash` in the args so fmem doesn't short
    /// circuit.
    pub force: bool,
    /// Session to record on fmem's side (optional).
    pub session_id: Option<String>,
}

/// Summary emitted at the end of a run. Buckets split by phase so the
/// operator can tell immediately where a failure landed.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Summary {
    // Phase A — taxonomy edges.
    pub taxonomy_edges_created: usize,
    pub taxonomy_edges_skipped: usize,
    pub taxonomy_edges_failed: usize,

    // Phase B — skill ingest.
    pub skills_created: usize,
    pub skills_updated: usize,
    pub skills_skipped_unchanged: usize,
    pub skills_failed: usize,
    pub skills_filtered_out: usize,

    // Diagnostic info.
    pub empty_steps_warnings: Vec<String>,
    pub parse_errors: Vec<String>,

    // Phase C — re-pass for missing prereqs.
    pub repass_attempts: usize,
    pub repass_completed: usize,

    // Phase D — verification.
    pub verified: usize,
    pub verification_failures: Vec<VerifyFailure>,

    // Timing.
    pub duration_ms: u128,
}

impl Summary {
    /// Map to the process exit code per the locked design.
    pub fn exit_code(&self) -> i32 {
        if !self.verification_failures.is_empty() {
            return 4;
        }
        if self.skills_failed > 0 {
            return 2;
        }
        if !self.parse_errors.is_empty() {
            return 1;
        }
        if self.taxonomy_edges_failed > 0 {
            return 2;
        }
        0
    }

    /// True if every bucket that mattered is zero-failure.
    pub fn is_clean(&self) -> bool {
        self.exit_code() == 0
    }
}

/// One verification failure for a named skill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyFailure {
    pub skill: String,
    pub reason: VerifyFailureReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyFailureReason {
    /// fmem says the skill doesn't exist at all.
    DoesNotExist,
    /// `missing_prerequisites` was non-empty after phase C.
    MissingPrerequisites(Vec<String>),
    /// Expected tags (category + frontmatter `tags:`) not all present.
    MissingTags {
        expected: Vec<String>,
        actual: Vec<String>,
    },
    /// Transport error during verification.
    TransportError(String),
}

/// Top-level error surface from [`run`]. Kept small — bucketed failures
/// live in the returned `Summary`; this enum is for conditions that
/// abort the run before any phase completes.
#[derive(Debug)]
pub enum RunError {
    Walk(walk::WalkError),
    Collision(collision::CollisionError),
    Plan(taxonomy::PlanError),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Walk(e) => write!(f, "walk error: {e}"),
            Self::Collision(e) => write!(f, "{e}"),
            Self::Plan(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for RunError {}

impl From<walk::WalkError> for RunError {
    fn from(e: walk::WalkError) -> Self {
        Self::Walk(e)
    }
}
impl From<collision::CollisionError> for RunError {
    fn from(e: collision::CollisionError) -> Self {
        Self::Collision(e)
    }
}
impl From<taxonomy::PlanError> for RunError {
    fn from(e: taxonomy::PlanError) -> Self {
        Self::Plan(e)
    }
}

/// Execute the full four-phase ingest. Returns a [`Summary`] whose
/// [`Summary::exit_code`] is what the caller should exit with.
///
/// `transport` — any [`Transport`] implementation. Production wires
/// this to `StdioTransport`; tests use `MockTransport`.
pub fn run<T: Transport>(config: RunConfig, transport: &T) -> Result<Summary, RunError> {
    let start = Instant::now();
    let mut summary = Summary::default();

    // --- Pre-phase: walk + parse + collision check ---
    let files = walk::walk(&config.root)?;
    let parsed = parse_files(&files, &config, &mut summary);
    let skills_with_paths: Vec<(Skill, PathBuf)> = parsed
        .iter()
        .map(|(skill, file)| (skill.clone(), file.path.clone()))
        .collect();
    let skill_view: Vec<(&Skill, &Path)> = skills_with_paths
        .iter()
        .map(|(s, p)| (s, p.as_path()))
        .collect();
    collision::detect(skill_view.iter().copied())?;

    // Build taxonomy plan (validates hierarchy, orphans, collisions).
    let plan = taxonomy::build_plan(&config.root, &skills_with_paths)?;

    if plan.hierarchy_absent {
        eprintln!(
            "[skill-ingest] tag-hierarchy.yaml not found under {}; taxonomy will be flat",
            config.root.display()
        );
    }

    // Apply `--filter` eagerly so `filtered_out` is reported in both
    // dry-run and live modes.
    let skill_refs: Vec<(Skill, PathBuf)> = filter_and_collect(&parsed, &config, &mut summary);

    if config.dry_run {
        // FMEA F18 — dry-run must never touch the transport.
        summary.duration_ms = start.elapsed().as_millis();
        eprintln!(
            "[skill-ingest] dry-run: {} skill(s) would be ingested after filter, {} taxonomy edge(s) planned",
            skill_refs.len(),
            plan.edges.len(),
        );
        return Ok(summary);
    }

    // --- Phase A: taxonomy seed ---
    run_phase_a(transport, &plan, &config, &mut summary);
    if summary.taxonomy_edges_failed > 0 {
        summary.duration_ms = start.elapsed().as_millis();
        return Ok(summary);
    }

    let ingest_outcomes = run_phase_b(transport, &skill_refs, &config, &mut summary);

    // --- Phase C: re-pass for skills whose prereqs weren't seen yet ---
    run_phase_c(
        transport,
        &skill_refs,
        &ingest_outcomes,
        &config,
        &mut summary,
    );

    // --- Phase D: verify every parsed skill ---
    run_phase_d(transport, &skill_refs, &mut summary);

    summary.duration_ms = start.elapsed().as_millis();
    Ok(summary)
}

// ---------------------------------------------------------------------------
// Pre-phase helpers
// ---------------------------------------------------------------------------

fn parse_files<'a>(
    files: &'a [SkillFile],
    _config: &RunConfig,
    summary: &mut Summary,
) -> Vec<(Skill, &'a SkillFile)> {
    let mut out = Vec::with_capacity(files.len());
    for f in files {
        match parse::parse(&f.bytes, &f.category) {
            Ok(skill) => {
                if skill.steps_empty {
                    summary
                        .empty_steps_warnings
                        .push(format!("{} has no parseable steps", f.path.display()));
                }
                out.push((skill, f));
            }
            Err(e) => {
                summary
                    .parse_errors
                    .push(format!("{}: {}", f.path.display(), e));
            }
        }
    }
    out
}

/// Apply `--filter`, clone into an owned (Skill, PathBuf) list.
fn filter_and_collect(
    parsed: &[(Skill, &SkillFile)],
    config: &RunConfig,
    summary: &mut Summary,
) -> Vec<(Skill, PathBuf)> {
    let matcher = Matcher::new(config.filter.as_deref());
    let mut out = Vec::new();
    for (skill, file) in parsed {
        if !matcher.matches(&skill.name) {
            summary.skills_filtered_out += 1;
            continue;
        }
        out.push((skill.clone(), file.path.clone()));
    }
    if out.is_empty() && matcher.has_filter() {
        eprintln!(
            "[skill-ingest] filter `{}` matched zero skills",
            matcher.pattern.as_deref().unwrap_or("")
        );
    }
    out
}

struct Matcher {
    pattern: Option<String>,
}

impl Matcher {
    fn new(pattern: Option<&str>) -> Self {
        Self {
            pattern: pattern.map(str::to_string),
        }
    }
    fn has_filter(&self) -> bool {
        self.pattern.is_some()
    }
    /// Simple `*`-glob that supports `name*`, `*name`, `*name*`, and
    /// exact matches. More exotic globs can wait.
    fn matches(&self, name: &str) -> bool {
        let Some(pat) = &self.pattern else {
            return true;
        };
        if !pat.contains('*') {
            return name == pat;
        }
        let parts: Vec<&str> = pat.split('*').collect();
        let mut cursor = 0;
        for (i, part) in parts.iter().enumerate() {
            if part.is_empty() {
                continue;
            }
            let Some(pos) = name[cursor..].find(part) else {
                return false;
            };
            if i == 0 && !pat.starts_with('*') && pos != 0 {
                return false;
            }
            cursor += pos + part.len();
        }
        if !pat.ends_with('*') {
            // Last non-empty part must match at the end.
            if let Some(last) = parts.iter().rev().find(|p| !p.is_empty()) {
                if !name.ends_with(last) {
                    return false;
                }
            }
        }
        true
    }
}

// ---------------------------------------------------------------------------
// Phase A
// ---------------------------------------------------------------------------

fn run_phase_a<T: Transport>(
    transport: &T,
    plan: &TaxonomyPlan,
    config: &RunConfig,
    summary: &mut Summary,
) {
    for edge in &plan.edges {
        let args = EnsureParentTagArgs {
            child_tag: edge.child.clone(),
            parent_tag: edge.parent.clone(),
            session_id: config.session_id.clone(),
        };
        match ensure_parent_tag(transport, args) {
            Ok(resp) => match resp.action {
                forge_fmem_client::EnsureParentTagAction::Created => {
                    summary.taxonomy_edges_created += 1;
                }
                forge_fmem_client::EnsureParentTagAction::Skipped => {
                    summary.taxonomy_edges_skipped += 1;
                }
            },
            Err(e) => {
                summary.taxonomy_edges_failed += 1;
                eprintln!(
                    "[skill-ingest] phase A: ensure_parent_tag({} → {}) failed: {}",
                    edge.child, edge.parent, e
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Phase B
// ---------------------------------------------------------------------------

/// One ingest outcome, used by phase C to decide who needs a re-pass.
///
/// Indexed back into the `skills: &[(Skill, PathBuf)]` slice that Phase B
/// iterated, so Phase C can re-ingest a skill without re-looking it up
/// by name (fmem commit `aae5771` — the server returns the exact list
/// of unresolved prereqs on every successful ingest response, so
/// Phase C only acts on non-empty lists).
#[derive(Debug, Clone)]
struct IngestOutcome {
    /// Position in the `skills` slice passed to Phase B. Used by
    /// Phase C to recover the `(Skill, PathBuf)` in O(1) without a
    /// separate `HashMap<name, ...>` index.
    skill_index: usize,
    /// Server-reported prereq names whose REQUIRES edge didn't land.
    /// Empty when every declared prereq resolved, or when the call
    /// didn't succeed (transport error) or was `Skipped` (no edges
    /// were touched).
    missing_prerequisites: Vec<String>,
}

fn run_phase_b<T: Transport>(
    transport: &T,
    skills: &[(Skill, PathBuf)],
    config: &RunConfig,
    summary: &mut Summary,
) -> Vec<IngestOutcome> {
    let mut outcomes = Vec::with_capacity(skills.len());
    for (idx, (skill, path)) in skills.iter().enumerate() {
        let path_ref = path.as_path();
        let origin = path_ref.display().to_string();

        // Pre-flight: secret-scan + supplementary resolution.
        if let Err(e) = secret_check::check_skill(&origin, skill) {
            summary.skills_failed += 1;
            eprintln!("[skill-ingest] phase B: {}", e);
            continue;
        }

        let skill_dir = match path_ref.parent() {
            Some(p) => p.to_path_buf(),
            None => {
                summary.skills_failed += 1;
                eprintln!("[skill-ingest] phase B: {origin} has no parent dir");
                continue;
            }
        };
        let supplementary = match supplementary::resolve(&skill_dir, &skill.supplementary_files) {
            Ok(v) => v,
            Err(e) => {
                summary.skills_failed += 1;
                eprintln!("[skill-ingest] phase B: {e}");
                continue;
            }
        };

        let missing = send_ingest(transport, skill, &supplementary, config, summary, &origin);
        outcomes.push(IngestOutcome {
            skill_index: idx,
            missing_prerequisites: missing,
        });
    }
    outcomes
}

/// Send one `ingest_skill` call; bucket the outcome into `summary` and
/// return the server-reported `missing_prerequisites` list. An empty
/// return means every declared prereq resolved (or the call failed /
/// was a Skipped no-op — neither case needs a re-pass).
/// Build `IngestSkillArgs` for the orchestrator's send paths. Applies
/// the `--force` transform (drops `content_hash` so fmem always writes)
/// in one place so Phase B's first pass and Phase C's re-pass can't
/// diverge over time.
fn args_for_ingest(
    skill: &Skill,
    supplementary: &[ResolvedSupplementary],
    config: &RunConfig,
) -> forge_fmem_client::IngestSkillArgs {
    let mut args = build_ingest_args(skill, supplementary, config.session_id.clone());
    if config.force {
        args.content_hash = None;
    }
    args
}

fn send_ingest<T: Transport>(
    transport: &T,
    skill: &Skill,
    supplementary: &[ResolvedSupplementary],
    config: &RunConfig,
    summary: &mut Summary,
    origin: &str,
) -> Vec<String> {
    match ingest_skill(transport, args_for_ingest(skill, supplementary, config)) {
        Ok(resp) => {
            let missing = resp.action.missing_prerequisites().to_vec();
            match resp.action {
                IngestSkillAction::Created { .. } => summary.skills_created += 1,
                IngestSkillAction::Updated { .. } => summary.skills_updated += 1,
                IngestSkillAction::Skipped { .. } => summary.skills_skipped_unchanged += 1,
            }
            missing
        }
        Err(e) => {
            summary.skills_failed += 1;
            eprintln!("[skill-ingest] phase B: ingest_skill({origin}) failed: {e}");
            Vec::new()
        }
    }
}

// ---------------------------------------------------------------------------
// Phase C — re-pass for skipped REQUIRES (FMEA F16 / WI-FMEA-04)
// ---------------------------------------------------------------------------
//
// After fmem commit `aae5771` each `ingest_skill` response carries the
// exact list of declared prereqs whose REQUIRES edges didn't land.
// Phase C re-runs `ingest_skill` for every skill whose Phase B outcome
// had a non-empty `missing_prerequisites` — fmem will now succeed on
// the edges that failed before because the targets landed later in
// Phase B.

fn run_phase_c<T: Transport>(
    transport: &T,
    skills: &[(Skill, PathBuf)],
    outcomes: &[IngestOutcome],
    config: &RunConfig,
    summary: &mut Summary,
) {
    for outcome in outcomes {
        if outcome.missing_prerequisites.is_empty() {
            continue;
        }
        // skill_index was assigned from the same `skills` slice Phase B
        // iterated, so the lookup is guaranteed in-range. Use `.get()`
        // anyway — it's a free bounds check that fails loud rather than
        // panicking if Phase B is ever restructured.
        let Some((skill, path)) = skills.get(outcome.skill_index) else {
            eprintln!(
                "[skill-ingest] phase C: outcome skill_index {} out of range (skills.len={})",
                outcome.skill_index,
                skills.len()
            );
            continue;
        };
        repass_one(transport, skill, path, config, summary);
    }
}

/// Re-ingest a single skill whose first-pass response reported
/// `missing_prerequisites`. Idempotent: if the targets still aren't
/// present fmem will just report them again; if they are, the edges
/// land this time.
fn repass_one<T: Transport>(
    transport: &T,
    skill: &Skill,
    path: &Path,
    config: &RunConfig,
    summary: &mut Summary,
) {
    let Some(skill_dir) = path.parent() else {
        return;
    };
    let supp = match supplementary::resolve(skill_dir, &skill.supplementary_files) {
        Ok(s) => s,
        Err(_) => return, // already reported in phase B
    };
    summary.repass_attempts += 1;
    match ingest_skill(transport, args_for_ingest(skill, &supp, config)) {
        Ok(_) => summary.repass_completed += 1,
        Err(e) => {
            eprintln!(
                "[skill-ingest] phase C: re-ingest({}) failed: {}",
                skill.name, e
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Phase D — verification gate (exit 4 on any failure)
// ---------------------------------------------------------------------------

fn run_phase_d<T: Transport>(transport: &T, skills: &[(Skill, PathBuf)], summary: &mut Summary) {
    for (skill, _path) in skills {
        let args = VerifySkillArgs {
            skill_name: skill.name.clone(),
            session_id: None,
        };
        match verify_skill(transport, args) {
            Ok(resp) => {
                if !resp.exists {
                    summary.verification_failures.push(VerifyFailure {
                        skill: skill.name.clone(),
                        reason: VerifyFailureReason::DoesNotExist,
                    });
                    continue;
                }
                if !resp.missing_prerequisites.is_empty() {
                    summary.verification_failures.push(VerifyFailure {
                        skill: skill.name.clone(),
                        reason: VerifyFailureReason::MissingPrerequisites(
                            resp.missing_prerequisites.clone(),
                        ),
                    });
                    continue;
                }
                // Expected tags = normalized category + frontmatter tags.
                let expected = expected_tags(skill);
                let actual_set: HashSet<&str> = resp.tags.iter().map(String::as_str).collect();
                let missing_any = expected.iter().any(|e| !actual_set.contains(e.as_str()));
                if missing_any {
                    summary.verification_failures.push(VerifyFailure {
                        skill: skill.name.clone(),
                        reason: VerifyFailureReason::MissingTags {
                            expected,
                            actual: resp.tags.clone(),
                        },
                    });
                    continue;
                }
                summary.verified += 1;
            }
            Err(e) => {
                summary.verification_failures.push(VerifyFailure {
                    skill: skill.name.clone(),
                    reason: VerifyFailureReason::TransportError(e.to_string()),
                });
            }
        }
    }
}

fn expected_tags(skill: &Skill) -> Vec<String> {
    let mut out = Vec::with_capacity(1 + skill.tags.len());
    let cat = parse::normalize_tag(&skill.category);
    if !cat.is_empty() {
        out.push(cat);
    }
    for t in &skill.tags {
        if !t.is_empty() {
            out.push(t.clone());
        }
    }
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    //! The orchestrator is exercised end-to-end via
    //! `crates/ingest/tests/run_harness.rs` so we can build real
    //! temp-dir fixtures. This module covers the internal helpers
    //! (Matcher, expected_tags, Summary::exit_code) only.

    use super::*;
    use crate::skill_ingest::parse;

    #[test]
    fn matcher_no_filter_matches_all() {
        let m = Matcher::new(None);
        assert!(m.matches("anything"));
    }

    #[test]
    fn matcher_exact_match() {
        let m = Matcher::new(Some("tdd"));
        assert!(m.matches("tdd"));
        assert!(!m.matches("tdd2"));
    }

    #[test]
    fn matcher_star_prefix() {
        let m = Matcher::new(Some("tdd*"));
        assert!(m.matches("tdd"));
        assert!(m.matches("tdd-advanced"));
        assert!(!m.matches("bdd"));
    }

    #[test]
    fn matcher_star_suffix() {
        let m = Matcher::new(Some("*test"));
        assert!(m.matches("unit-test"));
        assert!(!m.matches("testing"));
    }

    #[test]
    fn matcher_star_contains() {
        let m = Matcher::new(Some("*test*"));
        assert!(m.matches("testing"));
        assert!(m.matches("unit-test-x"));
        assert!(!m.matches("tdd"));
    }

    #[test]
    fn expected_tags_dedupes_category_equal_to_tag() {
        let raw = "---\nname: x\ndescription: y\ntags:\n  - task-level\n---\nbody";
        let skill = parse::parse(raw.as_bytes(), "task-level").unwrap();
        let tags = expected_tags(&skill);
        assert_eq!(tags, vec!["task-level"]);
    }

    #[test]
    fn exit_code_maps_buckets() {
        let mut s = Summary::default();
        assert_eq!(s.exit_code(), 0);
        s.parse_errors.push("x".into());
        assert_eq!(s.exit_code(), 1);
        s.skills_failed = 1;
        assert_eq!(s.exit_code(), 2); // transport/schema wins over parse
        s.verification_failures.push(VerifyFailure {
            skill: "tdd".into(),
            reason: VerifyFailureReason::DoesNotExist,
        });
        assert_eq!(s.exit_code(), 4); // verification is highest
    }
}
