//! Module: Cognitive complexity engine — AST scoring, file walking, ranking.
//! Correctness: Correct when unparsable or unreadable files are disclosed as
//! failures rather than skipped, and every scored function reaches the totals.
//! Last revised: 2026-10-01
//! Last changed: Initial implementation, ranked refactoring targets.

use anyhow::{Context, Result};
use complexity::Complexity;
use std::path::{Path, PathBuf};
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::{Attribute, Block, ImplItem, Item, ItemFn, TraitItem, Type, Visibility};

use crate::report::{
    band, BandCounts, CognitiveReport, FailureKind, FileFailure, FunctionComplexity, FunctionKind,
    Percentiles,
};

/// Tuning for a cognitive complexity scan.
#[derive(Debug, Clone)]
pub struct CognitiveConfig {
    /// Only report functions at or above this cognitive complexity.
    pub max_cognitive: Option<u32>,
    /// Keep only the N worst functions after filtering.
    pub top: Option<usize>,
    /// Skip functions marked as tests (`#[test]`, or inside a `#[cfg(test)]` module).
    pub exclude_tests: bool,
}

/// Ranked results are capped by default so output stays bounded on a large
/// repository. The counts and `truncated` flag always disclose the full
/// population, so nothing is hidden — only the list is capped.
///
/// Calibrated for a codebase the size of a multi-crate Rust suite: the worst
/// 100 functions there are all at least ~40 cognitive complexity (the ~1% mark),
/// so the default list is entirely genuine hot spots.
pub const DEFAULT_TOP: usize = 100;

impl Default for CognitiveConfig {
    fn default() -> Self {
        Self {
            max_cognitive: None,
            top: Some(DEFAULT_TOP),
            exclude_tests: false,
        }
    }
}

/// Scan a file or directory tree for cognitive complexity hot spots.
///
/// Directories are walked recursively, honouring `.gitignore`, and only `.rs`
/// files are analyzed. The returned report always describes the whole scanned
/// population; `truncated` and the count fields disclose any narrowing.
pub fn analyze_path(path: &Path, config: &CognitiveConfig) -> Result<CognitiveReport> {
    if !path.exists() {
        anyhow::bail!("path does not exist: {}", path.display());
    }

    let mut files = Vec::new();
    if path.is_file() {
        files.push(path.to_path_buf());
    } else {
        for entry in ignore::WalkBuilder::new(path).build().flatten() {
            if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                continue;
            }
            if entry.path().extension().is_some_and(|ext| ext == "rs") {
                files.push(entry.path().to_path_buf());
            }
        }
        // Sort for deterministic file ordering across runs and filesystems.
        files.sort();
    }

    let mut report = CognitiveReport {
        files_scanned: files.len(),
        files_analyzed: 0,
        functions_analyzed: 0,
        functions_total: 0,
        returned: 0,
        truncated: false,
        threshold: config.max_cognitive,
        top: config.top,
        total_cognitive: 0,
        max_cognitive: 0,
        bands: BandCounts::default(),
        percentiles: Percentiles::default(),
        file_hotspots: None,
        churn: None,
        functions: Vec::new(),
        failures: Vec::new(),
        warnings: Vec::new(),
    };

    let mut scored = Vec::new();

    for file in &files {
        let display = file.display().to_string();
        let source = match std::fs::read_to_string(file) {
            Ok(source) => source,
            Err(err) => {
                report.failures.push(FileFailure {
                    file: display,
                    kind: FailureKind::Read,
                    detail: err.to_string(),
                });
                continue;
            }
        };

        match parse_and_score(&source, &display, config, &mut scored) {
            Ok(()) => report.files_analyzed += 1,
            Err(failure) => report.failures.push(failure),
        }
    }

    finish_report(&mut report, scored, config);
    Ok(report)
}

/// Score a single in-memory source file.
///
/// Parse failures are recorded in `failures` rather than returned as an error,
/// so a caller scoring a mixed batch can keep going and still see what failed.
pub fn analyze_source(file: &str, source: &str, config: &CognitiveConfig) -> CognitiveReport {
    let mut report = CognitiveReport {
        files_scanned: 1,
        files_analyzed: 0,
        functions_analyzed: 0,
        functions_total: 0,
        returned: 0,
        truncated: false,
        threshold: config.max_cognitive,
        top: config.top,
        total_cognitive: 0,
        max_cognitive: 0,
        bands: BandCounts::default(),
        percentiles: Percentiles::default(),
        file_hotspots: None,
        churn: None,
        functions: Vec::new(),
        failures: Vec::new(),
        warnings: Vec::new(),
    };

    let mut scored = Vec::new();
    match parse_and_score(source, file, config, &mut scored) {
        Ok(()) => report.files_analyzed = 1,
        Err(failure) => report.failures.push(failure),
    }

    finish_report(&mut report, scored, config);
    report
}

