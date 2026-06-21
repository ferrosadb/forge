//! Filesystem walker for `SKILL.md` files.
//!
//! Recursively enumerates `SKILL.md` under a root directory, with three
//! safety properties enforced from the threat model and FMEA:
//!
//! 1. Symlinks are not followed (threat T4 — symlink escape).
//! 2. Recursion depth is bounded (threat D2, FMEA F3 — symlink loops via
//!    bind mounts can still create cycles even without follow_links).
//! 3. Output is sorted lexicographically (FMEA F17 — deterministic runs).
//!
//! Non-UTF-8 paths and per-file size violations are *not* checked here —
//! those belong to the parser, which has the byte content.

use std::fs;
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

/// Hard cap on recursion depth. The skill catalog is ~3 levels deep
/// (`category/skill/SKILL.md`); 10 leaves comfortable headroom.
pub const MAX_DEPTH: usize = 10;

/// One discovered skill file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillFile {
    /// Absolute path to the SKILL.md file.
    pub path: PathBuf,
    /// Top-level category directory name (immediate child of `root`).
    /// e.g. for `<root>/task-level/refactor/SKILL.md` → `"task-level"`.
    pub category: String,
    /// Bytes of the SKILL.md file (read eagerly so the parser doesn't
    /// have to re-stat).
    pub bytes: Vec<u8>,
}

#[derive(Debug)]
pub enum WalkError {
    RootMissing(PathBuf),
    NotADirectory(PathBuf),
    NonUtf8Path(PathBuf),
    SkillOutsideCategory(PathBuf),
    Io(std::io::Error),
}

impl std::fmt::Display for WalkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RootMissing(p) => write!(f, "skill root does not exist: {}", p.display()),
            Self::NotADirectory(p) => write!(f, "skill root is not a directory: {}", p.display()),
            Self::NonUtf8Path(p) => write!(f, "skill path is not valid UTF-8: {}", p.display()),
            Self::SkillOutsideCategory(p) => {
                write!(
                    f,
                    "SKILL.md not nested under a category directory: {}",
                    p.display()
                )
            }
            Self::Io(e) => write!(f, "i/o error during walk: {e}"),
        }
    }
}

impl std::error::Error for WalkError {}

impl From<std::io::Error> for WalkError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Walk `root` and return every discovered SKILL.md, sorted by path.
///
/// `root` must be an existing directory. Symlinks under `root` are not
/// followed; if encountered they are silently skipped (the walkdir
/// iterator simply doesn't descend into them when `follow_links(false)`).
pub fn walk(root: &Path) -> Result<Vec<SkillFile>, WalkError> {
    if !root.exists() {
        return Err(WalkError::RootMissing(root.to_path_buf()));
    }
    if !root.is_dir() {
        return Err(WalkError::NotADirectory(root.to_path_buf()));
    }

    let root_canonical = root.canonicalize()?;

    let mut files: Vec<SkillFile> = Vec::new();
    for entry in WalkDir::new(&root_canonical)
        .follow_links(false)
        .max_depth(MAX_DEPTH)
        .sort_by_file_name()
    {
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                // Fail-loud: surface the path + reason rather than silently
                // pretending the subtree wasn't there. Common causes:
                // permission-denied, stale NFS mount, concurrent deletion.
                eprintln!("[forge] skill walk: skipping unreadable entry: {err}");
                continue;
            }
        };

        if !entry.file_type().is_file() {
            continue;
        }
        if entry.file_name() != "SKILL.md" {
            continue;
        }

        let path = entry.path().to_path_buf();
        let category = derive_category(&root_canonical, &path)?;
        let bytes = fs::read(&path)?;

        files.push(SkillFile {
            path,
            category,
            bytes,
        });
    }

    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// Extract the top-level category directory name from a SKILL.md path.
