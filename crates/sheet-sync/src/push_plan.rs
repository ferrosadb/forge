//! Push edit computation: dev-side write-back planning.
//!
//! [`plan_push`] turns a [`PushRequest`] into the exact [`CellEdit`]s to
//! write back to the sheet. It is pure logic — no network — and never
//! writes anything itself; a later (not-yet-built) task applies the edits
//! this module produces via the Sheets API.
//!
//! ## Assumptions
//!
//! The sheet has exactly one header row at sheet row 1, so
//! `grid.rows[i]` is sheet row `i + 2` (one header row, one-based sheet
//! rows). Rows may be ragged (the Sheets API omits trailing empty cells):
//! a missing cell's `old` value is `""`, never a panic — see
//! `crate::mapping::map_grid`'s doc for the same convention.
//!
//! ## Lifecycle handoff and blast radius (fail loud)
//!
//! 1. `mapping.id_column` must be present in `grid.headers`, or `Err`.
//! 2. A `row_id` that normalizes to an id shared by more than one row in
//!    the grid (see [`crate::normalize::find_duplicate_ids`]) is ambiguous
//!    and refused with `Err` — never silently joined to "the first match".
//! 3. A `row_id` matching no row is `Err`.
//! 4. Blast radius: a requested field whose [`CanonicalField`] is not in
//!    `mapping.writable` is silently dropped from the plan (not an error —
//!    the mapping author's `writable` set is the authority on what the dev
//!    side may ever touch). A field that *is* writable but whose mapped
//!    header is missing from `grid.headers` is `Err` (no partial write).
//! 5. Status-only handoff gate: a status edit is only ever emitted when the
//!    requested value is in `mapping.dev_writable_status` *and* the row's
//!    **current** sheet Status is not in `mapping.terminal_status` (the
//!    sheet owner has already closed the row out; the dev side must never
//!    reopen it by pushing a status). `fix_ver` and `resolution_notes` have
//!    no such gate — they're emitted whenever requested and writable.
//! 6. As defense in depth, every edit this function returns is re-checked
//!    against `mapping.writable` before returning — see the private
//!    `assert_blast_radius` helper below.

use crate::config::SheetMapping;
use crate::model::{col_index_to_a1, CanonicalField, CellEdit, Grid};
use crate::normalize::{find_duplicate_ids, normalize_id};
use crate::state::State;

/// One dev-side push request: the sheet row to write back to, plus the
/// (optional) new value for each of the three fields the sync engine ever
/// pushes. A `None` field means "the caller didn't change it" — it's simply
/// left out of the plan, not written as blank.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushRequest {
    pub row_id: String,
    pub status: Option<String>,
    pub fix_ver: Option<String>,
    pub notes: Option<String>,
}

/// Plans the [`CellEdit`]s to write back for `req` against `grid`, honoring
/// `mapping`'s blast radius and lifecycle handoff rules. See the module doc
/// for the full rule order. `state` is accepted per the task interface but
/// unused by the core computation — the row-to-task join it holds is not
/// needed to decide *which cells* to write, only relevant to a later
/// executor step.
pub fn plan_push(
    req: &PushRequest,
    grid: &Grid,
    mapping: &SheetMapping,
    _state: &State,
) -> anyhow::Result<Vec<CellEdit>> {
    let id_index = id_column_index(grid, mapping)?;
    let row_index = find_target_row(req, grid, id_index)?;
    let row = &grid.rows[row_index];

    let mut edits = Vec::new();

    if let Some(new_status) = &req.status {
        if let Some((col_index, header)) =
            resolve_writable_column(grid, mapping, CanonicalField::Status)?
        {
            let old = cell(row, col_index).to_string();
            let dev_writable = mapping.dev_writable_status.contains(new_status.as_str());
            let not_terminal = !mapping.terminal_status.contains(old.as_str());
            if dev_writable && not_terminal {
                edits.push(CellEdit {
                    a1: format!("{}{}", col_index_to_a1(col_index), row_index + 2),
                    header,
                    old,
                    new: new_status.clone(),
                });
            }
        }
    }

    for (value, field) in [
        (&req.fix_ver, CanonicalField::FixVer),
        (&req.notes, CanonicalField::ResolutionNotes),
    ] {
        let Some(new_value) = value else { continue };
        if let Some((col_index, header)) = resolve_writable_column(grid, mapping, field)? {
            let old = cell(row, col_index).to_string();
            edits.push(CellEdit {
                a1: format!("{}{}", col_index_to_a1(col_index), row_index + 2),
                header,
                old,
                new: new_value.clone(),
            });
        }
    }

    assert_blast_radius(&edits, mapping)?;

    Ok(edits)
}

