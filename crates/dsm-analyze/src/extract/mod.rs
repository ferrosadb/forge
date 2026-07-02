pub mod bridge;
pub mod csharp;
pub mod dot_parser;
pub mod elixir;
pub mod go;
pub mod java;
pub mod multi;
pub mod python;
pub mod rust_lang;
pub mod typescript;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// A dependency edge between two elements.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub source: String,
    pub target: String,
    pub weight: f64,
    pub kind: EdgeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cross_language: Option<CrossLanguageEdge>,
}

/// The kind of dependency relationship.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EdgeKind {
    Import,
    Inheritance,
    Call,
    Data,
    Ffi,
    Ipc,
    Unknown,
}

/// Metadata for edges that cross language boundaries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossLanguageEdge {
    pub source_lang: Language,
    pub target_lang: Language,
    pub mechanism: FfiMechanism,
}

/// Supported languages for extraction.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Language {
    Java,
    Rust,
    Python,
    Go,
    TypeScript,
    Elixir,
    CSharp,
    Erlang,
    C,
    Cpp,
    Unknown(String),
}

impl std::fmt::Display for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Language::Java => write!(f, "Java"),
            Language::Rust => write!(f, "Rust"),
            Language::Python => write!(f, "Python"),
            Language::Go => write!(f, "Go"),
            Language::TypeScript => write!(f, "TypeScript"),
            Language::CSharp => write!(f, "C#"),
            Language::Elixir => write!(f, "Elixir"),
            Language::Erlang => write!(f, "Erlang"),
            Language::C => write!(f, "C"),
            Language::Cpp => write!(f, "C++"),
            Language::Unknown(s) => write!(f, "{}", s),
        }
    }
}

/// Cross-language call mechanism.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FfiMechanism {
    Jni,
    Jna,
    PyExtension,
    Ctypes,
    Cffi,
    PyO3,
    Cgo,
    RustFfi,
    RustBindgen,
    Nif,
    Port,
    NativeAddon,
    PInvoke,
    ComInterop,
    Wasm,
    Grpc,
    Rest,
    SharedProto,
    Unknown(String),
}

/// Granularity level for extraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GranularityLevel {
    Summary,
    Full,
}

/// Configuration for dependency extraction.
#[derive(Debug, Clone)]
pub struct ExtractConfig {
    pub level: GranularityLevel,
    pub prefix_filter: Option<String>,
    pub exclude_patterns: Vec<String>,
    pub detect_cross_language: bool,
}

impl Default for ExtractConfig {
    fn default() -> Self {
        Self {
            level: GranularityLevel::Summary,
            prefix_filter: None,
            exclude_patterns: vec![],
            detect_cross_language: false,
        }
    }
}

/// Trait for language-specific dependency extractors.
pub trait Extractor {
    fn name(&self) -> &str;
    fn language(&self) -> Language;
    fn detect(&self, dir: &Path) -> bool;
    fn extract(&self, dir: &Path, config: &ExtractConfig) -> Result<Vec<Edge>>;
    fn detect_cross_language(&self, dir: &Path, config: &ExtractConfig) -> Result<Vec<Edge>>;
}

// ---------------------------------------------------------------------------
// Dead-code analysis types
// ---------------------------------------------------------------------------

/// A symbol declaration found in source code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Declaration {
    pub name: String,
    pub kind: DeclarationKind,
    pub visibility: Visibility,
    pub file: String,
    pub line: usize,
    pub language: Language,
    pub is_entry_point: bool,
    pub entry_point_reason: Option<String>,
    /// True when the declaration is test-only code (e.g. inside a Rust
    /// `#[cfg(test)]` item). Test declarations are excluded from dead-code
    /// findings unless tests are explicitly included.
    #[serde(default)]
    pub is_test: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeclarationKind {
    Function,
    Method,
    Type,
    Trait,
    Constant,
    Module,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Visibility {
    Public,
    Private,
    PubCrate,
    PubSuper,
    Exported,
    Internal,
}

/// A reference from one symbol to another.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolReference {
    pub from_file: String,
    pub to_symbol: String,
    pub line: usize,
}

