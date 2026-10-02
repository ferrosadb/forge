//! Symbol lookup with language-server fast path and a parallel scan fallback.
//!
//! Provides `frg lookup <symbol>` and the `find_definition` MCP tool.
//!
//! Order of preference:
//!
//! 1. **Language server**, when one is installed and matches the project. A
//!    `workspace/symbol` query is served from the server's index and answers in
//!    milliseconds.
//! 2. **Parallel scan**, which searches every source file concurrently and only
//!    parses files that actually mention the symbol.
//!
//! The scan exists because a language server is not always installed, and it has
//! to stay fast on a large tree: symbol lookup is a navigation primitive, and a
//! multi-minute lookup is worse than no lookup at all.

use std::path::Path;
use std::sync::Mutex;

use serde::Serialize;

use crate::excerpt;
use crate::lsp::{self, AvailableServer};
use crate::summarizer::{self, ElementKind};

/// How long a language server gets to answer before the scan takes over.
/// Covers a cold `rust-analyzer` start on a large workspace; a server that has
/// already indexed this project returns well inside it.
const LSP_TIMEOUT_MS: u64 = 30_000;

/// Candidate source files below which scanning beats a language server.
///
/// Measured: the scan costs roughly 2 ms per file (199 files in ~0.45 s), while a
/// cold language server costs ~6.5 s of indexing regardless of project size. The
/// crossover is a few thousand files, so small trees scan and large trees use the
/// index. Getting this wrong in either direction is a real regression — the LSP
/// path would make every small lookup ~15x slower.
const SCAN_PREFERRED_FILES: usize = 2_000;

/// Upper bound on matches returned, so an ambiguous name on a large tree cannot
/// flood the caller. The total found is reported separately.
const MAX_MATCHES: usize = 200;

#[derive(Debug, Serialize)]
pub struct LookupResult {
    /// How the result was produced: `lsp`, `scan`, or `lsp_fallback`.
    pub method: String,
    /// Whether a usable language server was detected for this root.
    pub lsp_available: bool,
    /// The server used, when one was.
    pub lsp_server: Option<String>,
    /// Why the language server was not used, when it was detected but failed.
    pub lsp_error: Option<String>,
    /// Language servers present on PATH that cannot run, with the reason. Lets a
    /// caller tell "no server installed" from "installed but broken".
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unusable_servers: Vec<UnusableServer>,
    /// Every match found, before the cap.
    pub total_matches: usize,
    /// True when `matches` was truncated to `MAX_MATCHES`.
    pub truncated: bool,
    pub matches: Vec<SymbolMatch>,
}

/// A language server that exists on PATH but does not execute.
#[derive(Debug, Serialize)]
pub struct UnusableServer {
    pub binary: String,
    pub language: String,
    pub reason: String,
}

#[derive(Debug, Serialize)]
pub struct SymbolMatch {
    pub file: String,
    pub line: usize,
    pub end_line: Option<usize>,
    pub kind: String,
    pub preview: String,
    pub excerpt_command: String,
}

/// Check whether any language server Forge can drive is installed for `root`.
pub fn lsp_available(root: &Path) -> bool {
    lsp::best_server_for(root).is_some()
}

