//! Build, validate, and emit a [`TaxonomyPlan`] for the skill root.
//!
//! The plan is the single source of truth for Phase A (taxonomy seed).
//! It captures:
//!
//! - `tags` — every tag the pipeline will ever mention, sorted.
//! - `edges` — PARENT_TAG edges to create, in a deterministic order.
//!
//! Constructed by the orchestrator via [`build_plan`], which runs four
//! validations (FMEA F25–F29):
//!
//! - [`detect_cycles`] over the edge DAG
//! - [`detect_orphans`] — every edge endpoint must be a known tag
//! - [`collect_all_tags`] — preflight union so Phase B never lazy-creates
//! - [`check_name_collisions`] — no skill shares a name with any tag

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use super::hierarchy::{parse_hierarchy, HierarchyError, HierarchyOutcome};
use crate::skill_ingest::parse::{normalize_tag, Skill};

/// One PARENT_TAG edge: `child` is the more specific tag; `parent` is
/// the broader one. Direction locked per feature spec — `tdd` has parent
/// `testing`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TagEdge {
    pub child: String,
    pub parent: String,
}

/// The validated taxonomy plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaxonomyPlan {
    /// Sorted superset of every tag the pipeline will see.
    pub tags: Vec<String>,
    /// Edges to create, in deterministic order.
    pub edges: Vec<TagEdge>,
    /// True if the hierarchy file was missing — orchestrator emits an
    /// info-level log in that case (WI-FMEA-05).
    pub hierarchy_absent: bool,
}

#[derive(Debug)]
pub enum PlanError {
    Io(std::io::Error),
    Hierarchy(HierarchyError),
    InvalidCategoryName { path: PathBuf, raw: String },
    Cycle { chain: Vec<String> },
    Orphan { node: String },
    SkillTagNameCollision { name: String, skill_path: PathBuf },
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "taxonomy i/o error: {e}"),
            Self::Hierarchy(e) => write!(f, "{e}"),
            Self::InvalidCategoryName { path, raw } => write!(
                f,
                "category directory `{raw}` at {} normalizes to an empty tag",
                path.display()
            ),
            Self::Cycle { chain } => {
                write!(f, "tag-hierarchy.yaml contains cycle: ")?;
                write!(f, "{}", chain.join(" → "))
            }
            Self::Orphan { node } => write!(
                f,
                "tag-hierarchy.yaml references `{node}` which is neither a top-level dir nor a tag on any skill"
            ),
            Self::SkillTagNameCollision { name, skill_path } => write!(
                f,
                "skill `{name}` at {} shares its name with a tag — one must be renamed",
                skill_path.display()
            ),
        }
    }
}

impl std::error::Error for PlanError {}

impl From<std::io::Error> for PlanError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<HierarchyError> for PlanError {
    fn from(e: HierarchyError) -> Self {
        Self::Hierarchy(e)
    }
}

// -----------------------------------------------------------------------
// Public API
// -----------------------------------------------------------------------

/// Compose the full plan: walk top-level dirs, parse hierarchy, collect
/// tags from parsed skills, run all validations.
///
/// `skills` is the list of already-parsed skills from Phase 0 (walk +
/// parse). The caller has already read each skill's path so we can cite
/// it in error messages (used for the skill/tag name collision check).
pub fn build_plan(
    skill_root: &Path,
    skills_with_paths: &[(Skill, PathBuf)],
) -> Result<TaxonomyPlan, PlanError> {
    let top_level = walk_top_level(skill_root)?;

    let hierarchy_outcome = parse_hierarchy(skill_root)?;
    let (edges, hierarchy_absent) = match hierarchy_outcome {
        HierarchyOutcome::Loaded(e) => (e, false),
        HierarchyOutcome::Absent => (Vec::new(), true),
    };

    let skills_view: Vec<&Skill> = skills_with_paths.iter().map(|(s, _)| s).collect();
    let all_tags = collect_all_tags(&top_level, &skills_view);

    // Validate structure before caring about orphans / collisions.
    detect_cycles(&edges)?;
    detect_orphans(&edges, &all_tags)?;
    check_name_collisions(skills_with_paths, &all_tags)?;

    // Emit tags sorted, edges in a deterministic (child, parent) order.
    let mut tags_sorted: Vec<String> = all_tags.into_iter().collect();
    tags_sorted.sort();

    let mut edges_sorted = edges;
    edges_sorted.sort_by(|a, b| a.child.cmp(&b.child).then_with(|| a.parent.cmp(&b.parent)));

    Ok(TaxonomyPlan {
        tags: tags_sorted,
        edges: edges_sorted,
        hierarchy_absent,
    })
}

