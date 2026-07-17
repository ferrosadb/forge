//! Canonical row/field model shared across the sheet-sync engine.
//!
//! A [`CanonicalRow`] is a single sheet row *after* header resolution:
//! sheet-specific header text (which varies per spreadsheet) has already
//! been translated to the fixed [`CanonicalField`] vocabulary via a
//! `.forge/sheets/<alias>.toml` column mapping (see
//! `specs/todo/feat-sheet-sync.md`).

use std::collections::BTreeMap;

/// The fixed set of fields the sync engine understands, independent of
/// whatever header text a given spreadsheet uses. Sheet-specific mappings
/// translate sheet headers to these via [`CanonicalField::parse`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CanonicalField {
    Id,
    Title,
    Type,
    Category,
    Description,
    Steps,
    Expected,
    Actual,
    Environment,
    Severity,
    Priority,
    MvpBlocker,
    Status,
    FixVer,
    ResolutionNotes,
}

impl CanonicalField {
    /// Parses the lowercase `snake_case` mapping target used as a column
    /// mapping value in `.forge/sheets/<alias>.toml` (e.g. `"mvp_blocker"`).
    ///
    /// Returns `None` for anything unrecognized — callers must fail loud
    /// on an unmapped/misspelled target rather than silently dropping the
    /// column.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "id" => Some(Self::Id),
            "title" => Some(Self::Title),
            "type" => Some(Self::Type),
            "category" => Some(Self::Category),
            "description" => Some(Self::Description),
            "steps" => Some(Self::Steps),
            "expected" => Some(Self::Expected),
            "actual" => Some(Self::Actual),
            "environment" => Some(Self::Environment),
            "severity" => Some(Self::Severity),
            "priority" => Some(Self::Priority),
            "mvp_blocker" => Some(Self::MvpBlocker),
            "status" => Some(Self::Status),
            "fix_ver" => Some(Self::FixVer),
            "resolution_notes" => Some(Self::ResolutionNotes),
            _ => None,
        }
    }

    /// The stable, explicit field identifier for this variant — the exact
    /// same token [`Self::parse`] accepts for it. Used by
    /// [`crate::board_plan::content_hash`], which is persisted to
    /// `.forge/sheets/<alias>.state.toml`: hashing this instead of the
    /// `derive(Debug)` variant name keeps the persisted hash stable across
    /// `Debug`-format changes. Exhaustive match (no wildcard) so a new
    /// variant fails to compile here rather than silently falling through.
    pub fn as_canonical_str(&self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Title => "title",
            Self::Type => "type",
            Self::Category => "category",
            Self::Description => "description",
            Self::Steps => "steps",
            Self::Expected => "expected",
            Self::Actual => "actual",
            Self::Environment => "environment",
            Self::Severity => "severity",
            Self::Priority => "priority",
            Self::MvpBlocker => "mvp_blocker",
            Self::Status => "status",
            Self::FixVer => "fix_ver",
            Self::ResolutionNotes => "resolution_notes",
        }
    }
}

/// A single sheet row after header resolution: its normalized id (see
/// [`crate::normalize::normalize_id`]), its mapped field values keyed by
/// [`CanonicalField`], and its 0-based position among the sheet's data
/// rows (used for diagnostics and later A1 addressing in push planning).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalRow {
    pub id: String,
    pub fields: BTreeMap<CanonicalField, String>,
    pub sheet_row_index: usize,
}

/// The raw, unmapped contents of a Google Sheet tab as fetched from the
/// Sheets API: a header row plus zero or more data rows. Sheet rows may be
/// *ragged* — the Sheets API omits trailing empty cells, so a data row can
/// be shorter than `headers` — callers must treat any missing cell index as
/// an empty string rather than indexing out of bounds (see
/// [`crate::mapping::map_grid`]).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Grid {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

/// One planned cell write: the A1-notation address, the mapped header it
/// belongs to, and the value being replaced and the value replacing it.
/// Produced by [`crate::push_plan::plan_push`]; a later (not-yet-built)
/// writer task is responsible for actually sending it to the Sheets API.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CellEdit {
    pub a1: String,
    pub header: String,
    pub old: String,
    pub new: String,
}

/// Converts a 0-based column index to spreadsheet column letters
/// (`0` → `"A"`, `25` → `"Z"`, `26` → `"AA"`, `27` → `"AB"`).
///
/// This is a bijective base-26 conversion (not plain base-26): there is no
/// digit for zero, so each "digit" ranges `1..=26` rather than `0..=25`.
/// Implemented by repeatedly taking `(n - 1) % 26` / `(n - 1) / 26` on the
/// 1-based column number, prepending each resulting letter.
pub fn col_index_to_a1(col0: usize) -> String {
    let mut n = col0 + 1;
    let mut letters = Vec::new();
    while n > 0 {
        let remainder = (n - 1) % 26;
        letters.push((b'A' + remainder as u8) as char);
        n = (n - 1) / 26;
    }
    letters.iter().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn col_index_to_a1_covers_single_and_double_letter_boundaries() {
        assert_eq!(col_index_to_a1(0), "A");
        assert_eq!(col_index_to_a1(25), "Z");
        assert_eq!(col_index_to_a1(26), "AA");
        assert_eq!(col_index_to_a1(27), "AB");
    }

    #[test]
    fn parses_every_documented_field_name() {
        let names = [
            "id",
            "title",
            "type",
            "category",
            "description",
            "steps",
            "expected",
            "actual",
            "environment",
            "severity",
            "priority",
            "mvp_blocker",
            "status",
            "fix_ver",
            "resolution_notes",
        ];
        for name in names {
            assert!(
                CanonicalField::parse(name).is_some(),
                "expected {name:?} to parse"
            );
        }
    }

    #[test]
    fn rejects_unknown_field_names() {
        assert_eq!(CanonicalField::parse("not_a_field"), None);
    }

    #[test]
    fn as_canonical_str_round_trips_through_parse_for_every_variant() {
        let variants = [
            CanonicalField::Id,
            CanonicalField::Title,
            CanonicalField::Type,
            CanonicalField::Category,
            CanonicalField::Description,
            CanonicalField::Steps,
            CanonicalField::Expected,
            CanonicalField::Actual,
            CanonicalField::Environment,
            CanonicalField::Severity,
            CanonicalField::Priority,
            CanonicalField::MvpBlocker,
            CanonicalField::Status,
            CanonicalField::FixVer,
            CanonicalField::ResolutionNotes,
        ];
        for field in variants {
            assert_eq!(
                CanonicalField::parse(field.as_canonical_str()),
                Some(field),
                "as_canonical_str/parse drifted for {field:?}"
            );
        }
    }
}
