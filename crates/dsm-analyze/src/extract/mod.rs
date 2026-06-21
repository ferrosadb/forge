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
