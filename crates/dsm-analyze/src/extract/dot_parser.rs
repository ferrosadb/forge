use crate::extract::{Edge, EdgeKind};
use anyhow::Result;
use regex::Regex;

/// Parse a DOT format string into edges.
/// Supports both `digraph` and plain edge lists.
/// Handles jdeps multi-archive format (multiple digraphs per file).
pub fn parse_dot(input: &str) -> Result<Vec<Edge>> {
    let mut edges = Vec::new();
    let edge_re = Regex::new(r#""([^"]+)"\s*->\s*"([^"]+)""#)?;
    // Also handle unquoted: a -> b
    let edge_unquoted_re = Regex::new(r#"(\S+)\s*->\s*(\S+)\s*[;\[]?"#)?;

    for line in input.lines() {
        let line = line.trim();
        // Skip graph declarations, closing braces, comments
        if line.starts_with("digraph")
            || line.starts_with("graph")
            || line.starts_with("//")
            || line.starts_with('#')
            || line == "{"
            || line == "}"
            || line.is_empty()
        {
            continue;
        }

        // Try quoted edge first
        if let Some(cap) = edge_re.captures(line) {
            let source = cap[1].trim().to_string();
            let target = cap[2].trim().to_string();
            if source != target {
                edges.push(Edge {
                    source,
                    target,
                    weight: 1.0,
                    kind: EdgeKind::Import,
                    cross_language: None,
                });
            }
            continue;
        }

        // Try unquoted edge
        if let Some(cap) = edge_unquoted_re.captures(line) {
            let source = cap[1]
                .trim_matches(|c: char| c == '"' || c == ';')
                .to_string();
            let target = cap[2]
                .trim_matches(|c: char| c == '"' || c == ';')
                .to_string();
            if source != target && !source.is_empty() && !target.is_empty() {
                edges.push(Edge {
                    source,
                    target,
                    weight: 1.0,
                    kind: EdgeKind::Import,
                    cross_language: None,
                });
            }
        }
    }

    Ok(edges)
}

/// Parse a plain edge list (one "source target" per line, tab or space separated).
pub fn parse_edge_list(input: &str) -> Result<Vec<Edge>> {
    let mut edges = Vec::new();
    for line in input.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 {
            let source = parts[0].to_string();
            let target = parts[1].to_string();
            let weight = if parts.len() >= 3 {
                parts[2].parse::<f64>().unwrap_or(1.0)
            } else {
                1.0
            };
            if source != target {
                edges.push(Edge {
                    source,
                    target,
                    weight,
                    kind: EdgeKind::Import,
                    cross_language: None,
                });
            }
        }
    }
    Ok(edges)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_digraph() {
        let dot = r#"
digraph "myproject.jar" {
    "com.example.db"      -> "com.example.io";
    "com.example.service"  -> "com.example.db";
    "com.example.service"  -> "com.example.net";
}
"#;
        let edges = parse_dot(dot).unwrap();
        assert_eq!(edges.len(), 3);
        assert_eq!(edges[0].source, "com.example.db");
        assert_eq!(edges[0].target, "com.example.io");
    }

    #[test]
    fn parse_multi_digraph() {
        let dot = r#"
digraph "a.jar" {
    "com.a" -> "com.b";
}
digraph "b.jar" {
    "com.c" -> "com.d";
}
"#;
        let edges = parse_dot(dot).unwrap();
        assert_eq!(edges.len(), 2);
    }

    #[test]
    fn parse_self_loops_excluded() {
        let dot = r#"
digraph "test" {
    "com.a" -> "com.a";
    "com.a" -> "com.b";
}
"#;
        let edges = parse_dot(dot).unwrap();
        assert_eq!(edges.len(), 1);
    }

    #[test]
    fn parse_unquoted_edges() {
        let dot = r#"
digraph test {
    a -> b;
    b -> c;
}
"#;
        let edges = parse_dot(dot).unwrap();
        assert_eq!(edges.len(), 2);
    }

    #[test]
    fn parse_edge_list_basic() {
        let input = "a b\nb c 2.5\n# comment\n";
        let edges = parse_edge_list(input).unwrap();
        assert_eq!(edges.len(), 2);
        assert_eq!(edges[1].weight, 2.5);
    }

    #[test]
    fn parse_empty_input() {
        let edges = parse_dot("").unwrap();
        assert!(edges.is_empty());
    }

    #[test]
    fn parse_comments_and_blanks() {
        let dot = "// comment\n\n# another\ndigraph x {\n}\n";
        let edges = parse_dot(dot).unwrap();
        assert!(edges.is_empty());
    }
}
