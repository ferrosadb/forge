//! Regression tests for dead-code audit false positives.
//!
//! Each test is a minimal fixture reproducing a confirmed false positive from
//! a real `frg dsm dead-code` run against a large Rust codebase:
//!
//! 1. Constants used only as bare identifiers (`FOO.inc()`, `x = FOO + 1`)
//!    were flagged "definitely dead" because the reference extractor only
//!    captured qualified paths and call syntax.
//! 2. `#[cfg(test)]` items were flagged as dead product code.
//! 3. `const fn` was parsed as a Constant named `fn`; `static mut` as `mut`;
//!    `const _:` as `_`.
//! 4. Generated files (wit-bindgen etc.) were audited.
//! 5. `--exclude` globs were not wired to `ExtractConfig.exclude_patterns`.

use forge_dsm_analyze::dead_code::{find_dead_code, Confidence, DeadCodeReport};
use forge_dsm_analyze::extract::rust_lang::RustExtractor;
use forge_dsm_analyze::extract::{
    Declaration, DeclarationExtractor, DeclarationKind, ExtractConfig, GranularityLevel,
    SymbolReference,
};
use std::path::Path;

fn write_project(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
    )
    .expect("write Cargo.toml");
    for (rel, content) in files {
        let path = dir.path().join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, content).expect("write source file");
    }
    dir
}

fn full_config() -> ExtractConfig {
    ExtractConfig {
        level: GranularityLevel::Full,
        prefix_filter: None,
        exclude_patterns: vec![],
        detect_cross_language: false,
    }
}

fn extract(dir: &Path, config: &ExtractConfig) -> (Vec<Declaration>, Vec<SymbolReference>) {
    let ext = RustExtractor;
    let decls = ext.extract_declarations(dir, config).expect("declarations");
    let refs = ext.extract_references(dir, config).expect("references");
    (decls, refs)
}

fn analyze(dir: &Path, config: &ExtractConfig) -> DeadCodeReport {
    let (decls, refs) = extract(dir, config);
    find_dead_code(&decls, &refs, false)
}

fn definite_names(report: &DeadCodeReport) -> Vec<String> {
    report
        .findings
        .iter()
        .filter(|f| f.confidence == Confidence::Definite)
        .map(|f| f.declaration.name.clone())
        .collect()
}

// ---------------------------------------------------------------------------
// 1. Bare-identifier constant usage must count as a reference
// ---------------------------------------------------------------------------

#[test]
fn const_used_as_bare_identifier_in_expression_is_not_dead() {
    let dir = write_project(&[(
        "src/main.rs",
        r#"
const BASE_DELAY_MS: u64 = 250;

fn main() {
    let delay = BASE_DELAY_MS + 1;
    println!("{delay}");
}
"#,
    )]);
    let report = analyze(dir.path(), &full_config());
    let dead = definite_names(&report);
    assert!(
        !dead.iter().any(|n| n.ends_with("BASE_DELAY_MS")),
        "BASE_DELAY_MS is used as a bare identifier and must not be dead; findings: {dead:?}"
    );
}

#[test]
fn const_used_via_method_call_receiver_is_not_dead() {
    // Corpus item: `COMMITLOG_APPENDS_TOTAL.inc()` — metric statics whose only
    // uses are `FOO.method()` receivers.
    let dir = write_project(&[(
        "src/main.rs",
        r#"
struct Counter;
impl Counter {
    fn inc(&self) {}
}

static COMMITLOG_APPENDS_TOTAL: Counter = Counter;

fn main() {
    COMMITLOG_APPENDS_TOTAL.inc();
}
"#,
    )]);
    let report = analyze(dir.path(), &full_config());
    let dead = definite_names(&report);
    assert!(
        !dead.iter().any(|n| n.ends_with("COMMITLOG_APPENDS_TOTAL")),
        "COMMITLOG_APPENDS_TOTAL is used as a method receiver; findings: {dead:?}"
    );
}