/// Parse one file and score everything in it.
///
/// A parse *error* comes back as a `Parse` failure. A parse *panic* does too:
/// `syn` 1 panics rather than erroring on syntax it cannot represent — notably
/// Rust C-string literals (`c"..."`), which are valid Rust 1.77+ and appear in
/// real code. An uncaught panic there would abort the entire scan and produce
/// no report at all, so every file is isolated here. A scan must degrade to a
/// disclosed per-file failure, never to a dead process.
fn parse_and_score(
    source: &str,
    file: &str,
    config: &CognitiveConfig,
    out: &mut Vec<FunctionComplexity>,
) -> Result<(), FileFailure> {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let parsed = syn::parse_file(source)?;
        let mut found = Vec::new();
        collect_items(
            &parsed.items,
            file,
            &[],
            false,
            config.exclude_tests,
            &mut found,
        );
        Ok::<Vec<FunctionComplexity>, syn::Error>(found)
    }));

    match result {
        Ok(Ok(found)) => {
            out.extend(found);
            Ok(())
        }
        Ok(Err(err)) => Err(FileFailure {
            file: file.to_string(),
            kind: FailureKind::Parse,
            detail: err.to_string(),
        }),
        Err(payload) => Err(FileFailure {
            file: file.to_string(),
            kind: FailureKind::Parse,
            detail: format!(
                "parser panicked on unsupported syntax (commonly a C-string literal like c\"...\"): {}",
                panic_message(&payload)
            ),
        }),
    }
}

/// Extract a readable message from a caught panic payload.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "unknown panic".to_string()
    }
}

/// Apply threshold filtering, ranking, the `top` cut-off, and totals.
///
/// Order matters: totals and bands are computed over *everything* scored, so a
/// caller can always tell how much was withheld from the ranked list.
fn finish_report(
    report: &mut CognitiveReport,
    mut scored: Vec<FunctionComplexity>,
    config: &CognitiveConfig,
) {
    report.functions_analyzed = scored.len();
    report.total_cognitive = scored.iter().map(|f| f.cognitive).sum();
    report.max_cognitive = scored.iter().map(|f| f.cognitive).max().unwrap_or(0);
    let mut scores: Vec<u32> = scored.iter().map(|f| f.cognitive).collect();
    report.percentiles = Percentiles::of(&mut scores);
    for item in &scored {
        match item.band {
            crate::report::Band::Low => report.bands.low += 1,
            crate::report::Band::Moderate => report.bands.moderate += 1,
            crate::report::Band::High => report.bands.high += 1,
            crate::report::Band::Severe => report.bands.severe += 1,
        }
    }

    if let Some(threshold) = config.max_cognitive {
        scored.retain(|f| f.cognitive >= threshold);
    }
    report.functions_total = scored.len();

    CognitiveReport::rank(&mut scored);

    if let Some(top) = config.top {
        if scored.len() > top {
            scored.truncate(top);
        }
    }

    report.returned = scored.len();
    report.truncated = report.functions_total > report.returned;
    report.functions = scored;

    if report.truncated {
        report.warnings.push(format!(
            "showing {} of {} matching functions (worst first); raise `top` to see the rest",
            report.returned, report.functions_total
        ));
    }
    if !report.failures.is_empty() {
        report.warnings.push(format!(
            "{} file(s) could not be analyzed; see `failures`",
            report.failures.len()
        ));
    }
}

