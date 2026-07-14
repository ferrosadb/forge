//! Pull planning: canonical rows → board operations.
//!
//! [`plan_pull`] is pure logic — no CQL/network — that decides, per sheet
//! row, whether the pull side of sync should create a new task, update an
//! existing one, or skip it because nothing changed. The later executor
//! task applies the [`BoardOp`]s this module produces; this module never
//! calls the task store itself (`existing_status` is injected precisely so
//! tests and the live executor can both drive this logic without a
//! network).
//!
//! ## Status mapping and the never-move-backward rule
//!
//! A sheet row's `Status` cell maps to a [`forge_tasks::TaskStatus`] via
//! [`crate::config::SheetMapping::status_map`] (see
//! [`target_status`]). On create, that mapped status is always used
//! (there's nothing to conflict with yet).
//!
//! On update, the picture is different: once a developer has picked the
//! task up on the board (`InProgress`, `Blocked`, or `Complete` — the
//! "dev-owned" statuses), the sheet must never be allowed to shove the task
//! back to an earlier stage (e.g. a stale `New` row re-synced after the dev
//! already moved the task to `InProgress`). So [`plan_pull`] only sets
//! `UpdateTaskPatch::status` when the task's *current* board status (as
//! reported by the injected `existing_status` closure) is **not**
//! dev-owned; otherwise it leaves `status: None`, meaning "don't touch it".

use crate::config::SheetMapping;
use crate::model::{CanonicalField, CanonicalRow};
use crate::state::State;
use forge_tasks::{CreateTaskRequest, TaskStatus, UpdateTaskPatch};

/// One planned board operation for a single sheet row.
#[derive(Debug)]
pub enum BoardOp {
    /// The row has no [`crate::state::StateEntry`] yet — no task exists.
    Create {
        row_id: String,
        req: CreateTaskRequest,
        target_status: TaskStatus,
    },
    /// The row is already joined to a task and its content changed.
    Update {
        row_id: String,
        task_id: String,
        patch: UpdateTaskPatch,
    },
    /// The row is already joined to a task and its content is unchanged.
    Skip { row_id: String, reason: String },
}

/// The board statuses a developer (not the sheet) owns once reached. Once a
/// task is in one of these, pull planning never overwrites its status — see
/// the module doc's "never-move-backward" rule.
const DEV_OWNED_STATUSES: [TaskStatus; 3] = [
    TaskStatus::InProgress,
    TaskStatus::Blocked,
    TaskStatus::Complete,
];

/// Plans the pull-side board operation for every row in `rows`, in order.
///
/// Per row:
/// 1. No [`State`] entry for `row.id` → [`BoardOp::Create`].
/// 2. An entry exists and its `content_hash` equals the row's [`content_hash`]
///    → [`BoardOp::Skip`] (nothing changed since the last sync).
/// 3. An entry exists and the content changed → [`BoardOp::Update`]. The
///    patch always refreshes `title`/`body`/`priority` from the row; it
///    only sets `status` when `existing_status(&entry.task_id)` is *not*
///    dev-owned (see the module doc).
pub fn plan_pull(
    rows: &[CanonicalRow],
    mapping: &SheetMapping,
    state: &State,
    existing_status: &dyn Fn(&str) -> Option<TaskStatus>,
) -> Vec<BoardOp> {
    rows.iter()
        .map(|row| plan_row(row, mapping, state, existing_status))
        .collect()
}

