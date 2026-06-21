//! Mermaid diagram validator.
//!
//! Lints Mermaid source text for common syntactic errors before it gets
//! written to a spec file: unknown diagram types, unbalanced brackets, bad
//! edge syntax, undeclared sequence participants, unterminated edges, and
//! accidentally included ```mermaid fences. No runtime dependency on the
//! mermaid JavaScript library — pure state machine + regex.

use regex::Regex;
use serde::Serialize;

// ── Public types ────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ValidationReport {
    pub valid: bool,
    pub diagram_type: Option<String>,
    pub errors: Vec<Issue>,
    pub warnings: Vec<Issue>,
}

#[derive(Debug, Serialize)]
pub struct Issue {
    pub line: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<usize>,
    pub code: String,
    pub message: String,
}

impl Issue {
    fn err(line: usize, column: Option<usize>, code: &str, message: impl Into<String>) -> Self {
        Issue {
            line,
            column,
            code: code.to_string(),
            message: message.into(),
        }
    }
}

// ── Known diagram types ─────────────────────────────────────────────────────

const KNOWN_TYPES: &[&str] = &[
    "graph",
    "flowchart",
    "sequenceDiagram",
    "stateDiagram",
    "stateDiagram-v2",
    "classDiagram",
    "erDiagram",
    "gantt",
    "pie",
    "journey",
    "gitGraph",
    "mindmap",
    "timeline",
    "quadrantChart",
    "requirementDiagram",
    "C4Context",
    "C4Container",
    "C4Component",
    "C4Dynamic",
];

// ── Entry point ─────────────────────────────────────────────────────────────

/// Validate a Mermaid diagram source. Never returns `Err` — all findings
/// surface through `ValidationReport`.
pub fn validate(input: &str) -> ValidationReport {
    let mut errors: Vec<Issue> = Vec::new();
    let mut warnings: Vec<Issue> = Vec::new();

    // Check 7: code fence present?
    let trimmed_first = input.lines().next().map(|l| l.trim()).unwrap_or("");
    if trimmed_first.starts_with("```mermaid") || trimmed_first == "```" {
        warnings.push(Issue::err(
            1,
            None,
            "FENCE_INCLUDED",
            "input appears to include ```mermaid fence; pass raw content",
        ));
    }

    // Check 1: diagram type
    let (diagram_type, type_line_idx) = detect_diagram_type(input);
    match &diagram_type {
        Some(t) if KNOWN_TYPES.contains(&t.as_str()) => {}
        Some(t) => {
            errors.push(Issue::err(
                type_line_idx + 1,
                None,
                "UNKNOWN_DIAGRAM_TYPE",
                format!("unknown diagram type '{}'", t),
            ));
        }
        None => {
            errors.push(Issue::err(
                1,
                None,
                "NO_DIAGRAM_TYPE",
                "could not determine diagram type (no non-empty, non-comment line found)",
            ));
        }
    }

    // Check 2: bracket balance
    check_brackets(input, &mut errors);

    // Check 6: unterminated edges (applies broadly; most relevant in flowcharts)
    check_unterminated_edges(input, &mut errors);

    // Type-specific checks
    let dtype_str = diagram_type.as_deref().unwrap_or("");
    if dtype_str == "flowchart" || dtype_str == "graph" {
        check_flowchart_edges(input, &mut errors);
        check_node_ids(input, &mut errors);
    } else if dtype_str == "sequenceDiagram" {
        check_sequence_participants(input, &mut errors);
    }

    ValidationReport {
        valid: errors.is_empty(),
        diagram_type,
        errors,
        warnings,
    }
}

// ── Diagram type detection ──────────────────────────────────────────────────

fn detect_diagram_type(input: &str) -> (Option<String>, usize) {
    for (idx, raw) in input.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("%%") {
            continue;
        }
        if line.starts_with("```") {
            continue;
        }
        // First token is the diagram type (possibly with a direction like
        // `flowchart TD` or `graph LR`).
        let first = line.split_whitespace().next().unwrap_or("");
        return (Some(first.to_string()), idx);
    }
    (None, 0)
}

// ── Bracket balance ─────────────────────────────────────────────────────────

