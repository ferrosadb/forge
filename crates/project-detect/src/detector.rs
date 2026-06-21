//! Project detection logic.
//!
//! Scans a directory for marker files and reports detected stack components.

use serde::Serialize;
use std::path::Path;

#[derive(Debug, Serialize, PartialEq)]
pub struct ProjectReport {
    pub languages: Vec<DetectedComponent>,
    pub frameworks: Vec<DetectedComponent>,
    pub test_runners: Vec<DetectedComponent>,
    pub linters: Vec<DetectedComponent>,
    pub build_tools: Vec<DetectedComponent>,
    pub ci: Vec<DetectedComponent>,
    pub suggested_skills: Vec<String>,
}

#[derive(Debug, Serialize, PartialEq, Clone)]
pub struct DetectedComponent {
    pub name: String,
    pub evidence: String,
}

/// Detect project stack from a directory.
pub fn detect(dir: &Path) -> ProjectReport {
    let mut languages = Vec::new();
    let mut frameworks = Vec::new();
    let mut test_runners = Vec::new();
    let mut linters = Vec::new();
    let mut build_tools = Vec::new();
    let mut ci = Vec::new();
    let mut suggested_skills = Vec::new();

    // Rust
    if dir.join("Cargo.toml").exists() {
        languages.push(comp("Rust", "Cargo.toml"));
        build_tools.push(comp("Cargo", "Cargo.toml"));
        test_runners.push(comp("cargo test", "Cargo.toml"));
        linters.push(comp("clippy", "Cargo.toml"));
        suggested_skills.push("rust.skill.md".to_string());
    }

    // Python
    if dir.join("pyproject.toml").exists() || dir.join("setup.py").exists() {
        let evidence = if dir.join("pyproject.toml").exists() {
            "pyproject.toml"
        } else {
            "setup.py"
        };
        languages.push(comp("Python", evidence));
        suggested_skills.push("python.skill.md".to_string());

        if dir.join("pytest.ini").exists()
            || dir.join("pyproject.toml").exists()
            || dir.join("tests").exists()
        {
            test_runners.push(comp("pytest", "tests/ directory"));
        }
    }
    if dir.join("requirements.txt").exists() && !languages.iter().any(|c| c.name == "Python") {
        languages.push(comp("Python", "requirements.txt"));
        suggested_skills.push("python.skill.md".to_string());
    }

    // Elixir
    if dir.join("mix.exs").exists() {
        languages.push(comp("Elixir", "mix.exs"));
        build_tools.push(comp("Mix", "mix.exs"));
        test_runners.push(comp("ExUnit", "mix.exs"));
        suggested_skills.push("elixir.skill.md".to_string());

        if dir.join("config").exists() {
            frameworks.push(comp("Phoenix (possible)", "config/ directory"));
        }
    }

    // Go
    if dir.join("go.mod").exists() {
        languages.push(comp("Go", "go.mod"));
        test_runners.push(comp("go test", "go.mod"));
        suggested_skills.push("go.skill.md".to_string());
    }

    // Node.js / TypeScript
    if dir.join("package.json").exists() {
        let evidence = "package.json";
        if dir.join("tsconfig.json").exists() {
            languages.push(comp("TypeScript", "tsconfig.json"));
            suggested_skills.push("typescript.skill.md".to_string());
        } else {
            languages.push(comp("JavaScript", evidence));
            suggested_skills.push("typescript.skill.md".to_string());
        }
        build_tools.push(comp("npm/yarn", evidence));

        // Detect test runners
        if dir.join("jest.config.js").exists()
            || dir.join("jest.config.ts").exists()
            || dir.join("jest.config.cjs").exists()
        {
            test_runners.push(comp("Jest", "jest.config.*"));
        }
        if dir.join("vitest.config.ts").exists() || dir.join("vitest.config.js").exists() {
            test_runners.push(comp("Vitest", "vitest.config.*"));
        }

        // Detect frameworks
        if dir.join("next.config.js").exists() || dir.join("next.config.mjs").exists() {
            frameworks.push(comp("Next.js", "next.config.*"));
        }
    }

    // Swift
    if dir.join("Package.swift").exists() {
        languages.push(comp("Swift", "Package.swift"));
        build_tools.push(comp("SwiftPM", "Package.swift"));
        test_runners.push(comp("swift test", "Package.swift"));
        suggested_skills.push("swift.skill.md".to_string());
    }

    // C# / .NET
    if has_csproj_or_sln(dir) {
        let evidence = if has_ext(dir, "sln") {
            "*.sln"
        } else {
            "*.csproj"
        };
        languages.push(comp("C#", evidence));
        build_tools.push(comp("dotnet CLI", evidence));
        test_runners.push(comp("dotnet test", evidence));
        linters.push(comp("Roslyn analyzers", evidence));
        suggested_skills.push("csharp.skill.md".to_string());

        // Detect ASP.NET Core
        if dir.join("Program.cs").exists() || dir.join("Startup.cs").exists() {
            frameworks.push(comp("ASP.NET Core (possible)", "Program.cs"));
        }
    }

    // C / C++
    //
    // Recognize the common C/C++ build systems (CMake, Make, Meson, Autotools).
    // When any is present, classify the language as "C" vs "C++" using the
    // source files actually in the directory: a tree with only *.c/*.h and no
    // C++ sources is C; any *.cpp/*.cc/*.cxx/*.hpp/*.hxx implies C++. When no
    // source files are present (or only a bare Makefile with no sources), record
    // the build tool but do not claim a C/C++ language — a Makefile alone is a
    // weak signal (it may drive docs, asm, or anything).
    let cmake = dir.join("CMakeLists.txt").exists();
    let makefile = dir.join("Makefile").exists() || dir.join("makefile").exists();
    let meson = dir.join("meson.build").exists();
    let autotools = dir.join("configure.ac").exists()
        || dir.join("configure.in").exists()
        || dir.join("configure").exists();

    if cmake {
        build_tools.push(comp("CMake", "CMakeLists.txt"));
    }
    if meson {
        build_tools.push(comp("Meson", "meson.build"));
    }
    if autotools {
        build_tools.push(comp("Autotools", "configure.ac"));
    }
    if makefile {
        build_tools.push(comp("Make", "Makefile"));
    }

    if cmake || makefile || meson || autotools {
        match classify_c_family(dir) {
            Some(CFamily::Cpp) => {
                languages.push(comp(
                    "C++",
                    c_family_evidence(cmake, meson, autotools, makefile),
                ));
                suggested_skills.push("cpp.skill.md".to_string());
            }
            Some(CFamily::C) => {
                languages.push(comp(
                    "C",
                    c_family_evidence(cmake, meson, autotools, makefile),
                ));
                suggested_skills.push("cpp.skill.md".to_string());
            }
            None => {
                // A build system whose source language is undeterminable.
                // CMake/Meson/Autotools strongly imply a C-family project even
                // without sources checked in (sources may be generated), so we
                // tag the ambiguous "C/C++"; a bare Makefile alone does not.
                if cmake || meson || autotools {
                    languages.push(comp(
                        "C/C++",
                        c_family_evidence(cmake, meson, autotools, makefile),
                    ));
                    suggested_skills.push("cpp.skill.md".to_string());
                }
            }
        }
    }

    // Docker
    if dir.join("Dockerfile").exists() || dir.join("docker-compose.yml").exists() {
        let evidence = if dir.join("Dockerfile").exists() {
            "Dockerfile"
        } else {
            "docker-compose.yml"
        };
        build_tools.push(comp("Docker", evidence));
        suggested_skills.push("docker-dev.skill.md".to_string());
    }

    // CI
    if dir.join(".github/workflows").exists() {
        ci.push(comp("GitHub Actions", ".github/workflows/"));
        suggested_skills.push("ci-cd.skill.md".to_string());
    }
    if dir.join(".gitlab-ci.yml").exists() {
        ci.push(comp("GitLab CI", ".gitlab-ci.yml"));
        suggested_skills.push("ci-cd.skill.md".to_string());
    }

    // Fly.io
    if dir.join("fly.toml").exists() {
        build_tools.push(comp("Fly.io", "fly.toml"));
        if languages.iter().any(|c| c.name == "Elixir") {
            suggested_skills.push("devops-fly-elixir.skill.md".to_string());
        }
    }

    // Linters
    if dir.join(".eslintrc.js").exists()
        || dir.join(".eslintrc.json").exists()
        || dir.join("eslint.config.js").exists()
    {
        linters.push(comp("ESLint", ".eslintrc.*"));
    }
    if dir.join("ruff.toml").exists() || dir.join(".ruff.toml").exists() {
        linters.push(comp("Ruff", "ruff.toml"));
    }
    if dir.join(".pre-commit-config.yaml").exists() {
        linters.push(comp("pre-commit", ".pre-commit-config.yaml"));
    }

    // Deduplicate suggested skills
    suggested_skills.sort();
    suggested_skills.dedup();

    ProjectReport {
        languages,
        frameworks,
        test_runners,
        linters,
        build_tools,
        ci,
        suggested_skills,
    }
}