/// Look up a symbol definition, choosing the faster path for this tree.
///
/// A language server indexes the whole project before it can answer, so it wins
/// on large trees and loses badly on small ones. Tree size therefore decides the
/// path; `SCAN_PREFERRED_FILES` is the measured crossover. Whichever path runs,
/// the language server can only make the answer faster, never worse: an empty or
/// failed server answer falls through to the scan rather than standing as "no
/// such symbol".
pub fn lookup_symbol(symbol: &str, dir: &Path) -> anyhow::Result<LookupResult> {
    let server = lsp::best_server_for(dir);
    let files = count_source_files(dir);

    if let Some(server) = &server {
        if files > SCAN_PREFERRED_FILES {
            match query_server(server, dir, symbol) {
                Ok(result) if result.total_matches > 0 => return Ok(result),
                Ok(_) => {
                    let mut result = scan_symbol(symbol, dir)?;
                    result.method = "lsp_fallback".to_string();
                    result.lsp_available = true;
                    result.lsp_server = Some(server.binary.clone());
                    result.lsp_error = Some(format!(
                        "{} returned no symbols for this name (usually still indexing); \
                         using the scan so the answer is complete",
                        server.binary
                    ));
                    result.unusable_servers = unusable(dir);
                    return Ok(result);
                }
                Err(err) => {
                    let mut result = scan_symbol(symbol, dir)?;
                    result.method = "lsp_fallback".to_string();
                    result.lsp_available = true;
                    result.lsp_server = Some(server.binary.clone());
                    result.lsp_error = Some(err.to_string());
                    result.unusable_servers = unusable(dir);
                    return Ok(result);
                }
            }
        }
    }

    let mut result = scan_symbol(symbol, dir)?;
    result.lsp_available = server.is_some();
    result.lsp_server = server.as_ref().map(|s| s.binary.clone());
    result.unusable_servers = unusable(dir);
    // A tiny tree is scanned because that is faster, which is different from
    // having no server; say so rather than implying the machine is unconfigured.
    if let Some(server) = &server {
        if files <= SCAN_PREFERRED_FILES {
            result.lsp_error = Some(format!(
                "{} detected but this tree is small enough ({files} files) that \
                 scanning is faster than waiting for the server to index",
                server.binary
            ));
        }
    }
    Ok(result)
}

/// Count candidate source files, bounded by the threshold that matters.
///
/// Stops as soon as the count passes `SCAN_PREFERRED_FILES`: the only question is
/// which side of the crossover the tree is on, so counting all of a 14k-file tree
/// is wasted work.
fn count_source_files(dir: &Path) -> usize {
    let mut count = 0;
    let walker = ignore::WalkBuilder::new(dir)
        .hidden(true)
        .git_ignore(true)
        .build();
    for entry in walker.flatten() {
        let path = entry.path();
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !is_source_ext(ext) {
            continue;
        }
        count += 1;
        if count > SCAN_PREFERRED_FILES {
            break;
        }
    }
    count
}

/// Servers on PATH that cannot run, shaped for disclosure.
fn unusable(dir: &Path) -> Vec<UnusableServer> {
    lsp::unusable_servers(dir)
        .into_iter()
        .map(|s| UnusableServer {
            binary: s.binary,
            language: s.language,
            reason: s
                .probe_error
                .unwrap_or_else(|| "did not report a version".to_string()),
        })
        .collect()
}

/// Ask the language server for workspace symbols and shape the answer.
fn query_server(
    server: &AvailableServer,
    dir: &Path,
    symbol: &str,
) -> anyhow::Result<LookupResult> {
    let query = lsp::workspace_symbols(
        server,
        dir,
        symbol,
        std::time::Duration::from_millis(LSP_TIMEOUT_MS),
    )?;

    // A server answers with fuzzy matches; keep the ones that really name the
    // requested symbol so `find_definition` does not report near-misses as hits.
    let needle = symbol.to_lowercase();
    let mut matches: Vec<SymbolMatch> = query
        .symbols
        .into_iter()
        .filter(|s| s.name.to_lowercase().contains(&needle))
        .map(|s| SymbolMatch {
            file: s.file.clone(),
            line: s.line,
            end_line: s.end_line,
            kind: s.kind,
            preview: s.name,
            excerpt_command: format!("frg excerpt {}:{}", s.file, symbol),
        })
        .collect();

    matches.sort_by(|a, b| a.file.cmp(&b.file).then_with(|| a.line.cmp(&b.line)));
    let total_matches = matches.len();
    let truncated = total_matches > MAX_MATCHES;
    matches.truncate(MAX_MATCHES);

    Ok(LookupResult {
        method: "lsp".to_string(),
        lsp_available: true,
        lsp_server: Some(query.server),
        lsp_error: None,
        unusable_servers: Vec::new(),
        total_matches,
        truncated,
        matches,
    })
}