#[test]
fn const_used_by_reference_is_not_dead() {
    let dir = write_project(&[(
        "src/main.rs",
        r#"
static CHECKPOINT_FILENAME: &str = "checkpoint.json";

fn main() {
    consume(&CHECKPOINT_FILENAME);
}

fn consume(_s: &&str) {}
"#,
    )]);
    let report = analyze(dir.path(), &full_config());
    let dead = definite_names(&report);
    assert!(
        !dead.iter().any(|n| n.ends_with("CHECKPOINT_FILENAME")),
        "CHECKPOINT_FILENAME is used via `&CHECKPOINT_FILENAME`; findings: {dead:?}"
    );
}

#[test]
fn declaration_line_itself_does_not_count_as_a_reference() {
    // A constant whose only occurrence is its own declaration must still be
    // reported dead — the bare-identifier fix must skip the declaration line.
    let dir = write_project(&[(
        "src/main.rs",
        r#"
const TRULY_UNUSED_CONST: u64 = 7;

fn main() {}
"#,
    )]);
    let report = analyze(dir.path(), &full_config());
    let dead = definite_names(&report);
    assert!(
        dead.iter().any(|n| n.ends_with("TRULY_UNUSED_CONST")),
        "an actually-unused constant must still be flagged; findings: {dead:?}"
    );
}

#[test]
fn const_mentioned_only_in_comment_or_string_is_still_dead() {
    let dir = write_project(&[(
        "src/main.rs",
        r#"
const DOC_ONLY_CONST: u64 = 7;

fn main() {
    // DOC_ONLY_CONST is mentioned here but never used.
    println!("DOC_ONLY_CONST");
}
"#,
    )]);
    let report = analyze(dir.path(), &full_config());
    let dead = definite_names(&report);
    assert!(
        dead.iter().any(|n| n.ends_with("DOC_ONLY_CONST")),
        "mentions in comments/strings must not count as references; findings: {dead:?}"
    );
}

// ---------------------------------------------------------------------------
// 2. #[cfg(test)] items are test code, not dead product code
// ---------------------------------------------------------------------------

#[test]
fn cfg_test_inline_mod_is_excluded_as_test_code() {
    let dir = write_project(&[(
        "src/main.rs",
        r#"
fn main() {}

#[cfg(test)]
mod engine_applier_checks {
    #[test]
    fn applies() {
        assert!(true);
    }
}
"#,
    )]);
    let report = analyze(dir.path(), &full_config());
    let flagged: Vec<String> = report
        .findings
        .iter()
        .map(|f| f.declaration.name.clone())
        .collect();
    assert!(
        !flagged.iter().any(|n| n.ends_with("engine_applier_checks")),
        "#[cfg(test)] mod must be excluded from dead-code findings; findings: {flagged:?}"
    );
}

#[test]
fn cfg_test_declarations_inside_mod_are_excluded() {
    let dir = write_project(&[(
        "src/main.rs",
        r#"
fn main() {}

#[cfg(test)]
mod harness {
    const FIXTURE_SEED: u64 = 42;

    fn helper() -> u64 {
        FIXTURE_SEED
    }

    #[test]
    fn runs() {
        assert_eq!(helper(), 42);
    }
}
"#,
    )]);
    let report = analyze(dir.path(), &full_config());
    let flagged: Vec<String> = report
        .findings
        .iter()
        .map(|f| f.declaration.name.clone())
        .collect();
    assert!(
        !flagged
            .iter()
            .any(|n| n.ends_with("FIXTURE_SEED") || n.ends_with("helper")),
        "items inside a #[cfg(test)] mod are test code; findings: {flagged:?}"
    );
}

#[test]
fn cfg_test_out_of_line_mod_is_excluded_as_test_code() {
    // Corpus item: `#[cfg(test)] mod driver_cross_shard_e2e;` in mod.rs with
    // the body in its own file.
    let dir = write_project(&[
        (
            "src/main.rs",
            r#"
mod accord;

fn main() {
    accord::run();
}
"#,
        ),
        (
            "src/accord/mod.rs",
            r#"
#[cfg(test)]
mod driver_cross_shard_e2e;

pub fn run() {}
"#,
        ),
        (
            "src/accord/driver_cross_shard_e2e.rs",
            r#"
fn cross_shard_helper() -> u32 {
    7
}

#[test]
fn e2e() {
    assert_eq!(cross_shard_helper(), 7);
}
"#,
        ),
    ]);
    let report = analyze(dir.path(), &full_config());
    let flagged: Vec<String> = report
        .findings
        .iter()
        .map(|f| f.declaration.name.clone())
        .collect();
    assert!(
        !flagged.iter().any(|n| n.contains("driver_cross_shard_e2e")),
        "out-of-line #[cfg(test)] mod and its contents are test code; findings: {flagged:?}"
    );
}

