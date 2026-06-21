//! Parse a SKILL.md byte buffer into a typed `Skill` struct.
//!
//! Handles YAML frontmatter, markdown body, and "Instructions" / "Steps"
//! section extraction. Enforces:
//!
//! - 2 MiB per-file size cap (threat-model D1, FMEA F8)
//! - Valid UTF-8 (non-UTF-8 → fail; never silently lossy-decode)
//! - Required frontmatter fields `name` + `description` (FMEA F4, F5)
//! - Tag normalization at parse time per locked design choice 3
//!   (`_` → `-`, lowercase, collapse runs of `-` — matches fmem's
//!   `normalize_tag` in skill.rs:85)

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Cap on per-file size. Anything above is rejected (FMEA F8).
pub const MAX_FILE_SIZE_BYTES: usize = 2 * 1024 * 1024;

/// A parsed SKILL.md, ready for hashing and ingest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skill {
    /// Skill identifier — opaque slug from frontmatter `name`.
    pub name: String,
    /// Top-level category (passed in from the walker; not from frontmatter).
    pub category: String,
    /// Description from frontmatter.
    pub description: String,
    /// Optional explicit `argument-hint` from frontmatter.
    pub argument_hint: Option<String>,
    /// Trigger keywords — explicit `keywords:` from frontmatter, or
    /// derived heuristically from `description` (P10 / future work).
    pub trigger_keywords: Vec<String>,
    /// Tags from frontmatter `tags:`. Already normalized at parse time.
    pub tags: Vec<String>,
    /// Names of other skills this one requires.
    pub prerequisites: Vec<String>,
    /// Names of related skills (from `related:` frontmatter).
    pub related: Vec<String>,
    /// Output artifacts produced by this skill.
    pub output_artifacts: Vec<String>,
    /// Filenames of supplementary files referenced from frontmatter.
    pub supplementary_files: Vec<String>,
    /// Parsed steps from the body.
    pub steps: Vec<Step>,
    /// Raw frontmatter YAML bytes, captured for hashing.
    pub frontmatter_bytes: Vec<u8>,
    /// Raw body markdown bytes, captured for hashing.
    pub body_bytes: Vec<u8>,
    /// Whether step parsing produced no steps. Drives an empty-steps
    /// warning at the orchestrator (FMEA F9, WI-FMEA-02).
    pub steps_empty: bool,
}

/// One step from `## Instructions` / `## Steps` / `### Step N:`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    /// Heading the step lived under (e.g. "Step 1: Build the Test List").
    /// `None` if the step came from a flat list under `## Instructions`.
    pub phase: Option<String>,
    /// The step text.
    pub instruction: String,
}

#[derive(Debug)]
pub enum ParseError {
    /// File exceeds `MAX_FILE_SIZE_BYTES`.
    TooLarge { actual: usize, max: usize },
    /// File is not valid UTF-8.
    NotUtf8,
    /// No `---` frontmatter delimiter found.
    NoFrontmatter,
    /// Unterminated frontmatter — only an opening `---`.
    UnterminatedFrontmatter,
    /// YAML parse error inside frontmatter.
    Yaml(serde_yaml::Error),
    /// Required field missing from frontmatter.
    MissingField(&'static str),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge { actual, max } => {
                write!(f, "SKILL.md too large: {actual} bytes (max {max})")
            }
            Self::NotUtf8 => write!(f, "SKILL.md is not valid UTF-8"),
            Self::NoFrontmatter => write!(f, "SKILL.md has no `---` frontmatter block"),
            Self::UnterminatedFrontmatter => {
                write!(f, "SKILL.md frontmatter is unterminated (no closing `---`)")
            }
            Self::Yaml(e) => write!(f, "SKILL.md frontmatter YAML error: {e}"),
            Self::MissingField(field) => {
                write!(f, "SKILL.md frontmatter missing required field: {field}")
            }
        }
    }
}

impl std::error::Error for ParseError {}

