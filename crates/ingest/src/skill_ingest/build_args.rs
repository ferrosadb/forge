//! Convert a parsed `Skill` (plus its resolved supplementary files)
//! into the `IngestSkillArgs` the fmem client's `ingest_skill` wrapper
//! accepts.
//!
//! This is the narrow seam between the filesystem-shaped world
//! (`skill_ingest::parse`) and the wire-shaped world
//! (`forge_fmem_client::IngestSkillArgs`). Keeping it a pure function
//! with no I/O and no side effects means the orchestrator's Phase B
//! wiring collapses to `ingest_skill(transport, build_args(&skill, ...))`.
//!
//! Responsibilities:
//!
//! 1. Compute the `content_hash` via `skill_ingest::hash` so the same
//!    skill + supplementary produces the same hash across runs
//!    (idempotency — FMEA F10, F11).
//! 2. Normalize `category` with the fmem-matching rule (locked design
//!    choice 3). The walker hands us the raw directory name; fmem
//!    expects the normalized form so its auto-created tag entity
//!    lines up with anything we might `ensure_parent_tag` against.
//! 3. Pass through frontmatter tags (already normalized at parse time).
//! 4. Map `parse::Step` → client `Step` 1:1.
//! 5. Omit empty optional fields so they don't appear on the wire
//!    (serde's `skip_serializing_if` does the rest).

use forge_fmem_client::{IngestSkillArgs, Step as ClientStep};

use super::hash;
use super::parse::{normalize_tag, Skill};
use super::supplementary::ResolvedSupplementary;

/// Build `IngestSkillArgs` for the given parsed skill.
///
/// `session_id` — passed through verbatim to fmem. fmem accepts either
/// a UUID string or the literal `"default"`. Pass `None` to omit the
/// field so fmem uses its configured default session.
pub fn build_ingest_args(
    skill: &Skill,
    supplementary: &[ResolvedSupplementary],
    session_id: Option<String>,
) -> IngestSkillArgs {
    IngestSkillArgs {
        name: skill.name.clone(),
        category: normalize_tag(&skill.category),
        description: skill.description.clone(),
        trigger_keywords: skill.trigger_keywords.clone(),
        tags: skill.tags.clone(),
        prerequisites: skill.prerequisites.clone(),
        steps: skill.steps.iter().map(to_client_step).collect(),
        output_artifacts: skill.output_artifacts.clone(),
        // Our parser doesn't extract `completion_criteria` today —
        // fmem treats it as optional. If/when the parser learns to
        // harvest it, fill it in here. Keep as None for now so we
        // don't lie on the wire.
        completion_criteria: None,
        content_hash: Some(hash::content_hash(skill, supplementary)),
        session_id,
    }
}