/// Locates `mapping.id_column` in `grid.headers`, failing loud (naming the
/// column) if it's absent.
fn id_column_index(grid: &Grid, mapping: &SheetMapping) -> anyhow::Result<usize> {
    grid.headers
        .iter()
        .position(|header| header == &mapping.id_column)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "push plan: id column {:?} not found in grid headers {:?}",
                mapping.id_column,
                grid.headers
            )
        })
}

/// Finds the single data-row index whose normalized id-cell matches
/// `req.row_id` (also normalized). `Err` if `req.row_id` normalizes to an
/// id shared by more than one row (ambiguous) or to no row at all.
fn find_target_row(req: &PushRequest, grid: &Grid, id_index: usize) -> anyhow::Result<usize> {
    let normalized_ids: Vec<String> = grid
        .rows
        .iter()
        .map(|row| normalize_id(cell(row, id_index)))
        .filter(|id| !id.is_empty())
        .collect();
    let duplicate_ids = find_duplicate_ids(&normalized_ids);

    let target_id = normalize_id(&req.row_id);
    if target_id.is_empty() {
        anyhow::bail!(
            "push plan: row_id {:?} normalizes to an empty id",
            req.row_id
        );
    }
    if duplicate_ids.contains(&target_id) {
        anyhow::bail!(
            "push plan: row_id {:?} is ambiguous — normalized id {target_id:?} appears on more than one sheet row",
            req.row_id
        );
    }

    grid.rows
        .iter()
        .position(|row| normalize_id(cell(row, id_index)) == target_id)
        .ok_or_else(|| anyhow::anyhow!("push plan: row_id {:?} not found in grid", req.row_id))
}

/// Resolves `field`'s writable column: `Ok(None)` if `field` is outside
/// `mapping.writable` (the blast-radius skip — not an error). Otherwise
/// looks up the header `mapping.columns` maps to `field` and that header's
/// position in `grid.headers`, failing loud if the header is missing from
/// the grid — a writable field with no matching column must never result
/// in a partial/silent write.
fn resolve_writable_column(
    grid: &Grid,
    mapping: &SheetMapping,
    field: CanonicalField,
) -> anyhow::Result<Option<(usize, String)>> {
    if !mapping.writable.contains(&field) {
        return Ok(None);
    }

    // Invariant from `SheetMapping::try_from`: every `writable` entry names
    // a field present as a value in `columns`, so this lookup cannot fail
    // for a validly constructed mapping — but we still fail loud rather
    // than `unwrap`, in case that invariant is ever weakened.
    let header = mapping
        .columns
        .iter()
        .find(|(_, mapped)| **mapped == field)
        .map(|(header, _)| header.clone())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "push plan: {field:?} is in `writable` but not mapped in `columns` (invalid SheetMapping)"
            )
        })?;

    let col_index = grid
        .headers
        .iter()
        .position(|candidate| candidate == &header)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "push plan: writable header {header:?} not found in grid headers {:?}",
                grid.headers
            )
        })?;

    Ok(Some((col_index, header)))
}

/// Defense in depth: every edit this function is about to return must
/// resolve (via `mapping.columns`) to a [`CanonicalField`] in
/// `mapping.writable`. Given the construction above this should be
/// unreachable — every edit already passed the [`resolve_writable_column`]
/// gate before being pushed — but it exists so a future change that
/// accidentally emits an edit for a non-writable field fails loud instead
/// of silently expanding the write blast radius.
fn assert_blast_radius(edits: &[CellEdit], mapping: &SheetMapping) -> anyhow::Result<()> {
    for edit in edits {
        let writable = mapping
            .columns
            .get(&edit.header)
            .is_some_and(|field| mapping.writable.contains(field));
        if !writable {
            anyhow::bail!(
                "push plan: blast-radius violation — edit for header {:?} is not in `writable`",
                edit.header
            );
        }
    }
    Ok(())
}

