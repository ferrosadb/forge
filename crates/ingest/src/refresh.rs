//! Refresh decision layer — per-file classification for incremental ingest.
//!
//! This module does NOT perform the extraction or graph writes itself — it
//! takes a snapshot of the current filesystem + the on-disk cache and
//! produces a `RefreshDecisions` struct that callers act on.  Keeping it
//! decoupled from `LspSession` / `GraphLoader` / `ingest_entities` lets
//! the rules be unit-tested without a live LSP or MCP transport, and
//! keeps the write-side policy (see `overview.md` §Refresh model)
//! explicit at the call site.
//!
//! ## Policy — deletion handling (D1)
//!
//! Per the current ferrosa-memory dependency note, no generic
//! `delete_entities` tool exists.  This module records missing files and
//! missing symbols in the returned report for operator visibility but
//! does **not** emit any graph write to model deletion.  The feature
//! docs are explicit: the soft-delete flow (terminal `state` via
//! `ingest_entities`) is not standardized and must not be silently
//! invented here.
//!
//! ## Policy — renames
//!
//! When a new file has the same sha256 as a file that's no longer on
//! disk at its old path, it's treated as a rename.  The decision is
//! `Decision::Renamed { old_path, new_path, preserved_id }` and callers
//! re-emit the `file` entity via `ingest_entities` with the preserved
//! entity id and the updated `path` attribute.  This is upsert —
//! supported by the existing CRUD surface.
//!
//! ## Policy — extractor schema drift (F15)
//!
//! When any cache entry carries an `extractor_schema_version < CURRENT`
//! the decision for that entry is `Decision::VersionDrift` regardless of
//! hash match.  The caller must re-extract so the graph catches the new
//! schema shape.
//!
//! ## Policy — 1-hop closure
//!
//! Intentionally not handled here.  Closing the reverse-reference set
//! requires reading `references`/`calls` edges pointing INTO a changed
//! file, which the current `fmem-client` transport doesn't expose.
//! Tracked as a follow-on dependency.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::cache::{CacheHandle, FileCacheEntry};
use crate::extractor::EXTRACTOR_SCHEMA_VERSION;
use crate::source_buffer::SourceBuffer;

/// What should the caller do for a given file (or former file)?
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// File exists, has never been ingested.
    New { path: String, sha256: String },
    /// File exists, content has changed since last refresh.
    Changed {
        path: String,
        sha256: String,
        prior_sha256: String,
    },
    /// File exists, content unchanged, extractor_schema_version matches.
    Unchanged { path: String },
    /// File exists, content unchanged, but the extractor schema has
    /// advanced — must re-extract.
    VersionDrift {
        path: String,
        prior_version: u32,
        current_version: u32,
    },
    /// File was present in the cache, not present on disk now, and no
    /// new file with matching sha256 was found.
    Missing { path: String, prior_sha256: String },
    /// File present in the cache at `old_path` is now present at
    /// `new_path` (matched by sha256).  Caller re-emits the file entity
    /// with preserved id + updated path attr.
    Renamed {
        old_path: String,
        new_path: String,
        sha256: String,
    },
    /// A refresh attempt crashed previously (pending marker set in cache).
    /// Caller must re-extract regardless of hash — the partial commit may
    /// have left the graph and file state inconsistent.
    PendingRecovery { path: String },
}

/// Aggregate decisions for one refresh pass.
#[derive(Debug, Default, Clone)]
pub struct RefreshDecisions {
    pub decisions: Vec<Decision>,
    /// Convenience buckets — never required, but useful for reports.
    pub new_count: usize,
    pub changed_count: usize,
    pub unchanged_count: usize,
    pub version_drift_count: usize,
    pub missing_count: usize,
    pub renamed_count: usize,
    pub pending_recovery_count: usize,
}

/// Input describing a file the walker discovered on disk.
#[derive(Debug, Clone)]
pub struct DiscoveredFile {
    pub path: String, // project-relative, POSIX separators
    pub sha256: String,
}