/// Plans a single row's [`BoardOp`]; see [`plan_pull`] for the rule order.
fn plan_row(
    row: &CanonicalRow,
    mapping: &SheetMapping,
    state: &State,
    existing_status: &dyn Fn(&str) -> Option<TaskStatus>,
) -> BoardOp {
    let Some(entry) = state.rows.get(&row.id) else {
        return BoardOp::Create {
            row_id: row.id.clone(),
            req: build_create_request(row, mapping),
            target_status: target_status(row, mapping),
        };
    };

    let hash = content_hash(row);
    if entry.content_hash == hash {
        return BoardOp::Skip {
            row_id: row.id.clone(),
            reason: "unchanged".to_string(),
        };
    }

    let dev_owned =
        existing_status(&entry.task_id).is_some_and(|status| DEV_OWNED_STATUSES.contains(&status));
    let status = if dev_owned {
        None
    } else {
        Some(target_status(row, mapping).as_str().to_string())
    };

    let patch = UpdateTaskPatch {
        status,
        assignee: None,
        reviewer: None,
        priority: parse_priority(row),
        title: Some(build_title(row)),
        body: build_body(row),
        block_reason: None,
        result: None,
        summary: None,
    };

    BoardOp::Update {
        row_id: row.id.clone(),
        task_id: entry.task_id.clone(),
        patch,
    }
}

/// Builds the `CreateTaskRequest` for a brand-new row. All fields not
/// listed in the task brief's "ALL you may set" set are left `None`
/// (`assignee`, `reviewer`, `workspace_kind`, `workspace_path`, `parents`).
pub fn build_create_request(row: &CanonicalRow, mapping: &SheetMapping) -> CreateTaskRequest {
    CreateTaskRequest {
        title: build_title(row),
        body: build_body(row),
        assignee: None,
        reviewer: None,
        priority: parse_priority(row),
        workspace_kind: None,
        workspace_path: None,
        metadata: Some(build_metadata(row, mapping)),
        created_by: Some("sheet-sync".to_string()),
        skills: build_skills(row),
        parents: None,
    }
}

/// The row's `Status` field looked up in `mapping.status_map`; a blank,
/// absent, or unmapped Status value defaults to [`TaskStatus::Triage`]
/// rather than failing loud — a new/unrecognized sheet status is still a
/// legitimate row to bring onto the board, just at the front of triage.
pub fn target_status(row: &CanonicalRow, mapping: &SheetMapping) -> TaskStatus {
    row.fields
        .get(&CanonicalField::Status)
        .filter(|value| !value.is_empty())
        .and_then(|value| mapping.status_map.get(value.as_str()))
        .cloned()
        .unwrap_or(TaskStatus::Triage)
}

/// A deterministic content hash over `row.fields`, used to detect
/// sheet-side edits between syncs.
///
/// Implementation: inline FNV-1a (64-bit) over the concatenation of
/// `field_name=value\n` for every present field, visited in
/// [`CanonicalField`]'s `Ord` (the fields are already stored in a
/// `BTreeMap`, so iteration is sorted and thus stable). No new crate
/// dependency, no clock/random input — same row always hashes the same.
pub fn content_hash(row: &CanonicalRow) -> String {
    const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;

    let mut hash = FNV_OFFSET_BASIS;
    for (field, value) in &row.fields {
        let line = format!("{field:?}={value}\n");
        for byte in line.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
    }
    format!("fnv1a:{hash:016x}")
}

/// `[<row.id>] <title>`, with an absent `Title` field rendered as `""`.
fn build_title(row: &CanonicalRow) -> String {
    let title = row
        .fields
        .get(&CanonicalField::Title)
        .map(String::as_str)
        .unwrap_or("");
    format!("[{}] {title}", row.id)
}

/// Deterministic labeled-section body: Description, Steps to Reproduce,
/// Expected Result, Actual Result, Environment — in that fixed order,
/// skipping any field that's absent or empty. `None` if every section was
/// skipped (nothing to put in the body).
///
/// Note: the task brief additionally calls for an `Evidence:`/attachments
/// line "only if present", but [`CanonicalField`] has no evidence/
/// attachment variant in the current model — there is no source field to
/// read, so that line is omitted here. Flagged in the task report as a
/// spec/model mismatch rather than guessed at.
fn build_body(row: &CanonicalRow) -> Option<String> {
    const SECTIONS: [(&str, CanonicalField); 5] = [
        ("Description", CanonicalField::Description),
        ("Steps to Reproduce", CanonicalField::Steps),
        ("Expected Result", CanonicalField::Expected),
        ("Actual Result", CanonicalField::Actual),
        ("Environment", CanonicalField::Environment),
    ];

    let sections: Vec<String> = SECTIONS
        .into_iter()
        .filter_map(|(label, field)| {
            let value = row.fields.get(&field)?;
            if value.is_empty() {
                return None;
            }
            Some(format!("{label}:\n{value}"))
        })
        .collect();

    if sections.is_empty() {
        None
    } else {
        Some(sections.join("\n\n"))
    }
}