fn check_brackets(input: &str, errors: &mut Vec<Issue>) {
    // Track single-char brackets. Report first unbalanced occurrence.
    let mut stack: Vec<(char, usize, usize)> = Vec::new();
    let mut in_string = false;
    let mut string_quote = '"';

    for (line_idx, line) in input.lines().enumerate() {
        // Skip comment lines
        let trimmed = line.trim_start();
        if trimmed.starts_with("%%") {
            continue;
        }
        for (col_idx, ch) in line.char_indices() {
            if in_string {
                if ch == string_quote {
                    in_string = false;
                }
                continue;
            }
            match ch {
                '"' | '\'' => {
                    in_string = true;
                    string_quote = ch;
                }
                '(' | '[' | '{' => {
                    stack.push((ch, line_idx + 1, col_idx + 1));
                }
                ')' | ']' | '}' => {
                    let expected_open = match ch {
                        ')' => '(',
                        ']' => '[',
                        '}' => '{',
                        _ => unreachable!(),
                    };
                    match stack.pop() {
                        Some((open, _, _)) if open == expected_open => {}
                        Some((open, ol, oc)) => {
                            errors.push(Issue::err(
                                line_idx + 1,
                                Some(col_idx + 1),
                                "UNBALANCED_BRACKET",
                                format!(
                                    "mismatched '{}' (expected to close '{}' from line {} col {})",
                                    ch, open, ol, oc
                                ),
                            ));
                            return;
                        }
                        None => {
                            errors.push(Issue::err(
                                line_idx + 1,
                                Some(col_idx + 1),
                                "UNBALANCED_BRACKET",
                                format!("unmatched closing '{}'", ch),
                            ));
                            return;
                        }
                    }
                }
                _ => {}
            }
        }
        // Strings do not span lines in Mermaid — reset.
        in_string = false;
    }
    if let Some((open, line, col)) = stack.first().copied() {
        errors.push(Issue::err(
            line,
            Some(col),
            "UNBALANCED_BRACKET",
            format!("unmatched '{}'", open),
        ));
    }
}

// ── Flowchart edge syntax ───────────────────────────────────────────────────

fn check_flowchart_edges(input: &str, errors: &mut Vec<Issue>) {
    // Detect bad arrows like `A->B` (no space) or `A ---> B` (>3 dashes).
    // We look for a dash run followed by `>` and verify the shape.
    let bad_compact: Regex = Regex::new(r"[A-Za-z0-9_\]\)]->[A-Za-z0-9_\[\(]").unwrap();
    let long_arrow: Regex = Regex::new(r"-{4,}>").unwrap();

    for (idx, raw) in input.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("%%") {
            continue;
        }
        if bad_compact.is_match(raw) {
            errors.push(Issue::err(
                idx + 1,
                None,
                "BAD_EDGE_SYNTAX",
                "edge arrow '->' must have surrounding spaces (e.g. 'A --> B')",
            ));
        }
        if long_arrow.is_match(raw) {
            errors.push(Issue::err(
                idx + 1,
                None,
                "BAD_EDGE_SYNTAX",
                "arrow has too many dashes; use '-->' not '---->' etc.",
            ));
        }
    }
}

// ── Node ID charset ─────────────────────────────────────────────────────────

fn check_node_ids(input: &str, errors: &mut Vec<Issue>) {
    // For each edge like `A --> B` in flowchart, both endpoints must be valid
    // IDs unless quoted. We extract probable identifier tokens adjacent to
    // arrow operators and check them.
    let id_re: Regex = Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").unwrap();
    let edge_re: Regex =
        Regex::new(r"(?P<lhs>\S+)\s*(?:-->|---|-\.->|==>|===)\s*(?P<rhs>\S+)").unwrap();

    for (idx, raw) in input.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("%%") {
            continue;
        }
        // Skip direction declarations / subgraph lines
        if line.starts_with("subgraph") || line.starts_with("end") {
            continue;
        }
        for caps in edge_re.captures_iter(line) {
            for name in ["lhs", "rhs"] {
                let token = caps.name(name).unwrap().as_str();
                // Strip a trailing shape suffix e.g. `A[Label]` or `B(Text)` — take
                // the leading identifier portion.
                let id_part: String = token
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                if id_part.is_empty() {
                    // Quoted or structural — let bracket checker handle it.
                    continue;
                }
                if !id_re.is_match(&id_part) {
                    errors.push(Issue::err(
                        idx + 1,
                        None,
                        "BAD_NODE_ID",
                        format!("invalid node id '{}'", id_part),
                    ));
                }
            }
        }
    }
}

// ── Sequence diagram participants ───────────────────────────────────────────

fn check_sequence_participants(input: &str, errors: &mut Vec<Issue>) {
    let decl_re: Regex =
        Regex::new(r"^\s*(?:participant|actor)\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)").unwrap();
    // Message arrows in sequence diagrams: ->>, -->>, ->, -->, -x, --x, -)
    let msg_re: Regex = Regex::new(
        r"^\s*(?P<from>[A-Za-z_][A-Za-z0-9_]*)\s*(?:->>|-->>|->|-->|-x|--x|-\))\s*(?P<to>[A-Za-z_][A-Za-z0-9_]*)",
    )
    .unwrap();

    let mut declared: Vec<String> = Vec::new();

    // First pass: collect declarations.
    for raw in input.lines() {
        if let Some(caps) = decl_re.captures(raw) {
            declared.push(caps.name("name").unwrap().as_str().to_string());
        }
    }

    // Second pass: verify message endpoints (auto-declare if first mention).
    // Spec says "messages must reference declared participants" — we treat
    // an endpoint that never appears in a declaration AND is not auto-used
    // elsewhere as an error if ANY participants are declared. If NO
    // participants are declared, Mermaid auto-creates them, so skip the
    // check to avoid false positives.
    if declared.is_empty() {
        return;
    }

    for (idx, raw) in input.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty()
            || line.starts_with("%%")
            || line.starts_with("participant")
            || line.starts_with("actor")
        {
            continue;
        }
        if let Some(caps) = msg_re.captures(raw) {
            for name in ["from", "to"] {
                let ident = caps.name(name).unwrap().as_str().to_string();
                if !declared.iter().any(|d| d == &ident) {
                    errors.push(Issue::err(
                        idx + 1,
                        None,
                        "UNKNOWN_PARTICIPANT",
                        format!("message references undeclared '{}'", ident),
                    ));
                }
            }
        }
    }
}

