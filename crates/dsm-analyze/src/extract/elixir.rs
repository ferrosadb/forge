use crate::extract::dot_parser::parse_dot;
use crate::extract::*;
use anyhow::Result;
use std::collections::HashSet;
use std::path::Path;

pub struct ElixirExtractor;

impl Extractor for ElixirExtractor {
    fn name(&self) -> &str {
        "elixir"
    }
    fn language(&self) -> Language {
        Language::Elixir
    }

    fn detect(&self, dir: &Path) -> bool {
        // Elixir projects
        dir.join("mix.exs").exists()
            // Erlang projects
            || dir.join("rebar.config").exists()
            || dir.join("rebar.config.script").exists()
            || dir.join("erlang.mk").exists()
            || dir.join("Emakefile").exists()
            || has_erl_files(dir)
    }

    fn extract(&self, dir: &Path, config: &ExtractConfig) -> Result<Vec<Edge>> {
        let is_elixir = dir.join("mix.exs").exists();

        if is_elixir {
            match config.level {
                GranularityLevel::Summary => extract_app_level(dir, config),
                GranularityLevel::Full => extract_via_xref(dir, config),
            }
        } else {
            // Pure Erlang project (rebar, erlang.mk, or bare .erl files)
            match config.level {
                GranularityLevel::Summary => extract_erlang_app_level(dir, config),
                GranularityLevel::Full => extract_from_erlang_source(dir, config),
            }
        }
    }

    fn detect_cross_language(&self, dir: &Path, config: &ExtractConfig) -> Result<Vec<Edge>> {
        if !config.detect_cross_language {
            return Ok(vec![]);
        }
        let mut edges = Vec::new();
        scan_nifs(dir, &mut edges)?;
        scan_ports(dir, &mut edges)?;
        scan_rustler_elixir_side(dir, &mut edges)?;
        Ok(edges)
    }
}