///
/// For `<root>/task-level/refactor/SKILL.md` this is `task-level`;
/// for `<root>/task-level/repo/ship-it/SKILL.md` it is also `task-level`
/// (per the feature spec example, which picks the immediate child of
/// `root` regardless of nesting depth).
fn derive_category(root: &Path, skill_path: &Path) -> Result<String, WalkError> {
    let rel = skill_path
        .strip_prefix(root)
        .map_err(|_| WalkError::SkillOutsideCategory(skill_path.to_path_buf()))?;
    let first = rel
        .components()
        .next()
        .ok_or_else(|| WalkError::SkillOutsideCategory(skill_path.to_path_buf()))?;

    let s = first
        .as_os_str()
        .to_str()
        .ok_or_else(|| WalkError::NonUtf8Path(skill_path.to_path_buf()))?;
    Ok(s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_skill(root: &Path, rel: &str, body: &str) {
        let full = root.join(rel);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, body).unwrap();
    }

    #[test]
    fn missing_root_errors() {
        let err = walk(Path::new("/nonexistent/zzz")).unwrap_err();
        assert!(matches!(err, WalkError::RootMissing(_)));
    }

    #[test]
    fn root_is_file_errors() {
        let tmp = TempDir::new().unwrap();
        let f = tmp.path().join("not-a-dir");
        fs::write(&f, "x").unwrap();
        let err = walk(&f).unwrap_err();
        assert!(matches!(err, WalkError::NotADirectory(_)));
    }

    #[test]
    fn finds_skills_with_category() {
        let tmp = TempDir::new().unwrap();
        make_skill(tmp.path(), "task-level/tdd/SKILL.md", "tdd body");
        make_skill(tmp.path(), "tech/rust/SKILL.md", "rust body");
        let files = walk(tmp.path()).unwrap();
        assert_eq!(files.len(), 2);
        let categories: Vec<_> = files.iter().map(|f| f.category.as_str()).collect();
        assert!(categories.contains(&"task-level"));
        assert!(categories.contains(&"tech"));
    }

    #[test]
    fn deterministic_ordering() {
        let tmp = TempDir::new().unwrap();
        make_skill(tmp.path(), "z/zzz/SKILL.md", "z");
        make_skill(tmp.path(), "a/aaa/SKILL.md", "a");
        make_skill(tmp.path(), "m/mmm/SKILL.md", "m");
        let run1: Vec<_> = walk(tmp.path())
            .unwrap()
            .into_iter()
            .map(|f| f.path)
            .collect();
        let run2: Vec<_> = walk(tmp.path())
            .unwrap()
            .into_iter()
            .map(|f| f.path)
            .collect();
        assert_eq!(run1, run2);
    }

    #[test]
    fn nested_skill_keeps_top_level_category() {
        // task-level/repo/ship-it/SKILL.md → category is "task-level"
        let tmp = TempDir::new().unwrap();
        make_skill(tmp.path(), "task-level/repo/ship-it/SKILL.md", "x");
        let files = walk(tmp.path()).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].category, "task-level");
    }

    #[test]
    fn ignores_non_skill_md_files() {
        let tmp = TempDir::new().unwrap();
        make_skill(tmp.path(), "category/skill/SKILL.md", "yes");
        make_skill(tmp.path(), "category/skill/README.md", "no");
        make_skill(tmp.path(), "category/skill/other.md", "no");
        let files = walk(tmp.path()).unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].path.ends_with("SKILL.md"));
    }

    #[test]
    fn reads_bytes() {
        let tmp = TempDir::new().unwrap();
        make_skill(tmp.path(), "c/s/SKILL.md", "hello");
        let files = walk(tmp.path()).unwrap();
        assert_eq!(files[0].bytes, b"hello");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_inside_root_not_followed() {
        use std::os::unix::fs::symlink;
        let tmp = TempDir::new().unwrap();
        // Real skill outside the root — accessible only via symlink.
        let outside = TempDir::new().unwrap();
        make_skill(outside.path(), "private/SKILL.md", "secret");
        let inside_link = tmp.path().join("category");
        symlink(outside.path(), &inside_link).unwrap();
        let files = walk(tmp.path()).unwrap();
        // walkdir.follow_links(false) means we never descend into the symlink,
        // so the secret SKILL.md should not appear in results.
        assert_eq!(
            files.len(),
            0,
            "symlinked dir was followed: {:?}",
            files.iter().map(|f| &f.path).collect::<Vec<_>>()
        );
    }
}
