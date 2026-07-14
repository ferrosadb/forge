//! Live [`crate::sheets::SheetsApi`] implementation: the Google Sheets v4
//! REST API over `ureq`, authenticated with a bearer
//! [`crate::oauth::AccessToken`].
//!
//! As with [`crate::oauth`], all branching logic lives in *pure* helpers
//! ([`parse_values_response`], [`build_batch_update_body`],
//! [`quote_sheet_name`]) that are unit-tested without any network access;
//! [`GoogleSheets::read_grid`]/[`GoogleSheets::write_cells`] are thin glue
//! over those helpers plus `ureq`.

use std::time::Duration;

use serde::Deserialize;

use crate::model::{CellEdit, Grid};
use crate::oauth::AccessToken;
use crate::sheets::SheetsApi;

/// Root of the Sheets API v4 REST surface.
const SHEETS_API_BASE: &str = "https://sheets.googleapis.com/v4/spreadsheets";

/// Per-request timeout, matching the pattern in
/// `crates/ingest`/`crates/fmem-client`/`crate::oauth`.
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// Live [`SheetsApi`] backed by the Google Sheets v4 REST API.
pub struct GoogleSheets {
    token: AccessToken,
}

impl GoogleSheets {
    pub fn new(token: AccessToken) -> Self {
        Self { token }
    }

    fn agent(&self) -> ureq::Agent {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(HTTP_TIMEOUT))
            .build();
        ureq::Agent::new_with_config(config)
    }

    fn auth_header(&self) -> String {
        format!("Bearer {}", self.token.token)
    }
}

impl SheetsApi for GoogleSheets {
    fn read_grid(&self, spreadsheet_id: &str, tab: &str) -> anyhow::Result<Grid> {
        let url = format!(
            "{SHEETS_API_BASE}/{}/values/{}",
            url_path_encode(spreadsheet_id),
            url_path_encode(tab)
        );

        let mut resp = self
            .agent()
            .get(&url)
            .header("Authorization", &self.auth_header())
            .query("majorDimension", "ROWS")
            .call()
            .map_err(|e| anyhow::anyhow!("sheets: read_grid request to {url} failed: {e}"))?;

        let status = resp.status();
        let body = resp
            .body_mut()
            .read_to_string()
            .map_err(|e| anyhow::anyhow!("sheets: failed to read read_grid response body: {e}"))?;

        if !status.is_success() {
            anyhow::bail!(
                "sheets: read_grid for spreadsheet {spreadsheet_id:?} tab {tab:?} returned HTTP {}: {}",
                status.as_u16(),
                body.chars().take(500).collect::<String>()
            );
        }

        parse_values_response(&body)
    }

    fn write_cells(
        &self,
        spreadsheet_id: &str,
        tab: &str,
        edits: &[CellEdit],
    ) -> anyhow::Result<()> {
        // Nothing to write: skip the HTTP call entirely rather than sending
        // a `data: []` batchUpdate the API would happily (and pointlessly)
        // accept.
        if edits.is_empty() {
            return Ok(());
        }

        let url = format!(
            "{SHEETS_API_BASE}/{}/values:batchUpdate",
            url_path_encode(spreadsheet_id)
        );
        let body = build_batch_update_body(tab, edits);

        let mut resp = self
            .agent()
            .post(&url)
            .header("Authorization", &self.auth_header())
            .send_json(&body)
            .map_err(|e| anyhow::anyhow!("sheets: write_cells request to {url} failed: {e}"))?;

        let status = resp.status();
        let resp_body = resp.body_mut().read_to_string().map_err(|e| {
            anyhow::anyhow!("sheets: failed to read write_cells response body: {e}")
        })?;

        if !status.is_success() {
            anyhow::bail!(
                "sheets: write_cells for spreadsheet {spreadsheet_id:?} tab {tab:?} returned HTTP {}: {}",
                status.as_u16(),
                resp_body.chars().take(500).collect::<String>()
            );
        }

        Ok(())
    }
}

/// Percent-encodes a single URL path segment (a spreadsheet id or tab
/// name — the latter routinely contains spaces, e.g. `"QA Log"`).
/// Deliberately self-contained rather than importing `crate::oauth`'s
/// equivalent helper — this module has no other reason to depend on
/// `oauth`, matching the crate's existing convention of duplicating small
/// private helpers across modules (e.g. `push_plan::cell` vs
/// `mapping::cell`) rather than coupling them.
fn url_path_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The Sheets API v4 `values.get` response shape. A tab with no data at
/// all omits `values` entirely, hence `#[serde(default)]`.
#[derive(Debug, Deserialize)]
struct ValuesResponse {
    #[serde(default)]
    values: Vec<Vec<String>>,
}

/// Parses a `values.get` JSON response into a [`Grid`]: row 0 is headers,
/// the rest are data rows. An absent/empty `values` array yields an empty
/// [`Grid`] (empty headers, no rows) rather than an error — a tab that has
/// literally no cells is a legitimate (if unusual) state, not a failure.
/// Ragged inner arrays (the API omits trailing empty cells per row) are
/// preserved as-is; downstream (`crate::mapping::map_grid`) already treats
/// a short row's missing cells as empty strings. Pure — no network — so
/// this is unit-testable directly.
pub fn parse_values_response(body: &str) -> anyhow::Result<Grid> {
    let parsed: ValuesResponse = serde_json::from_str(body)
        .map_err(|e| anyhow::anyhow!("sheets: invalid values response JSON: {e}"))?;

    let mut rows = parsed.values.into_iter();
    let headers = rows.next().unwrap_or_default();
    let rows: Vec<Vec<String>> = rows.collect();

    Ok(Grid { headers, rows })
}

