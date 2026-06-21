use crate::extract::bridge::detect_cross_language_bridges;
use crate::extract::*;
use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;

/// Multi-language extractor that auto-detects languages and merges results.
pub struct MultiExtractor {
    extractors: Vec<Box<dyn Extractor>>,
}

impl MultiExtractor {
    /// Create a new MultiExtractor with all supported language extractors.
    pub fn new() -> Self {
        Self {
            extractors: vec![
                Box::new(super::java::JavaExtractor),
                Box::new(super::rust_lang::RustExtractor),
                Box::new(super::python::PythonExtractor),
                Box::new(super::go::GoExtractor),
                Box::new(super::typescript::TypeScriptExtractor),
                Box::new(super::elixir::ElixirExtractor),
                Box::new(super::csharp::CSharpExtractor),
            ],
        }
    }

    /// Auto-detect all languages present in the project and run
    /// each applicable extractor. Merge results into a single edge
    /// list with cross-language edges bridging the language boundaries.
    pub fn extract_all(&self, dir: &Path, config: &ExtractConfig) -> Result<Vec<Edge>> {
        let mut all_edges = Vec::new();
        let mut per_language: HashMap<Language, Vec<Edge>> = HashMap::new();
        let mut detected_languages = Vec::new();

        for extractor in &self.extractors {
            if extractor.detect(dir) {
                detected_languages.push(extractor.language());

                // Extract intra-language dependencies
                match extractor.extract(dir, config) {
                    Ok(edges) => {
                        per_language.insert(extractor.language(), edges.clone());
                        all_edges.extend(edges);
                    }
                    Err(e) => {
                        eprintln!("Warning: {} extractor failed: {}", extractor.name(), e);
                    }
                }

                // Extract cross-language dependencies
                if config.detect_cross_language {
                    match extractor.detect_cross_language(dir, config) {
                        Ok(edges) => {
                            all_edges.extend(edges);
                        }
                        Err(e) => {
                            eprintln!(
                                "Warning: {} cross-language detection failed: {}",
                                extractor.name(),
                                e
                            );
                        }
                    }
                }
            }
        }

        // Detect cross-language bridges (shared schemas, docker, etc.)
        if config.detect_cross_language && detected_languages.len() > 1 {
            let bridge_edges = detect_cross_language_bridges(dir, &per_language);
            all_edges.extend(bridge_edges);
        }

        // Deduplicate edges
        dedup_edges(&mut all_edges);

        Ok(all_edges)
    }

    /// Extract declarations and references for dead-code analysis.
    /// Supports Rust, Elixir, Java, TypeScript, and C#.
    pub fn extract_declarations(
        &self,
        dir: &Path,
        config: &ExtractConfig,
    ) -> Result<(Vec<Declaration>, Vec<SymbolReference>)> {
        use super::csharp::CSharpExtractor;
        use super::elixir::ElixirExtractor;
        use super::java::JavaExtractor;
        use super::rust_lang::RustExtractor;
        use super::typescript::TypeScriptExtractor;
        use crate::extract::DeclarationExtractor;

        let mut all_decls = Vec::new();
        let mut all_refs = Vec::new();

        let rust_ext = RustExtractor;
        if rust_ext.detect(dir) {
            all_decls.extend(rust_ext.extract_declarations(dir, config)?);
            all_refs.extend(rust_ext.extract_references(dir, config)?);
        }

        let elixir_ext = ElixirExtractor;
        if elixir_ext.detect(dir) {
            all_decls.extend(elixir_ext.extract_declarations(dir, config)?);
            all_refs.extend(elixir_ext.extract_references(dir, config)?);
        }

        let java_ext = JavaExtractor;
        if java_ext.detect(dir) {
            all_decls.extend(java_ext.extract_declarations(dir, config)?);
            all_refs.extend(java_ext.extract_references(dir, config)?);
        }

        let ts_ext = TypeScriptExtractor;
        if ts_ext.detect(dir) {
            all_decls.extend(ts_ext.extract_declarations(dir, config)?);
            all_refs.extend(ts_ext.extract_references(dir, config)?);
        }

        let csharp_ext = CSharpExtractor;
        if csharp_ext.detect(dir) {
            all_decls.extend(csharp_ext.extract_declarations(dir, config)?);
            all_refs.extend(csharp_ext.extract_references(dir, config)?);
        }

        Ok((all_decls, all_refs))
    }

    /// Get list of detected languages in a directory.
    pub fn detect_languages(&self, dir: &Path) -> Vec<Language> {
        self.extractors
            .iter()
            .filter(|e| e.detect(dir))
            .map(|e| e.language())
            .collect()
    }
}

impl Default for MultiExtractor {
    fn default() -> Self {
        Self::new()
    }
}

/// Remove duplicate edges, summing weights for identical source/target pairs.
fn dedup_edges(edges: &mut Vec<Edge>) {
    let mut seen: HashMap<(String, String), usize> = HashMap::new();
    let mut deduped: Vec<Edge> = Vec::new();

    for edge in edges.drain(..) {
        let key = (edge.source.clone(), edge.target.clone());
        if let Some(&idx) = seen.get(&key) {
            deduped[idx].weight += edge.weight;
            // Prefer Ffi/Ipc kind over Import
            if matches!(edge.kind, EdgeKind::Ffi | EdgeKind::Ipc) {
                deduped[idx].kind = edge.kind;
                deduped[idx].cross_language = edge.cross_language;
            }
        } else {
            seen.insert(key, deduped.len());
            deduped.push(edge);
        }
    }

    *edges = deduped;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multi_extractor_detects_rust() {
        let ext = MultiExtractor::new();
        // We're in a Rust project
        let langs = ext.detect_languages(Path::new("."));
        assert!(langs.contains(&Language::Rust));
    }

    #[test]
    fn dedup_merges_weights() {
        let mut edges = vec![
            Edge {
                source: "a".to_string(),
                target: "b".to_string(),
                weight: 1.0,
                kind: EdgeKind::Import,
                cross_language: None,
            },
            Edge {
                source: "a".to_string(),
                target: "b".to_string(),
                weight: 2.0,
                kind: EdgeKind::Import,
                cross_language: None,
            },
        ];
        dedup_edges(&mut edges);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].weight, 3.0);
    }

    #[test]
    fn dedup_prefers_ffi_kind() {
        let mut edges = vec![
            Edge {
                source: "a".to_string(),
                target: "b".to_string(),
                weight: 1.0,
                kind: EdgeKind::Import,
                cross_language: None,
            },
            Edge {
                source: "a".to_string(),
                target: "b".to_string(),
                weight: 1.0,
                kind: EdgeKind::Ffi,
                cross_language: Some(CrossLanguageEdge {
                    source_lang: Language::Java,
                    target_lang: Language::C,
                    mechanism: FfiMechanism::Jni,
                }),
            },
        ];
        dedup_edges(&mut edges);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].kind, EdgeKind::Ffi);
        assert!(edges[0].cross_language.is_some());
    }
}