/// Classify every discovered file against the cache state.  Produces
/// decisions in a deterministic order: `VersionDrift`/`PendingRecovery`
/// for cached entries (first), then `New`/`Changed`/`Unchanged` for
/// discovered files, then `Renamed`/`Missing` for cache entries with no
/// matching discovered file.
///
/// This function is pure — it consumes the two snapshots and emits
/// decisions.  The caller is responsible for ordering: (a) setting
/// pending markers on the cache before extraction, (b) performing the
/// extraction + ingest_entities calls, (c) clearing pending markers +
/// bumping sha256/timestamp on success.
pub fn classify(
    discovered: &[DiscoveredFile],
    cache: &HashMap<String, FileCacheEntry>,
    current_version: u32,
) -> RefreshDecisions {
    let discovered_by_path: HashMap<&str, &DiscoveredFile> =
        discovered.iter().map(|f| (f.path.as_str(), f)).collect();
    let discovered_by_hash: HashMap<&str, &DiscoveredFile> =
        discovered.iter().map(|f| (f.sha256.as_str(), f)).collect();

    let mut out = RefreshDecisions::default();
    let mut used_discovered: HashSet<&str> = HashSet::new();

    // First pass: cached entries with pending marker (priority — recover first).
    for entry in cache.values() {
        if entry.pending_file_id.is_some() {
            out.decisions.push(Decision::PendingRecovery {
                path: entry.path.clone(),
            });
            out.pending_recovery_count += 1;
            // PendingRecovery is a forced re-extract; mark as "used"
            // so the file isn't double-classified below.
            if discovered_by_path.contains_key(entry.path.as_str()) {
                used_discovered.insert(entry.path.as_str());
            }
        }
    }

    // Second pass: walk discovered files against the cache.
    for f in discovered {
        if used_discovered.contains(f.path.as_str()) {
            continue; // already handled by PendingRecovery
        }
        match cache.get(&f.path) {
            None => {
                // Could be a rename — a cache entry with matching sha256
                // whose old path is no longer on disk.  Defer the
                // rename/new decision to the third pass for clarity.
                // For now, tentatively classify as New; the third pass
                // upgrades to Renamed when it detects the match.
                out.decisions.push(Decision::New {
                    path: f.path.clone(),
                    sha256: f.sha256.clone(),
                });
                out.new_count += 1;
                used_discovered.insert(f.path.as_str());
            }
            Some(entry) if entry.sha256 == f.sha256 => {
                // Hash match — check version.
                if entry.extractor_schema_version < current_version {
                    out.decisions.push(Decision::VersionDrift {
                        path: f.path.clone(),
                        prior_version: entry.extractor_schema_version,
                        current_version,
                    });
                    out.version_drift_count += 1;
                } else {
                    out.decisions.push(Decision::Unchanged {
                        path: f.path.clone(),
                    });
                    out.unchanged_count += 1;
                }
                used_discovered.insert(f.path.as_str());
            }
            Some(entry) => {
                out.decisions.push(Decision::Changed {
                    path: f.path.clone(),
                    sha256: f.sha256.clone(),
                    prior_sha256: entry.sha256.clone(),
                });
                out.changed_count += 1;
                used_discovered.insert(f.path.as_str());
            }
        }
    }

    // Third pass: cached entries with no matching discovered-path →
    // rename-detect via sha256, else Missing.
    //
    // We post-process the decisions Vec to upgrade any tentative `New`
    // that's actually a rename of a cache entry.
    let mut rename_upgrades: Vec<(usize, Decision)> = Vec::new();
    let mut new_missing: Vec<Decision> = Vec::new();
    let mut renamed_newpaths: HashSet<String> = HashSet::new();
    for entry in cache.values() {
        if entry.pending_file_id.is_some() {
            continue; // already handled
        }
        if discovered_by_path.contains_key(entry.path.as_str()) {
            continue; // still present; handled in the second pass
        }
        // Entry is in cache but not found on disk at its old path.
        match discovered_by_hash.get(entry.sha256.as_str()) {
            Some(new_f) if !cache.contains_key(&new_f.path) => {
                // Rename: sha256 of an absent old path matches a
                // discovered file whose path isn't in cache.
                let idx = out.decisions.iter().position(|d| {
                    matches!(
                        d,
                        Decision::New { path, .. } if path == &new_f.path
                    )
                });
                if let Some(i) = idx {
                    rename_upgrades.push((
                        i,
                        Decision::Renamed {
                            old_path: entry.path.clone(),
                            new_path: new_f.path.clone(),
                            sha256: new_f.sha256.clone(),
                        },
                    ));
                    renamed_newpaths.insert(new_f.path.clone());
                }
            }
            _ => {
                new_missing.push(Decision::Missing {
                    path: entry.path.clone(),
                    prior_sha256: entry.sha256.clone(),
                });
            }
        }
    }
    for (i, upgraded) in rename_upgrades {
        if matches!(out.decisions[i], Decision::New { .. }) {
            out.new_count = out.new_count.saturating_sub(1);
            out.renamed_count += 1;
            out.decisions[i] = upgraded;
        }
    }
    for d in new_missing {
        out.missing_count += 1;
        out.decisions.push(d);
    }
    out
}

