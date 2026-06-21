//! Parse the optional `tag-hierarchy.yaml` that sits at the skill root.
//!
//! Schema (locked 2026-04-16 — `PARENT_TAG` edge direction is
//! `child → parent`, matching the feature-spec example
//! `tdd PARENT_TAG testing`):
//!
//! ```yaml
//! # Each mapping key is a child tag; its value is the parent.
//! tdd: testing
//! bdd: testing
//! testing: quality
//!
//! # …or an alternative list shape, each entry `{child, parent}`:
//! edges:
//!   - child: tdd
//!     parent: testing
//! ```
//!
//! Either shape is accepted; the list shape wins if both are present
//! (a warning is emitted in the orchestrator when that happens).
//!
//! Security properties:
//! - Per-file cap of 64 KiB (threat-model T5 — YAML bomb).
//! - Edge count cap of 1000 (threat-model D4).
//! - Safe loader via `serde_yaml` — no anchor resolution beyond the
//!   default.

use std::fs;
use std::path::Path;

use serde::Deserialize;

use super::plan::TagEdge;
use crate::skill_ingest::parse::normalize_tag;

/// Hard cap on `tag-hierarchy.yaml` file size.
pub const MAX_HIERARCHY_BYTES: usize = 64 * 1024;

/// Hard cap on the number of PARENT_TAG edges in one hierarchy file.
pub const MAX_HIERARCHY_EDGES: usize = 1000;

/// File name the parser looks for at the skill root.
pub const HIERARCHY_FILENAME: &str = "tag-hierarchy.yaml";

#[derive(Debug)]
pub enum HierarchyError {
    TooLarge { actual: usize, max: usize },
    TooManyEdges { actual: usize, max: usize },
    Yaml(serde_yaml::Error),
    Io(std::io::Error),
    InvalidTagName { raw: String, reason: &'static str },
    SelfLoop { node: String },
}

impl std::fmt::Display for HierarchyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge { actual, max } => {
                write!(
                    f,
                    "tag-hierarchy.yaml too large: {actual} bytes (max {max})"
                )
            }
            Self::TooManyEdges { actual, max } => {
                write!(f, "tag-hierarchy.yaml has {actual} edges (max {max})")
            }
            Self::Yaml(e) => write!(f, "tag-hierarchy.yaml YAML error: {e}"),
            Self::Io(e) => write!(f, "tag-hierarchy.yaml i/o error: {e}"),
            Self::InvalidTagName { raw, reason } => {
                write!(f, "tag-hierarchy.yaml invalid tag name `{raw}`: {reason}")
            }
            Self::SelfLoop { node } => {
                write!(f, "tag-hierarchy.yaml has self-loop on `{node}`")
            }
        }
    }
}

impl std::error::Error for HierarchyError {}

impl From<std::io::Error> for HierarchyError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Outcome of a hierarchy parse.
#[derive(Debug)]
pub enum HierarchyOutcome {
    /// File present and parsed successfully.
    Loaded(Vec<TagEdge>),
    /// File absent — caller should log at info that the taxonomy is flat.
    Absent,
}

/// Parse `<skill_root>/tag-hierarchy.yaml` if present. Returns the
/// list of edges (child → parent) or `Absent` if no such file exists.
///
/// Names are normalized at parse time per the locked design choice.
pub fn parse_hierarchy(skill_root: &Path) -> Result<HierarchyOutcome, HierarchyError> {
    let path = skill_root.join(HIERARCHY_FILENAME);
    if !path.exists() {
        return Ok(HierarchyOutcome::Absent);
    }
    let bytes = fs::read(&path)?;
    if bytes.len() > MAX_HIERARCHY_BYTES {
        return Err(HierarchyError::TooLarge {
            actual: bytes.len(),
            max: MAX_HIERARCHY_BYTES,
        });
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| HierarchyError::InvalidTagName {
        raw: "<non-utf8>".into(),
        reason: "hierarchy file is not valid UTF-8",
    })?;

    let doc: HierarchyDoc = serde_yaml::from_str(text).map_err(HierarchyError::Yaml)?;

    let edges = doc.into_edges()?;
    if edges.len() > MAX_HIERARCHY_EDGES {
        return Err(HierarchyError::TooManyEdges {
            actual: edges.len(),
            max: MAX_HIERARCHY_EDGES,
        });
    }
    for e in &edges {
        if e.child == e.parent {
            return Err(HierarchyError::SelfLoop {
                node: e.child.clone(),
            });
        }
    }
    Ok(HierarchyOutcome::Loaded(edges))
}

// -----------------------------------------------------------------------
// YAML deserialization
// -----------------------------------------------------------------------

/// The hierarchy file is deserialized into one of two supported
/// shapes; we accept either and merge.
#[derive(Debug, Default, Deserialize)]
struct HierarchyDoc {
    /// Flat map: child → parent.
    #[serde(default, flatten)]
    flat: std::collections::BTreeMap<String, serde_yaml::Value>,
    /// Explicit list of `{child, parent}` records.
    #[serde(default)]
    edges: Vec<EdgeRecord>,
}

#[derive(Debug, Deserialize)]
struct EdgeRecord {
    child: String,
    parent: String,
}

