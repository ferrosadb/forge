//! Live check that the real OAuth loopback flow + `GoogleSheets` round-trip
//! against an actual Google account and spreadsheet. Requires:
//!   - `FORGE_GOOGLE_OAUTH_CLIENT` pointing at a Google `client_secret.json`
//!     for an installed-app OAuth client.
//!   - `FORGE_SHEET_SYNC_LIVE_SPREADSHEET_ID` naming a scratch spreadsheet
//!     the authorizing account can read, with a `Sheet1` tab.
//!   - An already-cached refresh token for alias `live-smoke` (run
//!     `oauth::authorize("live-smoke", &client)` interactively once first —
//!     this test does not drive the interactive consent step itself, since
//!     that requires a human in a browser).
//!
//! Mirrors `crates/sheet-sync/tests/board_exec_live.rs`'s / `crates/tasks/
//! tests/board_health_live.rs`'s pattern: gated with `#[ignore]` so the
//! default `cargo test` run (no Google creds required) never hits this
//! file. Run with:
//!   cargo test -p forge-sheet-sync --test google_sheets_live -- --ignored --nocapture

use forge_sheet_sync::oauth::{self, OAuthClient};
use forge_sheet_sync::sheets::google::GoogleSheets;
use forge_sheet_sync::SheetsApi;

#[test]
#[ignore = "requires FORGE_GOOGLE_OAUTH_CLIENT, a scratch spreadsheet id, and a pre-authorized refresh token"]
fn authorize_then_read_grid_round_trips_against_a_real_spreadsheet() {
    let client = OAuthClient::load().expect(
        "FORGE_GOOGLE_OAUTH_CLIENT must point at a valid client_secret.json for this live test",
    );
    let spreadsheet_id = std::env::var("FORGE_SHEET_SYNC_LIVE_SPREADSHEET_ID")
        .expect("FORGE_SHEET_SYNC_LIVE_SPREADSHEET_ID must name a scratch spreadsheet");

    // Assumes a refresh token is already cached for this alias — see the
    // module doc. `access_token` alone (no interactive `authorize` call)
    // keeps this test runnable unattended once the one-time interactive
    // setup has happened.
    let token = oauth::access_token("live-smoke", &client).expect(
        "no cached refresh token for alias `live-smoke` — run authorize(\"live-smoke\", &client) once interactively first",
    );

    let sheets = GoogleSheets::new(token);
    let grid = sheets
        .read_grid(&spreadsheet_id, "Sheet1")
        .expect("read_grid against the live spreadsheet");

    eprintln!(
        "live read_grid: {} headers, {} rows",
        grid.headers.len(),
        grid.rows.len()
    );
}