/// Convenience: read a single file via `SourceBuffer` and produce a
/// `DiscoveredFile`.  Used by the walker path.
pub fn discover_file(project_root: &Path, rel_path: &Path) -> Result<DiscoveredFile> {
    let abs = project_root.join(rel_path);
    let sb =
        SourceBuffer::read(&abs).with_context(|| format!("refresh: read {}", abs.display()))?;
    Ok(DiscoveredFile {
        path: to_posix(rel_path),
        sha256: sb.sha256,
    })
}

/// Public helper: classify directly against an open `CacheHandle`.
/// Convenience over the pure `classify` for the common call path.
pub fn classify_with_cache(discovered: &[DiscoveredFile], cache: &CacheHandle) -> RefreshDecisions {
    classify(discovered, &cache.doc().files, EXTRACTOR_SCHEMA_VERSION)
}

fn to_posix(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// Suppress dead-code warnings on path-resolution hook; used by future
/// integration glue.
#[allow(dead_code)]
fn _used(_: PathBuf) {}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn entry(path: &str, sha: &str, version: u32, pending: Option<Uuid>) -> FileCacheEntry {
        FileCacheEntry {
            path: path.to_string(),
            sha256: sha.to_string(),
            last_refreshed_at_ms: 0,
            extractor_schema_version: version,
            pending_file_id: pending,
        }
    }

    fn disc(path: &str, sha: &str) -> DiscoveredFile {
        DiscoveredFile {
            path: path.to_string(),
            sha256: sha.to_string(),
        }
    }

    fn cache_from(entries: Vec<FileCacheEntry>) -> HashMap<String, FileCacheEntry> {
        entries.into_iter().map(|e| (e.path.clone(), e)).collect()
    }

    #[test]
    fn new_file_detected() {
        let r = classify(&[disc("a.rs", "h1")], &cache_from(vec![]), 1);
        assert_eq!(r.new_count, 1);
        assert!(matches!(&r.decisions[0], Decision::New { path, .. } if path == "a.rs"));
    }

    #[test]
    fn unchanged_when_hash_and_version_match() {
        let cache = cache_from(vec![entry("a.rs", "h1", 1, None)]);
        let r = classify(&[disc("a.rs", "h1")], &cache, 1);
        assert_eq!(r.unchanged_count, 1);
        assert!(matches!(&r.decisions[0], Decision::Unchanged { .. }));
    }

    #[test]
    fn changed_when_hash_differs() {
        let cache = cache_from(vec![entry("a.rs", "h1", 1, None)]);
        let r = classify(&[disc("a.rs", "h2")], &cache, 1);
        assert_eq!(r.changed_count, 1);
        assert!(matches!(
            &r.decisions[0],
            Decision::Changed { prior_sha256, sha256, .. } if prior_sha256 == "h1" && sha256 == "h2"
        ));
    }

    #[test]
    fn version_drift_forces_reextract_even_on_hash_match() {
        let cache = cache_from(vec![entry("a.rs", "h1", 1, None)]);
        let r = classify(&[disc("a.rs", "h1")], &cache, 2);
        assert_eq!(r.version_drift_count, 1);
        assert!(matches!(
            &r.decisions[0],
            Decision::VersionDrift {
                prior_version: 1,
                current_version: 2,
                ..
            }
        ));
    }

    #[test]
    fn missing_file_reported_but_no_graph_write() {
        let cache = cache_from(vec![entry("a.rs", "h1", 1, None)]);
        let r = classify(&[], &cache, 1);
        assert_eq!(r.missing_count, 1);
        assert!(matches!(&r.decisions[0], Decision::Missing { path, .. } if path == "a.rs"));
    }

    #[test]
    fn rename_detected_by_matching_sha() {
        // a.rs (h1) in cache, not on disk; b.rs (h1) discovered, not in cache.
        let cache = cache_from(vec![entry("a.rs", "h1", 1, None)]);
        let r = classify(&[disc("b.rs", "h1")], &cache, 1);
        assert_eq!(r.renamed_count, 1);
        assert_eq!(r.new_count, 0);
        let renamed = r.decisions.iter().find_map(|d| match d {
            Decision::Renamed {
                old_path,
                new_path,
                sha256,
            } => Some((old_path.clone(), new_path.clone(), sha256.clone())),
            _ => None,
        });
        assert_eq!(renamed, Some(("a.rs".into(), "b.rs".into(), "h1".into())));
    }

    #[test]
    fn rename_not_detected_when_new_path_already_in_cache() {
        // Two files existed (a.rs h1, b.rs h2). User replaces a.rs with b.rs's
        // content; a.rs becomes h2, b.rs is deleted.  NOT a rename.
        let cache = cache_from(vec![
            entry("a.rs", "h1", 1, None),
            entry("b.rs", "h2", 1, None),
        ]);
        let r = classify(&[disc("a.rs", "h2")], &cache, 1);
        // a.rs: changed; b.rs: missing; no rename.
        assert_eq!(r.renamed_count, 0);
        assert_eq!(r.changed_count, 1);
        assert_eq!(r.missing_count, 1);
    }

    #[test]
    fn pending_recovery_wins_over_hash_match() {
        let cache = cache_from(vec![entry("a.rs", "h1", 1, Some(Uuid::new_v4()))]);
        let r = classify(&[disc("a.rs", "h1")], &cache, 1);
        assert_eq!(r.pending_recovery_count, 1);
        // Unchanged is NOT produced — the pending marker forces re-extract.
        assert_eq!(r.unchanged_count, 0);
    }

    #[test]
    fn multiple_decisions_bucketed_correctly() {
        // Exhaustive mix: 1 new, 1 changed, 1 unchanged, 1 version-drift,
        // 1 missing, 1 rename (a_old.rs→a_new.rs), 1 pending.
        let cache = cache_from(vec![
            entry("changed.rs", "h_old", 1, None),
            entry("unchanged.rs", "h_u", 1, None),
            entry("drift.rs", "h_d", 0, None), // older schema → drift
            entry("missing.rs", "h_m", 1, None),
            entry("a_old.rs", "h_r", 1, None),
            entry("pending.rs", "h_p", 1, Some(Uuid::new_v4())),
        ]);
        let discovered = vec![
            disc("new.rs", "h_new"),
            disc("changed.rs", "h_new2"),
            disc("unchanged.rs", "h_u"),
            disc("drift.rs", "h_d"),
            disc("a_new.rs", "h_r"),
            disc("pending.rs", "h_p"),
        ];
        let r = classify(&discovered, &cache, 1);
        assert_eq!(r.new_count, 1);
        assert_eq!(r.changed_count, 1);
        assert_eq!(r.unchanged_count, 1);
        assert_eq!(r.version_drift_count, 1);
        assert_eq!(r.missing_count, 1);
        assert_eq!(r.renamed_count, 1);
        assert_eq!(r.pending_recovery_count, 1);
        assert_eq!(r.decisions.len(), 7);
    }

    #[test]
    fn empty_inputs_produce_empty_report() {
        let r = classify(&[], &HashMap::new(), 1);
        assert_eq!(r.decisions.len(), 0);
    }
}