/// Quotes `tab` for use as an A1 range's sheet-name prefix, per Google
/// Sheets' own quoting rule: a sheet name that isn't a plain run of
/// ASCII letters/digits/underscores not starting with a digit must be
/// single-quoted, with any embedded `'` doubled. `"QA Log"` (contains a
/// space) → `"'QA Log'"`; `"Sheet1"` (simple name) → `"Sheet1"`
/// unchanged. Pure; unit-tested directly.
pub fn quote_sheet_name(tab: &str) -> String {
    let starts_with_digit = tab.chars().next().is_some_and(|c| c.is_ascii_digit());
    let all_simple = !tab.is_empty() && tab.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');

    if all_simple && !starts_with_digit {
        tab.to_string()
    } else {
        format!("'{}'", tab.replace('\'', "''"))
    }
}

/// Builds the `values:batchUpdate` request body for `edits` against `tab`:
/// `{ "valueInputOption": "RAW", "data": [ { "range": "'<tab>'!<a1>",
/// "values": [[<new>]] }, ... ] }`. The range is always sheet-qualified
/// (via [`quote_sheet_name`]) because a spreadsheet with multiple tabs has
/// no other way to disambiguate which tab a bare `M2` targets. Pure — no
/// network — so this is unit-testable directly. Callers (`write_cells`)
/// are responsible for skipping the HTTP call entirely when `edits` is
/// empty; this function does not special-case that (an empty `edits` here
/// just yields an empty `data` array).
pub fn build_batch_update_body(tab: &str, edits: &[CellEdit]) -> serde_json::Value {
    let quoted_tab = quote_sheet_name(tab);
    let data: Vec<serde_json::Value> = edits
        .iter()
        .map(|edit| {
            serde_json::json!({
                "range": format!("{quoted_tab}!{}", edit.a1),
                "values": [[edit.new]],
            })
        })
        .collect();

    serde_json::json!({
        "valueInputOption": "RAW",
        "data": data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_values_response_splits_header_and_rows() {
        let body = r#"{"values":[["QA Log ID","Title"],["QA-1","x"]]}"#;
        let grid = parse_values_response(body).expect("valid values response");
        assert_eq!(grid.headers.len(), 2);
        assert_eq!(grid.headers, vec!["QA Log ID", "Title"]);
        assert_eq!(grid.rows.len(), 1);
        assert_eq!(grid.rows[0], vec!["QA-1", "x"]);
    }

    #[test]
    fn parse_values_response_empty_object_is_empty_grid() {
        let grid = parse_values_response("{}").expect("empty object is valid");
        assert!(grid.headers.is_empty());
        assert!(grid.rows.is_empty());
    }

    #[test]
    fn parse_values_response_preserves_ragged_rows() {
        let body = r#"{"values":[["A","B","C"],["only-one"],["two","cells"]]}"#;
        let grid = parse_values_response(body).expect("valid values response");
        assert_eq!(grid.headers, vec!["A", "B", "C"]);
        assert_eq!(grid.rows[0], vec!["only-one"]);
        assert_eq!(grid.rows[1], vec!["two", "cells"]);
    }

    #[test]
    fn parse_values_response_rejects_garbage() {
        let err = parse_values_response("not json").expect_err("garbage should fail");
        assert!(err.to_string().contains("invalid values response JSON"));
    }

    #[test]
    fn quote_sheet_name_quotes_names_with_spaces() {
        assert_eq!(quote_sheet_name("QA Log"), "'QA Log'");
    }

    #[test]
    fn quote_sheet_name_leaves_simple_names_unquoted() {
        assert_eq!(quote_sheet_name("Sheet1"), "Sheet1");
    }

    #[test]
    fn quote_sheet_name_quotes_names_starting_with_digit() {
        assert_eq!(quote_sheet_name("1stTab"), "'1stTab'");
    }

    #[test]
    fn quote_sheet_name_doubles_embedded_quotes() {
        assert_eq!(quote_sheet_name("Bob's Log"), "'Bob''s Log'");
    }

    fn sample_edits() -> Vec<CellEdit> {
        vec![
            CellEdit {
                a1: "M2".to_string(),
                header: "Status".to_string(),
                old: "New".to_string(),
                new: "In Progress".to_string(),
            },
            CellEdit {
                a1: "N2".to_string(),
                header: "Fix Ver".to_string(),
                old: "".to_string(),
                new: "v0.3".to_string(),
            },
        ]
    }

    #[test]
    fn build_batch_update_body_shapes_range_and_values() {
        let body = build_batch_update_body("QA Log", &sample_edits());
        assert_eq!(body["valueInputOption"], "RAW");
        let data = body["data"].as_array().expect("data is an array");
        assert_eq!(data.len(), 2);
        assert_eq!(data[0]["range"], "'QA Log'!M2");
        assert_eq!(data[0]["values"], serde_json::json!([["In Progress"]]));
        assert_eq!(data[1]["range"], "'QA Log'!N2");
        assert_eq!(data[1]["values"], serde_json::json!([["v0.3"]]));
    }

    #[test]
    fn build_batch_update_body_unquoted_tab() {
        let body = build_batch_update_body("Sheet1", &sample_edits());
        let data = body["data"].as_array().expect("data is an array");
        assert_eq!(data[0]["range"], "Sheet1!M2");
    }

    #[test]
    fn url_path_encode_handles_spaces() {
        assert_eq!(url_path_encode("QA Log"), "QA%20Log");
    }
}
