//! C# / .NET dependency extractor for DSM analysis.
//!
//! Summary level: parses `.csproj` files for `<ProjectReference>` and
//! `<PackageReference>` elements.
//! Full level: scans `.cs` source files for `using` directives and
//! `namespace` declarations to build file-level dependency edges.

use crate::extract::*;
use anyhow::Result;
use std::path::Path;

pub struct CSharpExtractor;

impl Extractor for CSharpExtractor {
    fn name(&self) -> &str {
        "csharp"
    }
    fn language(&self) -> Language {
        Language::CSharp
    }

    fn detect(&self, dir: &Path) -> bool {
        has_ext_in(dir, "csproj") || has_ext_in(dir, "sln")
    }

    fn extract(&self, dir: &Path, config: &ExtractConfig) -> Result<Vec<Edge>> {
        match config.level {
            GranularityLevel::Summary => extract_from_csproj(dir, config),
            GranularityLevel::Full => extract_from_source(dir, config),
        }
    }

    fn detect_cross_language(&self, dir: &Path, config: &ExtractConfig) -> Result<Vec<Edge>> {
        if !config.detect_cross_language {
            return Ok(vec![]);
        }
        let mut edges = Vec::new();
        scan_pinvoke(dir, &mut edges)?;
        scan_com_interop(dir, &mut edges)?;
        scan_grpc_csharp(dir, &mut edges)?;
        Ok(edges)
    }
}

impl DeclarationExtractor for CSharpExtractor {
    fn language(&self) -> Language {
        Language::CSharp
    }

    fn extract_declarations(
        &self,
        dir: &Path,
        _config: &ExtractConfig,
    ) -> Result<Vec<Declaration>> {
        let type_re = regex::Regex::new(
            r"(?m)^\s*(public|internal)\s+(?:(?:static|sealed|abstract|partial|readonly)\s+)*(?:class|interface|record|struct|enum)\s+(\w+)",
        )?;
        let method_re = regex::Regex::new(
            r"(?m)^\s*(?:public|internal)\s+(?:(?:static|async|virtual|override|abstract|sealed)\s+)*\S+\s+(\w+)\s*[<(]",
        )?;
        let entry_re = regex::Regex::new(
            r"(?m)(?:static\s+(?:async\s+)?(?:Task\s+|void\s+|int\s+)Main\s*\()|WebApplication\.CreateBuilder|Host\.CreateDefaultBuilder",
        )?;

        let mut decls = Vec::new();

        for entry in super::java::walkdir(dir, "cs") {
            let content = std::fs::read_to_string(&entry)?;
            let file = entry
                .strip_prefix(dir)
                .unwrap_or(&entry)
                .to_string_lossy()
                .to_string();

            let has_entry_point = entry_re.is_match(&content);

            for (line_num, line) in content.lines().enumerate() {
                if let Some(cap) = type_re.captures(line) {
                    let vis = match &cap[1] {
                        "public" => Visibility::Public,
                        _ => Visibility::Internal,
                    };
                    decls.push(Declaration {
                        name: cap[2].to_string(),
                        kind: DeclarationKind::Type,
                        visibility: vis,
                        file: file.clone(),
                        line: line_num + 1,
                        language: Language::CSharp,
                        is_entry_point: has_entry_point && line_num < 5,
                        entry_point_reason: None,
                        is_test: false,
                    });
                }

                if let Some(cap) = method_re.captures(line) {
                    let name = &cap[1];
                    // Skip common false positives
                    if matches!(
                        name,
                        "if" | "for"
                            | "foreach"
                            | "while"
                            | "switch"
                            | "catch"
                            | "using"
                            | "lock"
                            | "return"
                            | "throw"
                            | "new"
                            | "var"
                            | "get"
                            | "set"
                    ) {
                        continue;
                    }
                    decls.push(Declaration {
                        name: name.to_string(),
                        kind: DeclarationKind::Method,
                        visibility: Visibility::Public,
                        file: file.clone(),
                        line: line_num + 1,
                        language: Language::CSharp,
                        is_entry_point: name == "Main",
                        entry_point_reason: if name == "Main" {
                            Some("Program entry point".to_string())
                        } else {
                            None
                        },
                        is_test: false,
                    });
                }
            }
        }

        Ok(decls)
    }