// ── apply_decisions — wire decisions to ferrosa-memory tools ─────────────────

use forge_fmem_client::transport::Transport;
use forge_fmem_client::{
    batch_delete_entities, batch_update_entities, BatchDeleteEntitiesArgs, BatchUpdateEntitiesArgs,
    WireDeleteTarget, WirePatchEntity, BATCH_DELETE_MAX,
};

/// Summary of applying a `RefreshDecisions` against ferrosa-memory.
///
/// Counts reflect what actually landed on the server after per-row
/// reconciliation.  Non-zero `failed_*` entries mean either server-side
/// errors or reconciliation mismatches (surfaced in the error message of
/// the `Result::Err` returned by `apply_decisions` — we never silently
/// succeed with dropped rows).
#[derive(Debug, Default, Clone)]
pub struct RefreshReport {
    pub files_scanned: usize,
    pub files_new: usize,
    pub files_changed: usize,
    pub files_unchanged: usize,
    pub files_version_drift: usize,
    pub files_pending_recovery: usize,

    /// Renames: file entity path updated via `batch_update_entities`.
    pub files_renamed_updated: usize,
    pub files_renamed_failed: usize,

    /// Deletions: file entities (and their symbols) hard-deleted via
    /// `batch_delete_entities`.  `files_missing_deleted` counts the
    /// file entities removed; `symbols_missing_deleted` counts their
    /// symbol children if the caller supplies a resolver for them.
    pub files_missing_deleted: usize,
    pub files_missing_not_found: usize,
    pub files_missing_errors: usize,

