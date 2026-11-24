//! Safe writes to a harness's configuration files (harness-plugins B4/B5).
//!
//! Every write to a file another tool also owns goes through here: the file is
//! copied into a backup set first, then replaced atomically (temp file in the
//! same directory, fsync, rename), writing through a symlink rather than over
//! it, and keeping the file's permissions.
//!
//! Backup layout, shared with `fmem harness migrate`:
//! `~/.ferrosa/backups/harness/<ts>/files/<path relative to HOME>` (a path
//! outside HOME goes under `files/_abs/<absolute path>`), with an optional
//! `report.json` beside `files/`.

use std::io::Write;
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context};

/// One backup directory, created on first use so a run that changes nothing
/// leaves nothing behind.
pub struct BackupSet {
    home: PathBuf,
    dir: Option<PathBuf>,
}

impl BackupSet {
    pub fn new(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
            dir: None,
        }
    }

    /// The directory, creating a fresh `<ts>` directory on first call.
    pub fn ensure_dir(&mut self) -> anyhow::Result<PathBuf> {
        if let Some(dir) = &self.dir {
            return Ok(dir.clone());
        }
        let root = backups_root(&self.home);
        std::fs::create_dir_all(&root)
            .with_context(|| format!("cannot create backup directory {}", root.display()))?;
        let stamp = jiff::Timestamp::now()
            .strftime("%Y%m%dT%H%M%SZ")
            .to_string();
        // create_dir, not create_dir_all: an existing directory from a run in
        // the same second must not be shared, so take the next suffix.
        for attempt in 0..100u32 {
            let name = if attempt == 0 {
                stamp.clone()
            } else {
                format!("{stamp}-{attempt}")
            };
            let dir = root.join(name);
            match std::fs::create_dir(&dir) {
                Ok(()) => {
                    self.dir = Some(dir.clone());
                    return Ok(dir);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => {
                    return Err(e)
                        .with_context(|| format!("cannot create backup {}", dir.display()))
                }
            }
        }
        bail!(
            "cannot create a backup directory under {}: 100 already exist for {stamp}",
            root.display()
        )
    }

    /// Copy `file` into the backup set and return where it went.
    pub fn save(&mut self, file: &Path) -> anyhow::Result<PathBuf> {
        let dir = self.ensure_dir()?;
        let dest = dir
            .join("files")
            .join(relative_backup_path(&self.home, file)?);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }
        std::fs::copy(file, &dest)
            .with_context(|| format!("cannot back up {} to {}", file.display(), dest.display()))?;
        Ok(dest)
    }
}

pub fn backups_root(home: &Path) -> PathBuf {
    home.join(".ferrosa").join("backups").join("harness")
}

/// Where `file` lives inside a backup set's `files/` directory.
pub fn relative_backup_path(home: &Path, file: &Path) -> anyhow::Result<PathBuf> {
    let absolute = if file.is_absolute() {
        file.to_path_buf()
    } else {
        std::env::current_dir()?.join(file)
    };
    if let Ok(rel) = absolute.strip_prefix(home) {
        return Ok(rel.to_path_buf());
    }
    let mut out = PathBuf::from("_abs");
    for part in absolute.components() {
        if let Component::Normal(p) = part {
            out.push(p);
        }
    }
    Ok(out)
}

/// Replace `path`'s contents with `text` atomically. A symlink is written
/// through to its target, and an existing file keeps its permissions.
pub fn write_atomic(path: &Path, text: &str) -> anyhow::Result<()> {
    let target = match std::fs::canonicalize(path) {
        Ok(real) => real,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => path.to_path_buf(),
        Err(e) => return Err(e).with_context(|| format!("cannot resolve {}", path.display())),
    };
    let parent = match target.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    std::fs::create_dir_all(&parent)
        .with_context(|| format!("cannot create {}", parent.display()))?;
    let permissions = std::fs::metadata(&target).ok().map(|m| m.permissions());

    let mut temp = tempfile::NamedTempFile::new_in(&parent)
        .with_context(|| format!("cannot create a temp file in {}", parent.display()))?;
    temp.write_all(text.as_bytes())?;
    temp.as_file().sync_all()?;
    if let Some(permissions) = permissions {
        std::fs::set_permissions(temp.path(), permissions)?;
    }
    temp.persist(&target)
        .with_context(|| format!("cannot replace {}", target.display()))?;
    Ok(())
}

/// Serialize JSON the way Forge writes settings: pretty, keeping the
/// original's trailing newline (or adding one to a new file).
pub fn json_text(value: &serde_json::Value, original: Option<&str>) -> anyhow::Result<String> {
    let mut text = serde_json::to_string_pretty(value)?;
    if original.is_none_or(|o| o.ends_with('\n')) {
        text.push('\n');
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_under_home_keep_their_relative_shape() {
        let home = Path::new("/h");
        assert_eq!(
            relative_backup_path(home, Path::new("/h/.claude/settings.json")).unwrap(),
            PathBuf::from(".claude/settings.json")
        );
        assert_eq!(
            relative_backup_path(home, Path::new("/elsewhere/.claude/settings.json")).unwrap(),
            PathBuf::from("_abs/elsewhere/.claude/settings.json")
        );
    }

    #[test]
    fn two_backup_sets_in_one_second_get_separate_directories() {
        let home = tempfile::tempdir().unwrap();
        let mut a = BackupSet::new(home.path());
        let mut b = BackupSet::new(home.path());
        assert_ne!(a.ensure_dir().unwrap(), b.ensure_dir().unwrap());
    }

    #[test]
    fn json_text_keeps_a_missing_trailing_newline_missing() {
        let v = serde_json::json!({"a": 1});
        assert!(!json_text(&v, Some("{\"a\":1}")).unwrap().ends_with('\n'));
        assert!(json_text(&v, Some("{\"a\":1}\n")).unwrap().ends_with('\n'));
        assert!(json_text(&v, None).unwrap().ends_with('\n'));
    }
}
