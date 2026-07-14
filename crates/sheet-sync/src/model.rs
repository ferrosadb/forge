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

#[cfg(test)]
mod tests {
    use super::*;

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
}