/// Markers that identify machine-generated source files (wit-bindgen,
/// prost-build, protoc, etc.). Generated files are skipped by the
/// declaration/reference extractors: auditing them for dead code produces
/// noise the developer cannot act on.
pub fn is_generated_file(content: &str) -> bool {
    content.lines().take(20).any(|line| {
        line.contains("@generated")
            || line.contains("DO NOT EDIT")
            || line.contains("Generated by")
            || line.contains("generated by")
            || line.contains("Code generated")
    })
}

/// Compile a list of exclude glob patterns into anchored regexes.
///
/// Supported syntax: `**` (any path segment sequence), `*` (within one
/// segment), `?` (single non-separator character). A pattern with no `/`
/// also matches a bare file name at any depth (e.g. `bindings.rs`).
///
/// Fails loud on a pattern that produces an invalid regex rather than
/// silently ignoring it.
pub fn compile_exclude_globs(patterns: &[String]) -> Result<Vec<regex::Regex>> {
    patterns
        .iter()
        .map(|p| {
            let re = glob_to_regex(p);
            regex::Regex::new(&re)
                .map_err(|e| anyhow::anyhow!("invalid exclude pattern '{}': {}", p, e))
        })
        .collect()
}

/// True when `path` (a relative, `/`-separated path) matches any compiled
/// exclude glob.
pub fn path_is_excluded(path: &str, globs: &[regex::Regex]) -> bool {
    globs.iter().any(|g| g.is_match(path))
}

fn glob_to_regex(pattern: &str) -> String {
    // A pattern without a separator matches the file name at any depth.
    let pattern = if pattern.contains('/') {
        pattern.to_string()
    } else {
        format!("**/{}", pattern)
    };

    let mut re = String::from("^");
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' => {
                if chars.peek() == Some(&'*') {
                    chars.next();
                    // `**/` matches zero or more whole segments; trailing
                    // `**` matches the rest of the path.
                    if chars.peek() == Some(&'/') {
                        chars.next();
                        re.push_str("(?:.*/)?");
                    } else {
                        re.push_str(".*");
                    }
                } else {
                    re.push_str("[^/]*");
                }
            }
            '?' => re.push_str("[^/]"),
            c => re.push_str(&regex::escape(&c.to_string())),
        }
    }
    re.push('$');
    re
}

/// Trait for extracting declarations and references (symbol-level).
pub trait DeclarationExtractor {
    fn language(&self) -> Language;
    fn extract_declarations(&self, dir: &Path, config: &ExtractConfig) -> Result<Vec<Declaration>>;
    fn extract_references(
        &self,
        dir: &Path,
        config: &ExtractConfig,
    ) -> Result<Vec<SymbolReference>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_marker_detection() {
        assert!(is_generated_file("// Generated by `wit-bindgen` 0.36.0\n"));
        assert!(is_generated_file("// @generated by prost-build\n"));
        assert!(is_generated_file("// DO NOT EDIT\nfn x() {}\n"));
        assert!(!is_generated_file("// A hand-written module.\nfn x() {}\n"));
        // Marker must appear near the top, not anywhere in the file.
        let deep = format!("{}// DO NOT EDIT\n", "// line\n".repeat(30));
        assert!(!is_generated_file(&deep));
    }

    #[test]
    fn glob_double_star_matches_segments() {
        let globs = compile_exclude_globs(&["examples/**".to_string()]).unwrap();
        assert!(path_is_excluded("examples/guest/src/lib.rs", &globs));
        assert!(!path_is_excluded("src/lib.rs", &globs));
    }

    #[test]
    fn glob_bare_filename_matches_any_depth() {
        let globs = compile_exclude_globs(&["bindings.rs".to_string()]).unwrap();
        assert!(path_is_excluded("bindings.rs", &globs));
        assert!(path_is_excluded("a/b/bindings.rs", &globs));
        assert!(!path_is_excluded("a/b/other.rs", &globs));
    }

    #[test]
    fn glob_single_star_stays_within_segment() {
        let globs = compile_exclude_globs(&["src/*.rs".to_string()]).unwrap();
        assert!(path_is_excluded("src/lib.rs", &globs));
        assert!(!path_is_excluded("src/nested/lib.rs", &globs));
    }

    #[test]
    fn glob_double_star_prefix_matches_zero_segments() {
        let globs = compile_exclude_globs(&["**/gen.rs".to_string()]).unwrap();
        assert!(path_is_excluded("gen.rs", &globs));
        assert!(path_is_excluded("a/gen.rs", &globs));
    }
}