// ---------------------------------------------------------------------------
// 3. Declaration parse bugs: `const fn`, `static mut`, `const _:`
// ---------------------------------------------------------------------------

#[test]
fn const_fn_is_a_function_not_a_constant_named_fn() {
    let dir = write_project(&[(
        "src/main.rs",
        r#"
const fn weighted_avg(a: u64, b: u64) -> u64 {
    (a + b) / 2
}

fn main() {
    let _ = weighted_avg(1, 2);
}
"#,
    )]);
    let (decls, _) = extract(dir.path(), &full_config());
    assert!(
        !decls
            .iter()
            .any(|d| d.kind == DeclarationKind::Constant && d.name.ends_with("::fn")),
        "`const fn` must not yield a Constant named `fn`; decls: {:?}",
        decls.iter().map(|d| &d.name).collect::<Vec<_>>()
    );
    assert!(
        decls
            .iter()
            .any(|d| d.kind == DeclarationKind::Function && d.name.ends_with("weighted_avg")),
        "`const fn weighted_avg` must be extracted as a Function; decls: {:?}",
        decls.iter().map(|d| &d.name).collect::<Vec<_>>()
    );
}

#[test]
fn static_mut_is_named_after_the_static_not_mut() {
    let dir = write_project(&[(
        "src/main.rs",
        r#"
static mut GLOBAL_FLAG: bool = false;

fn main() {}
"#,
    )]);
    let (decls, _) = extract(dir.path(), &full_config());
    assert!(
        !decls.iter().any(|d| d.name.ends_with("::mut")),
        "`static mut` must not yield a declaration named `mut`; decls: {:?}",
        decls.iter().map(|d| &d.name).collect::<Vec<_>>()
    );
    assert!(
        decls
            .iter()
            .any(|d| d.kind == DeclarationKind::Constant && d.name.ends_with("GLOBAL_FLAG")),
        "`static mut GLOBAL_FLAG` must be extracted under its real name; decls: {:?}",
        decls.iter().map(|d| &d.name).collect::<Vec<_>>()
    );
}