/// `argument-hint` accepts either a plain string (`<feature>`) or a YAML
/// flow sequence (`[command]`). Some skill files use the bracket form
/// because that's how the value should display in `/help` output;
/// YAML parses it as a sequence. Either way we keep a string.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ArgumentHint {
    Single(String),
    Many(Vec<String>),
}

impl ArgumentHint {
    fn into_string(self) -> String {
        match self {
            Self::Single(s) => s,
            Self::Many(v) => {
                let inner = v.join(" ");
                format!("[{inner}]")
            }
        }
    }
}

/// Frontmatter shape — extra fields are tolerated and ignored.
#[derive(Debug, Default, Deserialize)]
struct Frontmatter {
    name: Option<String>,
    description: Option<String>,
    #[serde(rename = "argument-hint")]
    argument_hint: Option<ArgumentHint>,
    #[serde(default)]
    keywords: Vec<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    prerequisites: Vec<String>,
    #[serde(default)]
    related: Vec<String>,
    #[serde(default)]
    output_artifacts: Vec<String>,
    #[serde(default, rename = "supplementary-files")]
    supplementary_files: Vec<String>,
    /// Capture anything else so unknown fields don't fail parsing.
    #[serde(flatten)]
    _extra: BTreeMap<String, serde_yaml::Value>,
}

/// Parse a SKILL.md byte buffer into a `Skill`.
///
/// `category` comes from the walker (the immediate child of the skill
/// root); the parser does not derive it from the file path.
pub fn parse(bytes: &[u8], category: &str) -> Result<Skill, ParseError> {
    if bytes.len() > MAX_FILE_SIZE_BYTES {
        return Err(ParseError::TooLarge {
            actual: bytes.len(),
            max: MAX_FILE_SIZE_BYTES,
        });
    }
    let text = std::str::from_utf8(bytes).map_err(|_| ParseError::NotUtf8)?;

    let (fm_str, body_str) = split_frontmatter(text)?;
    let fm: Frontmatter = serde_yaml::from_str(fm_str).map_err(ParseError::Yaml)?;

    let name = fm.name.ok_or(ParseError::MissingField("name"))?;
    let description = fm
        .description
        .ok_or(ParseError::MissingField("description"))?;

    let trigger_keywords = if fm.keywords.is_empty() {
        derive_keywords(&description)
    } else {
        fm.keywords.into_iter().map(|k| normalize_tag(&k)).collect()
    };

    let tags: Vec<String> = fm.tags.iter().map(|t| normalize_tag(t)).collect();

    let steps = parse_steps(body_str);
    let steps_empty = steps.is_empty();

    Ok(Skill {
        name,
        category: category.to_string(),
        description,
        argument_hint: fm.argument_hint.map(ArgumentHint::into_string),
        trigger_keywords,
        tags,
        prerequisites: fm.prerequisites,
        related: fm.related,
        output_artifacts: fm.output_artifacts,
        supplementary_files: fm.supplementary_files,
        steps,
        frontmatter_bytes: fm_str.as_bytes().to_vec(),
        body_bytes: body_str.as_bytes().to_vec(),
        steps_empty,
    })
}