    /// `apply_decisions` does NOT run the extractor — that requires an
    /// `LspSession` which isn't plumbed through this API surface.  The
    /// "to_ingest" counter reflects the number of files that need
    /// extraction; the caller is responsible for feeding them into
    /// `GraphLoader::load` separately.  This explicit split is why the
    /// refresh orchestrator code sits in a future T12b packet.
    pub files_to_ingest: usize,

    pub duration_ms: u64,
}

/// How to resolve a removed file path to the entity ids that should be
/// deleted alongside the file entity (its symbols, parameters, etc.).
///
/// The caller typically feeds this from the forge-local cache or from a
/// prior graph query.  Returning an empty vec is valid (the file entity
/// itself is still deleted).
pub type FileChildResolver<'a> =
    &'a dyn Fn(&str /* file path */) -> Vec<String /* entity_id */>;

/// How to resolve a file path to its entity id for rename/delete ops.
///
/// Returns `None` when the path isn't mapped in the local cache — the
/// entity won't be patched or deleted, and the caller sees the count in
/// `files_renamed_failed` / `files_missing_not_found`.
pub type FileEntityResolver<'a> =
    &'a dyn Fn(&str /* file path */) -> Option<String /* entity_id */>;

/// Options controlling `apply_decisions` behavior.
pub struct ApplyOptions<'a> {
    pub session_id: Option<String>,
    /// Map a file path to its current entity id (for patching/deleting).
    pub resolve_file_entity_id: FileEntityResolver<'a>,
    /// Map a removed file path to its owned child entity ids (symbols,
    /// parameters, etc.).  The default resolver returns an empty vec —
    /// file entity alone is deleted, children become orphans until a
    /// future refresh prunes them.
    pub resolve_file_children: FileChildResolver<'a>,
}

