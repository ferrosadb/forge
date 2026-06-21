//! Skill name collision detection — runs before any fmem call
//! (FMEA F7 / WI-FMEA-01).
//!
//! Two different SKILL.md files whose frontmatter `name` resolves to
//! the same identifier would clobber each other in fmem's flat skill
//! namespace. Detect the collision locally and exit 3 before any data
//! lands.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::parse::Skill;

/// Result of detecting name collisions across all parsed skills.
#[derive(Debug)]
pub struct CollisionError {
    /// name → every path that declared that name.
    pub collisions: HashMap<String, Vec<PathBuf>>,
}

impl std::fmt::Display for CollisionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "skill name collision(s):")?;
        let mut names: Vec<&String> = self.collisions.keys().collect();
        names.sort();
        for name in names {
            let paths = &self.collisions[name];
            write!(f, "\n  `{name}` declared by:")?;
            for p in paths {
                write!(f, "\n    - {}", p.display())?;
            }
        }
        Ok(())
    }
}

impl std::error::Error for CollisionError {}

/// Scan the parsed skills + their paths and return an error if any
/// name is declared by two or more paths.
///
/// Inputs are paired; the caller is expected to pass `(skill, path)`
/// tuples from the walker.
pub fn detect<'a>(
    skills: impl IntoIterator<Item = (&'a Skill, &'a Path)>,
) -> Result<(), CollisionError> {
    let mut by_name: HashMap<String, Vec<PathBuf>> = HashMap::new();
    for (skill, path) in skills {
        by_name
            .entry(skill.name.clone())
            .or_default()
            .push(path.to_path_buf());
    }

    let collisions: HashMap<String, Vec<PathBuf>> = by_name
        .into_iter()
        .filter(|(_, paths)| paths.len() > 1)
        .collect();

    if collisions.is_empty() {
        Ok(())
    } else {
        Err(CollisionError { collisions })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill_ingest::parse;

    fn make(name: &str) -> Skill {
        let raw = format!("---\nname: {name}\ndescription: d\n---\nbody");
        parse::parse(raw.as_bytes(), "task-level").unwrap()
    }

    #[test]
    fn unique_names_clean() {
        let a = make("tdd");
        let b = make("refactor");
        let pa = PathBuf::from("/r/task-level/tdd/SKILL.md");
        let pb = PathBuf::from("/r/task-level/refactor/SKILL.md");
        detect([(&a, pa.as_path()), (&b, pb.as_path())]).unwrap();
    }

    #[test]
    fn duplicate_name_across_categories_errors() {
        let a = make("quality");
        let b = make("quality");
        let pa = PathBuf::from("/r/task-level/quality/SKILL.md");
        let pb = PathBuf::from("/r/tech/quality/SKILL.md");

        let err = detect([(&a, pa.as_path()), (&b, pb.as_path())]).unwrap_err();
        assert_eq!(err.collisions.len(), 1);
        let paths = &err.collisions["quality"];
        assert_eq!(paths.len(), 2);
        assert!(paths.contains(&pa));
        assert!(paths.contains(&pb));
    }

    #[test]
    fn error_message_names_both_paths() {
        let a = make("dup");
        let b = make("dup");
        let pa = PathBuf::from("/r/a/dup/SKILL.md");
        let pb = PathBuf::from("/r/b/dup/SKILL.md");
        let err = detect([(&a, pa.as_path()), (&b, pb.as_path())]).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("dup"));
        assert!(msg.contains(pa.to_str().unwrap()));
        assert!(msg.contains(pb.to_str().unwrap()));
    }

    #[test]
    fn triple_collision_reports_all_paths() {
        let a = make("x");
        let b = make("x");
        let c = make("x");
        let pa = PathBuf::from("/r/1/SKILL.md");
        let pb = PathBuf::from("/r/2/SKILL.md");
        let pc = PathBuf::from("/r/3/SKILL.md");
        let err = detect([(&a, pa.as_path()), (&b, pb.as_path()), (&c, pc.as_path())]).unwrap_err();
        assert_eq!(err.collisions["x"].len(), 3);
    }
}
