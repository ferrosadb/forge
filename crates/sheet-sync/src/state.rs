//! Sidecar state file: the row-id ↔ task-id join.
//!
//! The task store has no upsert-by-external-key (`create_task` always mints
//! a new `t_xxxxxxxx`; `update_task` cannot write `metadata`), so the join
//! between a sheet row and the task it produced lives in a per-project,
//! git-ignored sidecar: `.forge/sheets/<alias>.state.toml` (see
//! `specs/todo/feat-sheet-sync.md`, "Data model & idempotency — sidecar
//! state file"). This module only reads/writes that file; it does not know
//! about sheets, tasks, or the network.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// One row's join record: which task it produced, the content hash used to
/// detect sheet-side edits, and the status we last pushed back to the sheet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateEntry {
    pub task_id: String,
    pub content_hash: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub last_push_status: Option<String>,
}

/// The full sidecar: every known row, keyed by normalized sheet row id.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub rows: BTreeMap<String, StateEntry>,
}

impl State {
    /// Loads `.forge/sheets/<alias>.state.toml` from `path`.
    ///
    /// A missing file is not an error — a sheet that has never been synced
    /// has no sidecar yet — and returns an empty [`State`]. Any other I/O
    /// failure (permissions, a directory where a file is expected, …) or a
    /// malformed body fails loud.
    pub fn load(path: &Path) -> anyhow::Result<State> {
        let body = match std::fs::read_to_string(path) {
            Ok(body) => body,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(State::default()),
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "sheet-sync state: failed to read {}: {e}",
                    path.display()
                ))
            }
        };
        toml::from_str(&body).map_err(|e| {
            anyhow::anyhow!("sheet-sync state: invalid TOML in {}: {e}", path.display())
        })
    }

    /// Serializes `self` as TOML and writes it to `path`, creating any
    /// missing parent directories first.
    ///
    /// The sidecar directory is meant to be private (spec calls for
    /// 0600-ish permissions); we best-effort tighten permissions on Unix
    /// after writing, but never fail the save over an unsupported or
    /// rejected chmod — only genuine read/write/serialize failures are
    /// propagated.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                anyhow::anyhow!(
                    "sheet-sync state: failed to create parent dir {}: {e}",
                    parent.display()
                )
            })?;
        }

        let body = toml::to_string_pretty(self)
            .map_err(|e| anyhow::anyhow!("sheet-sync state: failed to serialize: {e}"))?;

        std::fs::write(path, body).map_err(|e| {
            anyhow::anyhow!("sheet-sync state: failed to write {}: {e}", path.display())
        })?;

        tighten_permissions_best_effort(path);

        Ok(())
    }

    /// Inserts `entry` under `row_id`, overwriting any existing entry for
    /// that row.
    pub fn upsert(&mut self, row_id: String, entry: StateEntry) {
        self.rows.insert(row_id, entry);
    }
}

/// Best-effort 0600/0700-style tightening of the state file and its parent
/// directory. Never fails the caller: a chmod failure (e.g. an unsupported
/// filesystem) is not a reason to lose a successful write, and this is not
/// a security boundary — see the module doc's "fail loud on genuine I/O
/// failure" carve-out. Best-effort is not the same as silent, though: per
/// the repo's disclosure rules (and mirroring `crate::oauth`'s identically
/// named helper), each chmod failure is logged to stderr (path + OS error
/// only, never file contents) so a permissive mode on a shared/multi-user
/// machine is observable instead of hidden.
#[cfg(unix)]
fn tighten_permissions_best_effort(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
        eprintln!(
            "warning: could not tighten permissions on {} : {e}",
            path.display()
        );
    }
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)) {
            eprintln!(
                "warning: could not tighten permissions on {} : {e}",
                parent.display()
            );
        }
    }
}

#[cfg(not(unix))]
fn tighten_permissions_best_effort(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_missing_file_returns_empty_state() {
        let tmpdir = tempfile::tempdir().expect("tempdir creation");
        let path = tmpdir.path().join("does-not-exist.state.toml");

        let state = State::load(&path).expect("missing file should load as empty state, not err");
        assert!(state.rows.is_empty());
    }

    #[test]
    fn save_then_load_round_trips_two_entries() {
        let tmpdir = tempfile::tempdir().expect("tempdir creation");
        // Parent dirs (.forge/sheets) don't exist yet — save must create them.
        let path = tmpdir
            .path()
            .join(".forge")
            .join("sheets")
            .join("spoton-qa.state.toml");

        let mut state = State::default();
        state.upsert(
            "QA-016".to_string(),
            StateEntry {
                task_id: "t_ab12cd34".to_string(),
                content_hash: "sha256:abc123".to_string(),
                last_push_status: Some("In Progress".to_string()),
            },
        );
        state.upsert(
            "QA-017".to_string(),
            StateEntry {
                task_id: "t_ef56gh78".to_string(),
                content_hash: "sha256:def456".to_string(),
                last_push_status: None,
            },
        );

        state
            .save(&path)
            .expect("save should succeed and create parent dirs");
        assert!(path.is_file());

        let loaded = State::load(&path).expect("load should succeed");
        assert_eq!(loaded.rows, state.rows);

        // Explicitly pin down that the `None` entry round-trips as `None`,
        // not e.g. `Some("")`.
        assert_eq!(
            loaded
                .rows
                .get("QA-017")
                .expect("QA-017 present")
                .last_push_status,
            None
        );
        assert_eq!(
            loaded
                .rows
                .get("QA-016")
                .expect("QA-016 present")
                .last_push_status,
            Some("In Progress".to_string())
        );
    }

    #[test]
    fn upsert_inserts_new_and_overwrites_existing() {
        let mut state = State::default();
        state.upsert(
            "QA-020".to_string(),
            StateEntry {
                task_id: "t_first0001".to_string(),
                content_hash: "sha256:first".to_string(),
                last_push_status: None,
            },
        );
        assert_eq!(state.rows.len(), 1);
        assert_eq!(state.rows["QA-020"].task_id, "t_first0001");

        state.upsert(
            "QA-020".to_string(),
            StateEntry {
                task_id: "t_second002".to_string(),
                content_hash: "sha256:second".to_string(),
                last_push_status: Some("In Review".to_string()),
            },
        );

        assert_eq!(state.rows.len(), 1, "overwrite must not add a new row");
        let entry = &state.rows["QA-020"];
        assert_eq!(entry.task_id, "t_second002");
        assert_eq!(entry.content_hash, "sha256:second");
        assert_eq!(entry.last_push_status, Some("In Review".to_string()));
    }
}