    fn extract_references(
        &self,
        dir: &Path,
        _config: &ExtractConfig,
    ) -> Result<Vec<SymbolReference>> {
        // For C#, using directives are the primary cross-file reference mechanism.
        // A full implementation would parse type references, but using-based is
        // sufficient for dead-code analysis at the namespace/type level.
        let using_re = regex::Regex::new(r"(?m)^\s*using\s+(?:static\s+)?([A-Z][\w.]+)\s*;")?;
        let mut refs = Vec::new();

        for entry in super::java::walkdir(dir, "cs") {
            let content = std::fs::read_to_string(&entry)?;
            let file = entry
                .strip_prefix(dir)
                .unwrap_or(&entry)
                .to_string_lossy()
                .to_string();

            for (line_num, line) in content.lines().enumerate() {
                if let Some(cap) = using_re.captures(line) {
                    refs.push(SymbolReference {
                        from_file: file.clone(),
                        to_symbol: cap[1].to_string(),
                        line: line_num + 1,
                    });
                }
            }
        }

        Ok(refs)
    }
}

// ── Detection helper ──────────────────────────────────────────────────────

/// Check if a directory contains files with a given extension (non-recursive).
fn has_ext_in(dir: &Path, ext: &str) -> bool {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .any(|e| e.path().extension().is_some_and(|x| x == ext))
}

// ── Summary extraction: .csproj parsing ───────────────────────────────────

