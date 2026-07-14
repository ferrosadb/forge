//! End-to-end pull/push orchestration.
//!
//! This module wires the pure engine modules (`mapping`, `board_plan`,
//! `push_plan`, `state`) to the I/O seam traits ([`crate::sheets::SheetsApi`],
//! [`crate::board::BoardSink`]), with `dry_run` support on both directions.
//! No module *above* this one reaches the network or CQL directly — that's
//! the point of the seam: [`pull`]/[`push`] are exercised end-to-end in this
//! module's tests via `FakeSheets`/`FakeBoard`, no network, no CQL.

use std::collections::HashMap;
use std::path::Path;

use crate::board::BoardSink;
use crate::board_plan::{content_hash, plan_pull, BoardOp};
use crate::config::SheetMapping;
use crate::mapping::map_grid;
use crate::model::{CanonicalField, CanonicalRow, CellEdit};
use crate::push_plan::{plan_push, PushRequest};
use crate::sheets::SheetsApi;
use crate::state::{State, StateEntry};

/// Options for [`pull`]. `dry_run: true` computes and reports what pull
/// *would* do without calling [`BoardSink::apply`] or saving the sidecar
/// state file.
#[derive(Debug, Clone, Copy, Default)]
pub struct PullOptions {
    pub dry_run: bool,
}

/// Options for [`push`]. `dry_run: true` computes and reports the edits
/// [`push`] *would* write without calling [`SheetsApi::write_cells`] or
/// saving the sidecar state file.
#[derive(Debug, Clone, Copy, Default)]
pub struct PushOptions {
    pub dry_run: bool,
}

/// Summary of a [`pull`] run. This becomes the CLI/MCP JSON output, so every
/// field is `Serialize`.
#[derive(Debug, serde::Serialize)]
pub struct PullReport {
    pub created: usize,
    pub updated: usize,
    pub skipped: usize,
    pub skipped_terminal: Vec<String>,
    pub duplicate_ids: Vec<String>,
    pub dry_run: bool,
}

/// Summary of a [`push`] run. This becomes the CLI/MCP JSON output, so every
/// field is `Serialize`.
#[derive(Debug, serde::Serialize)]
pub struct PushReport {
    pub row_id: String,
    pub edits: Vec<CellEdit>,
    pub wrote: bool,
    pub dry_run: bool,
}

/// Runs a full pull: sheet → canonical rows → board ops → (unless
/// `opts.dry_run`) board mutation + sidecar state update.
///
/// Steps:
/// 1. Read the grid and map it to canonical rows (`mapping::map_grid`).
/// 2. Load the sidecar `state::State`.
/// 3. Plan every row's [`BoardOp`] (`board_plan::plan_pull`), tally
///    created/updated/skipped from the op kinds.
/// 4. `dry_run`: return the report as-is — **no** `board.apply` call, **no**
///    `state.save` call, so a dry-run pull is guaranteed side-effect-free.
/// 5. Otherwise: apply every non-`Skip` op to `board`, `state.upsert` the
///    corresponding entry, then persist `state` once at the end.
///
/// Borrow-checker note: `plan_pull` takes `existing_status` as a `&dyn
/// Fn(&str) -> Option<TaskStatus>`, which borrows `board` immutably inside
/// the closure. `ops` is therefore computed to completion (dropping that
/// closure and its borrow) *before* the apply loop below ever borrows
/// `board` mutably.
pub fn pull(
    sheets: &dyn SheetsApi,
    board: &mut dyn BoardSink,
    mapping: &SheetMapping,
    state_path: &Path,
    opts: &PullOptions,
) -> anyhow::Result<PullReport> {
    let grid = sheets.read_grid(&mapping.spreadsheet_id, &mapping.tab)?;
    let mapped = map_grid(&grid, mapping)?;
    let mut state = State::load(state_path)?;

    let row_by_id: HashMap<&str, &CanonicalRow> = mapped
        .rows
        .iter()
        .map(|row| (row.id.as_str(), row))
        .collect();

    // See the borrow-checker note above: this must fully materialize (and
    // thus drop the `existing_status` closure's shared borrow of `board`)
    // before the apply loop takes a mutable borrow of `board`.
    let ops = plan_pull(&mapped.rows, mapping, &state, &|task_id| {
        board.existing_status(task_id)
    });

    let mut created = 0usize;
    let mut updated = 0usize;
    let mut skipped = 0usize;
    for op in &ops {
        match op {
            BoardOp::Create { .. } => created += 1,
            BoardOp::Update { .. } => updated += 1,
            BoardOp::Skip { .. } => skipped += 1,
        }
    }

    if opts.dry_run {
        return Ok(PullReport {
            created,
            updated,
            skipped,
            skipped_terminal: mapped.skipped_terminal,
            duplicate_ids: mapped.duplicate_ids,
            dry_run: true,
        });
    }

    for op in &ops {
        match op {
            BoardOp::Skip { .. } => {}
            BoardOp::Create { row_id, .. } => {
                let task_id = board.apply(op)?.ok_or_else(|| {
                    anyhow::anyhow!(
                        "pull: BoardSink::apply(Create) for row {row_id:?} returned no task_id"
                    )
                })?;
                let row = lookup_row(&row_by_id, row_id)?;
                state.upsert(
                    row_id.clone(),
                    StateEntry {
                        task_id,
                        content_hash: content_hash(row),
                        last_push_status: None,
                    },
                );
            }
            BoardOp::Update {
                row_id, task_id, ..
            } => {
                board.apply(op)?;
                let row = lookup_row(&row_by_id, row_id)?;
                let prior_push_status = state
                    .rows
                    .get(row_id)
                    .and_then(|entry| entry.last_push_status.clone());
                state.upsert(
                    row_id.clone(),
                    StateEntry {
                        task_id: task_id.clone(),
                        content_hash: content_hash(row),
                        last_push_status: prior_push_status,
                    },
                );
            }
        }
    }
    state.save(state_path)?;

    Ok(PullReport {
        created,
        updated,
        skipped,
        skipped_terminal: mapped.skipped_terminal,
        duplicate_ids: mapped.duplicate_ids,
        dry_run: false,
    })
}