/// Apply fmem's tag normalization rule (skill.rs:85): lowercase ASCII
/// alphanumerics survive; everything else becomes `-`; runs of `-` are
/// collapsed; leading/trailing `-` trimmed. Matches the locked design
/// choice — `_` → `-`, lowercase, on the way in.
pub fn normalize_tag(raw: &str) -> String {
    raw.trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Split a SKILL.md text into `(frontmatter_yaml_str, body_md_str)`.
///
/// Expects the file to start with `---\n`, then a block of YAML, then
/// a closing `---\n`. Returns the YAML body without the delimiters and
/// the markdown body that follows.
fn split_frontmatter(text: &str) -> Result<(&str, &str), ParseError> {
    // Allow an optional UTF-8 BOM and leading blank lines before the
    // opening `---`.
    let trimmed_start = text.trim_start_matches('\u{feff}');
    let after_lead = trimmed_start.trim_start_matches([' ', '\t', '\n']);

    let rest = after_lead
        .strip_prefix("---\n")
        .or_else(|| after_lead.strip_prefix("---\r\n"))
        .ok_or(ParseError::NoFrontmatter)?;

    // Find the next standalone `---` line.
    let close_idx = find_closing_delim(rest).ok_or(ParseError::UnterminatedFrontmatter)?;
    let fm = &rest[..close_idx];
    let after_close = &rest[close_idx..];
    // Skip the closing delimiter line itself.
    let body = match after_close.find('\n') {
        Some(n) => &after_close[n + 1..],
        None => "",
    };
    Ok((fm, body))
}

fn find_closing_delim(rest: &str) -> Option<usize> {
    let mut search_from = 0;
    while let Some(pos) = rest[search_from..].find("---") {
        let abs = search_from + pos;
        let at_line_start = abs == 0 || rest.as_bytes()[abs - 1] == b'\n';
        let after = &rest[abs + 3..];
        let line_terminated =
            after.is_empty() || after.starts_with('\n') || after.starts_with("\r\n");
        if at_line_start && line_terminated {
            return Some(abs);
        }
        search_from = abs + 3;
    }
    None
}

/// Heuristic keyword extraction from the description. Splits on
/// whitespace + punctuation, lowercases, drops a small stoplist, and
/// dedupes preserving order. Used when frontmatter has no explicit
/// `keywords:`.
fn derive_keywords(description: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "the", "a", "an", "of", "to", "for", "in", "on", "and", "or", "with", "by", "from", "is",
        "are", "be", "this", "that", "it", "as", "at", "use", "uses", "using", "when", "via",
        "into", "out", "over", "your",
    ];
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for raw in description.split(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_') {
        if raw.is_empty() {
            continue;
        }
        let k = normalize_tag(raw);
        if k.is_empty() || STOP.contains(&k.as_str()) {
            continue;
        }
        if seen.insert(k.clone()) {
            out.push(k);
        }
    }
    out
}

/// Walk the body markdown looking for step sections.
///
/// Recognized headings:
/// - `## Instructions` — flat list of steps; each list item is one step
/// - `## Steps` — same shape
/// - `### Step N: Title` — each such heading begins one phased step,
///   with the step text being the heading + the body up to the next heading
///
/// Returns an empty vec if no recognized section is present (caller
/// emits the empty-steps warning per FMEA F9).
fn parse_steps(body: &str) -> Vec<Step> {
    let lines: Vec<&str> = body.lines().collect();

    // First try `### Step N:` style — produces phased steps.
    let phased: Vec<Step> = parse_phased_steps(&lines);
    if !phased.is_empty() {
        return phased;
    }

    // Fall back to a flat `## Instructions` / `## Steps` list.
    parse_flat_steps(&lines)
}

fn parse_phased_steps(lines: &[&str]) -> Vec<Step> {
    let mut steps = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if let Some(heading) = strip_phase_heading(line) {
            // Collect the body until the next `### `, `## `, or `# ` heading.
            let mut body = String::new();
            let mut j = i + 1;
            while j < lines.len() {
                let l = lines[j];
                if l.starts_with("# ") || l.starts_with("## ") || l.starts_with("### ") {
                    break;
                }
                if !body.is_empty() {
                    body.push('\n');
                }
                body.push_str(l);
                j += 1;
            }
            steps.push(Step {
                phase: Some(heading),
                instruction: body.trim().to_string(),
            });
            i = j;
            continue;
        }
        i += 1;
    }
    steps
}

fn strip_phase_heading(line: &str) -> Option<String> {
    let rest = line.strip_prefix("### ")?;
    // Accept "Step N", "Step N:", "Step N: ..." (case-sensitive on
    // "Step", per spec).
    if !rest.starts_with("Step ") {
        return None;
    }
    Some(rest.to_string())
}