/// Apply a `RefreshDecisions` batch to ferrosa-memory.
///
/// What this does:
/// - `Renamed` → `batch_update_entities` with `entity_name = new_path`
///   (we use `entity_name` as the canonical path store; callers using a
///   different field can swap in a custom patch).  The file's `properties`
///   also gets a `path` key for compatibility with graph consumers.
/// - `Missing` → `batch_delete_entities` for the file entity + its
///   resolved children.  Hard-delete.
/// - `New`/`Changed`/`VersionDrift`/`PendingRecovery` → counted as
///   "needs ingest" — the actual extraction + `ingest_entities` call is
///   the caller's job (T12b).
/// - `Unchanged` → nothing.
///
/// Returns `Err` iff any call to ferrosa-memory failed at the transport
/// level (network / protocol).  Per-row failures do NOT error; they land
/// in the `RefreshReport.*_failed` / `*_errors` counters so the caller
/// can make the triage call.
pub fn apply_decisions(
    transport: &dyn Transport,
    decisions: &RefreshDecisions,
    options: &ApplyOptions,
) -> Result<RefreshReport> {
    let started = std::time::Instant::now();
    let mut report = RefreshReport::default();

    for decision in &decisions.decisions {
        report.files_scanned += 1;
        match decision {
            Decision::New { .. } => {
                report.files_new += 1;
                report.files_to_ingest += 1;
            }
            Decision::Changed { .. } => {
                report.files_changed += 1;
                report.files_to_ingest += 1;
            }
            Decision::Unchanged { .. } => {
                report.files_unchanged += 1;
            }
            Decision::VersionDrift { .. } => {
                report.files_version_drift += 1;
                report.files_to_ingest += 1;
            }
            Decision::PendingRecovery { .. } => {
                report.files_pending_recovery += 1;
                report.files_to_ingest += 1;
            }
            Decision::Renamed {
                old_path, new_path, ..
            } => {
                apply_rename(transport, options, old_path, new_path, &mut report)?;
            }
            Decision::Missing { path, .. } => {
                apply_missing(transport, options, path, &mut report)?;
            }
        }
    }

    report.duration_ms = started.elapsed().as_millis() as u64;
    Ok(report)
}

fn apply_rename(
    transport: &dyn Transport,
    options: &ApplyOptions,
    old_path: &str,
    new_path: &str,
    report: &mut RefreshReport,
) -> Result<()> {
    let Some(entity_id) = (options.resolve_file_entity_id)(old_path) else {
        // No entity id for the old path — can't patch.  Surface in the report.
        report.files_renamed_failed += 1;
        eprintln!(
            "[refresh] rename {old_path} -> {new_path}: no file entity id resolved; skipping patch"
        );
        return Ok(());
    };

    let patch = WirePatchEntity {
        entity_id,
        entity_name: Some(new_path.to_string()),
        // Mirror into properties.path so consumers that read the
        // generic attrs map see the new path too.
        properties: Some(serde_json::json!({ "path": new_path })),
        ..Default::default()
    };
    let args = BatchUpdateEntitiesArgs {
        session_id: options.session_id.clone(),
        entities: vec![patch],
    };
    let resp = batch_update_entities(transport, args)
        .with_context(|| format!("batch_update_entities(rename {old_path}→{new_path})"))?;
    // Count by result status.
    if resp.total != resp.accounted() {
        anyhow::bail!(
            "batch_update_entities reconciliation: total={} accounted={} for rename {old_path}",
            resp.total,
            resp.accounted()
        );
    }
    // One patch per call here; take the result's status as authoritative.
    match resp.results.first() {
        Some(r) if r.status == "updated" || r.status == "unchanged" => {
            report.files_renamed_updated += 1;
        }
        Some(r) => {
            report.files_renamed_failed += 1;
            eprintln!(
                "[refresh] rename {old_path}→{new_path}: server reported status='{}' reason='{}'",
                r.status, r.reason
            );
        }
        None => {
            report.files_renamed_failed += 1;
            eprintln!(
                "[refresh] rename {old_path}→{new_path}: empty results[] — possible server bug"
            );
        }
    }
    Ok(())
}