/// Looks up `row_id` in `row_by_id`. `plan_pull` only ever emits a `Create`/
/// `Update` op for a row present in `mapped.rows` (the same slice
/// `row_by_id` was built from), so a miss here means that invariant broke —
/// fail loud rather than silently skipping the state update.
fn lookup_row<'a>(
    row_by_id: &HashMap<&str, &'a CanonicalRow>,
    row_id: &str,
) -> anyhow::Result<&'a CanonicalRow> {
    row_by_id.get(row_id).copied().ok_or_else(|| {
        anyhow::anyhow!("pull: no canonical row found for row_id {row_id:?} after planning")
    })
}

/// Runs a full push: sheet grid + sidecar state → computed edits →
/// (unless `opts.dry_run`, or there's nothing to write) write + sidecar
/// `last_push_status` update.
///
/// `wrote` is `false` whenever `opts.dry_run` **or** `edits` came back empty
/// (nothing to write is not a dry-run, but it's still not a write) — in
/// neither case is `SheetsApi::write_cells` or `State::save` called.
pub fn push(
    sheets: &dyn SheetsApi,
    mapping: &SheetMapping,
    state_path: &Path,
    req: &PushRequest,
    opts: &PushOptions,
) -> anyhow::Result<PushReport> {
    let grid = sheets.read_grid(&mapping.spreadsheet_id, &mapping.tab)?;
    let mut state = State::load(state_path)?;

    let edits = plan_push(req, &grid, mapping, &state)?;

    let wrote = if opts.dry_run || edits.is_empty() {
        false
    } else {
        // `last_push_status` must record the status WE last *wrote* to the
        // sheet, not merely the status the caller *requested* — a
        // fix_ver/notes-only push (`req.status: None`) must not clobber a
        // real prior value back to `None`, and a status edit dropped by
        // `plan_push`'s terminal/handoff gate must not phantom-record a
        // status that was never actually written. So only update it when a
        // Status-column edit is present in `edits`.
        let status_header: Option<&String> = mapping
            .columns
            .iter()
            .find(|(_, field)| **field == CanonicalField::Status)
            .map(|(header, _)| header);
        let wrote_status =
            status_header.is_some_and(|header| edits.iter().any(|edit| &edit.header == header));

        sheets.write_cells(&mapping.spreadsheet_id, &edits)?;
        if wrote_status {
            if let Some(entry) = state.rows.get_mut(&req.row_id) {
                entry.last_push_status = req.status.clone();
            }
        }
        state.save(state_path)?;
        true
    };

    Ok(PushReport {
        row_id: req.row_id.clone(),
        edits,
        wrote,
        dry_run: opts.dry_run,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::FakeBoard;
    use crate::model::Grid;
    use crate::sheets::FakeSheets;

    /// Same shape as `mapping::tests::QA_MAPPING_TOML` /
    /// `board_plan::tests::QA_MAPPING_TOML`, reproduced here so this
    /// module's tests don't depend on another module's private fixtures.
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

    /// Fixture grid used by both pull tests: two fresh, importable rows
    /// (QA-010 "New", QA-011 "Triaged"), one terminal row (QA-012
    /// "Verified/Closed"), and a QA-005 id collision across two rows.
    fn pull_fixture_grid() -> Grid {
        Grid {
            headers: qa_headers(),
            rows: vec![
                qa_row("QA-010", "New"),
                qa_row("QA-011", "Triaged"),
                qa_row("QA-012", "Verified/Closed"),
                qa_row("QA-005", "New"),
                qa_row("QA-005 (This may not be a bug)", "New"),
            ],
        }
    }

    // -- pull: dry-run --------------------------------------------------

    #[test]
    fn pull_dry_run_reports_without_touching_board_or_state() {
        let mapping = qa_mapping();
        let sheets = FakeSheets::new(pull_fixture_grid());
        let mut board = FakeBoard::new();
        let tmpdir = tempfile::tempdir().expect("tempdir creation");
        let state_path = tmpdir
            .path()
            .join(".forge")
            .join("sheets")
            .join("qa.state.toml");

        let report = pull(
            &sheets,
            &mut board,
            &mapping,
            &state_path,
            &PullOptions { dry_run: true },
        )
        .expect("dry-run pull should succeed");

        assert!(report.dry_run);
        assert_eq!(
            report.created, 2,
            "QA-010 and QA-011 are new importable rows"
        );
        assert_eq!(report.updated, 0);
        assert_eq!(report.skipped, 0);
        assert_eq!(report.skipped_terminal, vec!["QA-012".to_string()]);
        assert_eq!(report.duplicate_ids, vec!["QA-005".to_string()]);

        assert!(
            board.applied.is_empty(),
            "dry-run pull must make zero board mutations, got {:?}",
            board.applied
        );
        assert!(
            !state_path.exists(),
            "dry-run pull must never create the state file"
        );
    }

    // -- pull: real -------------------------------------------------------

    #[test]
    fn pull_real_applies_creates_and_persists_state() {
        let mapping = qa_mapping();
        let sheets = FakeSheets::new(pull_fixture_grid());
        let mut board = FakeBoard::new();
        let tmpdir = tempfile::tempdir().expect("tempdir creation");
        let state_path = tmpdir
            .path()
            .join(".forge")
            .join("sheets")
            .join("qa.state.toml");

        let report = pull(
            &sheets,
            &mut board,
            &mapping,
            &state_path,
            &PullOptions { dry_run: false },
        )
        .expect("real pull should succeed");

        assert!(!report.dry_run);
        assert_eq!(report.created, 2);
        assert_eq!(report.skipped_terminal, vec!["QA-012".to_string()]);
        assert_eq!(report.duplicate_ids, vec!["QA-005".to_string()]);

        let create_rows: Vec<&str> = board
            .applied
            .iter()
            .filter(|(kind, _)| kind == "create")
            .map(|(_, row_id)| row_id.as_str())
            .collect();
        assert_eq!(create_rows.len(), 2, "applied: {:?}", board.applied);
        assert!(create_rows.contains(&"QA-010"));
        assert!(create_rows.contains(&"QA-011"));

        assert!(state_path.is_file(), "real pull must persist state");
        let reloaded = State::load(&state_path).expect("reload should succeed");
        assert_eq!(reloaded.rows.len(), 2);
        for row_id in ["QA-010", "QA-011"] {
            let entry = reloaded
                .rows
                .get(row_id)
                .unwrap_or_else(|| panic!("state entry for {row_id} should exist"));
            assert!(entry.task_id.starts_with("t_"));
            assert!(!entry.content_hash.is_empty());
        }
    }

    // -- push fixtures ------------------------------------------------------

    fn push_fixture_grid() -> Grid {
        Grid {
            headers: qa_headers(),
            rows: vec![qa_row("QA-020", "New"), qa_row("QA-021", "Won't Fix")],
        }
    }

    fn state_path_in(tmpdir: &tempfile::TempDir) -> std::path::PathBuf {
        tmpdir
            .path()
            .join(".forge")
            .join("sheets")
            .join("qa.state.toml")
    }

    // -- push: dry-run --------------------------------------------------

    #[test]
    fn push_dry_run_returns_edits_without_writing() {
        let mapping = qa_mapping();
        let sheets = FakeSheets::new(push_fixture_grid());
        let tmpdir = tempfile::tempdir().expect("tempdir creation");
        let state_path = state_path_in(&tmpdir);

        let req = PushRequest {
            row_id: "QA-020".to_string(),
            status: Some("In Progress".to_string()),
            fix_ver: None,
            notes: None,
        };

        let report = push(
            &sheets,
            &mapping,
            &state_path,
            &req,
            &PushOptions { dry_run: true },
        )
        .expect("dry-run push should succeed");

        assert!(report.dry_run);
        assert!(!report.wrote);
        assert_eq!(report.edits.len(), 1);
        assert_eq!(report.edits[0].header, "Status");
        assert!(
            sheets.writes.borrow().is_empty(),
            "dry-run push must never call write_cells"
        );
    }

    // -- push: real -------------------------------------------------------

    #[test]
    fn push_real_writes_exactly_the_computed_edits_and_updates_state() {
        let mapping = qa_mapping();
        let sheets = FakeSheets::new(push_fixture_grid());
        let tmpdir = tempfile::tempdir().expect("tempdir creation");
        let state_path = state_path_in(&tmpdir);

        // Row must already be tracked in state for last_push_status to update.
        let mut state = State::default();
        state.upsert(
            "QA-020".to_string(),
            StateEntry {
                task_id: "t_existing01".to_string(),
                content_hash: "fnv1a:whatever".to_string(),
                last_push_status: None,
            },
        );
        state.save(&state_path).expect("seed state save");

        let req = PushRequest {
            row_id: "QA-020".to_string(),
            status: Some("In Progress".to_string()),
            fix_ver: None,
            notes: None,
        };

        let report = push(
            &sheets,
            &mapping,
            &state_path,
            &req,
            &PushOptions { dry_run: false },
        )
        .expect("real push should succeed");

        assert!(!report.dry_run);
        assert!(report.wrote);
        assert_eq!(sheets.writes.borrow().clone(), report.edits);
        assert_eq!(report.edits.len(), 1);

        let reloaded = State::load(&state_path).expect("reload should succeed");
        assert_eq!(
            reloaded.rows["QA-020"].last_push_status,
            Some("In Progress".to_string())
        );
    }

    // -- push: last_push_status gating (only record when Status was written) --

    #[test]
    fn push_fix_ver_only_does_not_clobber_last_push_status() {
        let mapping = qa_mapping();
        // QA-020's current sheet Status is "New" (not terminal), so this is
        // a plain fix_ver-only push with no status edit at all.
        let sheets = FakeSheets::new(push_fixture_grid());
        let tmpdir = tempfile::tempdir().expect("tempdir creation");
        let state_path = state_path_in(&tmpdir);

        let mut state = State::default();
        state.upsert(
            "QA-020".to_string(),
            StateEntry {
                task_id: "t_existing01".to_string(),
                content_hash: "fnv1a:whatever".to_string(),
                last_push_status: Some("In Progress".to_string()),
            },
        );
        state.save(&state_path).expect("seed state save");

        let req = PushRequest {
            row_id: "QA-020".to_string(),
            status: None,
            fix_ver: Some("v1.2.3".to_string()),
            notes: None,
        };

        let report = push(
            &sheets,
            &mapping,
            &state_path,
            &req,
            &PushOptions { dry_run: false },
        )
        .expect("real push should succeed");

        assert!(report.wrote, "fix_ver edit should still be written");
        assert_eq!(report.edits.len(), 1);
        assert_eq!(
            report.edits[0].header,
            "Build/Version Fixed (git commit for demo, version tag for prod)"
        );

        let reloaded = State::load(&state_path).expect("reload should succeed");
        assert_eq!(
            reloaded.rows["QA-020"].last_push_status,
            Some("In Progress".to_string()),
            "a fix_ver-only push must not clobber a prior last_push_status"
        );
    }

    #[test]
    fn push_terminal_gated_status_does_not_record_phantom_last_push_status() {
        let mapping = qa_mapping();
        // QA-021's current sheet Status is "Won't Fix" -> terminal_status,
        // so the requested status edit is dropped by the handoff gate, but
        // the fix_ver edit is still requested and writable.
        let sheets = FakeSheets::new(push_fixture_grid());
        let tmpdir = tempfile::tempdir().expect("tempdir creation");
        let state_path = state_path_in(&tmpdir);

        let mut state = State::default();
        state.upsert(
            "QA-021".to_string(),
            StateEntry {
                task_id: "t_existing02".to_string(),
                content_hash: "fnv1a:whatever".to_string(),
                last_push_status: Some("Triaged".to_string()),
            },
        );
        state.save(&state_path).expect("seed state save");

        let req = PushRequest {
            row_id: "QA-021".to_string(),
            status: Some("In Progress".to_string()),
            fix_ver: Some("v9".to_string()),
            notes: None,
        };

        let report = push(
            &sheets,
            &mapping,
            &state_path,
            &req,
            &PushOptions { dry_run: false },
        )
        .expect("real push should succeed");

        assert!(report.wrote, "fix_ver edit should still be written");
        assert_eq!(report.edits.len(), 1, "status edit must be gated out");
        assert_eq!(
            report.edits[0].header,
            "Build/Version Fixed (git commit for demo, version tag for prod)"
        );

        let reloaded = State::load(&state_path).expect("reload should succeed");
        assert_eq!(
            reloaded.rows["QA-021"].last_push_status,
            Some("Triaged".to_string()),
            "a gated-out status edit must not phantom-record the requested status"
        );
    }

    #[test]
    fn push_real_status_write_records_last_push_status() {
        let mapping = qa_mapping();
        // QA-020's current sheet Status is "New" (not terminal), and
        // "In Progress" is dev_writable, so the status edit is emitted.
        let sheets = FakeSheets::new(push_fixture_grid());
        let tmpdir = tempfile::tempdir().expect("tempdir creation");
        let state_path = state_path_in(&tmpdir);

        let mut state = State::default();
        state.upsert(
            "QA-020".to_string(),
            StateEntry {
                task_id: "t_existing03".to_string(),
                content_hash: "fnv1a:whatever".to_string(),
                last_push_status: None,
            },
        );
        state.save(&state_path).expect("seed state save");

        let req = PushRequest {
            row_id: "QA-020".to_string(),
            status: Some("In Progress".to_string()),
            fix_ver: None,
            notes: None,
        };

        let report = push(
            &sheets,
            &mapping,
            &state_path,
            &req,
            &PushOptions { dry_run: false },
        )
        .expect("real push should succeed");

        assert!(report.wrote);
        assert_eq!(report.edits.len(), 1);
        assert_eq!(report.edits[0].header, "Status");

        let reloaded = State::load(&state_path).expect("reload should succeed");
        assert_eq!(
            reloaded.rows["QA-020"].last_push_status,
            Some("In Progress".to_string()),
            "a real status write must record the pushed status"
        );
    }

    // -- push: no-op --------------------------------------------------------

    #[test]
    fn push_terminal_gated_status_yields_no_write() {
        let mapping = qa_mapping();
        // QA-021's current sheet Status is "Won't Fix" -> terminal_status,
        // so a status push must be dropped, leaving zero edits.
        let sheets = FakeSheets::new(push_fixture_grid());
        let tmpdir = tempfile::tempdir().expect("tempdir creation");
        let state_path = state_path_in(&tmpdir);

        let req = PushRequest {
            row_id: "QA-021".to_string(),
            status: Some("In Progress".to_string()),
            fix_ver: None,
            notes: None,
        };

        let report = push(
            &sheets,
            &mapping,
            &state_path,
            &req,
            &PushOptions { dry_run: false },
        )
        .expect("no-op push should still succeed");

        assert!(report.edits.is_empty());
        assert!(!report.wrote);
        assert!(sheets.writes.borrow().is_empty());
        assert!(
            !state_path.exists(),
            "a no-op push must not create the state file"
        );
    }
}