fn parse_flat_steps(lines: &[&str]) -> Vec<Step> {
    let mut in_section = false;
    let mut steps = Vec::new();
    let mut current: Option<String> = None;

    for line in lines {
        if let Some(heading) = line.strip_prefix("## ") {
            in_section = matches!(heading, "Instructions" | "Steps");
            // Flush any in-progress step before changing section.
            if let Some(s) = current.take() {
                push_flat_step(&mut steps, s);
            }
            continue;
        }
        if line.starts_with('#') {
            // Some other heading — bail.
            in_section = false;
            if let Some(s) = current.take() {
                push_flat_step(&mut steps, s);
            }
            continue;
        }
        if !in_section {
            continue;
        }

        let trimmed = line.trim_start();
        if trimmed.starts_with("- ") || trimmed.starts_with("* ") || is_numbered_list_item(trimmed)
        {
            // Flush previous, start new.
            if let Some(s) = current.take() {
                push_flat_step(&mut steps, s);
            }
            current = Some(strip_list_marker(trimmed).to_string());
        } else if trimmed.is_empty() {
            // Blank line — terminate the current step, but stay in section.
            if let Some(s) = current.take() {
                push_flat_step(&mut steps, s);
            }
        } else if let Some(curr) = current.as_mut() {
            // Continuation of the current step (indented or wrapped).
            curr.push(' ');
            curr.push_str(trimmed);
        }
    }

    if let Some(s) = current {
        push_flat_step(&mut steps, s);
    }
    steps
}

fn push_flat_step(steps: &mut Vec<Step>, instruction: String) {
    let cleaned = instruction.trim().to_string();
    if !cleaned.is_empty() {
        steps.push(Step {
            phase: None,
            instruction: cleaned,
        });
    }
}

fn is_numbered_list_item(s: &str) -> bool {
    let mut chars = s.chars();
    let mut saw_digit = false;
    for c in chars.by_ref() {
        if c.is_ascii_digit() {
            saw_digit = true;
        } else if c == '.' && saw_digit {
            return chars.next() == Some(' ');
        } else {
            return false;
        }
    }
    false
}

