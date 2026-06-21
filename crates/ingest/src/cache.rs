//! Per-project refresh cache for code-graph ingest.
//!
//! Tracks `(path, sha256, last_refreshed_at_ms, extractor_schema_version,
//! pending_file_id?)` per file in a local TOML file under
//! `.forge/cache/code-graph/<project-id>.toml`.
//!
//! Guards:
//! - F13 (RPN 140) — advisory exclusive lock via `fs2`; a second concurrent
//!   forge process exits with a clear error instead of interleaving writes.
//! - F11 (RPN 256) — `pending_file_id` write-ahead marker lets a crashed
//!   mid-refresh be detected on next run; files with a non-`None`
//!   `pending_file_id` are treated as dirty and re-extracted.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Current on-disk schema version of the cache file itself (not the
/// extractor schema version carried per-entry).
pub const CACHE_SCHEMA_VERSION: u32 = 1;

/// One entry per tracked file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FileCacheEntry {
    /// Project-relative path with POSIX separators.
    pub path: String,
    /// Lowercase hex sha256 of the file content.
    pub sha256: String,
    /// Unix epoch milliseconds of the last successful refresh.
    pub last_refreshed_at_ms: i64,
    /// `EXTRACTOR_SCHEMA_VERSION` at the time of refresh.
    pub extractor_schema_version: u32,
    /// If `Some`, a refresh is mid-flight for this file. On recovery, treat
    /// as dirty and re-extract — the prior refresh may have partially
    /// committed entities/edges to ferrosa-memory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_file_id: Option<Uuid>,
}

/// Cache document (the whole TOML file).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheDoc {
    #[serde(default = "default_cache_schema")]
    pub cache_schema_version: u32,
    #[serde(default)]
    pub files: HashMap<String, FileCacheEntry>,
}

impl Default for CacheDoc {
    fn default() -> Self {
        Self {
            cache_schema_version: CACHE_SCHEMA_VERSION,
            files: HashMap::new(),
        }
    }
}

fn default_cache_schema() -> u32 {
    CACHE_SCHEMA_VERSION
}

/// A held exclusive lock on the cache file. Drop releases.
#[derive(Debug)]
pub struct CacheHandle {
    path: PathBuf,
    file: File,
    doc: CacheDoc,
    dirty: bool,
}

impl CacheHandle {
    /// Open (creating if missing) and acquire an exclusive lock.
    /// Returns `Err` if another process holds the lock, or if the on-disk
    /// cache is present but corrupt.
    pub fn open(cache_path: &Path) -> Result<Self> {
        if let Some(parent) = cache_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cache: create parent dirs: {}", parent.display()))?;
        }
        let file = acquire_lock(cache_path)?;
        let doc = load_or_create_doc(cache_path)?;
        Ok(Self {
            path: cache_path.to_path_buf(),
            file,
            doc,
            dirty: false,
        })
    }

    pub fn doc(&self) -> &CacheDoc {
        &self.doc
    }

    pub fn entry(&self, path: &str) -> Option<&FileCacheEntry> {
        self.doc.files.get(&normalize_path(path))
    }

    pub fn set_entry(&mut self, entry: FileCacheEntry) {
        let key = normalize_path(&entry.path);
        let mut normalized = entry;
        normalized.path = key.clone();
        self.doc.files.insert(key, normalized);
        self.dirty = true;
    }

    pub fn remove_entry(&mut self, path: &str) -> Option<FileCacheEntry> {
        let removed = self.doc.files.remove(&normalize_path(path));
        if removed.is_some() {
            self.dirty = true;
        }
        removed
    }

    /// Persist to disk atomically (temp-file + rename) if dirty.
    pub fn save(&mut self) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }
        self.save_atomic_inner()?;
        self.dirty = false;
        Ok(())
    }

    fn save_atomic_inner(&self) -> Result<()> {
        let serialized = toml::to_string_pretty(&self.doc)
            .with_context(|| format!("cache: serialize: {}", self.path.display()))?;
        let tmp = self.path.with_extension("toml.tmp");
        // Best-effort pre-clean: if a prior crash left a temp file, remove it.
        let _ = std::fs::remove_file(&tmp);
        let write_result = (|| -> Result<()> {
            let mut f = OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&tmp)
                .with_context(|| format!("cache: open temp: {}", tmp.display()))?;
            f.write_all(serialized.as_bytes())
                .with_context(|| format!("cache: write temp: {}", tmp.display()))?;
            f.sync_all()
                .with_context(|| format!("cache: sync temp: {}", tmp.display()))?;
            Ok(())
        })();
        if let Err(e) = write_result {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        std::fs::rename(&tmp, &self.path).with_context(|| {
            format!(
                "cache: atomic rename {} -> {}",
                tmp.display(),
                self.path.display()
            )
        })?;
        Ok(())
    }
}