fn to_client_step(step: &super::parse::Step) -> ClientStep {
    ClientStep {
        phase: step.phase.clone(),
        instruction: step.instruction.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill_ingest::parse;
    use std::path::PathBuf;

    fn parse_skill(frontmatter: &str, body: &str, category: &str) -> Skill {
        let mut s = String::new();
        s.push_str("---\n");
        s.push_str(frontmatter);
        s.push_str("---\n");
        s.push_str(body);
        parse::parse(s.as_bytes(), category).unwrap()
    }

    fn sup(name: &str, bytes: &[u8]) -> ResolvedSupplementary {
        ResolvedSupplementary {
            declared: name.into(),
            path: PathBuf::from(name),
            bytes: bytes.to_vec(),
        }
    }

    #[test]
    fn minimal_skill_maps_fields() {
        let skill = parse_skill("name: tdd\ndescription: do tdd\n", "body", "task-level");
        let args = build_ingest_args(&skill, &[], None);
        assert_eq!(args.name, "tdd");
        assert_eq!(args.category, "task-level");
        assert_eq!(args.description, "do tdd");
        assert!(args.content_hash.is_some());
        assert!(args.session_id.is_none());
    }

    #[test]
    fn content_hash_prefix_is_sha256() {
        let skill = parse_skill("name: x\ndescription: y\n", "body", "tech");
        let args = build_ingest_args(&skill, &[], None);
        assert!(args.content_hash.unwrap().starts_with("sha256:"));
    }

    #[test]
    fn category_is_normalized() {
        // The walker hands us the raw dir name; adapter must normalize
        // so fmem's auto-created tag entity lines up.
        let skill = parse_skill("name: x\ndescription: y\n", "body", "Task_Level");
        let args = build_ingest_args(&skill, &[], None);
        assert_eq!(args.category, "task-level");
    }

    #[test]
    fn tags_pass_through_already_normalized() {
        let skill = parse_skill(
            "name: x\ndescription: y\ntags: [Testing, Quality_Engineering]\n",
            "body",
            "task-level",
        );
        let args = build_ingest_args(&skill, &[], None);
        assert_eq!(args.tags, vec!["testing", "quality-engineering"]);
    }

    #[test]
    fn steps_map_one_to_one() {
        let body = "## Instructions\n\n- step one\n- step two\n";
        let skill = parse_skill("name: x\ndescription: y\n", body, "task-level");
        let args = build_ingest_args(&skill, &[], None);
        assert_eq!(args.steps.len(), 2);
        assert_eq!(args.steps[0].instruction, "step one");
        assert!(args.steps[0].phase.is_none());
    }

    #[test]
    fn phased_steps_preserve_phase() {
        let body = "### Step 1: Red\nWrite a failing test.\n\n### Step 2: Green\nMake it pass.\n";
        let skill = parse_skill("name: x\ndescription: y\n", body, "task-level");
        let args = build_ingest_args(&skill, &[], None);
        assert_eq!(args.steps.len(), 2);
        assert_eq!(args.steps[0].phase.as_deref(), Some("Step 1: Red"));
    }

    #[test]
    fn session_id_is_passed_through() {
        let skill = parse_skill("name: x\ndescription: y\n", "", "tech");
        let args = build_ingest_args(&skill, &[], Some("default".into()));
        assert_eq!(args.session_id.as_deref(), Some("default"));
    }

    #[test]
    fn supplementary_edit_changes_content_hash() {
        // Regression for WI-FMEA-03: editing a supplementary file must
        // produce a different content_hash so fmem sees the change.
        let skill = parse_skill("name: x\ndescription: y\n", "body", "tech");
        let before = build_ingest_args(&skill, &[sup("extra.md", b"v1")], None);
        let after = build_ingest_args(&skill, &[sup("extra.md", b"v2")], None);
        assert_ne!(before.content_hash, after.content_hash);
    }

    #[test]
    fn prereqs_and_related_pass_through() {
        let skill = parse_skill(
            "name: x\ndescription: y\nprerequisites: [unit-testing]\nrelated: [refactor]\n",
            "body",
            "task-level",
        );
        let args = build_ingest_args(&skill, &[], None);
        assert_eq!(args.prerequisites, vec!["unit-testing"]);
        // `related` is not part of IngestSkillArgs — fmem doesn't have
        // a RELATED field today; we intentionally drop it.
    }

    #[test]
    fn output_artifacts_pass_through() {
        let skill = parse_skill(
            "name: x\ndescription: y\noutput_artifacts: [checklist, diagram]\n",
            "",
            "task-level",
        );
        let args = build_ingest_args(&skill, &[], None);
        assert_eq!(args.output_artifacts, vec!["checklist", "diagram"]);
    }

    #[test]
    fn empty_trigger_keywords_stay_empty_not_derived_twice() {
        // Parser derives trigger_keywords from description when the
        // frontmatter has no `keywords:`. Adapter just passes that
        // through — it must not re-derive or modify.
        let skill = parse_skill(
            "name: x\ndescription: Short description\n",
            "body",
            "task-level",
        );
        let args = build_ingest_args(&skill, &[], None);
        assert_eq!(args.trigger_keywords, skill.trigger_keywords);
    }

    #[test]
    fn serializes_to_json_without_empty_optional_fields() {
        // Wire-format smoke: the args should serialize to a compact
        // JSON object with only the populated fields. This is what
        // fmem will actually see.
        let skill = parse_skill("name: x\ndescription: y\n", "", "tech");
        let args = build_ingest_args(&skill, &[], None);
        let v = serde_json::to_value(&args).unwrap();
        // Required fields present.
        assert_eq!(v["name"], "x");
        assert_eq!(v["category"], "tech");
        assert_eq!(v["description"], "y");
        assert!(v["content_hash"].as_str().unwrap().starts_with("sha256:"));
        // Empty optional fields omitted.
        assert!(v.get("tags").is_none());
        assert!(v.get("prerequisites").is_none());
        assert!(v.get("steps").is_none());
        assert!(v.get("completion_criteria").is_none());
        assert!(v.get("session_id").is_none());
    }
}
