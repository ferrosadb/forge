//! TOML-based filter registry for command detection.
//!
//! Replaces hardcoded if-contains chains with a declarative, extensible
//! filter configuration. Supports built-in defaults and user overrides.
//!
//! Patterns are pre-compiled at load time: plain strings become literal
//! substring matches, patterns containing regex metacharacters (`.*`, `\`,
//! `[`) are compiled as `Regex`.

use regex::Regex;
use serde::Deserialize;
use std::path::PathBuf;

/// A single filter rule as deserialized from TOML.
#[derive(Debug, Clone, Deserialize)]
pub struct FilterRule {
    pub name: String,
    pub patterns: Vec<String>,
    /// Commands matching an exclude pattern skip this filter (fall through).
    #[serde(default)]
    pub exclude: Vec<String>,
}

/// A pre-compiled pattern: either a literal substring or a regex.
#[derive(Debug, Clone)]
pub enum CompiledPattern {
    Literal(String),
    Regex(Regex),
}

impl CompiledPattern {
    /// Compile a pattern string. If it contains regex metacharacters,
    /// compile as regex; otherwise store as a lowercase literal.
    fn compile(pattern: &str) -> Self {
        let lower = pattern.to_lowercase();
        if lower.contains(".*") || lower.contains('\\') || lower.contains('[') {
            match Regex::new(&lower) {
                Ok(re) => CompiledPattern::Regex(re),
                Err(_) => CompiledPattern::Literal(lower),
            }
        } else {
            CompiledPattern::Literal(lower)
        }
    }

    /// Test whether this pattern matches the given (already-lowercased) command.
    fn matches(&self, cmd: &str) -> bool {
        match self {
            CompiledPattern::Literal(s) => cmd.contains(s.as_str()),
            CompiledPattern::Regex(re) => re.is_match(cmd),
        }
    }
}

/// A compiled filter rule ready for matching.
#[derive(Debug, Clone)]
pub struct CompiledRule {
    pub name: String,
    pub patterns: Vec<CompiledPattern>,
    pub excludes: Vec<CompiledPattern>,
    /// Original pattern strings (for display/serialization).
    pub pattern_sources: Vec<String>,
}

/// Fallback configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct FallbackConfig {
    pub name: String,
}

/// Top-level TOML config structure.
#[derive(Debug, Clone, Deserialize)]
pub struct FilterConfig {
    #[serde(default)]
    pub filter: Vec<FilterRule>,
    pub fallback: Option<FallbackConfig>,
}

/// Where the registry was loaded from.
#[derive(Debug, Clone)]
pub enum FilterSource {
    Builtin,
    UserFile(PathBuf),
    Merged { user_file: PathBuf },
}

/// Registry of filter rules with first-match detection.
/// Rules are pre-compiled at load time for efficient matching.
#[derive(Debug, Clone)]
pub struct FilterRegistry {
    pub rules: Vec<CompiledRule>,
    pub fallback: String,
    pub source: FilterSource,
}

/// Built-in filter definitions (compiled into the binary).
const BUILTIN_FILTERS: &str = r#"
[[filter]]
name = "test-summary"
patterns = [
    "cargo test", "cargo nextest", "pytest", "uv run pytest",
    "jest", "vitest", "go test", "mix test", "swift test",
    "dotnet test",
    "elixir.*test", "rebar3.*proper",
]
exclude = [
    "--no-run", "--list", "--list-tests",
]

[[filter]]
name = "lint-dedup"
patterns = [
    "clippy", "ruff", "eslint", "biome",
    "golangci-lint", "mypy", "oxlint",
    "dotnet format", "roslyn",
]

[[filter]]
name = "diff-filter"
patterns = ["git diff"]

[[filter]]
name = "log-distill"
patterns = [
    "cargo build", "cargo check", "npm run build",
    "dotnet build", "dotnet publish", "msbuild",
    "make", "gcc", "g++", "tsc",
]

[fallback]
name = "log-distill"
"#;

impl FilterRegistry {
    /// Compile a list of TOML `FilterRule`s into `CompiledRule`s.
    fn compile_rules(rules: Vec<FilterRule>) -> Vec<CompiledRule> {
        rules
            .into_iter()
            .map(|r| CompiledRule {
                name: r.name,
                excludes: r
                    .exclude
                    .iter()
                    .map(|p| CompiledPattern::compile(p))
                    .collect(),
                patterns: r
                    .patterns
                    .iter()
                    .map(|p| CompiledPattern::compile(p))
                    .collect(),
                pattern_sources: r.patterns,
            })
            .collect()
    }