/// Scan source files in parallel for a symbol declaration.
///
/// Two properties make this fast enough to be a fallback rather than a last
/// resort:
///
/// - **Filter before parse.** A file that does not contain the symbol text is
///   skipped without parsing. Parsing is orders of magnitude more expensive than
///   a byte search, and most files in a tree do not define any given symbol.
/// - **Walk in parallel.** Files are traversed and read concurrently; the scan
///   is I/O plus CPU bound and had no reason to be serial.
fn scan_symbol(symbol: &str, dir: &Path) -> anyhow::Result<LookupResult> {
    let matches: Mutex<Vec<SymbolMatch>> = Mutex::new(Vec::new());
    let needle = symbol.as_bytes();

    let walker = ignore::WalkBuilder::new(dir)
        .hidden(true)
        .git_ignore(true)
        .build_parallel();

    walker.run(|| {
        Box::new(|entry| {
            let Ok(entry) = entry else {
                return ignore::WalkState::Continue;
            };
            if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                return ignore::WalkState::Continue;
            }
            let path = entry.path();
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if !is_source_ext(ext) {
                return ignore::WalkState::Continue;
            }
            let Ok(source) = std::fs::read_to_string(path) else {
                return ignore::WalkState::Continue;
            };
            // Cheap rejection first: no parse, no regex, just bytes.
            if memchr::memmem::find(source.as_bytes(), needle).is_none() {
                return ignore::WalkState::Continue;
            }

            let filename = path.display().to_string();
            let digest = summarizer::summarize(&filename, &source);
            let mut found = Vec::new();
            for elem in &digest.elements {
                if !elem.text.contains(symbol) || elem.kind == ElementKind::Import {
                    continue;
                }
                found.push(SymbolMatch {
                    file: filename.clone(),
                    line: elem.line,
                    end_line: elem.end_line,
                    kind: kind_name(elem.kind.clone()).to_string(),
                    preview: elem.text.clone(),
                    excerpt_command: format!("frg excerpt {}:{}", filename, symbol),
                });
            }
            if !found.is_empty() {
                if let Ok(mut guard) = matches.lock() {
                    guard.extend(found);
                }
            }
            ignore::WalkState::Continue
        })
    });

    let mut matches = matches.into_inner().unwrap_or_default();
    // Deterministic order regardless of which thread finished first.
    matches.sort_by(|a, b| a.file.cmp(&b.file).then_with(|| a.line.cmp(&b.line)));
    let total_matches = matches.len();
    let truncated = total_matches > MAX_MATCHES;
    matches.truncate(MAX_MATCHES);

    Ok(LookupResult {
        method: "scan".to_string(),
        lsp_available: false,
        lsp_server: None,
        lsp_error: None,
        unusable_servers: Vec::new(),
        total_matches,
        truncated,
        matches,
    })
}

fn kind_name(kind: ElementKind) -> &'static str {
    match kind {
        ElementKind::Function => "function",
        ElementKind::Struct => "struct",
        ElementKind::Enum => "enum",
        ElementKind::Trait => "trait",
        ElementKind::Interface => "interface",
        ElementKind::Class => "class",
        ElementKind::Constant => "constant",
        ElementKind::TypeAlias => "type_alias",
        ElementKind::Module => "module",
        ElementKind::TestBlock => "test_block",
        ElementKind::Import => "import",
    }
}

/// Format lookup results for human/LLM consumption.
pub fn format_lookup(result: &LookupResult) -> String {
    let mut out = String::new();

    match result.method.as_str() {
        "lsp" => {
            out.push_str(&format!(
                "via {} (language server, {} ms)\n",
                result.lsp_server.as_deref().unwrap_or("lsp"),
                "indexed"
            ));
        }
        "lsp_fallback" => {
            out.push_str(&format!(
                "language server {} was detected but did not answer ({}); \
                 fell back to a scan\n",
                result.lsp_server.as_deref().unwrap_or("lsp"),
                result.lsp_error.as_deref().unwrap_or("unknown error")
            ));
        }
        _ => {}
    }

    if result.matches.is_empty() {
        out.push_str("No matches found.\n");
        return out;
    }

    out.push_str(&format!(
        "Found {} matches{}{}:\n",
        result.total_matches,
        if result.total_matches != result.matches.len() {
            format!(" (showing first {})", result.matches.len())
        } else {
            String::new()
        },
        if result.truncated {
            " — truncated"
        } else {
            ""
        }
    ));
    for m in &result.matches {
        let span = match m.end_line {
            Some(end) => format!("L{}-{}", m.line, end),
            None => format!("L{}", m.line),
        };
        out.push_str(&format!(
            "  {} {:>10}  {} [{}]\n",
            m.file, span, m.preview, m.kind
        ));
    }

    if let Some(hint) = unusable_server_hint(result) {
        out.push_str(&format!("\nnote: {hint}\n"));
    }

    out.push_str("\nTo extract a symbol's full body:\n");
    out.push_str(&format!("  {}\n", result.matches[0].excerpt_command));

    out
}