/// Parses the leading `P<n>` in the row's `Priority` field (e.g.
/// `"P1 - High"` → `Some(1)`, `"P0 - Blocker"` → `Some(0)`). `None` if the
/// field is absent or doesn't start with `P<digits>`.
fn parse_priority(row: &CanonicalRow) -> Option<i32> {
    let value = row.fields.get(&CanonicalField::Priority)?;
    let rest = value.strip_prefix('P')?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse::<i32>().ok()
    }
}

/// Tags: `type:<Type>` and `severity:<Severity>` if those fields are
/// present, plus `"mvp-blocker"` if `MvpBlocker` starts with "Yes"
/// (case-insensitive). `None` if no tag applies (rather than `Some(vec![])`).
fn build_skills(row: &CanonicalRow) -> Option<Vec<String>> {
    let mut skills = Vec::new();

    if let Some(type_value) = row.fields.get(&CanonicalField::Type) {
        if !type_value.is_empty() {
            skills.push(format!("type:{type_value}"));
        }
    }
    if let Some(severity) = row.fields.get(&CanonicalField::Severity) {
        if !severity.is_empty() {
            skills.push(format!("severity:{severity}"));
        }
    }
    if let Some(mvp_blocker) = row.fields.get(&CanonicalField::MvpBlocker) {
        if mvp_blocker.to_ascii_lowercase().starts_with("yes") {
            skills.push("mvp-blocker".to_string());
        }
    }

    if skills.is_empty() {
        None
    } else {
        Some(skills)
    }
}