    /// Load the filter registry: try user file, fall back to builtins.
    /// Patterns are pre-compiled at load time.
    pub fn load() -> Self {
        let user_path = Self::user_config_path();

        // Try to load and merge user config
        if let Some(ref path) = user_path {
            if path.exists() {
                if let Ok(content) = std::fs::read_to_string(path) {
                    if let Ok(user_config) = toml::from_str::<FilterConfig>(&content) {
                        let builtin_config: FilterConfig =
                            toml::from_str(BUILTIN_FILTERS).expect("builtin TOML is valid");

                        // User rules come first (higher priority), then builtins
                        let mut rules = user_config.filter;
                        rules.extend(builtin_config.filter);

                        let fallback = user_config
                            .fallback
                            .or(builtin_config.fallback)
                            .map(|f| f.name)
                            .unwrap_or_else(|| "log-distill".to_string());

                        return Self {
                            rules: Self::compile_rules(rules),
                            fallback,
                            source: FilterSource::Merged {
                                user_file: path.clone(),
                            },
                        };
                    }
                }
            }
        }

        // Fall back to builtins only
        let config: FilterConfig = toml::from_str(BUILTIN_FILTERS).expect("builtin TOML is valid");

        Self {
            rules: Self::compile_rules(config.filter),
            fallback: config
                .fallback
                .map(|f| f.name)
                .unwrap_or_else(|| "log-distill".to_string()),
            source: FilterSource::Builtin,
        }
    }

    /// Output-truncating commands that make specialized filter output useless.
    const PIPE_TRUNCATORS: &'static [&'static str] = &[
        "grep", "tail", "head", "awk", "sed", "wc", "cut", "sort", "uniq",
    ];

    /// Detect which filter to use for a given command string.
    /// First match wins against pre-compiled patterns.
    /// The command is normalized first: quoted argument values are stripped,
    /// `&&`/`;` chains are split (last segment wins), and piped commands
    /// that truncate output fall back to the default filter.
    pub fn detect(&self, command: &str) -> &str {
        let normalized = Self::normalize_command(command);
        let cmd = normalized.to_lowercase();
        let matched = self.match_filter(&cmd);

        // If the matched filter is NOT the fallback, check whether the
        // command pipes through a truncator — if so, the specialized filter
        // will receive useless input, so use the fallback instead.
        if matched != self.fallback && Self::has_pipe_truncator(command) {
            return &self.fallback;
        }

        matched
    }

    /// Match a normalized command against filter rules (no pipe check).
    /// If a command matches a rule's patterns but also matches an exclude,
    /// that rule is skipped and matching continues.
    fn match_filter(&self, cmd: &str) -> &str {
        for rule in &self.rules {
            let matches_pattern = rule.patterns.iter().any(|p| p.matches(cmd));
            if !matches_pattern {
                continue;
            }
            let excluded = rule.excludes.iter().any(|p| p.matches(cmd));
            if excluded {
                continue;
            }
            return &rule.name;
        }
        &self.fallback
    }

    /// Check if the original command pipes through a truncating tool.
    fn has_pipe_truncator(command: &str) -> bool {
        // Find the first unquoted pipe, then check what follows
        let mut in_single_quote = false;
        let mut in_double_quote = false;
        let mut found_pipe = false;

        for ch in command.chars() {
            match ch {
                '\'' if !in_double_quote => in_single_quote = !in_single_quote,
                '"' if !in_single_quote => in_double_quote = !in_double_quote,
                '|' if !in_single_quote && !in_double_quote => {
                    found_pipe = true;
                    break;
                }
                _ => {}
            }
        }

        if !found_pipe {
            return false;
        }

        // Get text after the first pipe
        let after_pipe = command
            .split_once('|')
            .map(|x| x.1)
            .unwrap_or("")
            .trim()
            .to_lowercase();
        Self::PIPE_TRUNCATORS
            .iter()
            .any(|t| after_pipe.starts_with(t))
    }

    /// Normalize a shell command for filter detection by:
    /// 1. Splitting on unquoted `&&` and `;` — using the last segment
    /// 2. Stripping content inside quotes (argument values like commit messages)
    /// 3. Truncating at the first unquoted pipe `|` (pipeline stages)
    /// 4. Removing shell redirections (`2>&1`, `>/dev/null`, etc.)
    ///
    /// This prevents false positives like `git commit -m "fix cargo test"`
    /// from matching the `cargo test` pattern, and ensures piped commands
    /// like `cargo test | grep foo` only match on the actual command.
    fn normalize_command(command: &str) -> String {
        // Step 1: split on unquoted && and ; — use the last segment
        let segment = Self::last_chain_segment(command);

        // Step 2-4: strip quotes, stop at pipe, remove redirections
        let mut result = String::new();
        let mut in_single_quote = false;
        let mut in_double_quote = false;
        let mut chars = segment.chars().peekable();

        while let Some(ch) = chars.next() {
            match ch {
                '\\' if in_double_quote => {
                    // Skip escaped char inside double quotes
                    chars.next();
                }
                '\'' if !in_double_quote => {
                    in_single_quote = !in_single_quote;
                    result.push(' ');
                }
                '"' if !in_single_quote => {
                    in_double_quote = !in_double_quote;
                    result.push(' ');
                }
                '|' if !in_single_quote && !in_double_quote => {
                    break; // Stop at first unquoted pipe
                }
                _ if in_single_quote || in_double_quote => {
                    // Inside quotes — skip (these are argument values)
                }
                _ => {
                    result.push(ch);
                }
            }
        }

        // Remove common shell redirections
        let re_redir = Regex::new(r"\d*>&?\d+|\d*>\s*/dev/null").unwrap();
        re_redir.replace_all(&result, " ").to_string()
    }

    /// Extract the last command segment from a `&&`/`;` chain,
    /// respecting quoted strings so that `git commit -m "a && b"` is
    /// treated as a single segment.
    fn last_chain_segment(command: &str) -> &str {
        let mut in_single_quote = false;
        let mut in_double_quote = false;
        let mut last_start = 0;
        let bytes = command.as_bytes();
        let len = bytes.len();
        let mut i = 0;

        while i < len {
            let ch = bytes[i] as char;
            match ch {
                '\'' if !in_double_quote => in_single_quote = !in_single_quote,
                '"' if !in_single_quote => in_double_quote = !in_double_quote,
                '&' if !in_single_quote
                    && !in_double_quote
                    && i + 1 < len
                    && bytes[i + 1] == b'&' =>
                {
                    last_start = i + 2;
                    i += 2;
                    continue;
                }
                ';' if !in_single_quote && !in_double_quote => {
                    last_start = i + 1;
                }
                _ => {}
            }
            i += 1;
        }

        command[last_start..].trim()
    }

    /// List all active filter rules (for `--list-filters`).
    pub fn list(&self) -> Vec<FilterInfo> {
        self.rules
            .iter()
            .map(|r| FilterInfo {
                name: r.name.clone(),
                patterns: r.pattern_sources.clone(),
            })
            .collect()
    }

    /// Path to user config file.
    fn user_config_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("forge").join("filters.toml"))
    }
}