fn extract_from_csproj(dir: &Path, config: &ExtractConfig) -> Result<Vec<Edge>> {
    let proj_ref_re = regex::Regex::new(r#"<ProjectReference\s+Include="([^"]+)""#)?;
    let pkg_ref_re = regex::Regex::new(r#"<PackageReference\s+Include="([^"]+)""#)?;

    let mut edges = Vec::new();

    for entry in super::java::walkdir(dir, "csproj") {
        let content = std::fs::read_to_string(&entry)?;
        let source = csproj_name(&entry);

        // Project-to-project references
        for cap in proj_ref_re.captures_iter(&content) {
            let ref_path = &cap[1];
            let target = csproj_name_from_ref(ref_path);
            if should_include_csharp(&target, &source, config) {
                edges.push(Edge {
                    source: source.clone(),
                    target,
                    weight: 1.0,
                    kind: EdgeKind::Import,
                    cross_language: None,
                });
            }
        }

        // NuGet package references (external deps)
        for cap in pkg_ref_re.captures_iter(&content) {
            let pkg = cap[1].to_string();
            if should_include_csharp(&pkg, &source, config) {
                edges.push(Edge {
                    source: source.clone(),
                    target: format!("nuget:{}", pkg),
                    weight: 1.0,
                    kind: EdgeKind::Import,
                    cross_language: None,
                });
            }
        }
    }

    Ok(edges)
}

/// Extract project name from a `.csproj` path.
fn csproj_name(path: &Path) -> String {
    path.file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string()
}

/// Extract project name from a `<ProjectReference Include="...">` path.
/// Handles both forward and backslash separators (Windows-style paths in .csproj).
fn csproj_name_from_ref(ref_path: &str) -> String {
    let filename = ref_path.rsplit(['/', '\\']).next().unwrap_or(ref_path);
    filename
        .strip_suffix(".csproj")
        .unwrap_or(filename)
        .to_string()
}

// ── Full extraction: .cs source parsing ───────────────────────────────────

fn extract_from_source(dir: &Path, config: &ExtractConfig) -> Result<Vec<Edge>> {
    let using_re = regex::Regex::new(r"(?m)^\s*using\s+(?:static\s+)?([A-Z][\w.]+)\s*;")?;
    let ns_re = regex::Regex::new(r"(?m)^\s*namespace\s+([\w.]+)")?;

    let mut edges = Vec::new();

    for entry in super::java::walkdir(dir, "cs") {
        let content = std::fs::read_to_string(&entry)?;
        let source = cs_module_from_path(&entry, dir, &ns_re, &content);

        for cap in using_re.captures_iter(&content) {
            let target = normalize_using(&cap[1], config);
            // Skip System/Microsoft stdlib namespaces
            if is_stdlib(&target) {
                continue;
            }
            if should_include_csharp(&target, &source, config) {
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

    Ok(edges)
}

/// Derive a module name from a `.cs` file — prefer the namespace declaration
/// if found, otherwise fall back to the file path.
fn cs_module_from_path(file: &Path, root: &Path, ns_re: &regex::Regex, content: &str) -> String {
    // Try to extract the first namespace declaration
    if let Some(cap) = ns_re.captures(content) {
        return cap[1].to_string();
    }
    // Fallback: derive from file path
    let relative = file.strip_prefix(root).unwrap_or(file);
    let stem = relative
        .with_extension("")
        .to_string_lossy()
        .replace(['/', '\\'], ".");
    stem
}

fn normalize_using(import: &str, config: &ExtractConfig) -> String {
    match config.level {
        GranularityLevel::Summary => {
            // Collapse to top-level namespace
            import.split('.').next().unwrap_or(import).to_string()
        }
        GranularityLevel::Full => import.to_string(),
    }
}

fn is_stdlib(target: &str) -> bool {
    let prefix = target.split('.').next().unwrap_or(target);
    matches!(
        prefix,
        "System" | "Microsoft" | "Windows" | "Xunit" | "NUnit" | "Moq" | "FluentAssertions"
    )
}

fn should_include_csharp(target: &str, source: &str, config: &ExtractConfig) -> bool {
    if target == source || target.is_empty() {
        return false;
    }
    if let Some(prefix) = &config.prefix_filter {
        if !target.starts_with(prefix) {
            return false;
        }
    }
    true
}

// ── Cross-language detection ──────────────────────────────────────────────

fn scan_pinvoke(dir: &Path, edges: &mut Vec<Edge>) -> Result<()> {
    let re = regex::Regex::new(r#"\[DllImport\("([^"]+)""#)?;

    for entry in super::java::walkdir(dir, "cs") {
        let content = std::fs::read_to_string(&entry)?;
        for cap in re.captures_iter(&content) {
            let lib = cap[1].to_string();
            let source = entry
                .strip_prefix(dir)
                .unwrap_or(&entry)
                .to_string_lossy()
                .replace(['/', '\\'], ".");
            edges.push(Edge {
                source,
                target: format!("native:{}", lib),
                weight: 1.0,
                kind: EdgeKind::Ffi,
                cross_language: Some(CrossLanguageEdge {
                    source_lang: Language::CSharp,
                    target_lang: Language::C,
                    mechanism: FfiMechanism::PInvoke,
                }),
            });
        }
    }
    Ok(())
}

fn scan_com_interop(dir: &Path, edges: &mut Vec<Edge>) -> Result<()> {
    let re = regex::Regex::new(r"\[ComImport\]")?;

    for entry in super::java::walkdir(dir, "cs") {
        let content = std::fs::read_to_string(&entry)?;
        if re.is_match(&content) {
            let source = entry
                .strip_prefix(dir)
                .unwrap_or(&entry)
                .to_string_lossy()
                .replace(['/', '\\'], ".");
            edges.push(Edge {
                source,
                target: "native:com_component".to_string(),
                weight: 1.0,
                kind: EdgeKind::Ffi,
                cross_language: Some(CrossLanguageEdge {
                    source_lang: Language::CSharp,
                    target_lang: Language::Cpp,
                    mechanism: FfiMechanism::ComInterop,
                }),
            });
        }
    }
    Ok(())
}

fn scan_grpc_csharp(dir: &Path, edges: &mut Vec<Edge>) -> Result<()> {
    let re = regex::Regex::new(r#"<Protobuf\s+Include="([^"]+)""#)?;

    for entry in super::java::walkdir(dir, "csproj") {
        let content = std::fs::read_to_string(&entry)?;
        for cap in re.captures_iter(&content) {
            let proto = &cap[1];
            let source = csproj_name(&entry);
            edges.push(Edge {
                source,
                target: format!("grpc:{}", proto),
                weight: 1.0,
                kind: EdgeKind::Ipc,
                cross_language: Some(CrossLanguageEdge {
                    source_lang: Language::CSharp,
                    target_lang: Language::Unknown("grpc".to_string()),
                    mechanism: FfiMechanism::Grpc,
                }),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU32, Ordering};

    static TEST_ID: AtomicU32 = AtomicU32::new(0);

    /// Create a unique temp directory with the given files.
    /// Returns the path — caller must clean up via `fs::remove_dir_all`.
    fn setup_dir(files: &[(&str, &str)]) -> std::path::PathBuf {
        let id = TEST_ID.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("dsm_csharp_test_{id}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for (name, content) in files {
            let path = dir.join(name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&path, content).unwrap();
        }
        dir
    }

    fn cleanup(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn detect_csproj() {
        let dir = setup_dir(&[("MyApp.csproj", "<Project/>")]);
        let ext = CSharpExtractor;
        assert!(ext.detect(&dir));
        cleanup(&dir);
    }

    #[test]
    fn detect_sln() {
        let dir = setup_dir(&[("MyApp.sln", "")]);
        let ext = CSharpExtractor;
        assert!(ext.detect(&dir));
        cleanup(&dir);
    }

    #[test]
    fn no_detect_empty() {
        let ext = CSharpExtractor;
        assert!(!ext.detect(Path::new("/nonexistent")));
    }

    #[test]
    fn extract_project_references() {
        let csproj = r#"
<Project Sdk="Microsoft.NET.Sdk">
  <ItemGroup>
    <ProjectReference Include="..\Core\Core.csproj" />
    <ProjectReference Include="..\Data\Data.csproj" />
    <PackageReference Include="Newtonsoft.Json" Version="13.0.3" />
  </ItemGroup>
</Project>"#;
        let dir = setup_dir(&[("MyApp.csproj", csproj)]);
        let config = ExtractConfig::default();
        let edges = extract_from_csproj(&dir, &config).unwrap();

        assert_eq!(edges.len(), 3);
        assert!(edges.iter().any(|e| e.target == "Core"));
        assert!(edges.iter().any(|e| e.target == "Data"));
        assert!(edges.iter().any(|e| e.target == "nuget:Newtonsoft.Json"));
        cleanup(&dir);
    }

    #[test]
    fn extract_using_directives() {
        let code = r#"
using System;
using System.Collections.Generic;
using MyApp.Core;
using MyApp.Data;

namespace MyApp.Web
{
    public class Startup { }
}
"#;
        let dir = setup_dir(&[("Startup.cs", code), ("MyApp.csproj", "<Project/>")]);
        let config = ExtractConfig {
            level: GranularityLevel::Full,
            ..Default::default()
        };
        let edges = extract_from_source(&dir, &config).unwrap();

        assert!(!edges.iter().any(|e| e.target.starts_with("System")));
        assert!(edges.iter().any(|e| e.target == "MyApp.Core"));
        assert!(edges.iter().any(|e| e.target == "MyApp.Data"));
        cleanup(&dir);
    }

    #[test]
    fn stdlib_excluded() {
        assert!(is_stdlib("System"));
        assert!(is_stdlib("System.Collections.Generic"));
        assert!(is_stdlib("Microsoft.Extensions.DependencyInjection"));
        assert!(!is_stdlib("MyApp.Core"));
        assert!(!is_stdlib("Newtonsoft.Json"));
    }

    #[test]
    fn csproj_name_extraction() {
        assert_eq!(csproj_name(Path::new("src/MyApp.csproj")), "MyApp");
        assert_eq!(csproj_name_from_ref(r"..\Core\Core.csproj"), "Core");
    }

    #[test]
    fn detect_pinvoke() {
        let code = r#"
using System.Runtime.InteropServices;

public class NativeWrapper
{
    [DllImport("kernel32.dll")]
    static extern IntPtr LoadLibrary(string path);

    [DllImport("mylib.so")]
    static extern int ProcessData(IntPtr data, int len);
}
"#;
        let dir = setup_dir(&[("Native.cs", code), ("MyApp.csproj", "<Project/>")]);
        let mut edges = Vec::new();
        scan_pinvoke(&dir, &mut edges).unwrap();

        assert_eq!(edges.len(), 2);
        assert!(edges.iter().any(|e| e.target == "native:kernel32.dll"));
        assert!(edges.iter().any(|e| e.target == "native:mylib.so"));
        assert!(edges.iter().all(|e| e.kind == EdgeKind::Ffi));
        cleanup(&dir);
    }

    #[test]
    fn namespace_as_module() {
        let ns_re = regex::Regex::new(r"(?m)^\s*namespace\s+([\w.]+)").unwrap();
        let content = "namespace MyApp.Core\n{\n}\n";
        let result =
            cs_module_from_path(Path::new("src/Core.cs"), Path::new("src"), &ns_re, content);
        assert_eq!(result, "MyApp.Core");
    }

    #[test]
    fn path_fallback_when_no_namespace() {
        let ns_re = regex::Regex::new(r"(?m)^\s*namespace\s+([\w.]+)").unwrap();
        let content = "public class Program { }";
        let result = cs_module_from_path(
            Path::new("src/Program.cs"),
            Path::new("src"),
            &ns_re,
            content,
        );
        assert_eq!(result, "Program");
    }

    #[test]
    fn extract_declarations_finds_types_and_methods() {
        let code = r#"
namespace MyApp.Core;

public class OrderService
{
    public async Task<Order> GetOrderAsync(int id) { }
    private void InternalHelper() { }
    public static void Main(string[] args) { }
}

public interface IOrderRepository { }
public record OrderDto(int Id, string Name);
internal sealed class CacheManager { }
"#;
        let dir = setup_dir(&[("OrderService.cs", code), ("MyApp.csproj", "<Project/>")]);
        let config = ExtractConfig::default();
        let ext = CSharpExtractor;
        let decls = ext.extract_declarations(&dir, &config).unwrap();

        let type_names: Vec<&str> = decls
            .iter()
            .filter(|d| d.kind == DeclarationKind::Type)
            .map(|d| d.name.as_str())
            .collect();
        assert!(type_names.contains(&"OrderService"));
        assert!(type_names.contains(&"IOrderRepository"));
        assert!(type_names.contains(&"OrderDto"));
        assert!(type_names.contains(&"CacheManager"));

        let main = decls.iter().find(|d| d.name == "Main");
        assert!(main.is_some());
        assert!(main.unwrap().is_entry_point);
        cleanup(&dir);
    }
}
