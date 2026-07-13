use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};

use crate::extract::{Declaration, DeclarationKind, SymbolReference, Visibility};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Confidence {
    Definite,
    Possible,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeadCodeFinding {
    pub declaration: Declaration,
    pub confidence: Confidence,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeadCodeReport {
    pub findings: Vec<DeadCodeFinding>,
    pub total_declarations: usize,
    pub total_entry_points: usize,
    pub total_reachable: usize,
    pub dead_definite: usize,
    pub dead_possible: usize,
}

/// Find dead code using BFS reachability from entry points.
pub fn find_dead_code(
    declarations: &[Declaration],
    references: &[SymbolReference],
    include_tests: bool,
) -> DeadCodeReport {
    // 1. Build a set of all declared symbol names
    let declared: HashSet<&str> = declarations.iter().map(|d| d.name.as_str()).collect();

    // 2. Build adjacency: for each reference, if to_symbol matches a declaration, add edge
    //    Key: symbol name, Value: set of symbols it references
    let mut graph: HashMap<&str, HashSet<&str>> = HashMap::new();

    // Build reverse mapping: file -> symbols declared in it
    let mut file_to_symbols: HashMap<&str, Vec<&str>> = HashMap::new();
    for decl in declarations {
        file_to_symbols
            .entry(decl.file.as_str())
            .or_default()
            .push(decl.name.as_str());
    }

    // Build suffix index for O(1) reference resolution:
    // "crate::foo::bar" is indexed under "bar", "foo::bar", and "crate::foo::bar"
    let mut suffix_to_decls: HashMap<&str, Vec<&str>> = HashMap::new();
    for &name in &declared {
        // Index full name
        suffix_to_decls.entry(name).or_default().push(name);
        // Index each suffix after "::"
        let mut rest = name;
        while let Some(pos) = rest.find("::") {
            rest = &rest[pos + 2..];
            if !rest.is_empty() {
                suffix_to_decls.entry(rest).or_default().push(name);
            }
        }
    }

    for reference in references {
        // Find which declared symbols are in the reference's source file
        if let Some(source_symbols) = file_to_symbols.get(reference.from_file.as_str()) {
            // Look up matching declarations via suffix index
            if let Some(targets) = suffix_to_decls.get(reference.to_symbol.as_str()) {
                for &src in source_symbols {
                    for &tgt in targets {
                        if tgt != src {
                            graph.entry(src).or_default().insert(tgt);
                        }
                    }
                }
            }
        }
    }

    // 2b. A module is alive when any of its members is alive: for every
    //     declaration whose fully-qualified name sits under a declared
    //     module, add a member -> module edge (`mod operator;` holding only
    //     `impl` blocks is reachable through its methods even though the
    //     module name never appears in a path).
    let module_names: HashSet<&str> = declarations
        .iter()
        .filter(|d| d.kind == DeclarationKind::Module)
        .map(|d| d.name.as_str())
        .collect();
    for decl in declarations {
        let mut prefix = decl.name.as_str();
        while let Some(pos) = prefix.rfind("::") {
            prefix = &prefix[..pos];
            if module_names.contains(prefix) && prefix != decl.name.as_str() {
                graph.entry(decl.name.as_str()).or_default().insert(prefix);
            }
        }
    }

    // 3. Identify entry points
    let entry_points: Vec<&str> = declarations
        .iter()
        .filter(|d| d.is_entry_point)
        .filter(|d| include_tests || !is_test_declaration(d))
        .map(|d| d.name.as_str())
        .collect();

    // 4. BFS from entry points
    let mut reachable: HashSet<&str> = HashSet::new();
    let mut queue: VecDeque<&str> = VecDeque::new();

    for &ep in &entry_points {
        queue.push_back(ep);
    }

    while let Some(symbol) = queue.pop_front() {
        if !reachable.insert(symbol) {
            continue;
        }
        if let Some(targets) = graph.get(symbol) {
            for &tgt in targets {
                if !reachable.contains(tgt) {
                    queue.push_back(tgt);
                }
            }
        }
    }

    // 5. Everything not reachable is dead
    let mut findings = Vec::new();
    for decl in declarations {
        if !include_tests && is_test_declaration(decl) {
            continue;
        }
        if !reachable.contains(decl.name.as_str()) {
            let confidence = match decl.visibility {
                Visibility::Private | Visibility::Internal => Confidence::Definite,
                _ => Confidence::Possible,
            };
            let reason = match confidence {
                Confidence::Definite => format!(
                    "Private {} '{}' is not reachable from any entry point",
                    kind_label(&decl.kind),
                    decl.name
                ),
                Confidence::Possible => format!(
                    "Public {} '{}' is not referenced internally (may be used externally)",
                    kind_label(&decl.kind),
                    decl.name
                ),
            };
            findings.push(DeadCodeFinding {
                declaration: decl.clone(),
                confidence,
                reason,
            });
        }
    }

    // Sort: definite first, then by file and line
    findings.sort_by(|a, b| {
        a.confidence
            .cmp(&b.confidence)
            .then_with(|| a.declaration.file.cmp(&b.declaration.file))
            .then_with(|| a.declaration.line.cmp(&b.declaration.line))
    });

    let dead_definite = findings
        .iter()
        .filter(|f| f.confidence == Confidence::Definite)
        .count();
    let dead_possible = findings
        .iter()
        .filter(|f| f.confidence == Confidence::Possible)
        .count();

    DeadCodeReport {
        total_declarations: declarations.len(),
        total_entry_points: entry_points.len(),
        total_reachable: reachable.len(),
        dead_definite,
        dead_possible,
        findings,
    }
}

/// Check if a declaration is test-related.
fn is_test_declaration(decl: &Declaration) -> bool {
    decl.is_test
        || decl.entry_point_reason.as_deref() == Some("test function")
        || decl.file.contains("/tests/")
        || decl.file.contains("_test.")
        || decl.file.contains("test_")
        || decl.name.contains("::tests::")
}

/// Check if a reference target matches a declaration name.
/// Handles both exact match and suffix match (e.g., "bar_fn" matches "crate::foo::bar_fn").
#[cfg(test)]
fn symbol_matches(declaration_name: &str, reference: &str) -> bool {
    declaration_name == reference || declaration_name.ends_with(&format!("::{}", reference))
}

fn kind_label(kind: &DeclarationKind) -> &'static str {
    match kind {
        DeclarationKind::Function => "function",
        DeclarationKind::Method => "method",
        DeclarationKind::Type => "type",
        DeclarationKind::Trait => "trait",
        DeclarationKind::Constant => "constant",
        DeclarationKind::Module => "module",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::Language;

    fn make_decl(
        name: &str,
        kind: DeclarationKind,
        vis: Visibility,
        is_entry: bool,
        ep_reason: Option<&str>,
    ) -> Declaration {
        Declaration {
            name: name.to_string(),
            kind,
            visibility: vis,
            file: "src/lib.rs".to_string(),
            line: 1,
            language: Language::Rust,
            is_entry_point: is_entry,
            entry_point_reason: ep_reason.map(|s| s.to_string()),
            is_test: false,
        }
    }

    fn make_ref(from: &str, to: &str) -> SymbolReference {
        SymbolReference {
            from_file: from.to_string(),
            to_symbol: to.to_string(),
            line: 1,
        }
    }

    #[test]
    fn empty_declarations_yields_empty_findings() {
        let report = find_dead_code(&[], &[], false);
        assert!(report.findings.is_empty());
        assert_eq!(report.total_declarations, 0);
        assert_eq!(report.total_entry_points, 0);
        assert_eq!(report.total_reachable, 0);
    }

    #[test]
    fn single_unreachable_private_function_is_definite() {
        let decls = vec![make_decl(
            "crate::foo",
            DeclarationKind::Function,
            Visibility::Private,
            false,
            None,
        )];
        let report = find_dead_code(&decls, &[], false);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].confidence, Confidence::Definite);
        assert_eq!(report.dead_definite, 1);
        assert_eq!(report.dead_possible, 0);
    }

    #[test]
    fn single_unreachable_public_function_is_possible() {
        let decls = vec![make_decl(
            "crate::bar",
            DeclarationKind::Function,
            Visibility::Public,
            false,
            None,
        )];
        let report = find_dead_code(&decls, &[], false);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].confidence, Confidence::Possible);
        assert_eq!(report.dead_definite, 0);
        assert_eq!(report.dead_possible, 1);
    }

    #[test]
    fn entry_point_is_always_reachable() {
        let decls = vec![make_decl(
            "crate::main",
            DeclarationKind::Function,
            Visibility::Private,
            true,
            Some("main function"),
        )];
        let report = find_dead_code(&decls, &[], false);
        assert!(report.findings.is_empty());
        assert_eq!(report.total_reachable, 1);
    }

    #[test]
    fn transitive_reachability() {
        // A (entry) -> B -> C, all reachable
        let decls = vec![
            make_decl(
                "crate::a",
                DeclarationKind::Function,
                Visibility::Private,
                true,
                Some("main function"),
            ),
            make_decl(
                "crate::b",
                DeclarationKind::Function,
                Visibility::Private,
                false,
                None,
            ),
            make_decl(
                "crate::c",
                DeclarationKind::Function,
                Visibility::Private,
                false,
                None,
            ),
        ];
        let refs = vec![make_ref("src/lib.rs", "b"), make_ref("src/lib.rs", "c")];
        let report = find_dead_code(&decls, &refs, false);
        assert_eq!(report.total_reachable, 3);
        assert!(report.findings.is_empty());
    }

    #[test]
    fn disconnected_subgraph_is_dead() {
        // D -> E, neither is an entry point, both dead
        let decls = vec![
            make_decl(
                "crate::d",
                DeclarationKind::Function,
                Visibility::Private,
                false,
                None,
            ),
            {
                let mut d = make_decl(
                    "crate::e",
                    DeclarationKind::Function,
                    Visibility::Private,
                    false,
                    None,
                );
                d.file = "src/other.rs".to_string();
                d
            },
        ];
        let refs = vec![make_ref("src/lib.rs", "e")];
        let report = find_dead_code(&decls, &refs, false);
        assert_eq!(report.findings.len(), 2);
        assert_eq!(report.dead_definite, 2);
    }

    #[test]
    fn test_exclusion_works() {
        let decls = vec![
            make_decl(
                "crate::tests::test_something",
                DeclarationKind::Function,
                Visibility::Private,
                true,
                Some("test function"),
            ),
            make_decl(
                "crate::real_fn",
                DeclarationKind::Function,
                Visibility::Private,
                false,
                None,
            ),
        ];
        // With include_tests=false, test entry points are excluded
        let report = find_dead_code(&decls, &[], false);
        // The test function is excluded from analysis, real_fn is dead
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].declaration.name, "crate::real_fn");
        assert_eq!(report.total_entry_points, 0);
    }

    #[test]
    fn test_inclusion_works() {
        let decls = vec![
            make_decl(
                "crate::tests::test_something",
                DeclarationKind::Function,
                Visibility::Private,
                true,
                Some("test function"),
            ),
            make_decl(
                "crate::real_fn",
                DeclarationKind::Function,
                Visibility::Private,
                false,
                None,
            ),
        ];
        let refs = vec![make_ref("src/lib.rs", "real_fn")];
        // With include_tests=true, test entry points count
        let report = find_dead_code(&decls, &refs, true);
        assert_eq!(report.total_entry_points, 1);
        // real_fn is reachable from test via the reference
        assert_eq!(report.total_reachable, 2);
        assert!(report.findings.is_empty());
    }

    #[test]
    fn symbol_matches_exact() {
        assert!(symbol_matches("crate::foo::bar", "crate::foo::bar"));
    }

    #[test]
    fn symbol_matches_suffix() {
        assert!(symbol_matches("crate::foo::bar", "bar"));
        assert!(symbol_matches("crate::foo::bar", "foo::bar"));
    }

    #[test]
    fn symbol_matches_no_false_positive() {
        assert!(!symbol_matches("crate::foo::bar", "baz"));
        assert!(!symbol_matches("crate::foo::bar", "foobar"));
    }
}