impl Drop for CacheHandle {
    fn drop(&mut self) {
        if self.dirty {
            if let Err(e) = self.save_atomic_inner() {
                eprintln!("[forge-cache] save on drop failed: {e}");
            }
        }
        if let Err(e) = self.file.unlock() {
            eprintln!("[forge-cache] unlock on drop failed: {e}");
        }
    }
}

/// Open the cache file (creating if missing) and acquire an exclusive
/// non-blocking lock. Fails loud if another process holds it.
fn acquire_lock(cache_path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(cache_path)
        .with_context(|| format!("cache: open: {}", cache_path.display()))?;
    file.try_lock_exclusive().map_err(|e| {
        anyhow::anyhow!(
            "cache file is locked by another forge process: {}\n\
             (another `frg ingest` or `frg refresh` may be running in this project)\n\
             underlying error: {e}",
            cache_path.display()
        )
    })?;
    Ok(file)
}

/// Load the cache doc. If the file is empty or absent (newly created),
/// return a default empty doc. If the file has content but fails to parse,
/// fail loud — never silently reset.
fn load_or_create_doc(cache_path: &Path) -> Result<CacheDoc> {
    let mut raw = String::new();
    match File::open(cache_path) {
        Ok(mut f) => {
            f.read_to_string(&mut raw)
                .with_context(|| format!("cache: read: {}", cache_path.display()))?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(CacheDoc::default()),
        Err(e) => {
            return Err(e)
                .with_context(|| format!("cache: open for read: {}", cache_path.display()));
        }
    }
    if raw.trim().is_empty() {
        return Ok(CacheDoc::default());
    }
    toml::from_str::<CacheDoc>(&raw).map_err(|e| {
        anyhow::anyhow!(
            "cache file is corrupt or unreadable: {}\n\
             parse error: {e}\n\
             to recover, remove the file and re-run ingest: rm {}",
            cache_path.display(),
            cache_path.display()
        )
    })
}

/// Normalize path separators to POSIX. Never stores absolute paths at the
/// project level — caller is expected to pass project-relative input.
fn normalize_path(p: &str) -> String {
    p.replace('\\', "/")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::Duration;

    use tempfile::TempDir;

    use super::*;

    fn sample_entry(path: &str, sha: &str) -> FileCacheEntry {
        FileCacheEntry {
            path: path.to_string(),
            sha256: sha.to_string(),
            last_refreshed_at_ms: 1_700_000_000_000,
            extractor_schema_version: 1,
            pending_file_id: None,
        }
    }

    #[test]
    fn open_creates_file_and_dirs() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("a").join("b").join("c").join("cache.toml");
        let handle = CacheHandle::open(&target).unwrap();
        assert_eq!(handle.doc().files.len(), 0);
        assert_eq!(handle.doc().cache_schema_version, CACHE_SCHEMA_VERSION);
        assert!(target.exists());
        assert!(target.parent().unwrap().is_dir());
    }

    #[test]
    fn roundtrip_entry() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("cache.toml");
        {
            let mut h = CacheHandle::open(&path).unwrap();
            h.set_entry(sample_entry("src/foo.rs", "abc123"));
            h.save().unwrap();
        }
        let h = CacheHandle::open(&path).unwrap();
        let e = h.entry("src/foo.rs").expect("entry present after reopen");
        assert_eq!(e.sha256, "abc123");
        assert_eq!(e.extractor_schema_version, 1);
        assert_eq!(e.pending_file_id, None);
    }

    #[test]
    fn atomic_write_leaves_no_tmp_on_success() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("cache.toml");
        {
            let mut h = CacheHandle::open(&path).unwrap();
            h.set_entry(sample_entry("src/a.rs", "1"));
            h.save().unwrap();
        }
        let tmp_sibling = path.with_extension("toml.tmp");
        assert!(!tmp_sibling.exists(), "temp file leaked: {tmp_sibling:?}");
    }

    #[test]
    fn corrupted_toml_fails_loud() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("cache.toml");
        std::fs::write(&path, "this is not valid toml } ][[").unwrap();
        let err = CacheHandle::open(&path).expect_err("must reject corrupt file");
        let msg = format!("{err}");
        assert!(
            msg.contains("corrupt") || msg.contains("parse"),
            "error message must mention parsing: {msg}"
        );
        assert!(msg.contains(path.to_string_lossy().as_ref()));
    }

    #[test]
    fn concurrent_lock_fails_fast() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("cache.toml");
        // Hold the first handle in the main thread; try to acquire from a
        // second thread and assert it fails.
        let holder = CacheHandle::open(&path).unwrap();
        let path_clone = path.clone();
        let barrier = Arc::new(Barrier::new(2));
        let b2 = Arc::clone(&barrier);
        let t = thread::spawn(move || {
            b2.wait();
            CacheHandle::open(&path_clone)
        });
        barrier.wait();
        // Give the thread a moment to attempt the lock.
        thread::sleep(Duration::from_millis(50));
        let result = t.join().unwrap();
        assert!(result.is_err(), "second lock must fail while first held");
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("locked"), "error must mention locking: {msg}");
        // Keep holder alive until here.
        drop(holder);
    }

    #[test]
    fn pending_marker_roundtrips() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("cache.toml");
        let id = Uuid::new_v4();
        {
            let mut h = CacheHandle::open(&path).unwrap();
            let mut e = sample_entry("src/a.rs", "1");
            e.pending_file_id = Some(id);
            h.set_entry(e);
            h.save().unwrap();
        }
        let h = CacheHandle::open(&path).unwrap();
        assert_eq!(h.entry("src/a.rs").unwrap().pending_file_id, Some(id));
    }

    #[test]
    fn remove_entry_removes_from_doc() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("cache.toml");
        {
            let mut h = CacheHandle::open(&path).unwrap();
            h.set_entry(sample_entry("src/a.rs", "1"));
            h.set_entry(sample_entry("src/b.rs", "2"));
            h.save().unwrap();
        }
        {
            let mut h = CacheHandle::open(&path).unwrap();
            let removed = h.remove_entry("src/a.rs");
            assert!(removed.is_some());
            h.save().unwrap();
        }
        let h = CacheHandle::open(&path).unwrap();
        assert!(h.entry("src/a.rs").is_none());
        assert!(h.entry("src/b.rs").is_some());
    }

    #[test]
    fn schema_version_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("cache.toml");
        {
            let mut h = CacheHandle::open(&path).unwrap();
            h.set_entry(sample_entry("x.rs", "1"));
            h.save().unwrap();
        }
        let h = CacheHandle::open(&path).unwrap();
        assert_eq!(h.doc().cache_schema_version, CACHE_SCHEMA_VERSION);
    }

    #[test]
    fn paths_normalized_to_posix() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("cache.toml");
        {
            let mut h = CacheHandle::open(&path).unwrap();
            h.set_entry(sample_entry(r"src\win\foo.rs", "1"));
            h.save().unwrap();
        }
        let h = CacheHandle::open(&path).unwrap();
        assert!(h.entry("src/win/foo.rs").is_some(), "lookup via posix");
        assert!(
            h.entry(r"src\win\foo.rs").is_some(),
            "lookup via backslash also normalized"
        );
        let stored = h.entry("src/win/foo.rs").unwrap();
        assert_eq!(stored.path, "src/win/foo.rs");
    }

    #[test]
    fn no_save_when_clean_does_not_rewrite() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("cache.toml");
        {
            let mut h = CacheHandle::open(&path).unwrap();
            h.set_entry(sample_entry("a.rs", "1"));
            h.save().unwrap();
        }
        let mtime_before = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        {
            // Open and close without changes — save should be a no-op.
            let mut h = CacheHandle::open(&path).unwrap();
            h.save().unwrap();
        }
        let mtime_after = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(mtime_before, mtime_after, "clean save must not rewrite");
    }
}