/// Reads `row[idx]`, treating any index past the (possibly ragged) row's
/// end as an empty string rather than panicking. Mirrors
/// `crate::mapping::cell`, duplicated locally since that helper is private
/// to its module.
fn cell(row: &[String], idx: usize) -> &str {
    row.get(idx).map(String::as_str).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Same shape as `config::tests::VALID_TOML` / `mapping::tests::QA_MAPPING_TOML`
    /// (real QA-sheet headers per `specs/todo/feat-sheet-sync.md`),
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

    /// A mapping identical to [`qa_mapping`] except `writable` excludes
    /// `status` — used by test (e), the blast-radius-skip case.
    fn qa_mapping_status_not_writable() -> SheetMapping {
        let toml = QA_MAPPING_TOML.replace(
            r#"writable = ["status", "fix_ver", "resolution_notes"]"#,
            r#"writable = ["fix_ver", "resolution_notes"]"#,
        );
        SheetMapping::from_toml_str(&toml).expect("fixture TOML should parse")
    }

    fn qa_headers() -> Vec<String> {
        [
            "QA Log ID",
            "Title",
            "Type",
            "Category",
            "Description",
            "Steps to Reproduce",
            "Expected Result",
            "Actual Result",
            "Environment",
            "Severity",
            "Priority",
            "MVP Blocker?",
            "Status",
            "Build/Version Fixed (git commit for demo, version tag for prod)",
            "Resolution Notes",
        ]
        .into_iter()
        .map(String::from)
        .collect()
    }

    fn row(cells: &[&str]) -> Vec<String> {
        cells.iter().map(|c| c.to_string()).collect()
    }

    /// `QA-005`, Status = `status`, with blank fix_ver/resolution_notes.
    fn qa_row(id: &str, status: &str) -> Vec<String> {
        row(&[
            id,
            "Some title",
            "Bug",
            "UI",
            "desc",
            "steps",
            "expected",
            "actual",
            "env",
            "Medium",
            "P2",
            "No",
            status,
            "",
            "",
        ])
    }

    fn grid_with_rows(rows: Vec<Vec<String>>) -> Grid {
        Grid {
            headers: qa_headers(),
            rows,
        }
    }

    fn base_request(row_id: &str) -> PushRequest {
        PushRequest {
            row_id: row_id.to_string(),
            status: None,
            fix_ver: None,
            notes: None,
        }
    }

    // -- (a) dev-writable status on a New row -> one status edit -----------

    #[test]
    fn dev_writable_status_on_new_row_emits_one_status_edit() {
        let mapping = qa_mapping();
        let grid = grid_with_rows(vec![qa_row("QA-001", "New")]);
        let req = PushRequest {
            status: Some("In Progress".to_string()),
            ..base_request("QA-001")
        };

        let edits = plan_push(&req, &grid, &mapping, &State::default())
            .expect("dev-writable status push should succeed");

        assert_eq!(edits.len(), 1);
        let edit = &edits[0];
        assert_eq!(edit.header, "Status");
        assert_eq!(edit.old, "New");
        assert_eq!(edit.new, "In Progress");
        // Row 0 -> sheet row 2; "Status" is column index 12 -> "M".
        assert_eq!(edit.a1, "M2");
    }

    // -- (b) status not dev-writable -> omitted, fix_ver still emitted -----

    #[test]
    fn non_dev_writable_status_is_omitted_but_fix_ver_still_emitted() {
        let mapping = qa_mapping();
        let grid = grid_with_rows(vec![qa_row("QA-002", "New")]);
        let req = PushRequest {
            status: Some("Verified/Closed".to_string()),
            fix_ver: Some("abc123".to_string()),
            ..base_request("QA-002")
        };

        let edits = plan_push(&req, &grid, &mapping, &State::default())
            .expect("push with non-dev-writable status should still succeed");

        assert_eq!(edits.len(), 1, "status edit must be omitted: {edits:?}");
        assert_eq!(
            edits[0].header,
            "Build/Version Fixed (git commit for demo, version tag for prod)"
        );
        assert_eq!(edits[0].new, "abc123");
    }

    // -- (c) row already terminal -> status omitted, fix_ver+notes emitted -

    #[test]
    fn terminal_current_status_omits_status_but_emits_fix_ver_and_notes() {
        let mapping = qa_mapping();
        let grid = grid_with_rows(vec![qa_row("QA-003", "Won't Fix")]);
        let req = PushRequest {
            row_id: "QA-003".to_string(),
            status: Some("In Progress".to_string()),
            fix_ver: Some("v1.2".to_string()),
            notes: Some("done".to_string()),
        };

        let edits = plan_push(&req, &grid, &mapping, &State::default())
            .expect("push against a terminal row should still succeed for non-status fields");

        assert_eq!(
            edits.len(),
            2,
            "status edit must be omitted for a terminal row: {edits:?}"
        );
        assert!(edits.iter().all(|e| e.header != "Status"));
        assert!(edits.iter().any(|e| e.new == "v1.2"));
        assert!(edits.iter().any(|e| e.new == "done"));
    }

    // -- (d) unknown row_id -> Err ------------------------------------------

    #[test]
    fn unknown_row_id_is_err() {
        let mapping = qa_mapping();
        let grid = grid_with_rows(vec![qa_row("QA-004", "New")]);
        let req = PushRequest {
            status: Some("In Progress".to_string()),
            ..base_request("QA-999")
        };

        let err = plan_push(&req, &grid, &mapping, &State::default())
            .expect_err("unknown row_id should fail loud");
        assert!(
            err.to_string().contains("QA-999"),
            "error should mention the missing row_id, got: {err}"
        );
    }

    // -- (e) status not in mapping.writable -> silently skipped ------------

    #[test]
    fn status_outside_writable_set_is_silently_skipped_fix_ver_still_emitted() {
        let mapping = qa_mapping_status_not_writable();
        let grid = grid_with_rows(vec![qa_row("QA-005a", "New")]);
        let req = PushRequest {
            status: Some("In Progress".to_string()),
            fix_ver: Some("v9".to_string()),
            ..base_request("QA-005a")
        };

        let edits = plan_push(&req, &grid, &mapping, &State::default())
            .expect("push with a non-writable status field should not error");

        assert_eq!(edits.len(), 1, "status must be dropped silently: {edits:?}");
        assert_eq!(
            edits[0].header,
            "Build/Version Fixed (git commit for demo, version tag for prod)"
        );
        assert_eq!(edits[0].new, "v9");
    }

    #[test]
    fn status_outside_writable_set_with_no_other_fields_yields_no_edits_and_no_error() {
        let mapping = qa_mapping_status_not_writable();
        let grid = grid_with_rows(vec![qa_row("QA-005b", "New")]);
        let req = PushRequest {
            status: Some("In Progress".to_string()),
            ..base_request("QA-005b")
        };

        let edits = plan_push(&req, &grid, &mapping, &State::default())
            .expect("push with only a non-writable status field should not error");
        assert!(edits.is_empty());
    }

    // -- (f) duplicate ids -> Err --------------------------------------------

    #[test]
    fn duplicate_row_id_is_err() {
        let mapping = qa_mapping();
        let grid = grid_with_rows(vec![
            qa_row("QA-005", "New"),
            qa_row("QA-005 (This may not be a bug)", "New"),
        ]);
        let req = PushRequest {
            status: Some("In Progress".to_string()),
            ..base_request("QA-005")
        };

        let err = plan_push(&req, &grid, &mapping, &State::default())
            .expect_err("duplicate row_id should fail loud");
        assert!(
            err.to_string().contains("ambiguous") || err.to_string().contains("QA-005"),
            "error should name the ambiguity, got: {err}"
        );
    }

    // -- ragged rows: missing cell -> old == "" ------------------------------

    #[test]
    fn ragged_row_reports_empty_old_value_without_panicking() {
        let mapping = qa_mapping();
        // Short row: only "QA-006" and "Title" present, everything else
        // (including Status, Resolution Notes) is missing/ragged.
        let grid = grid_with_rows(vec![row(&["QA-006", "Some title"])]);
        let req = PushRequest {
            row_id: "QA-006".to_string(),
            status: Some("In Progress".to_string()),
            fix_ver: None,
            notes: Some("done".to_string()),
        };

        let edits = plan_push(&req, &grid, &mapping, &State::default())
            .expect("ragged row should not panic");

        // Status old cell is missing -> "" -> not in terminal_status -> the
        // status edit is still emitted (dev-writable and not terminal).
        let status_edit = edits
            .iter()
            .find(|e| e.header == "Status")
            .expect("status edit should be present");
        assert_eq!(status_edit.old, "");

        let notes_edit = edits
            .iter()
            .find(|e| e.header == "Resolution Notes")
            .expect("notes edit should be present");
        assert_eq!(notes_edit.old, "");
        assert_eq!(notes_edit.new, "done");
    }

    // -- no fields requested -> empty edits, no error ------------------------

    #[test]
    fn no_requested_fields_yields_no_edits() {
        let mapping = qa_mapping();
        let grid = grid_with_rows(vec![qa_row("QA-007", "New")]);
        let req = base_request("QA-007");

        let edits = plan_push(&req, &grid, &mapping, &State::default())
            .expect("push with no requested fields should succeed trivially");
        assert!(edits.is_empty());
    }
}