/// Walk a set of items, tracking module path and test context.
fn collect_items(
    items: &[Item],
    file: &str,
    module_path: &[String],
    in_cfg_test: bool,
    exclude_tests: bool,
    out: &mut Vec<FunctionComplexity>,
) {
    for item in items {
        match item {
            Item::Fn(func) => {
                let is_test = in_cfg_test || has_test_attr(&func.attrs);
                if exclude_tests && is_test {
                    continue;
                }
                // `ItemFn::complexity()` is the upstream implementation and
                // evaluates the body at nesting level 0. Do not re-wrap the
                // body — that would add a nesting level and inflate the score.
                out.push(score(
                    file,
                    qualify(module_path, &func.sig.ident.to_string()),
                    FunctionKind::Function,
                    &func.sig,
                    &func.block,
                    func.complexity(),
                    is_test,
                ));
            }
            Item::Mod(module) => {
                let is_test_mod = in_cfg_test || has_test_attr(&module.attrs);
                if let Some((_, items)) = &module.content {
                    let mut path = module_path.to_vec();
                    path.push(module.ident.to_string());
                    collect_items(items, file, &path, is_test_mod, exclude_tests, out);
                }
            }
            Item::Impl(impl_block) => {
                let self_ty = type_name(&impl_block.self_ty);
                let is_test_impl = in_cfg_test || has_test_attr(&impl_block.attrs);
                for impl_item in &impl_block.items {
                    if let ImplItem::Method(method) = impl_item {
                        let is_test = is_test_impl || has_test_attr(&method.attrs);
                        if exclude_tests && is_test {
                            continue;
                        }
                        // `ImplItemMethod::complexity()` is upstream's own
                        // evaluation of the method body.
                        out.push(score(
                            file,
                            format!("{self_ty}::{}", method.sig.ident),
                            FunctionKind::Method,
                            &method.sig,
                            &method.block,
                            method.complexity(),
                            is_test,
                        ));
                    }
                }
            }
            Item::Trait(trait_block) => {
                let is_test_trait = in_cfg_test || has_test_attr(&trait_block.attrs);
                for trait_item in &trait_block.items {
                    // Only methods with a default body have code to score.
                    if let TraitItem::Method(method) = trait_item {
                        let Some(block) = &method.default else {
                            continue;
                        };
                        let is_test = is_test_trait || has_test_attr(&method.attrs);
                        if exclude_tests && is_test {
                            continue;
                        }
                        out.push(score(
                            file,
                            qualify(module_path, &method.sig.ident.to_string()),
                            FunctionKind::Method,
                            &method.sig,
                            block,
                            trait_default_complexity(&method.sig.ident, block),
                            is_test,
                        ));
                    }
                }
            }
            _ => {}
        }
    }
}

fn qualify(module_path: &[String], name: &str) -> String {
    if module_path.is_empty() {
        name.to_string()
    } else {
        format!("{}::{name}", module_path.join("::"))
    }
}

/// Build one scored entry for a scored block.
///
/// `cognitive` is passed in rather than computed here so that every caller
/// routes through the upstream implementation (never a re-wrapped body, which
/// would add a nesting level and inflate the score).
fn score(
    file: &str,
    name: String,
    kind: FunctionKind,
    sig: &syn::Signature,
    block: &Block,
    cognitive: u32,
    is_test: bool,
) -> FunctionComplexity {
    FunctionComplexity {
        file: file.to_string(),
        name,
        kind,
        line: sig.span().start().line,
        lines: body_lines(sig, block),
        cognitive,
        nesting: max_nesting(block),
        band: band(cognitive),
        is_test,
    }
}

/// Length of a function body in lines: the signature line through the closing
/// brace, inclusive.
///
/// This is deliberately inclusive of the signature — it matches the convention
/// used by other Rust complexity tooling, so counts are comparable across
/// tools. Attributes and doc comments above the signature are *not* counted.
fn body_lines(sig: &syn::Signature, block: &Block) -> usize {
    let start = sig.span().start().line;
    let end = block.span().end().line;
    end.saturating_sub(start) + 1
}

/// Score a trait method's default body.
///
/// `complexity` 0.2 only exposes `Complexity` for `ItemFn` and
/// `ImplItemMethod`, so rebuild the default body as an `ItemFn` with no
/// visibility and evaluate it with the upstream implementation. The score is
/// identical to evaluating the body directly; the shim is structural only.
fn trait_default_complexity(ident: &syn::Ident, block: &Block) -> u32 {
    let item = ItemFn {
        attrs: Vec::new(),
        vis: Visibility::Inherited,
        sig: syn::Signature {
            constness: None,
            asyncness: None,
            unsafety: None,
            abi: None,
            fn_token: Default::default(),
            ident: ident.clone(),
            generics: Default::default(),
            paren_token: Default::default(),
            inputs: Default::default(),
            variadic: None,
            output: syn::ReturnType::Default,
        },
        block: Box::new(block.clone()),
    };
    item.complexity()
}

/// Best-effort self-type label for name qualification, e.g. `Engine`.
fn type_name(ty: &Type) -> String {
    match ty {
        Type::Path(path) => path
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string())
            .unwrap_or_else(|| "impl".to_string()),
        _ => "impl".to_string(),
    }
}

/// True for `#[test]` or `#[cfg(test)]` items.
fn has_test_attr(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        if attr.path.is_ident("test") {
            return true;
        }
        attr.path.is_ident("cfg") && attr.tokens.to_string().contains("test")
    })
}