/// Serializable filter info for display.
#[derive(Debug, serde::Serialize)]
pub struct FilterInfo {
    pub name: String,
    pub patterns: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_loads() {
        let reg = FilterRegistry::load();
        assert!(!reg.rules.is_empty());
        assert_eq!(reg.fallback, "log-distill");
    }

    #[test]
    fn detect_test_runners() {
        let reg = FilterRegistry::load();
        assert_eq!(reg.detect("cargo test --release"), "test-summary");
        assert_eq!(reg.detect("pytest -x tests/"), "test-summary");
        assert_eq!(reg.detect("mix test --trace"), "test-summary");
        assert_eq!(reg.detect("go test ./..."), "test-summary");
    }

    #[test]
    fn detect_linters() {
        let reg = FilterRegistry::load();
        assert_eq!(reg.detect("cargo clippy --all-targets"), "lint-dedup");
        assert_eq!(reg.detect("ruff check src/"), "lint-dedup");
    }

    #[test]
    fn detect_diff() {
        let reg = FilterRegistry::load();
        assert_eq!(reg.detect("git diff --staged"), "diff-filter");
    }

    #[test]
    fn detect_builds() {
        let reg = FilterRegistry::load();
        assert_eq!(reg.detect("cargo build"), "log-distill");
        assert_eq!(reg.detect("npm run build"), "log-distill");
    }

    #[test]
    fn detect_unknown_falls_back() {
        let reg = FilterRegistry::load();
        assert_eq!(reg.detect("some random command"), "log-distill");
    }