#[test]
fn underscore_const_is_not_a_declaration() {
    let dir = write_project(&[(
        "src/main.rs",
        r#"
const _: () = assert!(true);

fn main() {}
"#,
    )]);
    let (decls, _) = extract(dir.path(), &full_config());
    assert!(
        !decls.iter().any(|d| d.name.ends_with("::_")),
        "`const _:` cannot be referenced and must not be a declaration; decls: {:?}",
        decls.iter().map(|d| &d.name).collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// 4. Generated files are skipped
// ---------------------------------------------------------------------------

#[test]
fn wit_bindgen_generated_file_is_skipped() {
    let dir = write_project(&[
        (
            "src/main.rs",
            r#"
fn main() {}
"#,
        ),
        (
            "src/bindings.rs",
            r#"// Generated by `wit-bindgen` 0.36.0. DO NOT EDIT!
const GENERATED_MAGIC: u32 = 0xDEAD;

fn generated_helper() -> u32 {
    GENERATED_MAGIC
}
"#,
        ),
    ]);
    let (decls, _) = extract(dir.path(), &full_config());
    assert!(
        !decls.iter().any(|d| d.file.ends_with("bindings.rs")),
        "files with generated-code markers must be skipped; decls: {:?}",
        decls.iter().map(|d| (&d.file, &d.name)).collect::<Vec<_>>()
    );
}

#[test]
fn at_generated_marker_file_is_skipped() {
    let dir = write_project(&[
        ("src/main.rs", "fn main() {}\n"),
        (
            "src/proto.rs",
            "// @generated by prost-build\nconst PROTO_VERSION: u32 = 3;\n",
        ),
    ]);
    let (decls, _) = extract(dir.path(), &full_config());
    assert!(
        !decls.iter().any(|d| d.file.ends_with("proto.rs")),
        "@generated files must be skipped; decls: {:?}",
        decls.iter().map(|d| (&d.file, &d.name)).collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// 5. exclude_patterns globs are respected
// ---------------------------------------------------------------------------

#[test]
fn exclude_glob_skips_matching_files() {
    let dir = write_project(&[
        ("src/main.rs", "fn main() {}\n"),
        (
            "examples/guest/src/lib.rs",
            "const EXAMPLE_ONLY: u32 = 1;\n",
        ),
    ]);
    let config = ExtractConfig {
        exclude_patterns: vec!["examples/**".to_string()],
        ..full_config()
    };
    let (decls, _) = extract(dir.path(), &config);
    assert!(
        !decls.iter().any(|d| d.file.starts_with("examples/")),
        "exclude glob 'examples/**' must skip files under examples/; decls: {:?}",
        decls.iter().map(|d| (&d.file, &d.name)).collect::<Vec<_>>()
    );
    assert!(
        decls.iter().any(|d| d.file.ends_with("main.rs")),
        "non-excluded files must still be extracted"
    );
}

#[test]
fn exclude_glob_bare_filename_matches_anywhere() {
    let dir = write_project(&[
        ("src/main.rs", "fn main() {}\n"),
        ("src/nested/bindings.rs", "const NESTED_GEN: u32 = 1;\n"),
    ]);
    let config = ExtractConfig {
        exclude_patterns: vec!["bindings.rs".to_string()],
        ..full_config()
    };
    let (decls, _) = extract(dir.path(), &config);
    assert!(
        !decls.iter().any(|d| d.file.ends_with("bindings.rs")),
        "a bare filename exclude pattern must match at any depth; decls: {:?}",
        decls.iter().map(|d| (&d.file, &d.name)).collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// 6. Additional reference-extraction gaps found while spot-checking a real
//    codebase run (each invented a false "definitely dead" finding)
// ---------------------------------------------------------------------------

#[test]
fn const_used_via_format_string_interpolation_is_not_dead() {
    // Real-world case: `format!("{prefix}/{MANIFEST_PATH}")` — the only use
    // of the constant is inline format-string interpolation.
    let dir = write_project(&[(
        "src/main.rs",
        r#"
const MANIFEST_PATH: &str = "archive-manifest.json";

fn main() {
    let prefix = "s3://bucket";
    let path = format!("{prefix}/{MANIFEST_PATH}");
    println!("{path}");
}
"#,
    )]);
    let report = analyze(dir.path(), &full_config());
    let dead = definite_names(&report);
    assert!(
        !dead.iter().any(|n| n.ends_with("MANIFEST_PATH")),
        "format-string interpolation is a use; findings: {dead:?}"
    );
}

#[test]
fn function_called_only_with_turbofish_is_not_dead() {
    // Real-world case: `data_array::<16>(bytes)` — turbofish call syntax.
    let dir = write_project(&[(
        "src/main.rs",
        r#"
fn data_array<const N: usize>(data: &[u8]) -> [u8; N] {
    let mut out = [0u8; N];
    out.copy_from_slice(data);
    out
}

fn main() {
    let bytes = [0u8; 16];
    let _ = data_array::<16>(&bytes);
}
"#,
    )]);
    let report = analyze(dir.path(), &full_config());
    let dead = definite_names(&report);
    assert!(
        !dead.iter().any(|n| n.ends_with("data_array")),
        "turbofish calls are uses; findings: {dead:?}"
    );
}

#[test]
fn module_used_via_serde_with_attribute_is_not_dead() {
    // Real-world case: `#[serde(with = "bigint_serde")]` — the module is
    // referenced only through a string path inside an attribute.
    let dir = write_project(&[(
        "src/main.rs",
        r#"
struct Wrapper {
    #[serde(with = "bigint_serde")]
    value: i64,
}

mod bigint_serde {
    pub fn serialize() {}
}

fn main() {
    let w = Wrapper { value: 1 };
    let _ = w.value;
}
"#,
    )]);
    let report = analyze(dir.path(), &full_config());
    let dead = definite_names(&report);
    assert!(
        !dead.iter().any(|n| n.ends_with("bigint_serde")),
        "serde(with = \"…\") is a use of the module; findings: {dead:?}"
    );
}

#[test]
fn module_used_via_qualified_path_is_not_dead() {
    // Real-world case: `mod commands;` + `commands::run_status(addr)` — a
    // lowercase left-hand path segment is a reference to the module.
    let dir = write_project(&[
        (
            "src/main.rs",
            r#"
mod commands;

fn main() {
    commands::run_status();
}
"#,
        ),
        ("src/commands.rs", "pub fn run_status() {}\n"),
    ]);
    let report = analyze(dir.path(), &full_config());
    let dead = definite_names(&report);
    assert!(
        !dead.iter().any(|n| n.ends_with("::commands")),
        "`commands::run_status()` references module `commands`; findings: {dead:?}"
    );
}

#[test]
fn module_used_via_use_statement_braces_is_not_dead() {
    // Real-world case: `use oom_audit::{audit_paths, Finding};` — the
    // `mod::{…}` form was not captured by the qualified-path pattern.
    let dir = write_project(&[
        (
            "src/main.rs",
            r#"
mod oom_audit;

use oom_audit::{audit_paths, Finding};

fn main() {
    let _f: Finding = audit_paths();
}
"#,
        ),
        (
            "src/oom_audit.rs",
            "pub struct Finding;\npub fn audit_paths() -> Finding {\n    Finding\n}\n",
        ),
    ]);
    let report = analyze(dir.path(), &full_config());
    let dead = definite_names(&report);
    assert!(
        !dead.iter().any(|n| n.ends_with("::oom_audit")),
        "`use oom_audit::{{…}}` references module `oom_audit`; findings: {dead:?}"
    );
}

#[test]
fn module_whose_members_are_alive_is_not_dead() {
    // Real-world case: `mod operator;` holding only `impl` blocks — the
    // module name never appears in a path, but its methods are called.
    let dir = write_project(&[
        (
            "src/main.rs",
            r#"
mod ctl;

fn main() {
    let c = ctl::Ctl;
    c.force_promote();
}
"#,
        ),
        ("src/ctl/mod.rs", "mod operator;\n\npub struct Ctl;\n"),
        (
            "src/ctl/operator.rs",
            "impl super::Ctl {\n    pub fn force_promote(&self) {}\n}\n",
        ),
    ]);
    let report = analyze(dir.path(), &full_config());
    let dead = definite_names(&report);
    assert!(
        !dead.iter().any(|n| n.ends_with("::operator")),
        "a module whose members are reachable is alive; findings: {dead:?}"
    );
}

#[test]
fn linker_retained_statics_are_entry_points() {
    // Real-world cases: `#[global_allocator]` and `#[used]`/`#[link_section]`
    // statics are kept alive by the runtime/linker, not by code references.
    let dir = write_project(&[(
        "src/main.rs",
        r#"
struct Alloc;

#[global_allocator]
static GLOBAL: Alloc = Alloc;

#[used]
#[link_section = "ferrosa:abi:v1"]
static ABI_MARKER: [u8; 4] = *b"abi1";

fn main() {}
"#,
    )]);
    let report = analyze(dir.path(), &full_config());
    let dead = definite_names(&report);
    assert!(
        !dead
            .iter()
            .any(|n| n.ends_with("::GLOBAL") || n.ends_with("::ABI_MARKER")),
        "linker-retained statics are entry points; findings: {dead:?}"
    );
}

#[test]
fn associated_type_in_impl_block_is_not_a_declaration() {
    // Real-world case: `impl Deref for S {{ type Target = T; }}` — the
    // associated type is not a standalone declaration to audit.
    let dir = write_project(&[(
        "src/main.rs",
        r#"
use std::ops::Deref;

struct Session;
struct SessionCore;

impl Deref for Session {
    type Target = SessionCore;

    fn deref(&self) -> &SessionCore {
        unimplemented!()
    }
}

fn main() {
    let _ = Session;
}
"#,
    )]);
    let (decls, _) = extract(dir.path(), &full_config());
    assert!(
        !decls
            .iter()
            .any(|d| d.kind == DeclarationKind::Type && d.name.ends_with("::Target")),
        "associated types inside impl blocks must not be audited; decls: {:?}",
        decls.iter().map(|d| &d.name).collect::<Vec<_>>()
    );
}