fn apply_missing(
    transport: &dyn Transport,
    options: &ApplyOptions,
    path: &str,
    report: &mut RefreshReport,
) -> Result<()> {
    let file_id = (options.resolve_file_entity_id)(path);
    let child_ids = (options.resolve_file_children)(path);

    // Collect all ids to delete: file entity (if resolved) + its children.
    let mut targets: Vec<String> = Vec::with_capacity(1 + child_ids.len());
    if let Some(id) = file_id.clone() {
        targets.push(id);
    }
    targets.extend(child_ids);

    if targets.is_empty() {
        report.files_missing_not_found += 1;
        eprintln!(
            "[refresh] missing {path}: no entity id resolved for file or children; nothing to delete"
        );
        return Ok(());
    }

    // Split into batches of BATCH_DELETE_MAX.
    for chunk in targets.chunks(BATCH_DELETE_MAX) {
        let args = BatchDeleteEntitiesArgs {
            session_id: options.session_id.clone(),
            entities: chunk
                .iter()
                .map(|id| WireDeleteTarget {
                    entity_id: id.clone(),
                })
                .collect(),
        };
        let resp = batch_delete_entities(transport, args)
            .with_context(|| format!("batch_delete_entities(missing {path})"))?;
        if resp.total != resp.accounted() {
            anyhow::bail!(
                "batch_delete_entities reconciliation: total={} accounted={} for missing {path}",
                resp.total,
                resp.accounted()
            );
        }
        // File entity's delete status — only the first id of the first chunk
        // is the file entity itself (guaranteed by the ordering above).
        if let Some(id) = &file_id {
            if let Some(r) = resp.results.iter().find(|r| &r.entity_id == id) {
                match r.status.as_str() {
                    "deleted" => report.files_missing_deleted += 1,
                    "not_found" => report.files_missing_not_found += 1,
                    "error" => {
                        report.files_missing_errors += 1;
                        eprintln!(
                            "[refresh] missing {path}: file-entity delete error: {}",
                            r.reason
                        );
                    }
                    other => {
                        report.files_missing_errors += 1;
                        eprintln!("[refresh] missing {path}: unexpected delete status '{other}'");
                    }
                }
            }
        }
        // Child errors bubble up too — they go into files_missing_errors so
        // the caller doesn't lose visibility.  Children counts per-file are
        // intentionally not tracked separately in this v1 report.
        report.files_missing_errors += resp.errors;
    }
    Ok(())
}

#[cfg(test)]
mod apply_tests {
    use super::*;
    use forge_fmem_client::transport::mock::{MockTransport, ScriptedResponse};
    use serde_json::json;

