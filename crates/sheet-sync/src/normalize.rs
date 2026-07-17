//! Sheet-side row ID normalization.
//!
//! Google Sheet ID cells sometimes carry a human note in a trailing
//! parenthetical (e.g. `"QA-005 (This may not be a bug)"`) and/or stray
//! whitespace. [`normalize_id`] strips both so the sync engine can join
//! sheet rows to a stable canonical ID and [`find_duplicate_ids`] can
//! detect accidental collisions (see `specs/todo/feat-sheet-sync.md`,
//! "Duplicate ID → fail loud").

use std::collections::BTreeMap;

/// Normalizes a raw sheet ID cell: trims whitespace, then drops a single
/// trailing `(...)` parenthetical group (and the whitespace before it),
/// then trims again.
///
/// Implemented without a regex dependency: if the trimmed string ends with
/// `)`, the last `(` is located with [`str::rfind`] and everything from
/// there on is sliced off before a final trim.
pub fn normalize_id(raw: &str) -> String {
    let trimmed = raw.trim();
    let without_parenthetical = if trimmed.ends_with(')') {
        match trimmed.rfind('(') {
            Some(open_paren) => &trimmed[..open_paren],
            None => trimmed,
        }
    } else {
        trimmed
    };
    without_parenthetical.trim().to_string()
}

/// Returns the normalized IDs that appear more than once in `ids`, sorted
/// and deduplicated.
///
/// Ambiguous (duplicate) rows must never be silently merged — callers use
/// this list to fail loud and skip both import and push for the affected
/// rows.
pub fn find_duplicate_ids(ids: &[String]) -> Vec<String> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for id in ids {
        *counts.entry(id.as_str()).or_insert(0) += 1;
    }
    counts
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(id, _)| id.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_trailing_parenthetical_and_whitespace() {
        assert_eq!(normalize_id("QA-005 (This may not be a bug)"), "QA-005");
        assert_eq!(normalize_id("  QA-016 "), "QA-016");
        assert_eq!(normalize_id("QA-005"), "QA-005");
    }
    #[test]
    fn detects_the_two_qa005_rows_as_duplicate() {
        let ids = vec!["QA-004", "QA-005", "QA-026", "QA-005"]
            .into_iter()
            .map(normalize_id)
            .collect::<Vec<_>>();
        assert_eq!(find_duplicate_ids(&ids), vec!["QA-005".to_string()]);
    }
    #[test]
    fn no_duplicates_when_all_unique() {
        let ids = ["QA-1", "QA-2"].map(normalize_id).to_vec();
        assert!(find_duplicate_ids(&ids).is_empty());
    }
}