/// A one-line warning for the CLI, or `None` when there is nothing to say.
///
/// Symbol lookup is measurably faster through an indexed language server, so an
/// operator whose machine could be faster should be told — including *how*. The
/// hint names whichever of these is true:
///
/// - a server is installed but cannot run (and why), or
/// - no server is installed for the project's languages (and what to install).
pub fn speedup_hint(result: &LookupResult, dir: &Path) -> Option<String> {
    if result.method == "lsp" {
        return None; // already on the fast path
    }

    // A server that was detected and deliberately skipped (small tree) is
    // already explained by `lsp_error`; adding a "install a server" hint here
    // would contradict the result.
    if result.lsp_available {
        return None;
    }

    if let Some(broken) = result.unusable_servers.first() {
        return Some(format!(
            "{} is installed but cannot run ({}), so lookup used the scan; \
             repairing it would make these lookups instant",
            broken.binary, broken.reason
        ));
    }

    let missing = missing_servers(dir);
    if missing.is_empty() {
        return None;
    }
    let names: Vec<String> = missing
        .iter()
        .map(|s| format!("{} (for {})", s.install_hint, s.language))
        .collect();
    Some(format!(
        "no language server for this project, so lookup used the scan; \
         install {} for instant, index-backed results",
        names.join(", ")
    ))
}

/// A language this project uses, and what would serve it.
struct MissingServer {
    language: String,
    install_hint: String,
}

/// Languages present in `dir` that have no runnable language server.
fn missing_servers(dir: &Path) -> Vec<MissingServer> {
    let mut present: Vec<&str> = Vec::new();
    for (ext, language) in [
        ("rs", "rust"),
        ("py", "python"),
        ("go", "go"),
        ("ts", "typescript"),
        ("tsx", "typescript"),
        ("ex", "elixir"),
        ("java", "java"),
        ("swift", "swift"),
        ("c", "c"),
        ("cpp", "c"),
    ] {
        // A marker file is stronger evidence than a stray file, but either means
        // a server would help here.
        if !present.contains(&language) && project_uses(dir, ext, language) {
            present.push(language);
        }
    }

    let runnable: Vec<String> = lsp::detect_servers(dir)
        .into_iter()
        .filter(|s| s.runnable)
        .map(|s| s.language)
        .collect();

    present
        .into_iter()
        .filter(|language| !runnable.iter().any(|l| l == language))
        .map(|language| MissingServer {
            language: language.to_string(),
            install_hint: install_hint_for(language).to_string(),
        })
        .collect()
}

/// Whether the project looks like it uses a language, by marker file or by
/// the presence of any file with that extension near the root.
fn project_uses(dir: &Path, ext: &str, language: &str) -> bool {
    let markers: &[&str] = match language {
        "rust" => &["Cargo.toml"],
        "python" => &["pyproject.toml", "setup.py", "requirements.txt"],
        "go" => &["go.mod"],
        "typescript" => &["package.json", "tsconfig.json"],
        "elixir" => &["mix.exs"],
        "java" => &["pom.xml", "build.gradle"],
        "swift" => &["Package.swift"],
        "c" => &["compile_commands.json", "CMakeLists.txt", "Makefile"],
        _ => &[],
    };
    if markers.iter().any(|marker| dir.join(marker).exists()) {
        return true;
    }

    // Fall back to a bounded look for the extension, so a project without its
    // marker in the scanned subtree is still recognised.
    let mut found = false;
    let walker = ignore::WalkBuilder::new(dir)
        .hidden(true)
        .git_ignore(true)
        .max_depth(Some(6))
        .build();
    for entry in walker.flatten() {
        if entry.path().extension().and_then(|e| e.to_str()) == Some(ext) {
            found = true;
            break;
        }
    }
    found
}

/// How to install the server for a language.
fn install_hint_for(language: &str) -> &'static str {
    match language {
        "rust" => "rust-analyzer (rustup component add rust-analyzer)",
        "python" => "pyright (npm i -g pyright)",
        "go" => "gopls (go install golang.org/x/tools/gopls@latest)",
        "typescript" => "typescript-language-server (npm i -g typescript-language-server)",
        "elixir" => "elixir-ls",
        "java" => "jdtls",
        "swift" => "sourcekit-lsp (ships with Xcode)",
        "c" => "clangd (ships with LLVM)",
        _ => "the relevant language server",
    }
}