/// Enumerate first-level directories under `skill_root`, apply the
/// parser's `normalize_tag`, and return the normalized names sorted.
///
/// Non-directory entries, hidden entries (`.git`, `.DS_Store`, etc.),
/// and the hierarchy file itself are skipped.
pub fn walk_top_level(skill_root: &Path) -> Result<Vec<String>, PlanError> {
    let mut out = Vec::new();
    for entry in fs::read_dir(skill_root)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        if !ft.is_dir() {
            continue;
        }
        let name = match entry.file_name().to_str() {
            Some(s) => s.to_string(),
            None => continue, // non-UTF-8 dir name — skip silently here; walker catches it too
        };
        if name.starts_with('.') {
            continue;
        }
        let normalized = normalize_tag(&name);
        if normalized.is_empty() {
            return Err(PlanError::InvalidCategoryName {
                path: entry.path(),
                raw: name,
            });
        }
        out.push(normalized);
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// Union: top-level dirs + every skill's category + every skill's
/// frontmatter `tags:` entries. Already normalized.
pub fn collect_all_tags(top_level: &[String], skills: &[&Skill]) -> BTreeSet<String> {
    let mut set: BTreeSet<String> = top_level.iter().cloned().collect();
    for s in skills {
        let cat = normalize_tag(&s.category);
        if !cat.is_empty() {
            set.insert(cat);
        }
        for t in &s.tags {
            if !t.is_empty() {
                set.insert(t.clone());
            }
        }
    }
    set
}

/// DFS cycle detection over the PARENT_TAG edge list.
///
/// Edges form a directed graph child → parent. A cycle means two tags
/// each claim the other as ancestor, which fmem's server-side check
/// would also reject (Sprint 2d). Running it client-side gives a
/// better error message before the round-trip.
pub fn detect_cycles(edges: &[TagEdge]) -> Result<(), PlanError> {
    // Adjacency: child → [parent...] (usually just one).
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for e in edges {
        adj.entry(&e.child).or_default().push(&e.parent);
    }

    // White/gray/black DFS.
    const WHITE: u8 = 0;
    const GRAY: u8 = 1;
    const BLACK: u8 = 2;
    let mut color: HashMap<&str, u8> = HashMap::new();
    for node in adj.keys() {
        color.insert(node, WHITE);
    }

    for &start in adj.keys() {
        if color[start] != WHITE {
            continue;
        }
        let mut stack: Vec<(&str, usize)> = vec![(start, 0)];
        let mut path: Vec<&str> = vec![start];
        color.insert(start, GRAY);

        while let Some(&(node, idx)) = stack.last() {
            let neighbors = adj.get(node).map(|v| v.as_slice()).unwrap_or(&[]);
            if idx < neighbors.len() {
                let next = neighbors[idx];
                // Advance the child's cursor.
                let last = stack.last_mut().unwrap();
                last.1 += 1;
                match color.get(next).copied().unwrap_or(WHITE) {
                    WHITE => {
                        color.insert(next, GRAY);
                        path.push(next);
                        stack.push((next, 0));
                    }
                    GRAY => {
                        // Cycle discovered — reconstruct from `path`.
                        let cycle_start = path.iter().position(|&n| n == next).unwrap();
                        let mut chain: Vec<String> = path[cycle_start..]
                            .iter()
                            .map(|s| (*s).to_string())
                            .collect();
                        chain.push(next.to_string());
                        return Err(PlanError::Cycle { chain });
                    }
                    _ => { /* BLACK: already fully explored */ }
                }
            } else {
                color.insert(node, BLACK);
                stack.pop();
                path.pop();
            }
        }
    }

    Ok(())
}

/// Every node referenced in a PARENT_TAG edge must be a known tag —
/// either a top-level category dir or a tag mentioned by some skill.
pub fn detect_orphans(edges: &[TagEdge], known_tags: &BTreeSet<String>) -> Result<(), PlanError> {
    for e in edges {
        if !known_tags.contains(&e.child) {
            return Err(PlanError::Orphan {
                node: e.child.clone(),
            });
        }
        if !known_tags.contains(&e.parent) {
            return Err(PlanError::Orphan {
                node: e.parent.clone(),
            });
        }
    }
    Ok(())
}

/// Run both name-collision checks against the parsed skills. Named
/// after its composite purpose — the underlying invariants are split
/// into [`check_skill_vs_tag_collisions`] and
/// [`check_skills_normalize_uniquely`].
pub fn check_name_collisions(
    skills: &[(Skill, PathBuf)],
    tags: &BTreeSet<String>,
) -> Result<(), PlanError> {
    check_skill_vs_tag_collisions(skills, tags)?;
    check_skills_normalize_uniquely(skills, tags)
}

/// Invariant: no skill's `name` may normalize to any tag in the
/// taxonomy. fmem's flat namespace would make retrieval ambiguous
/// if the same normalized string existed as both a skill and a tag.
pub fn check_skill_vs_tag_collisions(
    skills: &[(Skill, PathBuf)],
    tags: &BTreeSet<String>,
) -> Result<(), PlanError> {
    for (skill, path) in skills {
        let as_tag = normalize_tag(&skill.name);
        if as_tag.is_empty() {
            continue;
        }
        if tags.contains(&as_tag) {
            return Err(PlanError::SkillTagNameCollision {
                name: skill.name.clone(),
                skill_path: path.clone(),
            });
        }
    }
    Ok(())
}

/// Invariant: two skills must not normalize to the same name.
///
/// `collision::detect` catches exact-name collisions on the raw
/// frontmatter `name`; this check catches the normalized-form case
/// (e.g. `TDD` vs `tdd`, or `web_security` vs `web-security`) — same
/// entity in fmem after normalization.
///
/// The `tags` parameter scopes the check: if a normalized name *also*
/// equals a known tag, [`check_skill_vs_tag_collisions`] already
/// returned an error, so we don't double-report here.
pub fn check_skills_normalize_uniquely(
    skills: &[(Skill, PathBuf)],
    tags: &BTreeSet<String>,
) -> Result<(), PlanError> {
    let mut seen: HashSet<String> = HashSet::new();
    for (skill, path) in skills {
        let key = normalize_tag(&skill.name);
        if key.is_empty() {
            continue;
        }
        if !seen.insert(key.clone()) && !tags.contains(&key) {
            return Err(PlanError::SkillTagNameCollision {
                name: skill.name.clone(),
                skill_path: path.clone(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill_ingest::parse;
    use std::fs;
    use tempfile::TempDir;

    fn skill(name: &str, category: &str, tags: &[&str]) -> Skill {
        let tag_list = tags
            .iter()
            .map(|t| format!("  - {t}"))
            .collect::<Vec<_>>()
            .join("\n");
        let frontmatter = if tags.is_empty() {
            format!("name: {name}\ndescription: d\n")
        } else {
            format!("name: {name}\ndescription: d\ntags:\n{tag_list}\n")
        };
        let raw = format!("---\n{frontmatter}---\nbody");
        parse::parse(raw.as_bytes(), category).unwrap()
    }

    fn edge(child: &str, parent: &str) -> TagEdge {
        TagEdge {
            child: child.into(),
            parent: parent.into(),
        }
    }

    #[test]
    fn walk_top_level_lists_categories() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir(tmp.path().join("task-level")).unwrap();
        fs::create_dir(tmp.path().join("tech")).unwrap();
        fs::create_dir(tmp.path().join(".hidden")).unwrap();
        fs::write(tmp.path().join("tag-hierarchy.yaml"), "").unwrap(); // ignored
        let dirs = walk_top_level(tmp.path()).unwrap();
        assert_eq!(dirs, vec!["task-level", "tech"]);
    }

    #[test]
    fn walk_top_level_normalizes_names() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir(tmp.path().join("Task_Level")).unwrap();
        fs::create_dir(tmp.path().join("tech")).unwrap();
        let dirs = walk_top_level(tmp.path()).unwrap();
        assert_eq!(dirs, vec!["task-level", "tech"]);
    }

    #[test]
    fn collect_all_tags_unions_top_level_and_skills() {
        let s1 = skill("tdd", "task-level", &["testing", "quality"]);
        let s2 = skill("rust", "tech", &["lang"]);
        let tags = collect_all_tags(
            &["task-level".into(), "tech".into(), "rules".into()],
            &[&s1, &s2],
        );
        assert!(tags.contains("task-level"));
        assert!(tags.contains("tech"));
        assert!(tags.contains("rules"));
        assert!(tags.contains("testing"));
        assert!(tags.contains("quality"));
        assert!(tags.contains("lang"));
    }

    #[test]
    fn detect_cycles_linear_ok() {
        let edges = vec![edge("tdd", "testing"), edge("testing", "quality")];
        detect_cycles(&edges).unwrap();
    }

    #[test]
    fn detect_cycles_diamond_ok() {
        // a → b, a → c, b → d, c → d — no cycle.
        let edges = vec![
            edge("a", "b"),
            edge("a", "c"),
            edge("b", "d"),
            edge("c", "d"),
        ];
        detect_cycles(&edges).unwrap();
    }

    #[test]
    fn detect_cycles_two_node_cycle() {
        // a → b, b → a
        let edges = vec![edge("a", "b"), edge("b", "a")];
        let err = detect_cycles(&edges).unwrap_err();
        let chain = match err {
            PlanError::Cycle { chain } => chain,
            other => panic!("expected Cycle, got {other:?}"),
        };
        assert!(chain.contains(&"a".to_string()));
        assert!(chain.contains(&"b".to_string()));
    }

    #[test]
    fn detect_cycles_three_node_cycle() {
        let edges = vec![edge("a", "b"), edge("b", "c"), edge("c", "a")];
        let err = detect_cycles(&edges).unwrap_err();
        assert!(matches!(err, PlanError::Cycle { .. }));
    }

    #[test]
    fn detect_orphans_all_known_ok() {
        let edges = vec![edge("tdd", "testing")];
        let tags: BTreeSet<String> = ["tdd", "testing"].iter().map(|s| s.to_string()).collect();
        detect_orphans(&edges, &tags).unwrap();
    }

    #[test]
    fn detect_orphans_unknown_child() {
        let edges = vec![edge("unknown-tag", "testing")];
        let tags: BTreeSet<String> = ["testing".into()].into_iter().collect();
        let err = detect_orphans(&edges, &tags).unwrap_err();
        match err {
            PlanError::Orphan { node } => assert_eq!(node, "unknown-tag"),
            other => panic!("expected Orphan, got {other:?}"),
        }
    }

    #[test]
    fn detect_orphans_unknown_parent() {
        let edges = vec![edge("tdd", "ghost-parent")];
        let tags: BTreeSet<String> = ["tdd".into()].into_iter().collect();
        let err = detect_orphans(&edges, &tags).unwrap_err();
        assert!(matches!(err, PlanError::Orphan { .. }));
    }

    #[test]
    fn check_name_collisions_skill_matches_tag() {
        let s = skill("quality", "task-level", &[]);
        let skills = vec![(s, PathBuf::from("/r/task-level/quality/SKILL.md"))];
        let tags: BTreeSet<String> = ["quality".into(), "tech".into()].into_iter().collect();
        let err = check_name_collisions(&skills, &tags).unwrap_err();
        match err {
            PlanError::SkillTagNameCollision { name, .. } => assert_eq!(name, "quality"),
            other => panic!("expected SkillTagNameCollision, got {other:?}"),
        }
    }

    #[test]
    fn check_name_collisions_no_overlap() {
        let s = skill("tdd", "task-level", &[]);
        let skills = vec![(s, PathBuf::from("/r/task-level/tdd/SKILL.md"))];
        let tags: BTreeSet<String> = ["task-level".into(), "quality".into()]
            .into_iter()
            .collect();
        check_name_collisions(&skills, &tags).unwrap();
    }

    #[test]
    fn build_plan_happy_path() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir(tmp.path().join("task-level")).unwrap();
        fs::create_dir(tmp.path().join("tech")).unwrap();
        fs::create_dir(tmp.path().join("quality")).unwrap();
        fs::write(
            tmp.path().join("tag-hierarchy.yaml"),
            "task-level: quality\n",
        )
        .unwrap();
        let s = skill("tdd", "task-level", &["testing"]);
        let plan = build_plan(
            tmp.path(),
            &[(s, PathBuf::from("/r/task-level/tdd/SKILL.md"))],
        )
        .unwrap();
        assert!(plan.tags.contains(&"task-level".to_string()));
        assert!(plan.tags.contains(&"quality".to_string()));
        assert!(plan.tags.contains(&"tech".to_string()));
        assert!(plan.tags.contains(&"testing".to_string()));
        assert_eq!(plan.edges.len(), 1);
        assert!(!plan.hierarchy_absent);
    }

    #[test]
    fn build_plan_absent_hierarchy_flags() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir(tmp.path().join("task-level")).unwrap();
        let plan = build_plan(tmp.path(), &[]).unwrap();
        assert!(plan.hierarchy_absent);
        assert!(plan.edges.is_empty());
    }

    #[test]
    fn build_plan_orphan_rejected() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir(tmp.path().join("task-level")).unwrap();
        fs::write(tmp.path().join("tag-hierarchy.yaml"), "task-level: ghost\n").unwrap();
        let err = build_plan(tmp.path(), &[]).unwrap_err();
        assert!(matches!(err, PlanError::Orphan { .. }));
    }

    #[test]
    fn build_plan_cycle_rejected() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir(tmp.path().join("a")).unwrap();
        fs::create_dir(tmp.path().join("b")).unwrap();
        fs::write(tmp.path().join("tag-hierarchy.yaml"), "a: b\nb: a\n").unwrap();
        let err = build_plan(tmp.path(), &[]).unwrap_err();
        assert!(matches!(err, PlanError::Cycle { .. }));
    }

    #[test]
    fn build_plan_skill_tag_collision_rejected() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir(tmp.path().join("quality")).unwrap();
        let s = skill("quality", "quality", &[]);
        let err = build_plan(
            tmp.path(),
            &[(s, PathBuf::from("/r/quality/quality/SKILL.md"))],
        )
        .unwrap_err();
        assert!(matches!(err, PlanError::SkillTagNameCollision { .. }));
    }

    #[test]
    fn build_plan_emits_deterministic_order() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir(tmp.path().join("z-cat")).unwrap();
        fs::create_dir(tmp.path().join("a-cat")).unwrap();
        fs::create_dir(tmp.path().join("m-cat")).unwrap();
        let plan1 = build_plan(tmp.path(), &[]).unwrap();
        let plan2 = build_plan(tmp.path(), &[]).unwrap();
        assert_eq!(plan1.tags, plan2.tags);
        assert_eq!(plan1.tags, vec!["a-cat", "m-cat", "z-cat"]);
    }
}