    #[test]
    fn normalize_strips_quoted_content() {
        let norm = FilterRegistry::normalize_command;
        assert_eq!(
            norm(r#"git commit -m "fix cargo test issue""#).trim(),
            "git commit -m"
        );
        assert_eq!(
            norm("gh pr create --body 'contains mix test'").trim(),
            "gh pr create --body"
        );
    }

    #[test]
    fn normalize_stops_at_pipe() {
        let norm = FilterRegistry::normalize_command;
        assert!(norm("cargo test 2>&1 | grep passed").contains("cargo test"));
        assert!(!norm("cargo test 2>&1 | grep passed").contains("grep"));
    }

    #[test]
    fn normalize_strips_redirections() {
        let norm = FilterRegistry::normalize_command;
        let result = norm("cargo test 2>&1");
        assert!(result.contains("cargo test"));
        assert!(!result.contains("2>&1"));
    }

    #[test]
    fn detect_no_false_positive_in_quoted_args() {
        let reg = FilterRegistry::load();
        // "cargo test" inside a commit message should NOT trigger test-summary
        assert_eq!(
            reg.detect(r#"git commit -m "fix cargo test timeout""#),
            "log-distill"
        );
        // "mix test" inside a PR body should NOT trigger test-summary
        assert_eq!(
            reg.detect("gh pr create --body 'ran mix test and it passed'"),
            "log-distill"
        );
    }

    #[test]
    fn detect_piped_through_truncator_uses_fallback() {
        let reg = FilterRegistry::load();
        // Piped through grep/tail → fallback, because output is truncated
        assert_eq!(reg.detect("cargo test 2>&1 | grep passed"), "log-distill");
        assert_eq!(reg.detect("mix test --trace | tail -20"), "log-distill");
        assert_eq!(reg.detect("cargo test | head -5"), "log-distill");
    }

    #[test]
    fn detect_piped_through_non_truncator_keeps_filter() {
        let reg = FilterRegistry::load();
        // Piped through tee or cat → not a truncator, keep specialized filter
        assert_eq!(reg.detect("cargo test 2>&1 | tee log.txt"), "test-summary");
    }

    #[test]
    fn detect_regex_pattern_works() {
        let reg = FilterRegistry::load();
        // "elixir.*test" pattern should match as regex
        assert_eq!(reg.detect("elixir some_test.exs"), "test-summary");
    }

    #[test]
    fn detect_chain_uses_last_segment() {
        let reg = FilterRegistry::load();
        // Last command in chain is what matters
        assert_eq!(
            reg.detect("cargo fmt && cargo test --release"),
            "test-summary"
        );
        assert_eq!(reg.detect("cargo build && cargo clippy"), "lint-dedup");
        assert_eq!(reg.detect("cargo test && cargo build"), "log-distill");
    }

    #[test]
    fn detect_chain_with_semicolons() {
        let reg = FilterRegistry::load();
        assert_eq!(reg.detect("cd /tmp; cargo test"), "test-summary");
    }

    #[test]
    fn detect_chain_respects_quotes() {
        let reg = FilterRegistry::load();
        // && inside quotes is NOT a chain separator
        assert_eq!(
            reg.detect(r#"git commit -m "foo && bar"; cargo test"#),
            "test-summary"
        );
    }

    #[test]
    fn detect_chain_piped_through_truncator() {
        let reg = FilterRegistry::load();
        // Last segment is cargo test, but output is piped through grep
        assert_eq!(
            reg.detect("cargo fmt && cargo test 2>&1 | grep passed"),
            "log-distill"
        );
    }

    #[test]
    fn detect_test_no_run_excluded() {
        let reg = FilterRegistry::load();
        // --no-run compiles but doesn't run tests — no test output to parse
        assert_eq!(reg.detect("cargo test --no-run"), "log-distill");
        assert_eq!(
            reg.detect("cargo test -p ferrosa-test-graph --no-run"),
            "log-distill"
        );
    }

    #[test]
    fn detect_test_list_excluded() {
        let reg = FilterRegistry::load();
        // --list enumerates tests without running them
        assert_eq!(reg.detect("cargo test -- --list"), "log-distill");
        assert_eq!(
            reg.detect("cargo test -p ferrosa-cluster batchlog -- --list"),
            "log-distill"
        );
    }

    #[test]
    fn detect_test_normal_not_excluded() {
        let reg = FilterRegistry::load();
        // Normal test commands still match test-summary
        assert_eq!(reg.detect("cargo test"), "test-summary");
        assert_eq!(reg.detect("cargo test -p my-crate"), "test-summary");
        assert_eq!(reg.detect("pytest -x tests/"), "test-summary");
    }

    #[test]
    fn list_returns_all_rules() {
        let reg = FilterRegistry::load();
        let list = reg.list();
        let names: Vec<&str> = list.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"test-summary"));
        assert!(names.contains(&"lint-dedup"));
        assert!(names.contains(&"diff-filter"));
        assert!(names.contains(&"log-distill"));
    }
}
