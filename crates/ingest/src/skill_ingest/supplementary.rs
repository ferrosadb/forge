//! Resolve a skill's `supplementary-files` frontmatter into absolute,
//! canonicalized paths that are guaranteed to live inside the skill's
//! directory.
//!
//! Threat T3 — supplementary path traversal — is defeated by:
//!
//! 1. Canonicalizing the resolved path (resolves `..`, symlinks, etc.).
//! 2. Asserting the canonical result starts with the skill dir's
//!    canonical path.
//!
//! Absolute supplementary paths are rejected: frontmatter entries must
//! be relative to the SKILL.md's directory.

use std::fs;
use std::path::{Path, PathBuf};

/// A resolved supplementary file, ready for hashing or ingestion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSupplementary {
    /// Name as it appeared in frontmatter (e.g. `"tdd-strategies.md"`).
    pub declared: String,
    /// Canonical absolute path.
    pub path: PathBuf,
    /// File bytes — read eagerly so downstream hashing doesn't re-stat.
    pub bytes: Vec<u8>,
}

#[derive(Debug)]
pub enum SupplementaryError {
    /// Frontmatter entry used an absolute path.
    Absolute(String),
    /// Canonicalized target escapes the skill directory.
    EscapesSkillDir {
        declared: String,
        resolved: PathBuf,
        skill_dir: PathBuf,
    },
    /// File doesn't exist or couldn't be read.
    Io {
        declared: String,
        source: std::io::Error,
    },
}

impl std::fmt::Display for SupplementaryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Absolute(d) => {
                write!(f, "supplementary path must be relative, got absolute: {d}")
            }
            Self::EscapesSkillDir {
                declared,
                resolved,
                skill_dir,
            } => write!(
                f,
                "supplementary path `{}` resolves to `{}` which escapes skill dir `{}`",
                declared,
                resolved.display(),
                skill_dir.display()
            ),
            Self::Io { declared, source } => {
                write!(f, "supplementary file `{declared}` i/o error: {source}")
            }
        }
    }
}

impl std::error::Error for SupplementaryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Resolve a list of supplementary filenames relative to `skill_dir`.
///
/// `skill_dir` must already be canonicalized by the caller (typically
/// via the walker's canonical root). Returns resolved entries in the
/// same order as input — the hasher relies on deterministic ordering.
pub fn resolve(
    skill_dir: &Path,
    declared: &[String],
) -> Result<Vec<ResolvedSupplementary>, SupplementaryError> {
    let mut out = Vec::with_capacity(declared.len());
    for entry in declared {
        out.push(resolve_one(skill_dir, entry)?);
    }
    Ok(out)
}

fn resolve_one(
    skill_dir: &Path,
    declared: &str,
) -> Result<ResolvedSupplementary, SupplementaryError> {
    let raw = Path::new(declared);
    if raw.is_absolute() {
        return Err(SupplementaryError::Absolute(declared.to_string()));
    }

    let joined = skill_dir.join(raw);
    let resolved = fs::canonicalize(&joined).map_err(|e| SupplementaryError::Io {
        declared: declared.to_string(),
        source: e,
    })?;

    if !resolved.starts_with(skill_dir) {
        return Err(SupplementaryError::EscapesSkillDir {
            declared: declared.to_string(),
            resolved,
            skill_dir: skill_dir.to_path_buf(),
        });
    }

    let bytes = fs::read(&resolved).map_err(|e| SupplementaryError::Io {
        declared: declared.to_string(),
        source: e,
    })?;

    Ok(ResolvedSupplementary {
        declared: declared.to_string(),
        path: resolved,
        bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup_skill_dir(tmp: &TempDir) -> PathBuf {
        let skill_dir = tmp.path().join("skills").join("task-level").join("tdd");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::canonicalize(&skill_dir).unwrap()
    }

    #[test]
    fn resolves_sibling_file() {
        let tmp = TempDir::new().unwrap();
        let dir = setup_skill_dir(&tmp);
        fs::write(dir.join("strategies.md"), "content").unwrap();

        let resolved = resolve(&dir, &["strategies.md".to_string()]).unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].declared, "strategies.md");
        assert_eq!(resolved[0].bytes, b"content");
    }

    #[test]
    fn resolves_subdirectory_file() {
        let tmp = TempDir::new().unwrap();
        let dir = setup_skill_dir(&tmp);
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("sub").join("a.md"), "ok").unwrap();

        let resolved = resolve(&dir, &["sub/a.md".to_string()]).unwrap();
        assert_eq!(resolved[0].bytes, b"ok");
    }

    #[test]
    fn rejects_absolute_path() {
        let tmp = TempDir::new().unwrap();
        let dir = setup_skill_dir(&tmp);
        let err = resolve(&dir, &["/etc/passwd".to_string()]).unwrap_err();
        assert!(matches!(err, SupplementaryError::Absolute(_)));
    }

    #[test]
    fn rejects_parent_escape() {
        let tmp = TempDir::new().unwrap();
        let dir = setup_skill_dir(&tmp);
        // Create a file above the skill dir.
        let above = tmp.path().join("secrets.md");
        fs::write(&above, "sensitive").unwrap();

        let err = resolve(&dir, &["../../../secrets.md".to_string()]).unwrap_err();
        assert!(matches!(err, SupplementaryError::EscapesSkillDir { .. }));
    }

    #[test]
    fn missing_file_reports_declared_name() {
        let tmp = TempDir::new().unwrap();
        let dir = setup_skill_dir(&tmp);
        let err = resolve(&dir, &["nonexistent.md".to_string()]).unwrap_err();
        match err {
            SupplementaryError::Io { declared, .. } => {
                assert_eq!(declared, "nonexistent.md");
            }
            other => panic!("expected Io error, got {other:?}"),
        }
    }

    #[test]
    fn preserves_declared_order() {
        let tmp = TempDir::new().unwrap();
        let dir = setup_skill_dir(&tmp);
        fs::write(dir.join("b.md"), "B").unwrap();
        fs::write(dir.join("a.md"), "A").unwrap();
        fs::write(dir.join("c.md"), "C").unwrap();

        let resolved = resolve(
            &dir,
            &["b.md".to_string(), "a.md".to_string(), "c.md".to_string()],
        )
        .unwrap();
        let names: Vec<_> = resolved.iter().map(|r| r.declared.as_str()).collect();
        assert_eq!(names, vec!["b.md", "a.md", "c.md"]);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_rejected() {
        use std::os::unix::fs::symlink;
        let tmp = TempDir::new().unwrap();
        let dir = setup_skill_dir(&tmp);

        // File outside the skill tree.
        let outside_real = tmp.path().join("outside-real.md");
        fs::write(&outside_real, "real").unwrap();

        // Symlink inside the skill dir pointing to it.
        let link = dir.join("trick.md");
        symlink(&outside_real, &link).unwrap();

        let err = resolve(&dir, &["trick.md".to_string()]).unwrap_err();
        assert!(matches!(err, SupplementaryError::EscapesSkillDir { .. }));
    }
}
