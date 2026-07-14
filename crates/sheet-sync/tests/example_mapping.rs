//! Guards `examples/spoton-qa.toml` against rot: the shipped example must
//! actually parse as a valid [`forge_sheet_sync::config::SheetMapping`], and
//! its shape must match the spec's "Config" block (see
//! `specs/todo/feat-sheet-sync.md`). Users copy this file to
//! `.forge/sheets/<alias>.toml` (git-ignored) as their starting point, so a
//! silent break here would only surface at `frg sheet auth` time in the
//! field.

use forge_sheet_sync::config::SheetMapping;
use forge_sheet_sync::model::CanonicalField;

const EXAMPLE_TOML: &str = include_str!("../examples/spoton-qa.toml");

#[test]
fn example_mapping_parses() {
    let mapping = SheetMapping::from_toml_str(EXAMPLE_TOML).expect("example mapping should parse");

    assert_eq!(mapping.id_column, "QA Log ID");
    assert!(mapping.writable.contains(&CanonicalField::Status));
    assert!(mapping.writable.contains(&CanonicalField::FixVer));
    assert!(mapping.writable.contains(&CanonicalField::ResolutionNotes));
    assert_eq!(mapping.columns.len(), 15);
}