/// The C-family language detected from source file extensions.
#[derive(Debug, PartialEq)]
enum CFamily {
    C,
    Cpp,
}

/// Classify a directory's C-family source files.
///
/// Returns `Some(Cpp)` if any C++ source/header extension is present (`.cpp`,
/// `.cc`, `.cxx`, `.c++`, `.hpp`, `.hxx`, `.hh`), `Some(C)` if only C
/// extensions are present (`.c`, `.h`), or `None` if no C/C++ sources are
/// found (the language is then undeterminable from sources alone). `.h` is
/// treated as C-only here; mixed C++ trees are caught by their `.cpp`/`.hpp`
/// siblings, so a lone `.h` next to `.cpp` still classifies as C++.
fn classify_c_family(dir: &Path) -> Option<CFamily> {
    const CPP_EXTS: &[&str] = &["cpp", "cc", "cxx", "c++", "hpp", "hxx", "hh"];
    const C_EXTS: &[&str] = &["c", "h"];

    let mut has_c = false;
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if CPP_EXTS.iter().any(|e| e.eq_ignore_ascii_case(ext)) {
            return Some(CFamily::Cpp);
        }
        if C_EXTS.contains(&ext) {
            has_c = true;
        }
    }
    has_c.then_some(CFamily::C)
}