/// Check if directory contains .erl files at the top level or in src/
fn has_erl_files(dir: &Path) -> bool {
    for check_dir in &[dir.to_path_buf(), dir.join("src")] {
        if let Ok(entries) = std::fs::read_dir(check_dir) {
            for entry in entries.flatten() {
                if entry.path().extension().is_some_and(|e| e == "erl") {
                    return true;
                }
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Extractor (DSM edge extraction) — existing functionality
// ---------------------------------------------------------------------------

fn extract_app_level(dir: &Path, config: &ExtractConfig) -> Result<Vec<Edge>> {
    let mix_exs = dir.join("mix.exs");
    if !mix_exs.exists() {
        return Ok(vec![]);
    }

    let content = std::fs::read_to_string(&mix_exs)?;
    let dep_re = regex::Regex::new(r#"\{:(\w+)"#)?;

    let app_name = extract_app_name(&content).unwrap_or_else(|| "app".to_string());
    let mut edges = Vec::new();

    for cap in dep_re.captures_iter(&content) {
        let dep_name = &cap[1];
        if let Some(prefix) = &config.prefix_filter {
            if !dep_name.starts_with(prefix) {
                continue;
            }
        }
        edges.push(Edge {
            source: app_name.clone(),
            target: dep_name.to_string(),
            weight: 1.0,
            kind: EdgeKind::Import,
            cross_language: None,
        });
    }

    Ok(edges)
}

fn extract_via_xref(dir: &Path, config: &ExtractConfig) -> Result<Vec<Edge>> {
    let output = std::process::Command::new("mix")
        .args(["xref", "graph", "--format", "dot"])
        .current_dir(dir)
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let dot_content = String::from_utf8_lossy(&out.stdout);
            let mut edges = parse_dot(&dot_content)?;
            if let Some(prefix) = &config.prefix_filter {
                edges.retain(|e| e.source.starts_with(prefix) && e.target.starts_with(prefix));
            }
            Ok(edges)
        }
        _ => extract_from_source(dir, config),
    }
}

fn extract_from_source(dir: &Path, config: &ExtractConfig) -> Result<Vec<Edge>> {
    let alias_re = regex::Regex::new(r"alias\s+([A-Z][\w.]+)")?;
    let import_re = regex::Regex::new(r"import\s+([A-Z][\w.]+)")?;
    let use_re = regex::Regex::new(r"use\s+([A-Z][\w.]+)")?;
    let module_re = regex::Regex::new(r"defmodule\s+([A-Z][\w.]+)")?;

    let mut edges = Vec::new();

    for entry in super::java::walkdir(dir, "ex") {
        let content = std::fs::read_to_string(&entry)?;
        let source = module_re
            .captures(&content)
            .map(|c| c[1].to_string())
            .unwrap_or_else(|| {
                entry
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string()
            });

        for re in [&alias_re, &import_re, &use_re] {
            for cap in re.captures_iter(&content) {
                let target = cap[1].to_string();
                if let Some(prefix) = &config.prefix_filter {
                    if !target.starts_with(prefix) {
                        continue;
                    }
                }
                if target != source {
                    edges.push(Edge {
                        source: source.clone(),
                        target,
                        weight: 1.0,
                        kind: EdgeKind::Import,
                        cross_language: None,
                    });
                }
            }
        }
    }

    Ok(edges)
}

fn extract_app_name(mix_content: &str) -> Option<String> {
    let re = regex::Regex::new(r"app:\s*:(\w+)").ok()?;
    re.captures(mix_content).map(|c| c[1].to_string())
}

/// Extract app-level dependencies from rebar.config for Erlang projects.
fn extract_erlang_app_level(dir: &Path, config: &ExtractConfig) -> Result<Vec<Edge>> {
    let rebar_path = dir.join("rebar.config");
    if !rebar_path.exists() {
        // No rebar.config — fall through to source-level extraction
        return extract_from_erlang_source(dir, config);
    }

    let content = std::fs::read_to_string(&rebar_path)?;

    // Extract app name from .app.src or directory name
    let app_name = extract_erlang_app_name(dir).unwrap_or_else(|| {
        dir.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string()
    });

    // Parse deps from rebar.config: {dep_name, ...} patterns inside deps list
    let dep_re = regex::Regex::new(r"\{(\w+),\s*\{")?;
    let mut edges = Vec::new();

    for cap in dep_re.captures_iter(&content) {
        let dep_name = &cap[1];
        if let Some(prefix) = &config.prefix_filter {
            if !dep_name.starts_with(prefix) {
                continue;
            }
        }
        edges.push(Edge {
            source: app_name.clone(),
            target: dep_name.to_string(),
            weight: 1.0,
            kind: EdgeKind::Import,
            cross_language: None,
        });
    }

    Ok(edges)
}

/// Extract Erlang app name from src/<app>.app.src or directory name.
fn extract_erlang_app_name(dir: &Path) -> Option<String> {
    let src_dir = dir.join("src");
    if let Ok(entries) = std::fs::read_dir(&src_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.ends_with(".app.src") {
                return Some(name_str.trim_end_matches(".app.src").to_string());
            }
        }
    }
    dir.file_name().map(|n| n.to_string_lossy().to_string())
}

/// OTP/stdlib modules to filter out from dependency edges.
fn erlang_stdlib_modules() -> HashSet<&'static str> {
    [
        // kernel
        "application",
        "code",
        "erl_ddll",
        "error_handler",
        "error_logger",
        "file",
        "gen_tcp",
        "gen_udp",
        "gen_sctp",
        "global",
        "global_group",
        "inet",
        "logger",
        "net_adm",
        "net_kernel",
        "os",
        "rpc",
        "seq_trace",
        // stdlib
        "binary",
        "calendar",
        "dict",
        "digraph",
        "ets",
        "filelib",
        "filename",
        "gb_sets",
        "gb_trees",
        "gen_event",
        "gen_fsm",
        "gen_server",
        "gen_statem",
        "io",
        "io_lib",
        "lists",
        "maps",
        "math",
        "orddict",
        "ordsets",
        "proplists",
        "queue",
        "rand",
        "re",
        "sets",
        "string",
        "supervisor",
        "sys",
        "timer",
        "unicode",
        "uri_string",
        // erlang BIFs
        "erlang",
        "erts_internal",
        // crypto / ssl
        "crypto",
        "ssl",
        "public_key",
        // common_test / eunit
        "ct",
        "eunit",
        // mnesia
        "mnesia",
        // inets
        "httpc",
        "httpd",
        "inets",
        // compiler / tools
        "compile",
        "beam_lib",
        // proc_lib / gen
        "proc_lib",
        "gen",
        // other common
        "lager",
        "meck",
        "eqc",
        "proper",
    ]
    .into_iter()
    .collect()
}

/// Extract module-level dependency edges from Erlang .erl source files.
fn extract_from_erlang_source(dir: &Path, config: &ExtractConfig) -> Result<Vec<Edge>> {
    let module_re = regex::Regex::new(r"-module\(\s*'?(\w+)'?\s*\)")?;
    let remote_call_re = regex::Regex::new(r"\b([a-z_][a-z_0-9]*):[a-z_][a-z_0-9]*\s*\(")?;
    let behaviour_re = regex::Regex::new(r"-behaviou?r\(\s*(\w+)\s*\)")?;
    let include_re = regex::Regex::new(r#"-include\(\s*"([^"]+)"\s*\)"#)?;
    let include_lib_re = regex::Regex::new(r#"-include_lib\(\s*"([^"]+)"\s*\)"#)?;

    let stdlib = erlang_stdlib_modules();

    // Collect all local module names so we can filter to project-internal edges
    let erl_files = super::java::walkdir(dir, "erl");
    let mut local_modules: HashSet<String> = HashSet::new();
    for entry in &erl_files {
        let content = std::fs::read_to_string(entry)?;
        if let Some(cap) = module_re.captures(&content) {
            local_modules.insert(cap[1].to_string());
        }
    }

    let mut edges = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();

    for entry in &erl_files {
        let content = std::fs::read_to_string(entry)?;

        let source = module_re
            .captures(&content)
            .map(|c| c[1].to_string())
            .unwrap_or_else(|| {
                entry
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string()
            });

        if let Some(prefix) = &config.prefix_filter {
            if !source.starts_with(prefix) {
                continue;
            }
        }

        // Remote calls: module:function(
        for cap in remote_call_re.captures_iter(&content) {
            let target = cap[1].to_string();
            if stdlib.contains(target.as_str()) {
                continue;
            }
            if !local_modules.contains(&target) {
                continue;
            }
            if target != source && seen.insert((source.clone(), target.clone())) {
                edges.push(Edge {
                    source: source.clone(),
                    target,
                    weight: 1.0,
                    kind: EdgeKind::Call,
                    cross_language: None,
                });
            }
        }

        // Behaviour declarations
        for cap in behaviour_re.captures_iter(&content) {
            let target = cap[1].to_string();
            if stdlib.contains(target.as_str()) {
                continue;
            }
            if target != source && seen.insert((source.clone(), target.clone())) {
                edges.push(Edge {
                    source: source.clone(),
                    target,
                    weight: 1.0,
                    kind: EdgeKind::Import,
                    cross_language: None,
                });
            }
        }

        // Include directives — map to module name from header filename
        for re in [&include_re, &include_lib_re] {
            for cap in re.captures_iter(&content) {
                let header = &cap[1];
                // Extract module-like name from header path: "riak_core_vnode.hrl" -> "riak_core_vnode"
                let target = std::path::Path::new(header)
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                if stdlib.contains(target.as_str()) || target == "eunit" || target == "logger" {
                    continue;
                }
                if target != source && seen.insert((source.clone(), target.clone())) {
                    edges.push(Edge {
                        source: source.clone(),
                        target,
                        weight: 0.5,
                        kind: EdgeKind::Import,
                        cross_language: None,
                    });
                }
            }
        }
    }

    Ok(edges)
}

fn scan_nifs(dir: &Path, edges: &mut Vec<Edge>) -> Result<()> {
    let nif_re = regex::Regex::new(r":erlang\.load_nif|@on_load")?;
    let module_re = regex::Regex::new(r"defmodule\s+([A-Z][\w.]+)")?;

    for entry in super::java::walkdir(dir, "ex") {
        let content = std::fs::read_to_string(&entry)?;
        if nif_re.is_match(&content) {
            let source = module_re
                .captures(&content)
                .map(|c| c[1].to_string())
                .unwrap_or_default();

            edges.push(Edge {
                source,
                target: "native:nif".to_string(),
                weight: 1.0,
                kind: EdgeKind::Ffi,
                cross_language: Some(CrossLanguageEdge {
                    source_lang: Language::Elixir,
                    target_lang: Language::C,
                    mechanism: FfiMechanism::Nif,
                }),
            });
        }
    }
    Ok(())
}

fn scan_ports(dir: &Path, edges: &mut Vec<Edge>) -> Result<()> {
    let port_re = regex::Regex::new(r"Port\.open|System\.cmd")?;
    let module_re = regex::Regex::new(r"defmodule\s+([A-Z][\w.]+)")?;

    for entry in super::java::walkdir(dir, "ex") {
        let content = std::fs::read_to_string(&entry)?;
        if port_re.is_match(&content) {
            let source = module_re
                .captures(&content)
                .map(|c| c[1].to_string())
                .unwrap_or_default();

            edges.push(Edge {
                source,
                target: "external:port".to_string(),
                weight: 1.0,
                kind: EdgeKind::Ffi,
                cross_language: Some(CrossLanguageEdge {
                    source_lang: Language::Elixir,
                    target_lang: Language::Unknown("external".to_string()),
                    mechanism: FfiMechanism::Port,
                }),
            });
        }
    }
    Ok(())
}

fn scan_rustler_elixir_side(dir: &Path, edges: &mut Vec<Edge>) -> Result<()> {
    let mix_exs = dir.join("mix.exs");
    if mix_exs.exists() {
        let content = std::fs::read_to_string(&mix_exs)?;
        if content.contains("rustler") {
            edges.push(Edge {
                source: "elixir:app".to_string(),
                target: "rust:nif_crate".to_string(),
                weight: 1.0,
                kind: EdgeKind::Ffi,
                cross_language: Some(CrossLanguageEdge {
                    source_lang: Language::Elixir,
                    target_lang: Language::Rust,
                    mechanism: FfiMechanism::Nif,
                }),
            });
        }
    }

    let native_dir = dir.join("native");
    if native_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&native_dir) {
            for entry in entries.flatten() {
                if entry.path().join("Cargo.toml").exists() {
                    let crate_name = entry.file_name().to_string_lossy().to_string();
                    edges.push(Edge {
                        source: "elixir:app".to_string(),
                        target: format!("rust:{}", crate_name),
                        weight: 1.0,
                        kind: EdgeKind::Ffi,
                        cross_language: Some(CrossLanguageEdge {
                            source_lang: Language::Elixir,
                            target_lang: Language::Rust,
                            mechanism: FfiMechanism::Nif,
                        }),
                    });
                }
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// DeclarationExtractor — dead-code analysis for Elixir + Erlang
// ---------------------------------------------------------------------------

impl DeclarationExtractor for ElixirExtractor {
    fn language(&self) -> Language {
        Language::Elixir
    }

    fn extract_declarations(&self, dir: &Path, config: &ExtractConfig) -> Result<Vec<Declaration>> {
        let mut declarations = Vec::new();

        // Extract from Elixir files (.ex, .exs)
        for entry in walkdir_elixir(dir) {
            let rel = entry
                .strip_prefix(dir)
                .unwrap_or(&entry)
                .to_string_lossy()
                .to_string();

            if let Some(pf) = &config.prefix_filter {
                if !rel.starts_with(pf) {
                    continue;
                }
            }

            let content = std::fs::read_to_string(&entry)?;
            extract_elixir_declarations(&content, &rel, &mut declarations);
        }

        // Extract from Erlang files (.erl, .hrl)
        for entry in walkdir_erlang(dir) {
            let rel = entry
                .strip_prefix(dir)
                .unwrap_or(&entry)
                .to_string_lossy()
                .to_string();

            if let Some(pf) = &config.prefix_filter {
                if !rel.starts_with(pf) {
                    continue;
                }
            }

            let content = std::fs::read_to_string(&entry)?;
            extract_erlang_declarations(&content, &rel, &mut declarations);
        }

        Ok(declarations)
    }

    fn extract_references(
        &self,
        dir: &Path,
        config: &ExtractConfig,
    ) -> Result<Vec<SymbolReference>> {
        let mut references = Vec::new();

        // Extract references from Elixir files
        for entry in walkdir_elixir(dir) {
            let rel = entry
                .strip_prefix(dir)
                .unwrap_or(&entry)
                .to_string_lossy()
                .to_string();

            if let Some(pf) = &config.prefix_filter {
                if !rel.starts_with(pf) {
                    continue;
                }
            }

            let content = std::fs::read_to_string(&entry)?;
            extract_elixir_references(&content, &rel, &mut references);
        }

        // Extract references from Erlang files
        for entry in walkdir_erlang(dir) {
            let rel = entry
                .strip_prefix(dir)
                .unwrap_or(&entry)
                .to_string_lossy()
                .to_string();

            if let Some(pf) = &config.prefix_filter {
                if !rel.starts_with(pf) {
                    continue;
                }
            }

            let content = std::fs::read_to_string(&entry)?;
            extract_erlang_references(&content, &rel, &mut references);
        }

        Ok(references)
    }
}

// ---------------------------------------------------------------------------
// Elixir declaration extraction
// ---------------------------------------------------------------------------

fn extract_elixir_declarations(content: &str, file: &str, declarations: &mut Vec<Declaration>) {
    let module_re = regex::Regex::new(r"(?m)^\s*defmodule\s+([A-Z][\w.]+)").unwrap();
    let def_re = regex::Regex::new(r"(?m)^\s*(def|defp|defmacro|defmacrop)\s+(\w+[?!]?)").unwrap();
    let type_re = regex::Regex::new(r"(?m)^\s*@(type|typep|opaque)\s+(\w+)").unwrap();
    let struct_re = regex::Regex::new(r"(?m)^\s*defstruct\b").unwrap();
    let attr_re = regex::Regex::new(r"(?m)^\s*@(\w+)\s+.+").unwrap();
    let behaviour_re = regex::Regex::new(r"(?m)^\s*@behaviour\s+(\S+)").unwrap();
    let test_re = regex::Regex::new(r#"(?m)^\s*test\s+"#).unwrap();

    let line_number_of =
        |byte_offset: usize| -> usize { content[..byte_offset].matches('\n').count() + 1 };

    // Detect current module name(s)
    let modules: Vec<(String, usize)> = module_re
        .captures_iter(content)
        .map(|cap| {
            let name = cap[1].to_string();
            let line = line_number_of(cap.get(0).unwrap().start());
            (name, line)
        })
        .collect();

    // Get the module name for a given byte offset (use the most recent defmodule before it)
    let module_at = |byte_offset: usize| -> String {
        let line = content[..byte_offset].matches('\n').count() + 1;
        let mut best = String::new();
        for (name, mod_line) in &modules {
            if *mod_line <= line {
                best = name.clone();
            }
        }
        best
    };

    // Check if file is a test file
    let is_test_file = file.contains("/test/") || file.ends_with("_test.exs");

    // Check if any behaviour is declared (for entry point detection)
    let has_genserver = content.contains("GenServer")
        && (behaviour_re.is_match(content) || content.contains("use GenServer"));
    let has_supervisor = content.contains("Supervisor")
        && (behaviour_re.is_match(content) || content.contains("use Supervisor"));
    let has_application = content.contains("Application")
        && (behaviour_re.is_match(content) || content.contains("use Application"));
    let is_phoenix_router = content.contains("use") && content.contains("Router");
    let is_liveview = content.contains("use")
        && (content.contains("LiveView") || content.contains("LiveComponent"));

    // Module declarations
    for cap in module_re.captures_iter(content) {
        let name = cap[1].to_string();
        let line = line_number_of(cap.get(0).unwrap().start());
        declarations.push(Declaration {
            name: name.clone(),
            kind: DeclarationKind::Module,
            visibility: Visibility::Public,
            file: file.to_string(),
            line,
            language: Language::Elixir,
            is_entry_point: false,
            entry_point_reason: None,
        });
    }

    // Function/macro declarations
    for cap in def_re.captures_iter(content) {
        let keyword = &cap[1];
        let func_name = cap[2].to_string();
        let byte_offset = cap.get(0).unwrap().start();
        let line = line_number_of(byte_offset);
        let module = module_at(byte_offset);

        let visibility = match keyword {
            "def" | "defmacro" => Visibility::Public,
            "defp" | "defmacrop" => Visibility::Private,
            _ => Visibility::Public,
        };

        let kind = match keyword {
            "defmacro" | "defmacrop" => DeclarationKind::Function,
            _ => DeclarationKind::Function,
        };

        let fqn = if module.is_empty() {
            func_name.clone()
        } else {
            format!("{}::{}", module, func_name)
        };

        // Determine entry points
        let (is_ep, ep_reason) = elixir_entry_point(
            &func_name,
            has_genserver,
            has_supervisor,
            has_application,
            is_phoenix_router,
            is_liveview,
            is_test_file,
        );

        declarations.push(Declaration {
            name: fqn,
            kind,
            visibility,
            file: file.to_string(),
            line,
            language: Language::Elixir,
            is_entry_point: is_ep,
            entry_point_reason: ep_reason,
        });
    }

    // Type declarations (@type, @typep, @opaque)
    for cap in type_re.captures_iter(content) {
        let type_keyword = &cap[1];
        let type_name = cap[2].to_string();
        let byte_offset = cap.get(0).unwrap().start();
        let line = line_number_of(byte_offset);
        let module = module_at(byte_offset);

        let visibility = match type_keyword {
            "typep" => Visibility::Private,
            _ => Visibility::Public,
        };

        let fqn = if module.is_empty() {
            type_name
        } else {
            format!("{}::{}", module, type_name)
        };

        declarations.push(Declaration {
            name: fqn,
            kind: DeclarationKind::Type,
            visibility,
            file: file.to_string(),
            line,
            language: Language::Elixir,
            is_entry_point: false,
            entry_point_reason: None,
        });
    }

    // Struct declarations
    for m in struct_re.find_iter(content) {
        let byte_offset = m.start();
        let line = line_number_of(byte_offset);
        let module = module_at(byte_offset);

        if !module.is_empty() {
            declarations.push(Declaration {
                name: format!("{}::__struct__", module),
                kind: DeclarationKind::Type,
                visibility: Visibility::Public,
                file: file.to_string(),
                line,
                language: Language::Elixir,
                is_entry_point: false,
                entry_point_reason: None,
            });
        }
    }

    // Module attributes as constants (skip well-known meta-attributes)
    let skip_attrs: HashSet<&str> = [
        "moduledoc",
        "doc",
        "spec",
        "type",
        "typep",
        "opaque",
        "callback",
        "macrocallback",
        "behaviour",
        "behavior",
        "impl",
        "derive",
        "enforce_keys",
        "optional_callbacks",
        "dialyzer",
        "compile",
        "deprecated",
        "on_load",
        "before_compile",
        "after_compile",
        "external_resource",
    ]
    .iter()
    .copied()
    .collect();

    for cap in attr_re.captures_iter(content) {
        let attr_name = &cap[1];
        if skip_attrs.contains(attr_name) {
            continue;
        }
        let byte_offset = cap.get(0).unwrap().start();
        let line = line_number_of(byte_offset);
        let module = module_at(byte_offset);

        let fqn = if module.is_empty() {
            format!("@{}", attr_name)
        } else {
            format!("{}::@{}", module, attr_name)
        };

        declarations.push(Declaration {
            name: fqn,
            kind: DeclarationKind::Constant,
            visibility: Visibility::Internal,
            file: file.to_string(),
            line,
            language: Language::Elixir,
            is_entry_point: false,
            entry_point_reason: None,
        });
    }

    // Test blocks as entry points
    for m in test_re.find_iter(content) {
        let byte_offset = m.start();
        let line = line_number_of(byte_offset);
        let module = module_at(byte_offset);

        let fqn = if module.is_empty() {
            format!("test_line_{}", line)
        } else {
            format!("{}::test_line_{}", module, line)
        };

        declarations.push(Declaration {
            name: fqn,
            kind: DeclarationKind::Function,
            visibility: Visibility::Public,
            file: file.to_string(),
            line,
            language: Language::Elixir,
            is_entry_point: true,
            entry_point_reason: Some("test block".to_string()),
        });
    }
}

fn elixir_entry_point(
    func_name: &str,
    has_genserver: bool,
    has_supervisor: bool,
    has_application: bool,
    is_phoenix_router: bool,
    is_liveview: bool,
    is_test_file: bool,
) -> (bool, Option<String>) {
    // Application callbacks
    if has_application && (func_name == "start" || func_name == "stop") {
        return (true, Some("Application callback".to_string()));
    }

    // GenServer callbacks
    if has_genserver
        && matches!(
            func_name,
            "init"
                | "handle_call"
                | "handle_cast"
                | "handle_info"
                | "handle_continue"
                | "terminate"
                | "code_change"
                | "format_status"
                | "start_link"
                | "start"
        )
    {
        return (true, Some("GenServer callback".to_string()));
    }

    // Supervisor callbacks
    if has_supervisor && (func_name == "init" || func_name == "start_link") {
        return (true, Some("Supervisor callback".to_string()));
    }

    // Phoenix router actions
    if is_phoenix_router {
        return (true, Some("Phoenix router".to_string()));
    }

    // LiveView callbacks
    if is_liveview
        && matches!(
            func_name,
            "mount" | "render" | "handle_event" | "handle_info" | "handle_params" | "update"
        )
    {
        return (true, Some("LiveView callback".to_string()));
    }

    // Common start functions
    if matches!(func_name, "start_link" | "start" | "child_spec") {
        return (true, Some("OTP start function".to_string()));
    }

    // Test file functions
    if is_test_file
        && (func_name.starts_with("test_") || func_name == "setup" || func_name == "setup_all")
    {
        return (true, Some("test function".to_string()));
    }

    (false, None)
}

// ---------------------------------------------------------------------------
// Erlang declaration extraction
// ---------------------------------------------------------------------------

fn extract_erlang_declarations(content: &str, file: &str, declarations: &mut Vec<Declaration>) {
    let module_re = regex::Regex::new(r"(?m)^-module\((\w+)\)\.").unwrap();
    let export_re = regex::Regex::new(r"(?m)^-export\(\[([^\]]*)\]\)\.").unwrap();
    let func_re = regex::Regex::new(r"(?m)^(\w+)\s*\([^)]*\)\s*(?:when\s+.+)?\s*->").unwrap();
    let type_re = regex::Regex::new(r"(?m)^-(type|opaque)\s+(\w+)\(").unwrap();
    let record_re = regex::Regex::new(r"(?m)^-record\((\w+)\s*,").unwrap();
    let define_re = regex::Regex::new(r"(?m)^-define\((\w+)").unwrap();
    let behaviour_re = regex::Regex::new(r"(?m)^-behaviou?r\((\w+)\)\.").unwrap();
    let callback_re = regex::Regex::new(r"(?m)^-callback\s+(\w+)\(").unwrap();
    let export_entry_re = regex::Regex::new(r"(\w+)/(\d+)").unwrap();

    let line_number_of =
        |byte_offset: usize| -> usize { content[..byte_offset].matches('\n').count() + 1 };

    // Get module name
    let module_name = module_re
        .captures(content)
        .map(|c| c[1].to_string())
        .unwrap_or_default();

    // Build set of exported functions
    let mut exported: HashSet<String> = HashSet::new();
    for cap in export_re.captures_iter(content) {
        let export_list = &cap[1];
        for entry in export_entry_re.captures_iter(export_list) {
            let func = entry[1].to_string();
            let arity = &entry[2];
            exported.insert(format!("{}/{}", func, arity));
            // Also store just the name for simpler matching
            exported.insert(func);
        }
    }

    // Check for OTP behaviours
    let behaviours: Vec<String> = behaviour_re
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect();

    let has_gen_server = behaviours
        .iter()
        .any(|b| b == "gen_server" || b == "gen_statem");
    let has_supervisor = behaviours.iter().any(|b| b == "supervisor");
    let has_application = behaviours.iter().any(|b| b == "application");
    let has_gen_event = behaviours.iter().any(|b| b == "gen_event");

    // Check if test file
    let is_test_file =
        file.ends_with("_SUITE.erl") || file.ends_with("_test.erl") || file.contains("/test/");

    // Module declaration
    if !module_name.is_empty() {
        if let Some(cap) = module_re.captures(content) {
            let line = line_number_of(cap.get(0).unwrap().start());
            declarations.push(Declaration {
                name: module_name.clone(),
                kind: DeclarationKind::Module,
                visibility: Visibility::Public,
                file: file.to_string(),
                line,
                language: Language::Erlang,
                is_entry_point: false,
                entry_point_reason: None,
            });
        }
    }

    // Function declarations
    // Track which function names we've already seen to avoid duplicates from multi-clause functions
    let mut seen_functions: HashSet<String> = HashSet::new();
    for cap in func_re.captures_iter(content) {
        let func_name = cap[1].to_string();

        // Skip Erlang keywords that look like function definitions
        if matches!(func_name.as_str(), "if" | "case" | "receive" | "try") {
            continue;
        }

        if seen_functions.contains(&func_name) {
            continue;
        }
        seen_functions.insert(func_name.clone());

        let byte_offset = cap.get(0).unwrap().start();
        let line = line_number_of(byte_offset);

        // Count arity from the first clause
        let args_start = content[byte_offset..].find('(').unwrap_or(0) + byte_offset + 1;
        let args_end = content[args_start..].find(')').unwrap_or(0) + args_start;
        let args_text = &content[args_start..args_end];
        let arity = if args_text.trim().is_empty() {
            0
        } else {
            count_erlang_arity(args_text)
        };

        let is_exported =
            exported.contains(&func_name) || exported.contains(&format!("{}/{}", func_name, arity));
        let visibility = if is_exported {
            Visibility::Exported
        } else {
            Visibility::Private
        };

        let fqn = if module_name.is_empty() {
            func_name.clone()
        } else {
            format!("{}::{}", module_name, func_name)
        };

        let (is_ep, ep_reason) = erlang_entry_point(
            &func_name,
            is_exported,
            has_gen_server,
            has_supervisor,
            has_application,
            has_gen_event,
            is_test_file,
        );

        declarations.push(Declaration {
            name: fqn,
            kind: DeclarationKind::Function,
            visibility,
            file: file.to_string(),
            line,
            language: Language::Erlang,
            is_entry_point: is_ep,
            entry_point_reason: ep_reason,
        });
    }

    // Type declarations
    for cap in type_re.captures_iter(content) {
        let type_name = cap[2].to_string();
        let byte_offset = cap.get(0).unwrap().start();
        let line = line_number_of(byte_offset);

        let fqn = if module_name.is_empty() {
            type_name
        } else {
            format!("{}::{}", module_name, type_name)
        };

        declarations.push(Declaration {
            name: fqn,
            kind: DeclarationKind::Type,
            visibility: Visibility::Public,
            file: file.to_string(),
            line,
            language: Language::Erlang,
            is_entry_point: false,
            entry_point_reason: None,
        });
    }

    // Record declarations
    for cap in record_re.captures_iter(content) {
        let rec_name = cap[1].to_string();
        let byte_offset = cap.get(0).unwrap().start();
        let line = line_number_of(byte_offset);

        let fqn = if module_name.is_empty() {
            format!("#{}", rec_name)
        } else {
            format!("{}::#{}", module_name, rec_name)
        };

        declarations.push(Declaration {
            name: fqn,
            kind: DeclarationKind::Type,
            visibility: Visibility::Public,
            file: file.to_string(),
            line,
            language: Language::Erlang,
            is_entry_point: false,
            entry_point_reason: None,
        });
    }

    // Macro definitions (-define)
    for cap in define_re.captures_iter(content) {
        let macro_name = cap[1].to_string();
        let byte_offset = cap.get(0).unwrap().start();
        let line = line_number_of(byte_offset);

        let fqn = if module_name.is_empty() {
            format!("?{}", macro_name)
        } else {
            format!("{}::?{}", module_name, macro_name)
        };

        declarations.push(Declaration {
            name: fqn,
            kind: DeclarationKind::Constant,
            visibility: Visibility::Public,
            file: file.to_string(),
            line,
            language: Language::Erlang,
            is_entry_point: false,
            entry_point_reason: None,
        });
    }

    // Callback declarations
    for cap in callback_re.captures_iter(content) {
        let cb_name = cap[1].to_string();
        let byte_offset = cap.get(0).unwrap().start();
        let line = line_number_of(byte_offset);

        let fqn = if module_name.is_empty() {
            format!("callback::{}", cb_name)
        } else {
            format!("{}::callback::{}", module_name, cb_name)
        };

        declarations.push(Declaration {
            name: fqn,
            kind: DeclarationKind::Trait,
            visibility: Visibility::Public,
            file: file.to_string(),
            line,
            language: Language::Erlang,
            is_entry_point: false,
            entry_point_reason: None,
        });
    }
}

/// Count arity in an Erlang function argument string.
/// Handles nested parens/brackets but not full parsing.
fn count_erlang_arity(args: &str) -> usize {
    if args.trim().is_empty() {
        return 0;
    }
    let mut depth = 0;
    let mut count = 1;
    for ch in args.chars() {
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => count += 1,
            _ => {}
        }
    }
    count
}

fn erlang_entry_point(
    func_name: &str,
    is_exported: bool,
    has_gen_server: bool,
    has_supervisor: bool,
    has_application: bool,
    has_gen_event: bool,
    is_test_file: bool,
) -> (bool, Option<String>) {
    // Application callbacks
    if has_application && matches!(func_name, "start" | "stop" | "prep_stop" | "config_change") {
        return (true, Some("application callback".to_string()));
    }

    // gen_server callbacks
    if has_gen_server
        && matches!(
            func_name,
            "init"
                | "handle_call"
                | "handle_cast"
                | "handle_info"
                | "handle_continue"
                | "terminate"
                | "code_change"
                | "format_status"
        )
    {
        return (true, Some("gen_server callback".to_string()));
    }

    // supervisor callbacks
    if has_supervisor && func_name == "init" {
        return (true, Some("supervisor callback".to_string()));
    }

    // gen_event callbacks
    if has_gen_event
        && matches!(
            func_name,
            "init" | "handle_event" | "handle_call" | "handle_info" | "terminate" | "code_change"
        )
    {
        return (true, Some("gen_event callback".to_string()));
    }

    // Common OTP start functions
    if is_exported && matches!(func_name, "start" | "start_link") {
        return (true, Some("OTP start function".to_string()));
    }

    // Escript entry point
    if func_name == "main" && is_exported {
        return (true, Some("escript entry point".to_string()));
    }

    // Test functions
    if is_test_file && func_name.ends_with("_test") {
        return (true, Some("test function".to_string()));
    }

    // Common Test suite callbacks
    if is_test_file
        && matches!(
            func_name,
            "all"
                | "init_per_suite"
                | "end_per_suite"
                | "init_per_testcase"
                | "end_per_testcase"
                | "init_per_group"
                | "end_per_group"
                | "groups"
                | "suite"
        )
    {
        return (true, Some("Common Test callback".to_string()));
    }

    (false, None)
}

// ---------------------------------------------------------------------------
// Elixir reference extraction
// ---------------------------------------------------------------------------

fn extract_elixir_references(content: &str, file: &str, references: &mut Vec<SymbolReference>) {
    let remote_call_re = regex::Regex::new(r"([A-Z][\w.]+)\.(\w+[?!]?)\s*\(").unwrap();
    let local_call_re = regex::Regex::new(r"\b(\w+[?!]?)\s*\(").unwrap();
    let struct_re = regex::Regex::new(r"%([A-Z][\w.]+)\{").unwrap();
    let behaviour_re = regex::Regex::new(r"@behaviour\s+(\S+)").unwrap();
    let use_re = regex::Regex::new(r"(?m)^\s*use\s+([A-Z][\w.]+)").unwrap();
    let import_re = regex::Regex::new(r"(?m)^\s*import\s+([A-Z][\w.]+)").unwrap();
    let alias_re = regex::Regex::new(r"(?m)^\s*alias\s+([A-Z][\w.]+)").unwrap();
    let capture_re = regex::Regex::new(r"&([A-Z][\w.]+)\.(\w+)/(\d+)").unwrap();
    let local_capture_re = regex::Regex::new(r"&(\w+)/(\d+)").unwrap();

    // Elixir keywords to skip in local call matching
    let skip_keywords: HashSet<&str> = [
        "if",
        "unless",
        "case",
        "cond",
        "with",
        "for",
        "try",
        "receive",
        "raise",
        "reraise",
        "throw",
        "quote",
        "unquote",
        "fn",
        "do",
        "end",
        "def",
        "defp",
        "defmodule",
        "defmacro",
        "defmacrop",
        "defstruct",
        "defprotocol",
        "defimpl",
        "defguard",
        "defguardp",
        "defexception",
        "defdelegate",
        "defoverridable",
        "when",
        "and",
        "or",
        "not",
        "in",
        "assert",
        "refute",
        "describe",
        "test",
        "setup",
        "require",
        "use",
        "import",
        "alias",
    ]
    .iter()
    .copied()
    .collect();

    for (line_idx, line) in content.lines().enumerate() {
        let line_num = line_idx + 1;
        let trimmed = line.trim();

        // Skip comments
        if trimmed.starts_with('#') {
            continue;
        }

        // Remote calls: Module.function(
        for cap in remote_call_re.captures_iter(line) {
            let module = &cap[1];
            let func = &cap[2];
            // Fully qualified reference
            references.push(SymbolReference {
                from_file: file.to_string(),
                to_symbol: format!("{}::{}", module, func),
                line: line_num,
            });
            // Also record the module reference
            references.push(SymbolReference {
                from_file: file.to_string(),
                to_symbol: module.to_string(),
                line: line_num,
            });
        }

        // Struct construction: %ModuleName{
        for cap in struct_re.captures_iter(line) {
            let module = &cap[1];
            references.push(SymbolReference {
                from_file: file.to_string(),
                to_symbol: format!("{}::__struct__", module),
                line: line_num,
            });
            references.push(SymbolReference {
                from_file: file.to_string(),
                to_symbol: module.to_string(),
                line: line_num,
            });
        }

        // Function capture: &Module.func/arity
        for cap in capture_re.captures_iter(line) {
            let module = &cap[1];
            let func = &cap[2];
            references.push(SymbolReference {
                from_file: file.to_string(),
                to_symbol: format!("{}::{}", module, func),
                line: line_num,
            });
        }

        // Local function capture: &func/arity
        for cap in local_capture_re.captures_iter(line) {
            let func = &cap[1];
            if !skip_keywords.contains(func) {
                references.push(SymbolReference {
                    from_file: file.to_string(),
                    to_symbol: func.to_string(),
                    line: line_num,
                });
            }
        }

        // @behaviour ModuleName
        for cap in behaviour_re.captures_iter(line) {
            references.push(SymbolReference {
                from_file: file.to_string(),
                to_symbol: cap[1].to_string(),
                line: line_num,
            });
        }

        // use Module
        for cap in use_re.captures_iter(line) {
            references.push(SymbolReference {
                from_file: file.to_string(),
                to_symbol: cap[1].to_string(),
                line: line_num,
            });
        }

        // import Module
        for cap in import_re.captures_iter(line) {
            references.push(SymbolReference {
                from_file: file.to_string(),
                to_symbol: cap[1].to_string(),
                line: line_num,
            });
        }

        // alias Module
        for cap in alias_re.captures_iter(line) {
            references.push(SymbolReference {
                from_file: file.to_string(),
                to_symbol: cap[1].to_string(),
                line: line_num,
            });
        }

        // Local calls: function_name(
        for cap in local_call_re.captures_iter(line) {
            let name = &cap[1];
            if !skip_keywords.contains(name) {
                // Skip if it starts with uppercase (it's a module, not a function)
                if !name.starts_with(|c: char| c.is_uppercase()) {
                    references.push(SymbolReference {
                        from_file: file.to_string(),
                        to_symbol: name.to_string(),
                        line: line_num,
                    });
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Erlang reference extraction
// ---------------------------------------------------------------------------

fn extract_erlang_references(content: &str, file: &str, references: &mut Vec<SymbolReference>) {
    let remote_call_re = regex::Regex::new(r"(\w+):(\w+)\s*\(").unwrap();
    let local_call_re = regex::Regex::new(r"\b(\w+)\s*\(").unwrap();
    let macro_use_re = regex::Regex::new(r"\?(\w+)").unwrap();
    let record_use_re = regex::Regex::new(r"#(\w+)\{").unwrap();
    let type_ref_re = regex::Regex::new(r"(\w+)\s*\(\)").unwrap();
    let spec_re = regex::Regex::new(r"(?m)^-spec\s+(\w+)\(").unwrap();

    // Erlang keywords to skip
    let skip_keywords: HashSet<&str> = [
        "if", "case", "receive", "try", "catch", "begin", "end", "fun", "when", "of", "after",
        "throw", "error", "exit", "andalso", "orelse", "not", "and", "or", "band", "bor", "bxor",
        "bnot", "bsl", "bsr", "div", "rem",
    ]
    .iter()
    .copied()
    .collect();

    // Get module name for fully qualifying local references
    let module_re = regex::Regex::new(r"(?m)^-module\((\w+)\)\.").unwrap();
    let module_name = module_re
        .captures(content)
        .map(|c| c[1].to_string())
        .unwrap_or_default();

    for (line_idx, line) in content.lines().enumerate() {
        let line_num = line_idx + 1;
        let trimmed = line.trim();

        // Skip comments
        if trimmed.starts_with('%') {
            continue;
        }

        // Skip directives (they're declarations, not references)
        if trimmed.starts_with('-') && !trimmed.starts_with("-spec") {
            continue;
        }

        // Remote calls: module:function(
        for cap in remote_call_re.captures_iter(line) {
            let module = &cap[1];
            let func = &cap[2];

            // Skip keywords
            if skip_keywords.contains(module) {
                continue;
            }

            references.push(SymbolReference {
                from_file: file.to_string(),
                to_symbol: format!("{}::{}", module, func),
                line: line_num,
            });
            // Also record just the function name
            references.push(SymbolReference {
                from_file: file.to_string(),
                to_symbol: func.to_string(),
                line: line_num,
            });
        }

        // Macro usage: ?MACRO_NAME
        for cap in macro_use_re.captures_iter(line) {
            let macro_name = &cap[1];
            // Build fully qualified macro reference
            if !module_name.is_empty() {
                references.push(SymbolReference {
                    from_file: file.to_string(),
                    to_symbol: format!("{}::?{}", module_name, macro_name),
                    line: line_num,
                });
            }
            references.push(SymbolReference {
                from_file: file.to_string(),
                to_symbol: format!("?{}", macro_name),
                line: line_num,
            });
        }

        // Record usage: #record_name{
        for cap in record_use_re.captures_iter(line) {
            let rec_name = &cap[1];
            if !module_name.is_empty() {
                references.push(SymbolReference {
                    from_file: file.to_string(),
                    to_symbol: format!("{}::#{}", module_name, rec_name),
                    line: line_num,
                });
            }
            references.push(SymbolReference {
                from_file: file.to_string(),
                to_symbol: format!("#{}", rec_name),
                line: line_num,
            });
        }

        // Local calls: function(
        for cap in local_call_re.captures_iter(line) {
            let name = &cap[1];
            if !skip_keywords.contains(name) && !name.starts_with(|c: char| c.is_uppercase()) {
                references.push(SymbolReference {
                    from_file: file.to_string(),
                    to_symbol: name.to_string(),
                    line: line_num,
                });
            }
        }
    }

    // Type references in -spec directives
    for cap in spec_re.captures_iter(content) {
        let func_name = &cap[1];
        let byte_offset = cap.get(0).unwrap().start();
        let line = content[..byte_offset].matches('\n').count() + 1;

        // The spec references the function it describes
        if !module_name.is_empty() {
            references.push(SymbolReference {
                from_file: file.to_string(),
                to_symbol: format!("{}::{}", module_name, func_name),
                line,
            });
        }

        // Extract type references from the spec line
        let spec_line_end = content[byte_offset..]
            .find('\n')
            .map(|i| byte_offset + i)
            .unwrap_or(content.len());
        let spec_text = &content[byte_offset..spec_line_end];

        for type_cap in type_ref_re.captures_iter(spec_text) {
            let type_name = &type_cap[1];
            // Skip built-in types
            if !matches!(
                type_name,
                "integer"
                    | "float"
                    | "binary"
                    | "string"
                    | "atom"
                    | "boolean"
                    | "list"
                    | "tuple"
                    | "map"
                    | "pid"
                    | "port"
                    | "reference"
                    | "any"
                    | "none"
                    | "term"
                    | "number"
                    | "iolist"
                    | "iodata"
                    | "module"
                    | "mfa"
                    | "node"
                    | "timeout"
                    | "no_return"
                    | "non_neg_integer"
                    | "pos_integer"
                    | "neg_integer"
                    | "nonempty_list"
                    | "byte"
                    | "char"
                    | "spec"
            ) {
                references.push(SymbolReference {
                    from_file: file.to_string(),
                    to_symbol: type_name.to_string(),
                    line,
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// File walking helpers
// ---------------------------------------------------------------------------

/// Walk for Elixir files (.ex and .exs)
fn walkdir_elixir(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut files = super::java::walkdir(dir, "ex");
    files.extend(super::java::walkdir(dir, "exs"));
    files
}

/// Walk for Erlang files (.erl and .hrl)
fn walkdir_erlang(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut files = super::java::walkdir(dir, "erl");
    files.extend(super::java::walkdir(dir, "hrl"));
    files
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_elixir_project() {
        let ext = ElixirExtractor;
        assert!(!ext.detect(Path::new("/nonexistent")));
    }

    #[test]
    fn extract_app_name_works() {
        let mix = r#"
defmodule MyApp.MixProject do
  def project do
    [app: :my_app, version: "0.1.0"]
  end
end
"#;
        assert_eq!(extract_app_name(mix), Some("my_app".to_string()));
    }

    // -----------------------------------------------------------------------
    // Erlang declaration tests
    // -----------------------------------------------------------------------

    #[test]
    fn erlang_detect_exported_vs_private_functions() {
        let content = r#"
-module(my_server).
-export([start_link/0, get_state/1]).

start_link() ->
    gen_server:start_link(?MODULE, [], []).

get_state(Pid) ->
    gen_server:call(Pid, get_state).

internal_helper(X) ->
    X + 1.
"#;
        let mut decls = Vec::new();
        extract_erlang_declarations(content, "src/my_server.erl", &mut decls);

        let start_link = decls
            .iter()
            .find(|d| d.name == "my_server::start_link")
            .expect("start_link should be declared");
        assert_eq!(start_link.visibility, Visibility::Exported);

        let get_state = decls
            .iter()
            .find(|d| d.name == "my_server::get_state")
            .expect("get_state should be declared");
        assert_eq!(get_state.visibility, Visibility::Exported);

        let helper = decls
            .iter()
            .find(|d| d.name == "my_server::internal_helper")
            .expect("internal_helper should be declared");
        assert_eq!(helper.visibility, Visibility::Private);
    }

    #[test]
    fn erlang_detect_otp_callbacks_as_entry_points() {
        let content = r#"
-module(my_gen_server).
-behaviour(gen_server).
-export([start_link/0, init/1, handle_call/3, handle_cast/2, handle_info/2, terminate/2, code_change/3]).

start_link() ->
    gen_server:start_link({local, ?MODULE}, ?MODULE, [], []).

init([]) ->
    {ok, #state{}}.

handle_call(get_state, _From, State) ->
    {reply, State, State}.

handle_cast(_Msg, State) ->
    {noreply, State}.

handle_info(_Info, State) ->
    {noreply, State}.

terminate(_Reason, _State) ->
    ok.

code_change(_OldVsn, State, _Extra) ->
    {ok, State}.

private_helper() ->
    ok.
"#;
        let mut decls = Vec::new();
        extract_erlang_declarations(content, "src/my_gen_server.erl", &mut decls);

        let init = decls
            .iter()
            .find(|d| d.name == "my_gen_server::init")
            .expect("init should be declared");
        assert!(init.is_entry_point);
        assert_eq!(
            init.entry_point_reason.as_deref(),
            Some("gen_server callback")
        );

        let handle_call = decls
            .iter()
            .find(|d| d.name == "my_gen_server::handle_call")
            .expect("handle_call should be declared");
        assert!(handle_call.is_entry_point);

        let start_link = decls
            .iter()
            .find(|d| d.name == "my_gen_server::start_link")
            .expect("start_link should be declared");
        assert!(start_link.is_entry_point);

        let helper = decls
            .iter()
            .find(|d| d.name == "my_gen_server::private_helper")
            .expect("private_helper should be declared");
        assert!(!helper.is_entry_point);
    }

    #[test]
    fn erlang_detect_record_types() {
        let content = r#"
-module(my_records).
-record(user, {name, email, age}).
-record(session, {id, user_id, token}).

-type user_name() :: binary().
-opaque session_token() :: binary().
"#;
        let mut decls = Vec::new();
        extract_erlang_declarations(content, "src/my_records.erl", &mut decls);

        let user_rec = decls
            .iter()
            .find(|d| d.name == "my_records::#user")
            .expect("user record should be declared");
        assert_eq!(user_rec.kind, DeclarationKind::Type);

        let session_rec = decls
            .iter()
            .find(|d| d.name == "my_records::#session")
            .expect("session record should be declared");
        assert_eq!(session_rec.kind, DeclarationKind::Type);

        let user_name_type = decls
            .iter()
            .find(|d| d.name == "my_records::user_name")
            .expect("user_name type should be declared");
        assert_eq!(user_name_type.kind, DeclarationKind::Type);

        let session_token_type = decls
            .iter()
            .find(|d| d.name == "my_records::session_token")
            .expect("session_token type should be declared");
        assert_eq!(session_token_type.kind, DeclarationKind::Type);
    }

    #[test]
    fn erlang_detect_macros() {
        let content = r#"
-module(my_macros).
-define(TIMEOUT, 5000).
-define(MAX_RETRIES, 3).
"#;
        let mut decls = Vec::new();
        extract_erlang_declarations(content, "src/my_macros.erl", &mut decls);

        let timeout = decls
            .iter()
            .find(|d| d.name == "my_macros::?TIMEOUT")
            .expect("TIMEOUT macro should be declared");
        assert_eq!(timeout.kind, DeclarationKind::Constant);

        let max_retries = decls
            .iter()
            .find(|d| d.name == "my_macros::?MAX_RETRIES")
            .expect("MAX_RETRIES macro should be declared");
        assert_eq!(max_retries.kind, DeclarationKind::Constant);
    }

    #[test]
    fn erlang_detect_test_entry_points() {
        let content = r#"
-module(my_server_test).
-export([all/0, basic_test/1, connect_test/1]).

all() ->
    [basic_test, connect_test].

basic_test(_Config) ->
    ok.

connect_test(_Config) ->
    ok.
"#;
        let mut decls = Vec::new();
        extract_erlang_declarations(content, "test/my_server_test.erl", &mut decls);

        let basic_test = decls
            .iter()
            .find(|d| d.name == "my_server_test::basic_test")
            .expect("basic_test should be declared");
        assert!(basic_test.is_entry_point);
        assert_eq!(
            basic_test.entry_point_reason.as_deref(),
            Some("test function")
        );
    }

    #[test]
    fn erlang_detect_common_test_suite_callbacks() {
        let content = r#"
-module(my_SUITE).
-export([all/0, init_per_suite/1, end_per_suite/1, my_test_case/1]).

all() -> [my_test_case].

init_per_suite(Config) ->
    Config.

end_per_suite(_Config) ->
    ok.

my_test_case(_Config) ->
    ok.
"#;
        let mut decls = Vec::new();
        extract_erlang_declarations(content, "test/my_SUITE.erl", &mut decls);

        let all = decls
            .iter()
            .find(|d| d.name == "my_SUITE::all")
            .expect("all should be declared");
        assert!(all.is_entry_point);
        assert_eq!(
            all.entry_point_reason.as_deref(),
            Some("Common Test callback")
        );

        let init = decls
            .iter()
            .find(|d| d.name == "my_SUITE::init_per_suite")
            .expect("init_per_suite should be declared");
        assert!(init.is_entry_point);
    }

    #[test]
    fn erlang_detect_escript_main() {
        let content = r#"
-module(my_script).
-export([main/1]).

main(Args) ->
    io:format("Hello ~p~n", [Args]).
"#;
        let mut decls = Vec::new();
        extract_erlang_declarations(content, "src/my_script.erl", &mut decls);

        let main = decls
            .iter()
            .find(|d| d.name == "my_script::main")
            .expect("main should be declared");
        assert!(main.is_entry_point);
        assert_eq!(
            main.entry_point_reason.as_deref(),
            Some("escript entry point")
        );
    }

    #[test]
    fn erlang_detect_callbacks() {
        let content = r#"
-module(my_behaviour).
-callback init(Args :: term()) -> {ok, State :: term()}.
-callback handle(Event :: term(), State :: term()) -> {ok, State :: term()}.
"#;
        let mut decls = Vec::new();
        extract_erlang_declarations(content, "src/my_behaviour.erl", &mut decls);

        let init_cb = decls
            .iter()
            .find(|d| d.name == "my_behaviour::callback::init")
            .expect("init callback should be declared");
        assert_eq!(init_cb.kind, DeclarationKind::Trait);
    }

    #[test]
    fn erlang_module_declaration() {
        let content = "-module(my_mod).\n";
        let mut decls = Vec::new();
        extract_erlang_declarations(content, "src/my_mod.erl", &mut decls);

        let module = decls
            .iter()
            .find(|d| d.name == "my_mod" && d.kind == DeclarationKind::Module)
            .expect("module declaration should exist");
        assert_eq!(module.language, Language::Erlang);
    }

    // -----------------------------------------------------------------------
    // Elixir declaration tests
    // -----------------------------------------------------------------------

    #[test]
    fn elixir_def_vs_defp_visibility() {
        let content = r#"
defmodule MyApp.Calculator do
  def add(a, b) do
    a + b
  end

  defp validate(x) do
    is_number(x)
  end

  defmacro log_call(func) do
    quote do
      IO.puts("Calling #{unquote(func)}")
    end
  end
end
"#;
        let mut decls = Vec::new();
        extract_elixir_declarations(content, "lib/calculator.ex", &mut decls);

        let add = decls
            .iter()
            .find(|d| d.name == "MyApp.Calculator::add")
            .expect("add should be declared");
        assert_eq!(add.visibility, Visibility::Public);
        assert_eq!(add.kind, DeclarationKind::Function);

        let validate = decls
            .iter()
            .find(|d| d.name == "MyApp.Calculator::validate")
            .expect("validate should be declared");
        assert_eq!(validate.visibility, Visibility::Private);

        let log_call = decls
            .iter()
            .find(|d| d.name == "MyApp.Calculator::log_call")
            .expect("log_call should be declared");
        assert_eq!(log_call.visibility, Visibility::Public);
    }

    #[test]
    fn elixir_defmodule_detection() {
        let content = r#"
defmodule MyApp.Web.Router do
  use MyApp.Web, :router

  pipeline :api do
    plug :accepts, ["json"]
  end
end
"#;
        let mut decls = Vec::new();
        extract_elixir_declarations(content, "lib/web/router.ex", &mut decls);

        let module = decls
            .iter()
            .find(|d| d.name == "MyApp.Web.Router" && d.kind == DeclarationKind::Module)
            .expect("module should be declared");
        assert_eq!(module.language, Language::Elixir);
    }

    #[test]
    fn elixir_genserver_callbacks_are_entry_points() {
        let content = r#"
defmodule MyApp.Worker do
  use GenServer

  def start_link(args) do
    GenServer.start_link(__MODULE__, args)
  end

  def init(args) do
    {:ok, args}
  end

  def handle_call(:get, _from, state) do
    {:reply, state, state}
  end

  def handle_cast({:set, val}, _state) do
    {:noreply, val}
  end

  def handle_info(:tick, state) do
    {:noreply, state}
  end

  defp do_work(state) do
    state
  end
end
"#;
        let mut decls = Vec::new();
        extract_elixir_declarations(content, "lib/worker.ex", &mut decls);

        let init = decls
            .iter()
            .find(|d| d.name == "MyApp.Worker::init")
            .expect("init should be declared");
        assert!(init.is_entry_point);
        assert_eq!(
            init.entry_point_reason.as_deref(),
            Some("GenServer callback")
        );

        let handle_call = decls
            .iter()
            .find(|d| d.name == "MyApp.Worker::handle_call")
            .expect("handle_call should be declared");
        assert!(handle_call.is_entry_point);

        let start_link = decls
            .iter()
            .find(|d| d.name == "MyApp.Worker::start_link")
            .expect("start_link should be declared");
        assert!(start_link.is_entry_point);

        let do_work = decls
            .iter()
            .find(|d| d.name == "MyApp.Worker::do_work")
            .expect("do_work should be declared");
        assert!(!do_work.is_entry_point);
    }

    #[test]
    fn elixir_application_callback_entry_point() {
        let content = r#"
defmodule MyApp.Application do
  use Application

  def start(_type, _args) do
    children = []
    opts = [strategy: :one_for_one]
    Supervisor.start_link(children, opts)
  end
end
"#;
        let mut decls = Vec::new();
        extract_elixir_declarations(content, "lib/application.ex", &mut decls);

        let start = decls
            .iter()
            .find(|d| d.name == "MyApp.Application::start")
            .expect("start should be declared");
        assert!(start.is_entry_point);
        assert_eq!(
            start.entry_point_reason.as_deref(),
            Some("Application callback")
        );
    }

    #[test]
    fn elixir_liveview_callbacks_are_entry_points() {
        let content = r#"
defmodule MyAppWeb.DashboardLive do
  use MyAppWeb, :live_view
  use Phoenix.LiveView

  def mount(_params, _session, socket) do
    {:ok, socket}
  end

  def render(assigns) do
    ~H"""
    <div>Dashboard</div>
    """
  end

  def handle_event("click", _params, socket) do
    {:noreply, socket}
  end
end
"#;
        let mut decls = Vec::new();
        extract_elixir_declarations(content, "lib/web/live/dashboard_live.ex", &mut decls);

        let mount = decls
            .iter()
            .find(|d| d.name == "MyAppWeb.DashboardLive::mount")
            .expect("mount should be declared");
        assert!(mount.is_entry_point);
        assert_eq!(
            mount.entry_point_reason.as_deref(),
            Some("LiveView callback")
        );

        let render = decls
            .iter()
            .find(|d| d.name == "MyAppWeb.DashboardLive::render")
            .expect("render should be declared");
        assert!(render.is_entry_point);
    }

    #[test]
    fn elixir_type_declarations() {
        let content = r#"
defmodule MyApp.Types do
  @type status :: :active | :inactive
  @typep internal_state :: map()
  @opaque token :: binary()

  defstruct [:name, :value]
end
"#;
        let mut decls = Vec::new();
        extract_elixir_declarations(content, "lib/types.ex", &mut decls);

        let status = decls
            .iter()
            .find(|d| d.name == "MyApp.Types::status")
            .expect("status type should be declared");
        assert_eq!(status.kind, DeclarationKind::Type);
        assert_eq!(status.visibility, Visibility::Public);

        let internal = decls
            .iter()
            .find(|d| d.name == "MyApp.Types::internal_state")
            .expect("internal_state type should be declared");
        assert_eq!(internal.visibility, Visibility::Private);

        let struct_decl = decls
            .iter()
            .find(|d| d.name == "MyApp.Types::__struct__")
            .expect("struct should be declared");
        assert_eq!(struct_decl.kind, DeclarationKind::Type);
    }

    #[test]
    fn elixir_test_blocks_are_entry_points() {
        let content = r#"
defmodule MyApp.CalculatorTest do
  use ExUnit.Case

  test "addition works" do
    assert MyApp.Calculator.add(1, 2) == 3
  end

  test "subtraction works" do
    assert MyApp.Calculator.sub(3, 1) == 2
  end
end
"#;
        let mut decls = Vec::new();
        extract_elixir_declarations(content, "test/calculator_test.exs", &mut decls);

        let test_decls: Vec<_> = decls
            .iter()
            .filter(|d| d.is_entry_point && d.entry_point_reason.as_deref() == Some("test block"))
            .collect();
        assert_eq!(test_decls.len(), 2, "Should find 2 test blocks");
    }

    // -----------------------------------------------------------------------
    // Erlang reference tests
    // -----------------------------------------------------------------------

    #[test]
    fn erlang_remote_call_references() {
        let content = r#"
-module(my_mod).
-export([do_stuff/0]).

do_stuff() ->
    io:format("hello~n"),
    lists:map(fun(X) -> X + 1 end, [1,2,3]).
"#;
        let mut refs = Vec::new();
        extract_erlang_references(content, "src/my_mod.erl", &mut refs);

        let io_ref = refs.iter().any(|r| r.to_symbol == "io::format");
        assert!(io_ref, "Should find reference to io::format");

        let lists_ref = refs.iter().any(|r| r.to_symbol == "lists::map");
        assert!(lists_ref, "Should find reference to lists::map");
    }

    #[test]
    fn erlang_macro_references() {
        let content = r#"
-module(my_mod).
do_stuff() ->
    Timeout = ?TIMEOUT,
    io:format("~p~n", [?MODULE]).
"#;
        let mut refs = Vec::new();
        extract_erlang_references(content, "src/my_mod.erl", &mut refs);

        let timeout_ref = refs.iter().any(|r| r.to_symbol == "?TIMEOUT");
        assert!(timeout_ref, "Should find reference to ?TIMEOUT");

        let module_ref = refs.iter().any(|r| r.to_symbol == "?MODULE");
        assert!(module_ref, "Should find reference to ?MODULE");
    }

    #[test]
    fn erlang_record_references() {
        let content = r#"
-module(my_mod).
do_stuff() ->
    User = #user{name = "test"},
    User#user.name.
"#;
        let mut refs = Vec::new();
        extract_erlang_references(content, "src/my_mod.erl", &mut refs);

        let user_ref = refs
            .iter()
            .any(|r| r.to_symbol == "#user" || r.to_symbol == "my_mod::#user");
        assert!(user_ref, "Should find reference to #user record");
    }

    // -----------------------------------------------------------------------
    // Elixir reference tests
    // -----------------------------------------------------------------------

    #[test]
    fn elixir_remote_call_references() {
        let content = r#"
defmodule MyApp.Worker do
  def run do
    MyApp.Calculator.add(1, 2)
    MyApp.Logger.info("done")
  end
end
"#;
        let mut refs = Vec::new();
        extract_elixir_references(content, "lib/worker.ex", &mut refs);

        let calc_ref = refs.iter().any(|r| r.to_symbol == "MyApp.Calculator::add");
        assert!(calc_ref, "Should find reference to MyApp.Calculator::add");

        let logger_ref = refs.iter().any(|r| r.to_symbol == "MyApp.Logger::info");
        assert!(logger_ref, "Should find reference to MyApp.Logger::info");
    }

    #[test]
    fn elixir_struct_references() {
        let content = r#"
defmodule MyApp.Builder do
  def build do
    %MyApp.User{name: "test"}
  end
end
"#;
        let mut refs = Vec::new();
        extract_elixir_references(content, "lib/builder.ex", &mut refs);

        let struct_ref = refs.iter().any(|r| r.to_symbol == "MyApp.User::__struct__");
        assert!(struct_ref, "Should find struct construction reference");
    }

    #[test]
    fn elixir_function_capture_references() {
        let content = r#"
defmodule MyApp.Pipeline do
  def run(items) do
    Enum.map(items, &MyApp.Transform.apply/1)
  end
end
"#;
        let mut refs = Vec::new();
        extract_elixir_references(content, "lib/pipeline.ex", &mut refs);

        let capture_ref = refs.iter().any(|r| r.to_symbol == "MyApp.Transform::apply");
        assert!(capture_ref, "Should find function capture reference");
    }

    #[test]
    fn elixir_use_import_alias_references() {
        let content = r#"
defmodule MyApp.Worker do
  use GenServer
  import MyApp.Helpers
  alias MyApp.Config
  @behaviour MyApp.WorkerBehaviour
end
"#;
        let mut refs = Vec::new();
        extract_elixir_references(content, "lib/worker.ex", &mut refs);

        let use_ref = refs.iter().any(|r| r.to_symbol == "GenServer");
        assert!(use_ref, "Should find use reference");

        let import_ref = refs.iter().any(|r| r.to_symbol == "MyApp.Helpers");
        assert!(import_ref, "Should find import reference");

        let alias_ref = refs.iter().any(|r| r.to_symbol == "MyApp.Config");
        assert!(alias_ref, "Should find alias reference");

        let behaviour_ref = refs.iter().any(|r| r.to_symbol == "MyApp.WorkerBehaviour");
        assert!(behaviour_ref, "Should find behaviour reference");
    }

    // -----------------------------------------------------------------------
    // Arity counting tests
    // -----------------------------------------------------------------------

    #[test]
    fn count_erlang_arity_works() {
        assert_eq!(count_erlang_arity(""), 0);
        assert_eq!(count_erlang_arity("X"), 1);
        assert_eq!(count_erlang_arity("X, Y"), 2);
        assert_eq!(count_erlang_arity("X, {Y, Z}, W"), 3);
        assert_eq!(count_erlang_arity("[H|T], Acc"), 2);
    }

    // -----------------------------------------------------------------------
    // Detect tests (Erlang project detection)
    // -----------------------------------------------------------------------

    #[test]
    fn detect_does_not_match_nonexistent() {
        let ext = ElixirExtractor;
        assert!(!ext.detect(Path::new("/nonexistent_path_that_should_not_exist")));
    }
}