impl HierarchyDoc {
    fn into_edges(self) -> Result<Vec<TagEdge>, HierarchyError> {
        // Precedence: explicit `edges:` list overrides flat mapping.
        if !self.edges.is_empty() {
            let mut out = Vec::with_capacity(self.edges.len());
            for e in self.edges {
                out.push(make_edge(&e.child, &e.parent)?);
            }
            return Ok(out);
        }
        let mut out = Vec::with_capacity(self.flat.len());
        for (child, parent_val) in self.flat {
            // Flat shape only accepts scalar parent names.
            match parent_val {
                serde_yaml::Value::String(parent) => {
                    out.push(make_edge(&child, &parent)?);
                }
                other => {
                    return Err(HierarchyError::Yaml(serde::de::Error::custom(format!(
                        "expected string parent for `{child}`, got {:?}",
                        short_type(&other)
                    ))));
                }
            }
        }
        Ok(out)
    }
}

fn short_type(v: &serde_yaml::Value) -> &'static str {
    match v {
        serde_yaml::Value::Null => "null",
        serde_yaml::Value::Bool(_) => "bool",
        serde_yaml::Value::Number(_) => "number",
        serde_yaml::Value::String(_) => "string",
        serde_yaml::Value::Sequence(_) => "sequence",
        serde_yaml::Value::Mapping(_) => "mapping",
        serde_yaml::Value::Tagged(_) => "tagged",
    }
}

fn make_edge(raw_child: &str, raw_parent: &str) -> Result<TagEdge, HierarchyError> {
    let child = normalize_and_validate(raw_child)?;
    let parent = normalize_and_validate(raw_parent)?;
    Ok(TagEdge { child, parent })
}

fn normalize_and_validate(raw: &str) -> Result<String, HierarchyError> {
    let normalized = normalize_tag(raw);
    if normalized.is_empty() {
        return Err(HierarchyError::InvalidTagName {
            raw: raw.to_string(),
            reason: "empty after normalization",
        });
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_hierarchy(dir: &Path, body: &str) {
        fs::write(dir.join(HIERARCHY_FILENAME), body).unwrap();
    }

    #[test]
    fn absent_file_returns_absent() {
        let tmp = TempDir::new().unwrap();
        let out = parse_hierarchy(tmp.path()).unwrap();
        assert!(matches!(out, HierarchyOutcome::Absent));
    }

    #[test]
    fn flat_shape_parses() {
        let tmp = TempDir::new().unwrap();
        write_hierarchy(tmp.path(), "tdd: testing\nbdd: testing\ntesting: quality\n");
        let edges = match parse_hierarchy(tmp.path()).unwrap() {
            HierarchyOutcome::Loaded(e) => e,
            _ => panic!("expected Loaded"),
        };
        assert_eq!(edges.len(), 3);
        assert!(edges
            .iter()
            .any(|e| e.child == "tdd" && e.parent == "testing"));
        assert!(edges
            .iter()
            .any(|e| e.child == "testing" && e.parent == "quality"));
    }

    #[test]
    fn list_shape_parses() {
        let tmp = TempDir::new().unwrap();
        write_hierarchy(
            tmp.path(),
            "edges:\n  - child: tdd\n    parent: testing\n  - child: bdd\n    parent: testing\n",
        );
        let edges = match parse_hierarchy(tmp.path()).unwrap() {
            HierarchyOutcome::Loaded(e) => e,
            _ => panic!(),
        };
        assert_eq!(edges.len(), 2);
    }

    #[test]
    fn list_shape_wins_over_flat() {
        let tmp = TempDir::new().unwrap();
        write_hierarchy(
            tmp.path(),
            "tdd: quality\nedges:\n  - child: tdd\n    parent: testing\n",
        );
        let edges = match parse_hierarchy(tmp.path()).unwrap() {
            HierarchyOutcome::Loaded(e) => e,
            _ => panic!(),
        };
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].parent, "testing");
    }

    #[test]
    fn names_normalized_at_parse() {
        let tmp = TempDir::new().unwrap();
        // Mixed-case + underscores should be normalized to the fmem rule.
        write_hierarchy(tmp.path(), "TDD: Quality_Engineering\n");
        let edges = match parse_hierarchy(tmp.path()).unwrap() {
            HierarchyOutcome::Loaded(e) => e,
            _ => panic!(),
        };
        assert_eq!(edges[0].child, "tdd");
        assert_eq!(edges[0].parent, "quality-engineering");
    }

    #[test]
    fn oversized_file_rejected() {
        let tmp = TempDir::new().unwrap();
        let mut body = String::new();
        while body.len() <= MAX_HIERARCHY_BYTES {
            body.push_str("a-really-long-child-tag-name: parent-tag\n");
        }
        write_hierarchy(tmp.path(), &body);
        let err = parse_hierarchy(tmp.path()).unwrap_err();
        assert!(matches!(err, HierarchyError::TooLarge { .. }));
    }

    #[test]
    fn self_loop_rejected() {
        let tmp = TempDir::new().unwrap();
        write_hierarchy(tmp.path(), "loop: loop\n");
        let err = parse_hierarchy(tmp.path()).unwrap_err();
        assert!(matches!(err, HierarchyError::SelfLoop { .. }));
    }

    #[test]
    fn empty_tag_name_rejected() {
        let tmp = TempDir::new().unwrap();
        // "!!!" normalizes to "".
        write_hierarchy(
            tmp.path(),
            "edges:\n  - child: '!!!'\n    parent: quality\n",
        );
        let err = parse_hierarchy(tmp.path()).unwrap_err();
        assert!(matches!(err, HierarchyError::InvalidTagName { .. }));
    }

    #[test]
    fn yaml_syntax_error_reported() {
        let tmp = TempDir::new().unwrap();
        write_hierarchy(tmp.path(), "edges: [unclosed\n");
        let err = parse_hierarchy(tmp.path()).unwrap_err();
        assert!(matches!(err, HierarchyError::Yaml(_)));
    }
}