fn strip_list_marker(s: &str) -> &str {
    if let Some(rest) = s.strip_prefix("- ") {
        return rest;
    }
    if let Some(rest) = s.strip_prefix("* ") {
        return rest;
    }
    // Numbered: skip digits, then `. `.
    let mut it = s.char_indices();
    let mut last = 0;
    for (i, c) in it.by_ref() {
        if c.is_ascii_digit() {
            last = i + 1;
        } else if c == '.' {
            last = i + 1;
            break;
        } else {
            return s;
        }
    }
    if let Some((i, c)) = it.next() {
        if c == ' ' {
            return &s[i + 1..];
        }
    }
    &s[last..]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill_with(frontmatter: &str, body: &str) -> Result<Skill, ParseError> {
        let mut s = String::new();
        s.push_str("---\n");
        s.push_str(frontmatter);
        s.push_str("---\n");
        s.push_str(body);
        parse(s.as_bytes(), "task-level")
    }

    #[test]
    fn rejects_oversize_file() {
        let big = vec![b'a'; MAX_FILE_SIZE_BYTES + 1];
        let err = parse(&big, "x").unwrap_err();
        assert!(matches!(err, ParseError::TooLarge { .. }));
    }

    #[test]
    fn rejects_non_utf8() {
        let bytes = vec![0xff, 0xfe, 0xfd, b'a'];
        let err = parse(&bytes, "x").unwrap_err();
        assert!(matches!(err, ParseError::NotUtf8));
    }

    #[test]
    fn requires_frontmatter_block() {
        let err = parse(b"# just markdown\n\nno frontmatter here", "x").unwrap_err();
        assert!(matches!(err, ParseError::NoFrontmatter));
    }

    #[test]
    fn requires_closed_frontmatter() {
        let err = parse(b"---\nname: x\n", "x").unwrap_err();
        assert!(matches!(err, ParseError::UnterminatedFrontmatter));
    }

    #[test]
    fn requires_name() {
        let err = skill_with("description: foo\n", "body").unwrap_err();
        assert!(matches!(err, ParseError::MissingField("name")));
    }

    #[test]
    fn requires_description() {
        let err = skill_with("name: foo\n", "body").unwrap_err();
        assert!(matches!(err, ParseError::MissingField("description")));
    }

    #[test]
    fn parses_minimal_skill() {
        let s = skill_with("name: tdd\ndescription: do tdd\n", "body").unwrap();
        assert_eq!(s.name, "tdd");
        assert_eq!(s.category, "task-level");
        assert_eq!(s.description, "do tdd");
        assert_eq!(s.frontmatter_bytes, b"name: tdd\ndescription: do tdd\n");
    }

    #[test]
    fn yaml_error_reports_with_context() {
        let bad = "---\nname: [unclosed\n---\nbody";
        let err = parse(bad.as_bytes(), "x").unwrap_err();
        assert!(matches!(err, ParseError::Yaml(_)));
    }

    #[test]
    fn keywords_explicit_overrides_derivation() {
        let s = skill_with(
            "name: x\ndescription: ignored description\nkeywords: [a, B_c]\n",
            "body",
        )
        .unwrap();
        // Both normalized: "B_c" → "b-c"
        assert_eq!(s.trigger_keywords, vec!["a".to_string(), "b-c".to_string()]);
    }

    #[test]
    fn keywords_derived_from_description_when_absent() {
        let s = skill_with(
            "name: x\ndescription: Test-driven development with red green refactor\n",
            "body",
        )
        .unwrap();
        assert!(s.trigger_keywords.contains(&"test-driven".to_string()));
        assert!(s.trigger_keywords.contains(&"development".to_string()));
        assert!(s.trigger_keywords.contains(&"red".to_string()));
        // Stoplist drops "with"
        assert!(!s.trigger_keywords.contains(&"with".to_string()));
    }

    #[test]
    fn tags_normalized_at_parse() {
        let s = skill_with(
            "name: x\ndescription: y\ntags: [Quality_Engineering, TDD, web/security]\n",
            "x",
        )
        .unwrap();
        assert_eq!(s.tags, vec!["quality-engineering", "tdd", "web-security"]);
    }

    #[test]
    fn normalize_tag_collapses_runs_and_lowercases() {
        assert_eq!(normalize_tag("Quality_Engineering"), "quality-engineering");
        assert_eq!(normalize_tag("foo__bar"), "foo-bar");
        assert_eq!(normalize_tag("--lead--mid--trail--"), "lead-mid-trail");
        assert_eq!(normalize_tag("UPPER"), "upper");
        assert_eq!(normalize_tag("a b c"), "a-b-c");
        assert_eq!(normalize_tag(""), "");
    }

    #[test]
    fn parses_flat_instructions_section() {
        let body = "\n## Instructions\n\n- First step\n- Second step\n- Third step\n";
        let s = skill_with("name: x\ndescription: y\n", body).unwrap();
        assert_eq!(s.steps.len(), 3);
        assert_eq!(s.steps[0].instruction, "First step");
        assert_eq!(s.steps[1].instruction, "Second step");
        assert_eq!(s.steps[2].instruction, "Third step");
        assert!(s.steps.iter().all(|st| st.phase.is_none()));
        assert!(!s.steps_empty);
    }

    #[test]
    fn parses_flat_numbered_steps() {
        let body = "\n## Steps\n\n1. Build the test list\n2. Pick simplest\n";
        let s = skill_with("name: x\ndescription: y\n", body).unwrap();
        assert_eq!(s.steps.len(), 2);
        assert_eq!(s.steps[0].instruction, "Build the test list");
    }

    #[test]
    fn parses_phased_steps() {
        let body = concat!(
            "\n",
            "### Step 1: Build the Test List\n",
            "Identify the inputs.\n",
            "\n",
            "### Step 2: Pick the Simplest\n",
            "Order by simplicity.\n",
        );
        let s = skill_with("name: x\ndescription: y\n", body).unwrap();
        assert_eq!(s.steps.len(), 2);
        assert_eq!(
            s.steps[0].phase.as_deref(),
            Some("Step 1: Build the Test List")
        );
        assert!(s.steps[0].instruction.contains("inputs"));
        assert_eq!(
            s.steps[1].phase.as_deref(),
            Some("Step 2: Pick the Simplest")
        );
    }

    #[test]
    fn unrecognized_step_section_yields_empty() {
        // FMEA F9 / WI-FMEA-02: warn on empty steps.
        let body = "\n## How to Use\n\n- step a\n- step b\n";
        let s = skill_with("name: x\ndescription: y\n", body).unwrap();
        assert!(s.steps.is_empty());
        assert!(s.steps_empty);
    }

    #[test]
    fn captures_supplementary_files() {
        let s = skill_with(
            "name: x\ndescription: y\nsupplementary-files:\n  - extra.md\n  - more.md\n",
            "body",
        )
        .unwrap();
        assert_eq!(s.supplementary_files, vec!["extra.md", "more.md"]);
    }

    #[test]
    fn captures_prerequisites_and_related() {
        let s = skill_with(
            "name: x\ndescription: y\nprerequisites: [a, b]\nrelated: [c]\n",
            "body",
        )
        .unwrap();
        assert_eq!(s.prerequisites, vec!["a", "b"]);
        assert_eq!(s.related, vec!["c"]);
    }

    #[test]
    fn frontmatter_bytes_captured_verbatim_for_hashing() {
        let raw = "name: x\ndescription: y\nweird: \"☃\"\n";
        let s = skill_with(raw, "body").unwrap();
        assert_eq!(s.frontmatter_bytes, raw.as_bytes());
    }

    #[test]
    fn body_bytes_captured_verbatim() {
        let body = "# Header\n\nSome body.\n";
        let s = skill_with("name: x\ndescription: y\n", body).unwrap();
        assert_eq!(s.body_bytes, body.as_bytes());
    }

    #[test]
    fn unknown_frontmatter_fields_are_tolerated() {
        let s = skill_with(
            "name: x\ndescription: y\nfuture-field: 42\nanother: [a, b]\n",
            "body",
        )
        .unwrap();
        assert_eq!(s.name, "x");
    }

    #[test]
    fn argument_hint_accepts_string() {
        let s = skill_with(
            "name: x\ndescription: y\nargument-hint: <feature-description>\n",
            "body",
        )
        .unwrap();
        assert_eq!(s.argument_hint.as_deref(), Some("<feature-description>"));
    }

    #[test]
    fn argument_hint_accepts_yaml_sequence() {
        // Real skills (e.g. task-level/try-cli/SKILL.md) use the bracket
        // form `[command]` which YAML parses as a flow sequence.
        let s = skill_with(
            "name: x\ndescription: y\nargument-hint: [command-or-test-description]\n",
            "body",
        )
        .unwrap();
        assert_eq!(
            s.argument_hint.as_deref(),
            Some("[command-or-test-description]")
        );
    }

    #[test]
    fn argument_hint_sequence_with_multiple_words() {
        let s = skill_with(
            "name: x\ndescription: y\nargument-hint: [foo, bar, baz]\n",
            "body",
        )
        .unwrap();
        assert_eq!(s.argument_hint.as_deref(), Some("[foo bar baz]"));
    }

    #[test]
    fn frontmatter_with_bom_and_blank_lines() {
        let raw = "\u{feff}\n\n---\nname: x\ndescription: y\n---\nbody";
        let s = parse(raw.as_bytes(), "task-level").unwrap();
        assert_eq!(s.name, "x");
    }
}