/// One-line hint about language servers that are installed but broken.
///
/// A server on `PATH` that cannot run is a fixable environment problem, so the
/// caller is told what to do rather than left wondering why lookups are slow.
pub fn unusable_server_hint(result: &LookupResult) -> Option<String> {
    let first = result.unusable_servers.first()?;
    Some(format!(
        "{} is on PATH but cannot run ({}), so lookups fall back to a scan; \
         install the server for its language to make these instant",
        first.binary, first.reason
    ))
}

fn is_source_ext(ext: &str) -> bool {
    matches!(
        ext,
        "rs" | "py"
            | "go"
            | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "ex"
            | "exs"
            | "cpp"
            | "cc"
            | "h"
            | "hpp"
            | "swift"
            | "java"
            | "rb"
            | "kt"
    )
}

/// Try to extract a specific symbol using the excerpt module.
/// Returns the excerpt body if found, or None.
pub fn extract_and_format(filename: &str, source: &str, symbol: &str) -> Option<String> {
    excerpt::extract_symbol(filename, source, symbol).map(|result| {
        let mut out = format!(
            "// {} :: {} (L{}-{})\n",
            result.file, result.symbol, result.start_line, result.end_line
        );
        // Add line numbers to body
        for (i, line) in result.body.lines().enumerate() {
            out.push_str(&format!("{:>5}  {}\n", result.start_line + i, line));
        }
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_source_ext_filters_correctly() {
        assert!(is_source_ext("rs"));
        assert!(is_source_ext("py"));
        assert!(!is_source_ext("md"));
        assert!(!is_source_ext("toml"));
    }

    fn result_with(method: &str, matches: Vec<SymbolMatch>) -> LookupResult {
        LookupResult {
            method: method.to_string(),
            lsp_available: method != "scan",
            lsp_server: None,
            lsp_error: None,
            unusable_servers: Vec::new(),
            total_matches: matches.len(),
            truncated: false,
            matches,
        }
    }

    #[test]
    fn format_lookup_empty_results() {
        let out = format_lookup(&result_with("scan", vec![]));
        assert!(out.contains("No matches found"));
    }

    #[test]
    fn format_lookup_with_matches() {
        let result = result_with(
            "scan",
            vec![SymbolMatch {
                file: "src/main.rs".to_string(),
                line: 42,
                end_line: Some(60),
                kind: "function".to_string(),
                preview: "pub fn process_data(input".to_string(),
                excerpt_command: "frg excerpt src/main.rs:process_data".to_string(),
            }],
        );
        let out = format_lookup(&result);
        assert!(out.contains("1 matches"));
        assert!(out.contains("L42-60"));
        assert!(out.contains("process_data"));
    }

    /// A language server that could not answer must be disclosed, and the
    /// result must be the scan's real output rather than a silent empty list.
    #[test]
    fn format_lookup_discloses_an_lsp_fallback() {
        let mut result = result_with(
            "lsp_fallback",
            vec![SymbolMatch {
                file: "src/lib.rs".to_string(),
                line: 7,
                end_line: None,
                kind: "function".to_string(),
                preview: "fn target".to_string(),
                excerpt_command: "frg excerpt src/lib.rs:target".to_string(),
            }],
        );
        result.lsp_server = Some("rust-analyzer".to_string());
        result.lsp_error = Some("timed out".to_string());
        let out = format_lookup(&result);
        assert!(out.contains("rust-analyzer"), "{out}");
        assert!(out.contains("timed out"), "{out}");
        assert!(
            out.contains("src/lib.rs"),
            "scan results still shown: {out}"
        );
    }

    #[test]
    fn format_lookup_reports_truncation() {
        let mut result = result_with("scan", vec![]);
        result.total_matches = 500;
        result.truncated = true;
        result.matches = vec![SymbolMatch {
            file: "a.rs".to_string(),
            line: 1,
            end_line: None,
            kind: "function".to_string(),
            preview: "fn a".to_string(),
            excerpt_command: "frg excerpt a.rs:a".to_string(),
        }];
        let out = format_lookup(&result);
        assert!(out.contains("Found 500 matches"), "{out}");
        assert!(out.contains("showing first 1"), "{out}");
    }

    // --- scan behaviour ---

    #[test]
    fn scan_finds_a_real_definition_and_ignores_prose() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "// prose mentioning widget_factory in a comment only\npub fn widget_factory() -> u32 { 1 }\n",
        )
        .unwrap();

        let result = scan_symbol("widget_factory", dir.path()).unwrap();
        assert_eq!(result.method, "scan");
        assert_eq!(result.total_matches, 1, "{:?}", result.matches);
        assert_eq!(
            result.matches[0].file,
            dir.path().join("lib.rs").display().to_string()
        );
        assert_eq!(result.matches[0].kind, "function");
    }

    #[test]
    fn scan_skips_files_that_do_not_mention_the_symbol() {
        let dir = tempfile::tempdir().unwrap();
        // A file that would panic the summarizer if it were parsed is absent
        // here; the point is that an unrelated file is never parsed at all.
        std::fs::write(
            dir.path().join("unrelated.rs"),
            "pub fn something_else() {}\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("target.rs"), "pub fn wanted() {}\n").unwrap();

        let result = scan_symbol("wanted", dir.path()).unwrap();
        assert_eq!(result.total_matches, 1);
        assert!(result.matches[0].file.ends_with("target.rs"));
    }

    #[test]
    fn scan_excludes_imports() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "use crate::wanted;\npub fn wanted() {}\n",
        )
        .unwrap();
        let result = scan_symbol("wanted", dir.path()).unwrap();
        assert!(
            result.matches.iter().all(|m| m.kind != "import"),
            "imports are not definitions: {:?}",
            result.matches
        );
    }

    #[test]
    fn scan_returns_deterministic_order() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["a.rs", "b.rs", "c.rs", "d.rs"] {
            std::fs::write(dir.path().join(name), "pub fn ordered() {}\n").unwrap();
        }
        let first = scan_symbol("ordered", dir.path()).unwrap();
        let second = scan_symbol("ordered", dir.path()).unwrap();
        let files: Vec<_> = first.matches.iter().map(|m| m.file.clone()).collect();
        let files2: Vec<_> = second.matches.iter().map(|m| m.file.clone()).collect();
        assert_eq!(files, files2, "parallel walk must still order results");
        assert!(files.windows(2).all(|w| w[0] <= w[1]), "{files:?}");
    }

    /// The lookup must never lose a symbol that exists, whichever path runs.
    /// A language server that answers empty (cold index) must fall through to
    /// the scan rather than reporting "no such symbol".
    #[test]
    fn an_existing_symbol_is_always_found_whatever_the_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "pub fn target_symbol() {}\n").unwrap();

        let result = lookup_symbol("target_symbol", dir.path()).unwrap();
        assert!(
            result.total_matches > 0,
            "the symbol exists, so it must be found (method={}, error={:?})",
            result.method,
            result.lsp_error
        );
        // A tiny tree scans on purpose; that is a deliberate choice, not a
        // missing server.
        assert_eq!(result.method, "scan");
    }

    /// Whatever this machine has installed, a result that claims to come from a
    /// language server must actually carry symbols: an empty `lsp` result is a
    /// false negative by definition.
    #[test]
    fn an_lsp_result_is_never_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "pub fn nothing_else() {}\n").unwrap();
        let result = lookup_symbol("nothing_else", dir.path()).unwrap();
        if result.method == "lsp" {
            assert!(
                result.total_matches > 0,
                "an `lsp` result must not be empty"
            );
        }
    }

    // --- speedup hint ---

    #[test]
    fn no_hint_when_already_on_the_fast_path() {
        let dir = tempfile::tempdir().unwrap();
        let result = result_with("lsp", vec![]);
        assert!(
            speedup_hint(&result, dir.path()).is_none(),
            "a lookup served by a server needs no advice"
        );
    }

    #[test]
    fn hint_names_the_broken_server_and_the_reason() {
        let dir = tempfile::tempdir().unwrap();
        let mut result = result_with("scan", vec![]);
        result.unusable_servers = vec![UnusableServer {
            binary: "rust-analyzer".to_string(),
            language: "rust".to_string(),
            reason: "Unknown binary in official toolchain".to_string(),
        }];
        let hint = speedup_hint(&result, dir.path()).expect("a broken server is worth reporting");
        assert!(hint.contains("rust-analyzer"), "{hint}");
        assert!(hint.contains("Unknown binary"), "{hint}");
        assert!(hint.contains("cannot run"), "{hint}");
    }

    #[test]
    fn hint_names_what_to_install_for_a_rust_project() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
        std::fs::write(dir.path().join("lib.rs"), "pub fn f() {}\n").unwrap();

        // Only meaningful when no runnable Rust server exists on this machine.
        let has_rust_server = lsp::detect_servers(dir.path())
            .iter()
            .any(|s| s.runnable && s.language == "rust");
        if has_rust_server {
            return;
        }
        let hint = speedup_hint(&result_with("scan", vec![]), dir.path())
            .expect("a Rust project with no server is worth a hint");
        assert!(hint.contains("rust-analyzer"), "{hint}");
        assert!(hint.contains("install"), "{hint}");
    }

    #[test]
    fn hint_is_silent_for_a_project_with_no_known_language() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), "nothing to index\n").unwrap();
        assert!(
            speedup_hint(&result_with("scan", vec![]), dir.path()).is_none(),
            "no known language means nothing to recommend"
        );
    }

    #[test]
    fn install_hints_are_actionable_for_every_supported_language() {
        for language in [
            "rust",
            "python",
            "go",
            "typescript",
            "elixir",
            "java",
            "swift",
            "c",
        ] {
            let hint = install_hint_for(language);
            assert!(!hint.is_empty(), "{language} has no install hint");
            assert_ne!(hint, "the relevant language server", "{language}");
        }
    }

    /// Small trees must not pay language-server indexing: the measured cost is
    /// ~6.5 s cold versus ~2 ms per file for the scan.
    #[test]
    fn small_trees_scan_instead_of_waiting_for_a_server() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
        std::fs::write(dir.path().join("lib.rs"), "pub fn tiny() {}\n").unwrap();

        let result = lookup_symbol("tiny", dir.path()).unwrap();
        assert_eq!(result.method, "scan", "a 1-file tree is faster to scan");
        assert!(result.total_matches > 0);
    }

    #[test]
    fn source_file_counting_is_bounded_by_the_threshold() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..10 {
            std::fs::write(dir.path().join(format!("f{i}.rs")), "fn f() {}\n").unwrap();
        }
        // Non-source files must not count toward the threshold.
        std::fs::write(dir.path().join("README.md"), "# hi\n").unwrap();
        let count = count_source_files(dir.path());
        assert_eq!(count, 10, "only source extensions count");
    }

    /// `missing_servers` must not report a language that already has a runnable
    /// server, or the hint contradicts the result it accompanies.
    #[test]
    fn missing_servers_excludes_languages_that_have_a_server() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
        std::fs::write(dir.path().join("lib.rs"), "pub fn f() {}\n").unwrap();

        let has_rust_server = lsp::detect_servers(dir.path())
            .iter()
            .any(|s| s.runnable && s.language == "rust");
        let missing = missing_servers(dir.path());
        if has_rust_server {
            assert!(
                !missing.iter().any(|m| m.language == "rust"),
                "rust has a runnable server, so it is not missing: {:?}",
                missing.iter().map(|m| &m.language).collect::<Vec<_>>()
            );
        }
    }

    /// When a server was detected and deliberately skipped, the speedup hint
    /// must stay silent — `lsp_error` already explains it, and recommending an
    /// install for a server that is present would be a contradiction.
    #[test]
    fn no_speedup_hint_when_a_server_was_detected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
        let mut result = result_with("scan", vec![]);
        result.lsp_available = true;
        result.lsp_server = Some("rust-analyzer".to_string());
        result.lsp_error = Some("tree is small enough that scanning is faster".to_string());
        assert!(
            speedup_hint(&result, dir.path()).is_none(),
            "a detected server already explains the scan"
        );
    }

    #[test]
    fn missing_symbol_is_an_empty_result_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "pub fn present() {}\n").unwrap();
        let result = scan_symbol("absent_entirely", dir.path()).unwrap();
        assert_eq!(result.total_matches, 0);
        assert!(!result.truncated);
    }
}