/// Maximum control-flow nesting depth inside a block.
///
/// Counts `if`, `for`, `while`, `loop`, and `match` — the constructs that make
/// a reader hold context. Braces from blocks or struct literals do not count.
fn max_nesting(block: &Block) -> usize {
    let mut visitor = NestingVisitor::default();
    visitor.visit_block(block);
    visitor.max
}

#[derive(Default)]
struct NestingVisitor {
    depth: usize,
    max: usize,
}

impl<'ast> Visit<'ast> for NestingVisitor {
    fn visit_expr(&mut self, expr: &'ast syn::Expr) {
        let is_control_flow = matches!(
            expr,
            syn::Expr::If(_)
                | syn::Expr::ForLoop(_)
                | syn::Expr::While(_)
                | syn::Expr::Loop(_)
                | syn::Expr::Match(_)
        );

        if is_control_flow {
            self.depth += 1;
            self.max = self.max.max(self.depth);
        }
        syn::visit::visit_expr(self, expr);
        if is_control_flow {
            self.depth -= 1;
        }
    }
}

/// Convenience: analyze a path and return an error when the path is unusable.
pub fn analyze_dir(dir: &Path, config: &CognitiveConfig) -> Result<CognitiveReport> {
    analyze_path(dir, config).with_context(|| format!("analyzing {}", dir.display()))
}

/// Combine per-file reports into a single report.
///
/// Totals, bands, ranking, and the `top` cut-off are recomputed over the union,
/// so the count contract holds for the merged result exactly as for a single
/// scan. Callers must pass reports produced with `top` and `max_cognitive`
/// cleared — merging already-truncated reports would silently lose functions.
pub fn merge_reports(reports: Vec<CognitiveReport>, config: &CognitiveConfig) -> CognitiveReport {
    let mut merged = CognitiveReport {
        files_scanned: 0,
        files_analyzed: 0,
        functions_analyzed: 0,
        functions_total: 0,
        returned: 0,
        truncated: false,
        threshold: config.max_cognitive,
        top: config.top,
        total_cognitive: 0,
        max_cognitive: 0,
        bands: BandCounts::default(),
        percentiles: Percentiles::default(),
        file_hotspots: None,
        churn: None,
        functions: Vec::new(),
        failures: Vec::new(),
        warnings: Vec::new(),
    };

    let mut scored = Vec::new();
    for report in reports {
        merged.files_scanned += report.files_scanned;
        merged.files_analyzed += report.files_analyzed;
        merged.failures.extend(report.failures);
        merged.warnings.extend(report.warnings);
        scored.extend(report.functions);
    }

    // Warnings describe the whole scan, so identical ones collapse.
    merged.warnings.sort();
    merged.warnings.dedup();

    finish_report(&mut merged, scored, config);
    merged
}

/// A config with the run-narrowing fields cleared, for per-file scoring before
/// a merge. `merge_reports` applies the real thresholds afterwards.
pub fn unbounded(config: &CognitiveConfig) -> CognitiveConfig {
    CognitiveConfig {
        max_cognitive: None,
        top: None,
        exclude_tests: config.exclude_tests,
    }
}

/// Derive a narrowed view of an unbounded report without re-reading or
/// re-scoring anything.
///
/// Totals, bands, and percentiles are recomputed over the full set, so a caller
/// that needs both views (e.g. an uncapped file ranking alongside a capped
/// function list) produces one scan and derives the other. `base` is expected
/// to carry the full function set — merging already-capped reports would lose
/// functions and understate these totals.
pub fn apply_limits(base: &CognitiveReport, config: &CognitiveConfig) -> CognitiveReport {
    let mut narrowed = base.clone();
    finish_report(&mut narrowed, base.functions.clone(), config);
    narrowed
}

/// Resolve a path argument into files the way `analyze_path` does, exposed for
/// callers that want to report on the file set (e.g. DSM integration).
pub fn rust_files(path: &Path) -> Result<Vec<PathBuf>> {
    if !path.exists() {
        anyhow::bail!("path does not exist: {}", path.display());
    }
    if path.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }
    let mut files: Vec<PathBuf> = ignore::WalkBuilder::new(path)
        .build()
        .flatten()
        .filter(|entry| entry.file_type().is_some_and(|ft| ft.is_file()))
        .map(|entry| entry.path().to_path_buf())
        .filter(|p| p.extension().is_some_and(|ext| ext == "rs"))
        .collect();
    files.sort();
    Ok(files)
}