/// Pick the strongest build-system evidence string for a C-family language tag.
fn c_family_evidence(cmake: bool, meson: bool, autotools: bool, makefile: bool) -> &'static str {
    if cmake {
        "CMakeLists.txt"
    } else if meson {
        "meson.build"
    } else if autotools {
        "configure.ac"
    } else if makefile {
        "Makefile"
    } else {
        "C/C++ sources"
    }
}

/// Check if the directory contains any file with the given extension.
fn has_ext(dir: &Path, ext: &str) -> bool {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .any(|e| e.path().extension().is_some_and(|x| x == ext))
}

/// Check if the directory contains any `.csproj` or `.sln` files.
fn has_csproj_or_sln(dir: &Path) -> bool {
    has_ext(dir, "csproj") || has_ext(dir, "sln")
}

fn comp(name: &str, evidence: &str) -> DetectedComponent {
    DetectedComponent {
        name: name.to_string(),
        evidence: evidence.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // === Test list (TDD) ===
    // [x] empty directory: no detections
    // [x] Rust project: Cargo.toml detected
    // [x] Python project: pyproject.toml detected
    // [x] Go project: go.mod detected
    // [x] TypeScript project: tsconfig.json detected
    // [x] Elixir project: mix.exs detected
    // [x] C# project: .csproj detected
    // [x] C# solution: .sln detected
    // [x] Docker detected
    // [x] GitHub Actions detected
    // [x] multi-language project
    // [x] suggested skills populated

    fn setup_dir(files: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for file in files {
            let path = dir.path().join(file);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&path, "").unwrap();
        }
        dir
    }

    #[test]
    fn empty_directory() {
        let dir = tempfile::tempdir().unwrap();
        let result = detect(dir.path());
        assert!(result.languages.is_empty());
    }

    #[test]
    fn rust_project() {
        let dir = setup_dir(&["Cargo.toml"]);
        let result = detect(dir.path());
        assert!(result.languages.iter().any(|c| c.name == "Rust"));
        assert!(result
            .suggested_skills
            .contains(&"rust.skill.md".to_string()));
    }

    #[test]
    fn python_project() {
        let dir = setup_dir(&["pyproject.toml", "tests/test_app.py"]);
        let result = detect(dir.path());
        assert!(result.languages.iter().any(|c| c.name == "Python"));
        assert!(result.test_runners.iter().any(|c| c.name == "pytest"));
    }

    #[test]
    fn go_project() {
        let dir = setup_dir(&["go.mod"]);
        let result = detect(dir.path());
        assert!(result.languages.iter().any(|c| c.name == "Go"));
    }

    #[test]
    fn typescript_project() {
        let dir = setup_dir(&["package.json", "tsconfig.json", "jest.config.ts"]);
        let result = detect(dir.path());
        assert!(result.languages.iter().any(|c| c.name == "TypeScript"));
        assert!(result.test_runners.iter().any(|c| c.name == "Jest"));
    }

    #[test]
    fn elixir_project() {
        let dir = setup_dir(&["mix.exs"]);
        let result = detect(dir.path());
        assert!(result.languages.iter().any(|c| c.name == "Elixir"));
    }

    #[test]
    fn csharp_csproj_detected() {
        let dir = setup_dir(&["MyApp.csproj"]);
        let result = detect(dir.path());
        assert!(result.languages.iter().any(|c| c.name == "C#"));
        assert!(result.build_tools.iter().any(|c| c.name == "dotnet CLI"));
        assert!(result.test_runners.iter().any(|c| c.name == "dotnet test"));
        assert!(result
            .suggested_skills
            .contains(&"csharp.skill.md".to_string()));
    }

    #[test]
    fn csharp_sln_detected() {
        let dir = setup_dir(&["MyApp.sln"]);
        let result = detect(dir.path());
        assert!(result.languages.iter().any(|c| c.name == "C#"));
        assert!(
            result.languages.iter().any(|c| c.evidence == "*.sln"),
            "evidence should be *.sln for solution files"
        );
    }

    #[test]
    fn csharp_aspnet_detected() {
        let dir = setup_dir(&["MyApp.csproj", "Program.cs"]);
        let result = detect(dir.path());
        assert!(result.languages.iter().any(|c| c.name == "C#"));
        assert!(result.frameworks.iter().any(|c| c.name.contains("ASP.NET")));
    }

    #[test]
    fn docker_detected() {
        let dir = setup_dir(&["Dockerfile"]);
        let result = detect(dir.path());
        assert!(result.build_tools.iter().any(|c| c.name == "Docker"));
    }

    #[test]
    fn github_actions_detected() {
        let dir = setup_dir(&[".github/workflows/ci.yml"]);
        let result = detect(dir.path());
        assert!(result.ci.iter().any(|c| c.name == "GitHub Actions"));
    }

    #[test]
    fn multi_language_project() {
        let dir = setup_dir(&["Cargo.toml", "pyproject.toml", "Dockerfile"]);
        let result = detect(dir.path());
        assert!(result.languages.len() >= 2);
        assert!(result.suggested_skills.len() >= 2);
    }

    #[test]
    fn suggested_skills_populated() {
        let dir = setup_dir(&["Cargo.toml", ".github/workflows/ci.yml"]);
        let result = detect(dir.path());
        assert!(result
            .suggested_skills
            .contains(&"rust.skill.md".to_string()));
        assert!(result
            .suggested_skills
            .contains(&"ci-cd.skill.md".to_string()));
    }

    // === C/C++ detection (task t_29e8dc26) ===

    #[test]
    fn cmake_cpp_project() {
        // Existing behavior must be preserved: CMakeLists.txt → C/C++.
        let dir = setup_dir(&["CMakeLists.txt", "main.cpp"]);
        let result = detect(dir.path());
        assert!(
            result.languages.iter().any(|c| c.name == "C++"),
            "CMake + .cpp should detect C++"
        );
        assert!(result.build_tools.iter().any(|c| c.name == "CMake"));
        assert!(result
            .suggested_skills
            .contains(&"cpp.skill.md".to_string()));
    }

    #[test]
    fn makefile_c_project() {
        // A Makefile next to only C sources is a C build, not just "Make".
        let dir = setup_dir(&["Makefile", "main.c", "util.h"]);
        let result = detect(dir.path());
        assert!(
            result.languages.iter().any(|c| c.name == "C"),
            "Makefile + only .c/.h should detect C, got {:?}",
            result.languages
        );
        assert!(result.build_tools.iter().any(|c| c.name == "Make"));
        assert!(result
            .suggested_skills
            .contains(&"cpp.skill.md".to_string()));
    }

    #[test]
    fn meson_project_detected() {
        let dir = setup_dir(&["meson.build", "main.cpp"]);
        let result = detect(dir.path());
        assert!(
            result.languages.iter().any(|c| c.name == "C++"),
            "meson.build + .cpp should detect C++"
        );
        assert!(result.build_tools.iter().any(|c| c.name == "Meson"));
    }

    #[test]
    fn autotools_project_detected() {
        let dir = setup_dir(&["configure.ac", "main.c"]);
        let result = detect(dir.path());
        assert!(
            result.languages.iter().any(|c| c.name == "C"),
            "configure.ac + only .c should detect C"
        );
        assert!(result.build_tools.iter().any(|c| c.name == "Autotools"));
    }

    #[test]
    fn c_only_vs_cpp_distinction() {
        // Only C sources present → "C".
        let c_dir = setup_dir(&["Makefile", "a.c", "b.h"]);
        let c_result = detect(c_dir.path());
        assert!(c_result.languages.iter().any(|c| c.name == "C"));
        assert!(
            !c_result.languages.iter().any(|c| c.name == "C++"),
            "pure C tree must not be tagged C++"
        );

        // C++ sources present → "C++".
        let cpp_dir = setup_dir(&["Makefile", "a.cpp", "b.hpp"]);
        let cpp_result = detect(cpp_dir.path());
        assert!(cpp_result.languages.iter().any(|c| c.name == "C++"));
        assert!(
            !cpp_result.languages.iter().any(|c| c.name == "C"),
            "C++ tree should be tagged C++, not C"
        );
    }

    #[test]
    fn makefile_without_c_sources_is_just_make() {
        // A Makefile with no C/C++ sources (e.g. a docs-only Makefile) should
        // not be misclassified as a C/C++ project — only "Make" build tool.
        let dir = setup_dir(&["Makefile", "README.md"]);
        let result = detect(dir.path());
        assert!(result.build_tools.iter().any(|c| c.name == "Make"));
        assert!(
            !result
                .languages
                .iter()
                .any(|c| c.name == "C" || c.name == "C++"),
            "Makefile with no C/C++ sources should not detect a C/C++ language"
        );
    }
}
