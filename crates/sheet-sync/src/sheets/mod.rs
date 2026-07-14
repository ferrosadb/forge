//! Sheets API seam: [`SheetsApi`] is the trait boundary between the sync
//! engine and Google Sheets. Every earlier engine module
//! (`mapping`/`board_plan`/`push_plan`/`state`) is pure and network-free;
//! this trait is the one place [`crate::sync::pull`]/[`crate::sync::push`]
//! reach out to the sheet, and it's injected so both can be exercised
//! end-to-end in tests via `FakeSheets` — no network, no CQL. [`google`]
//! holds the live implementation ([`google::GoogleSheets`]).

use std::collections::BTreeSet;

use crate::model::{CellEdit, Grid};

pub mod google;

/// Read/write boundary to a single Google Sheet.
pub trait SheetsApi {
    /// Fetches the full grid (header row + all data rows) for `tab` in
    /// `spreadsheet_id`.
    fn read_grid(&self, spreadsheet_id: &str, tab: &str) -> anyhow::Result<Grid>;

    /// Writes every cell edit in `edits` to `tab` in `spreadsheet_id`. `tab`
    /// is required (not derivable from `edits`) because Google's
    /// `values:batchUpdate` ranges must be sheet-qualified (`'QA Log'!M2`)
    /// — a spreadsheet with multiple tabs would otherwise have no way to
    /// disambiguate which tab a bare `M2` targets. Implementations must
    /// fail loud (no partial write left silently in place) if any edit
    /// cannot be applied.
    ///
    /// `allowed_headers` is the write blast-radius the caller computed from
    /// the active `SheetMapping` (every header whose mapped
    /// `CanonicalField` is in `mapping.writable`). Implementations must
    /// call [`assert_edits_within_writable`] against it as the very first
    /// thing they do, before any network call or side-effecting record —
    /// this is defense in depth: `push_plan::plan_push` already filters to
    /// writable columns, but the write boundary must never trust that as
    /// the *only* gate.
    fn write_cells(
        &self,
        spreadsheet_id: &str,
        tab: &str,
        allowed_headers: &BTreeSet<String>,
        edits: &[CellEdit],
    ) -> anyhow::Result<()>;
}

/// Defense in depth at the write boundary: fails loud, naming the
/// offending header, if any `edit.header` in `edits` is not a member of
/// `allowed_headers`. Pure — no I/O — so both [`SheetsApi`] implementations
/// ([`google::GoogleSheets`] and the test-only `FakeSheets` below) can call
/// it before doing anything else in `write_cells`, re-asserting the write
/// blast-radius independently of whatever computed `edits` in the first
/// place (see [`crate::push_plan::plan_push`]'s own `assert_blast_radius`,
/// which this mirrors one layer further out).
pub fn assert_edits_within_writable(
    allowed_headers: &BTreeSet<String>,
    edits: &[CellEdit],
) -> anyhow::Result<()> {
    for edit in edits {
        if !allowed_headers.contains(&edit.header) {
            anyhow::bail!(
                "sheets: write_cells blast-radius violation — header {:?} is not in the writable set {:?}",
                edit.header,
                allowed_headers
            );
        }
    }
    Ok(())
}

/// Test-only in-memory [`SheetsApi`]: `read_grid` returns a clone of `grid`
/// regardless of the requested `spreadsheet_id`/`tab` (single-sheet fixture,
/// no multi-tab bookkeeping needed for these tests); `write_cells` appends to
/// `writes` rather than mutating `grid` — callers assert against `writes`,
/// not a re-read of the grid. `tab` is recorded but not otherwise checked;
/// these tests assert on the recorded edits, not the tab.
#[cfg(test)]
pub(crate) struct FakeSheets {
    pub grid: std::cell::RefCell<Grid>,
    pub writes: std::cell::RefCell<Vec<CellEdit>>,
}

#[cfg(test)]
impl FakeSheets {
    pub(crate) fn new(grid: Grid) -> Self {
        Self {
            grid: std::cell::RefCell::new(grid),
            writes: std::cell::RefCell::new(Vec::new()),
        }
    }
}

#[cfg(test)]
impl SheetsApi for FakeSheets {
    fn read_grid(&self, _spreadsheet_id: &str, _tab: &str) -> anyhow::Result<Grid> {
        Ok(self.grid.borrow().clone())
    }

    fn write_cells(
        &self,
        _spreadsheet_id: &str,
        _tab: &str,
        allowed_headers: &BTreeSet<String>,
        edits: &[CellEdit],
    ) -> anyhow::Result<()> {
        assert_edits_within_writable(allowed_headers, edits)?;
        self.writes.borrow_mut().extend(edits.iter().cloned());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(header: &str) -> CellEdit {
        CellEdit {
            a1: "M2".to_string(),
            header: header.to_string(),
            old: "old".to_string(),
            new: "new".to_string(),
        }
    }

    #[test]
    fn assert_edits_within_writable_ok_when_every_header_is_allowed() {
        let allowed: BTreeSet<String> = ["Status", "Resolution Notes"]
            .into_iter()
            .map(String::from)
            .collect();
        let edits = vec![edit("Status"), edit("Resolution Notes")];

        assert_edits_within_writable(&allowed, &edits)
            .expect("every edit header is in the allowed set");
    }

    #[test]
    fn assert_edits_within_writable_errs_naming_the_disallowed_header() {
        let allowed: BTreeSet<String> = ["Status"].into_iter().map(String::from).collect();
        let edits = vec![edit("Status"), edit("Title")];

        let err = assert_edits_within_writable(&allowed, &edits)
            .expect_err("an edit outside `allowed_headers` must fail loud");
        assert!(
            err.to_string().contains("Title"),
            "error should name the offending header, got: {err}"
        );
    }

    #[test]
    fn assert_edits_within_writable_ok_for_empty_edits() {
        let allowed: BTreeSet<String> = BTreeSet::new();
        assert_edits_within_writable(&allowed, &[]).expect("no edits is trivially within bounds");
    }
}
