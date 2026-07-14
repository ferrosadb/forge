//! Sheets API seam: [`SheetsApi`] is the trait boundary between the sync
//! engine and Google Sheets. Every earlier engine module
//! (`mapping`/`board_plan`/`push_plan`/`state`) is pure and network-free;
//! this trait is the one place [`crate::sync::pull`]/[`crate::sync::push`]
//! reach out to the sheet, and it's injected so both can be exercised
//! end-to-end in tests via `FakeSheets` — no network, no CQL. [`google`]
//! holds the live implementation ([`google::GoogleSheets`]).

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
    fn write_cells(
        &self,
        spreadsheet_id: &str,
        tab: &str,
        edits: &[CellEdit],
    ) -> anyhow::Result<()>;
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
        edits: &[CellEdit],
    ) -> anyhow::Result<()> {
        self.writes.borrow_mut().extend(edits.iter().cloned());
        Ok(())
    }
}