// ── Unterminated edges ──────────────────────────────────────────────────────

fn check_unterminated_edges(input: &str, errors: &mut Vec<Issue>) {
    let trail_re: Regex = Regex::new(r"(-->|---)\s*$").unwrap();
    for (idx, raw) in input.lines().enumerate() {
        let line = raw.trim_end();
        if line.is_empty() {
            continue;
        }
        if trail_re.is_match(line) {
            errors.push(Issue::err(
                idx + 1,
                None,
                "UNTERMINATED_EDGE",
                "line ends with an edge operator without a target",
            ));
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_flowchart() {
        let src = "flowchart TD\n    A[Start] --> B[End]\n";
        let report = validate(src);
        assert!(report.valid, "expected valid, got {:?}", report.errors);
        assert_eq!(report.diagram_type.as_deref(), Some("flowchart"));
    }

    #[test]
    fn valid_sequence() {
        let src = "sequenceDiagram\n    participant A\n    participant B\n    A->>B: hi\n";
        let report = validate(src);
        assert!(report.valid, "expected valid, got {:?}", report.errors);
    }

    #[test]
    fn unknown_diagram_type() {
        let src = "flowcart TD\n    A --> B\n";
        let report = validate(src);
        assert!(!report.valid);
        assert!(report
            .errors
            .iter()
            .any(|e| e.code == "UNKNOWN_DIAGRAM_TYPE"));
    }

    #[test]
    fn missing_diagram_type() {
        let src = "\n%% just a comment\n";
        let report = validate(src);
        assert!(!report.valid);
        assert!(report.errors.iter().any(|e| e.code == "NO_DIAGRAM_TYPE"));
    }

    #[test]
    fn unbalanced_bracket() {
        let src = "flowchart TD\n    A[Start --> B\n";
        let report = validate(src);
        assert!(!report.valid);
        assert!(report.errors.iter().any(|e| e.code == "UNBALANCED_BRACKET"));
    }

    #[test]
    fn balanced_brackets_with_string() {
        let src = "flowchart TD\n    A[\"text with ] inside\"] --> B\n";
        let report = validate(src);
        assert!(report.valid, "got errors: {:?}", report.errors);
    }

    #[test]
    fn bad_edge_compact() {
        let src = "flowchart TD\n    A->B\n";
        let report = validate(src);
        assert!(!report.valid);
        assert!(report.errors.iter().any(|e| e.code == "BAD_EDGE_SYNTAX"));
    }

    #[test]
    fn bad_edge_too_long() {
        let src = "flowchart TD\n    A ----> B\n";
        let report = validate(src);
        assert!(!report.valid);
        assert!(report.errors.iter().any(|e| e.code == "BAD_EDGE_SYNTAX"));
    }

    #[test]
    fn valid_dotted_edge() {
        let src = "flowchart TD\n    A -.-> B\n";
        let report = validate(src);
        assert!(report.valid, "got errors: {:?}", report.errors);
    }

    #[test]
    fn bad_node_id() {
        let src = "flowchart TD\n    1A --> B\n";
        let report = validate(src);
        assert!(!report.valid);
        assert!(report.errors.iter().any(|e| e.code == "BAD_NODE_ID"));
    }

    #[test]
    fn unknown_participant() {
        let src = "sequenceDiagram\n    participant A\n    participant B\n    A->>C: hello\n";
        let report = validate(src);
        assert!(!report.valid);
        assert!(report
            .errors
            .iter()
            .any(|e| e.code == "UNKNOWN_PARTICIPANT"));
    }

    #[test]
    fn sequence_auto_participants_ok() {
        // No `participant` declarations: Mermaid auto-creates, we should not error.
        let src = "sequenceDiagram\n    A->>B: hi\n";
        let report = validate(src);
        assert!(report.valid, "got errors: {:?}", report.errors);
    }

    #[test]
    fn unterminated_edge() {
        let src = "flowchart TD\n    A -->\n";
        let report = validate(src);
        assert!(!report.valid);
        assert!(report.errors.iter().any(|e| e.code == "UNTERMINATED_EDGE"));
    }

    #[test]
    fn fence_included_warning() {
        let src = "```mermaid\nflowchart TD\n    A --> B\n```\n";
        let report = validate(src);
        assert!(report.warnings.iter().any(|w| w.code == "FENCE_INCLUDED"));
    }

    #[test]
    fn json_output_round_trips() {
        let src = "flowchart TD\n    A --> B\n";
        let report = validate(src);
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("\"valid\":true"));
    }
}