/// `{"source":"gsheet","sheet_id":<mapping.spreadsheet_id>,"row_id":<row.id>}`
fn build_metadata(row: &CanonicalRow, mapping: &SheetMapping) -> String {
    serde_json::json!({
        "source": "gsheet",
        "sheet_id": mapping.spreadsheet_id,
        "row_id": row.id,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::StateEntry;
    use std::collections::BTreeMap;

    /// Same shape as `config::tests::VALID_TOML` / `mapping::tests::QA_MAPPING_TOML`,
    /// reproduced here so this module's tests don't depend on another
    /// module's private test fixtures.
    const QA_MAPPING_TOML: &str = r#"
spreadsheet_id = "EXAMPLE_SPREADSHEET_ID"
tab            = "QA Log"
id_column      = "QA Log ID"

writable = ["status", "fix_ver", "resolution_notes"]
dev_writable_status = ["In Progress", "In Review", "Fixed - Needs Verification"]
terminal_status = ["Verified/Closed", "Won't Fix", "Duplicate"]

[columns]
"QA Log ID"          = "id"
"Title"              = "title"
"Type"               = "type"
"Category"           = "category"
"Description"        = "description"
"Steps to Reproduce" = "steps"
"Expected Result"    = "expected"
"Actual Result"      = "actual"
"Environment"        = "environment"
"Severity"           = "severity"
"Priority"           = "priority"
"MVP Blocker?"       = "mvp_blocker"
"Status"             = "status"
"Build/Version Fixed (git commit for demo, version tag for prod)" = "fix_ver"
"Resolution Notes"   = "resolution_notes"

[status_map]
"New"                       = "triage"
"Triaged"                   = "ready"
"In Progress"               = "in_progress"
"In Review"                 = "in_progress"
"Fixed - Needs Verification" = "complete"
"Verified/Closed"           = "archived"
"Won't Fix"                 = "archived"
"Duplicate"                 = "archived"
"Deferred"                  = "blocked"
"#;

    fn qa_mapping() -> SheetMapping {
        SheetMapping::from_toml_str(QA_MAPPING_TOML).expect("fixture TOML should parse")
    }

    /// `QA-016`, "In Progress" (maps to `TaskStatus::InProgress`), `P1 -
    /// High` priority, with every body-relevant field populated.
    fn qa_016() -> CanonicalRow {
        let mut fields = BTreeMap::new();
        fields.insert(
            CanonicalField::Title,
            "Login button unresponsive".to_string(),
        );
        fields.insert(CanonicalField::Type, "Bug".to_string());
        fields.insert(
            CanonicalField::Description,
            "Button does not respond to click".to_string(),
        );
        fields.insert(
            CanonicalField::Steps,
            "1. Open app\n2. Click login".to_string(),
        );
        fields.insert(CanonicalField::Expected, "Login modal opens".to_string());
        fields.insert(CanonicalField::Actual, "Nothing happens".to_string());
        fields.insert(
            CanonicalField::Environment,
            "Chrome 120 / macOS".to_string(),
        );
        fields.insert(CanonicalField::Severity, "High".to_string());
        fields.insert(CanonicalField::Priority, "P1 - High".to_string());
        fields.insert(CanonicalField::MvpBlocker, "Yes".to_string());
        fields.insert(CanonicalField::Status, "In Progress".to_string());

        CanonicalRow {
            id: "QA-016".to_string(),
            fields,
            sheet_row_index: 0,
        }
    }

    fn no_task_exists(_task_id: &str) -> Option<TaskStatus> {
        None
    }

    // -- build_create_request / target_status ------------------------------

    #[test]
    fn new_row_plans_create_with_title_priority_status_and_metadata() {
        let mapping = qa_mapping();
        let row = qa_016();
        let state = State::default();

        let ops = plan_pull(
            std::slice::from_ref(&row),
            &mapping,
            &state,
            &no_task_exists,
        );
        assert_eq!(ops.len(), 1);

        match &ops[0] {
            BoardOp::Create {
                row_id,
                req,
                target_status: status,
            } => {
                assert_eq!(row_id, "QA-016");
                assert_eq!(req.title, "[QA-016] Login button unresponsive");
                assert_eq!(req.priority, Some(1));
                assert_eq!(*status, TaskStatus::InProgress);

                let metadata = req.metadata.as_ref().expect("metadata should be set");
                let parsed: serde_json::Value =
                    serde_json::from_str(metadata).expect("metadata should be valid JSON");
                assert_eq!(parsed["row_id"], "QA-016");
                assert_eq!(parsed["source"], "gsheet");
                assert_eq!(parsed["sheet_id"], mapping.spreadsheet_id);
            }
            other => panic!("expected Create, got {other:?}"),
        }
    }

    #[test]
    fn build_create_request_sets_skills_and_created_by() {
        let mapping = qa_mapping();
        let row = qa_016();

        let req = build_create_request(&row, &mapping);
        assert_eq!(req.created_by, Some("sheet-sync".to_string()));
        let skills = req.skills.expect("skills should be Some");
        assert!(skills.contains(&"type:Bug".to_string()));
        assert!(skills.contains(&"severity:High".to_string()));
        assert!(skills.contains(&"mvp-blocker".to_string()));
    }

    #[test]
    fn target_status_maps_in_progress() {
        let mapping = qa_mapping();
        let row = qa_016();
        assert_eq!(target_status(&row, &mapping), TaskStatus::InProgress);
    }

    #[test]
    fn target_status_defaults_to_triage_for_blank_status() {
        let mapping = qa_mapping();
        let mut row = qa_016();
        row.fields.remove(&CanonicalField::Status);
        assert_eq!(target_status(&row, &mapping), TaskStatus::Triage);
    }

    #[test]
    fn target_status_defaults_to_triage_for_unmapped_status() {
        let mapping = qa_mapping();
        let mut row = qa_016();
        row.fields
            .insert(CanonicalField::Status, "Some Unknown Status".to_string());
        assert_eq!(target_status(&row, &mapping), TaskStatus::Triage);
    }

    // -- plan_pull: skip / update -------------------------------------------

    #[test]
    fn existing_row_unchanged_content_is_skipped() {
        let mapping = qa_mapping();
        let row = qa_016();
        let mut state = State::default();
        state.upsert(
            "QA-016".to_string(),
            StateEntry {
                task_id: "t_existing01".to_string(),
                content_hash: content_hash(&row),
                last_push_status: None,
            },
        );

        let ops = plan_pull(&[row], &mapping, &state, &no_task_exists);
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            BoardOp::Skip { row_id, reason } => {
                assert_eq!(row_id, "QA-016");
                assert_eq!(reason, "unchanged");
            }
            other => panic!("expected Skip, got {other:?}"),
        }
    }

    #[test]
    fn changed_row_with_dev_owned_status_never_moves_status_backward() {
        let mapping = qa_mapping();
        let mut row = qa_016();
        let mut state = State::default();
        // Stored hash reflects the *old* content, before the edit below.
        state.upsert(
            "QA-016".to_string(),
            StateEntry {
                task_id: "t_existing01".to_string(),
                content_hash: content_hash(&row),
                last_push_status: None,
            },
        );
        // Sheet-side edit: the row's content now differs from stored hash.
        row.fields.insert(
            CanonicalField::Description,
            "Updated description".to_string(),
        );

        let existing_status = |task_id: &str| -> Option<TaskStatus> {
            assert_eq!(task_id, "t_existing01");
            Some(TaskStatus::InProgress)
        };

        let ops = plan_pull(&[row], &mapping, &state, &existing_status);
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            BoardOp::Update {
                row_id,
                task_id,
                patch,
            } => {
                assert_eq!(row_id, "QA-016");
                assert_eq!(task_id, "t_existing01");
                assert_eq!(
                    patch.status, None,
                    "dev-owned InProgress status must never move backward"
                );
                assert_eq!(
                    patch.title,
                    Some("[QA-016] Login button unresponsive".to_string())
                );
            }
            other => panic!("expected Update, got {other:?}"),
        }
    }

    #[test]
    fn changed_row_with_non_dev_owned_status_advances_status_from_map() {
        let mapping = qa_mapping();
        let mut row = qa_016();
        let mut state = State::default();
        state.upsert(
            "QA-016".to_string(),
            StateEntry {
                task_id: "t_existing02".to_string(),
                content_hash: content_hash(&row),
                last_push_status: None,
            },
        );
        row.fields.insert(
            CanonicalField::Description,
            "Updated description".to_string(),
        );

        let existing_status = |_task_id: &str| -> Option<TaskStatus> { Some(TaskStatus::Triage) };

        let ops = plan_pull(&[row], &mapping, &state, &existing_status);
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            BoardOp::Update { patch, .. } => {
                // qa_016's Status field is "In Progress" -> status_map -> in_progress.
                assert_eq!(patch.status, Some("in_progress".to_string()));
            }
            other => panic!("expected Update, got {other:?}"),
        }
    }

    // -- content_hash --------------------------------------------------------

    #[test]
    fn content_hash_is_stable_across_calls() {
        let row = qa_016();
        assert_eq!(content_hash(&row), content_hash(&row));
    }

    #[test]
    fn content_hash_changes_when_a_field_changes() {
        let row = qa_016();
        let mut changed = row.clone();
        changed.fields.insert(
            CanonicalField::Description,
            "Something else entirely".to_string(),
        );

        assert_ne!(content_hash(&row), content_hash(&changed));
    }

    #[test]
    fn priority_parses_p0_and_p1() {
        let mut row = qa_016();
        row.fields
            .insert(CanonicalField::Priority, "P0 - Blocker".to_string());
        assert_eq!(parse_priority(&row), Some(0));

        row.fields
            .insert(CanonicalField::Priority, "P1 - High".to_string());
        assert_eq!(parse_priority(&row), Some(1));
    }

    #[test]
    fn priority_is_none_when_absent_or_unparseable() {
        let mut row = qa_016();
        row.fields.remove(&CanonicalField::Priority);
        assert_eq!(parse_priority(&row), None);

        row.fields
            .insert(CanonicalField::Priority, "High".to_string());
        assert_eq!(parse_priority(&row), None);
    }
}
