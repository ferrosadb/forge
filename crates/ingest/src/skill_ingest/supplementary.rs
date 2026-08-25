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
    ///
    /// Empty when `inlined` is false: the file exists but sits outside the
    /// skill, so it is REFERENCED rather than pulled in.
    pub bytes: Vec<u8>,
    /// Whether the content was taken into this skill.
    ///
    /// False for a file outside the skill directory. Those are real
    /// cross-references — a skill citing a sibling skill's conventions, or a
    /// corpus distillation it is built on — and they used to fail the whole
    /// skill. Recording the reference and moving on keeps the skill, keeps the
    /// pointer, and avoids copying a corpus document into a skill, which would
    /// duplicate Information-tier text into Wisdom.
    pub inlined: bool,
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

    // Outside the skill: keep the pointer, do not take the content.
    //
    // NOT an error. The guard exists to stop a skill inlining arbitrary files,
    // and it still does -- nothing outside is read. But a skill that cites a
    // sibling's conventions or the corpus document it was distilled from is
    // making a legitimate reference, and failing the whole skill over it lost
    // 3 of 98 in the real catalog.
    if !resolved.starts_with(skill_dir) {
        return Ok(ResolvedSupplementary {
            declared: declared.to_string(),
            path: resolved,
            bytes: Vec::new(),
            inlined: false,
        });
    }

    // A declared file that cannot be READ stays fatal. That is a typo or a
    // deleted file, and passing it silently would let a skill lose half its
    // content without anyone noticing.
    let bytes = fs::read(&resolved).map_err(|e| SupplementaryError::Io {
        declared: declared.to_string(),
        source: e,
    })?;

    Ok(ResolvedSupplementary {
        declared: declared.to_string(),
        path: resolved,
        bytes,
        inlined: true,
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

    /// A path outside the skill is REFERENCED, never read.
    ///
    /// This used to be an error, which failed the whole skill and lost 3 of
    /// 98 in the real catalog over legitimate cross-references. The property
    /// that matters is unchanged and is what this now asserts: not one byte
    /// from outside the skill directory is taken in.
    #[test]
    fn a_path_above_the_skill_is_referenced_and_never_read() {
        let tmp = TempDir::new().unwrap();
        let dir = setup_skill_dir(&tmp);
        let above = tmp.path().join("secrets.md");
        fs::write(&above, "sensitive").unwrap();

        let out =
            resolve(&dir, &["../../../secrets.md".to_string()]).expect("a reference, not an error");
        let sup = out.first().expect("one entry");
        assert!(!sup.inlined, "content outside the skill was inlined");
        assert!(
            sup.bytes.is_empty(),
            "bytes were read from outside the skill"
        );
        assert_eq!(sup.declared, "../../../secrets.md", "the pointer is kept");
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

    /// A symlink out of the skill directory reads nothing either.
    ///
    /// The check is on the CANONICALISED path, so a link that looks local and
    /// points away is caught the same as `../`. This is the case the guard
    /// exists for, and relaxing escapes from fatal to referenced must not
    /// weaken it: the assertion is on the bytes, not on the error type.
    #[cfg(unix)]
    #[test]
    fn a_symlink_out_of_the_skill_is_referenced_and_never_read() {
        use std::os::unix::fs::symlink;
        let tmp = TempDir::new().unwrap();
        let dir = setup_skill_dir(&tmp);

        let outside_real = tmp.path().join("outside-real.md");
        fs::write(&outside_real, "real").unwrap();
        let link = dir.join("trick.md");
        symlink(&outside_real, &link).unwrap();

        let out = resolve(&dir, &["trick.md".to_string()]).expect("a reference, not an error");
        let sup = out.first().expect("one entry");
        assert!(!sup.inlined, "a symlink pulled content in from outside");
        assert!(sup.bytes.is_empty(), "bytes were read through a symlink");
    }
}