    fn noop_resolver() -> &'static dyn Fn(&str) -> Vec<String> {
        &|_: &str| vec![]
    }

    #[test]
    fn unchanged_and_new_do_not_touch_wire() {
        let m = MockTransport::panicking();
        let file_id_resolver = |_: &str| -> Option<String> { None };
        let opts = ApplyOptions {
            session_id: None,
            resolve_file_entity_id: &file_id_resolver,
            resolve_file_children: noop_resolver(),
        };
        let decisions = RefreshDecisions {
            decisions: vec![
                Decision::Unchanged {
                    path: "a.rs".into(),
                },
                Decision::New {
                    path: "b.rs".into(),
                    sha256: "h".into(),
                },
                Decision::Changed {
                    path: "c.rs".into(),
                    sha256: "h2".into(),
                    prior_sha256: "h".into(),
                },
            ],
            new_count: 1,
            changed_count: 1,
            unchanged_count: 1,
            ..Default::default()
        };
        let r = apply_decisions(&m, &decisions, &opts).unwrap();
        assert_eq!(r.files_unchanged, 1);
        assert_eq!(r.files_new, 1);
        assert_eq!(r.files_changed, 1);
        assert_eq!(r.files_to_ingest, 2, "New + Changed need extraction");
    }

    #[test]
    fn rename_calls_batch_update() {
        let m = MockTransport::new();
        m.expect_call(
            "tools/call",
            ScriptedResponse::Ok(json!({
                "updated": 1, "unchanged": 0, "not_found": 0, "errors": 0, "total": 1,
                "results": [{ "index": 0, "entity_id": "file-1", "status": "updated" }]
            })),
        );
        let file_id_resolver = |p: &str| -> Option<String> {
            if p == "a.rs" {
                Some("file-1".into())
            } else {
                None
            }
        };
        let opts = ApplyOptions {
            session_id: Some("00000000-0000-0000-0000-000000000001".into()),
            resolve_file_entity_id: &file_id_resolver,
            resolve_file_children: noop_resolver(),
        };
        let decisions = RefreshDecisions {
            decisions: vec![Decision::Renamed {
                old_path: "a.rs".into(),
                new_path: "b.rs".into(),
                sha256: "h".into(),
            }],
            renamed_count: 1,
            ..Default::default()
        };
        let r = apply_decisions(&m, &decisions, &opts).unwrap();
        assert_eq!(r.files_renamed_updated, 1);
        assert_eq!(r.files_renamed_failed, 0);
        m.assert_done();
    }

    #[test]
    fn rename_without_resolved_id_is_reported_not_errored() {
        let m = MockTransport::panicking();
        let file_id_resolver = |_: &str| -> Option<String> { None };
        let opts = ApplyOptions {
            session_id: None,
            resolve_file_entity_id: &file_id_resolver,
            resolve_file_children: noop_resolver(),
        };
        let decisions = RefreshDecisions {
            decisions: vec![Decision::Renamed {
                old_path: "a.rs".into(),
                new_path: "b.rs".into(),
                sha256: "h".into(),
            }],
            renamed_count: 1,
            ..Default::default()
        };
        let r = apply_decisions(&m, &decisions, &opts).unwrap();
        assert_eq!(r.files_renamed_updated, 0);
        assert_eq!(r.files_renamed_failed, 1);
    }

    #[test]
    fn missing_calls_batch_delete_with_children() {
        let m = MockTransport::new();
        m.expect_call(
            "tools/call",
            ScriptedResponse::Ok(json!({
                "deleted": 3, "not_found": 0, "errors": 0, "total": 3,
                "results": [
                    { "index": 0, "entity_id": "file-1", "status": "deleted" },
                    { "index": 1, "entity_id": "sym-1",  "status": "deleted" },
                    { "index": 2, "entity_id": "sym-2",  "status": "deleted" }
                ]
            })),
        );
        let file_id_resolver = |p: &str| -> Option<String> {
            if p == "gone.rs" {
                Some("file-1".into())
            } else {
                None
            }
        };
        let child_resolver = |p: &str| -> Vec<String> {
            if p == "gone.rs" {
                vec!["sym-1".into(), "sym-2".into()]
            } else {
                vec![]
            }
        };
        let opts = ApplyOptions {
            session_id: None,
            resolve_file_entity_id: &file_id_resolver,
            resolve_file_children: &child_resolver,
        };
        let decisions = RefreshDecisions {
            decisions: vec![Decision::Missing {
                path: "gone.rs".into(),
                prior_sha256: "h".into(),
            }],
            missing_count: 1,
            ..Default::default()
        };
        let r = apply_decisions(&m, &decisions, &opts).unwrap();
        assert_eq!(r.files_missing_deleted, 1);
        assert_eq!(r.files_missing_errors, 0);
        m.assert_done();
    }

    #[test]
    fn missing_with_no_resolved_id_does_not_call_wire() {
        let m = MockTransport::panicking();
        let file_id_resolver = |_: &str| -> Option<String> { None };
        let opts = ApplyOptions {
            session_id: None,
            resolve_file_entity_id: &file_id_resolver,
            resolve_file_children: noop_resolver(),
        };
        let decisions = RefreshDecisions {
            decisions: vec![Decision::Missing {
                path: "gone.rs".into(),
                prior_sha256: "h".into(),
            }],
            missing_count: 1,
            ..Default::default()
        };
        let r = apply_decisions(&m, &decisions, &opts).unwrap();
        assert_eq!(r.files_missing_deleted, 0);
        assert_eq!(r.files_missing_not_found, 1);
    }
}
