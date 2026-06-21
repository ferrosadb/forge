//! Tool alias resolution for Foundry integration.
//!
//! Provides a canonical alias map that resolves common LLM tool name mismatches.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Built-in aliases compiled into the binary.
const BUILTIN_ALIASES_TOML: &str = include_str!("aliases_builtin.toml");

/// The alias map structure returned by `frg tool-aliases`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AliasMap {
    /// Schema version
    pub version: u32,
    /// List of canonical tool names
    pub canonical_tools: Vec<String>,
    /// Alias mappings: alias -> canonical_name
    pub aliases: HashMap<String, String>,
    /// Fuzzy suggestions for common typos
    pub fuzzy_suggestions: HashMap<String, String>,
}

/// Returns the complete alias map (built-in + config file overrides).
pub fn get_alias_map() -> AliasMap {
    // Parse built-in aliases
    let mut aliases: HashMap<String, String> = parse_alias_file(BUILTIN_ALIASES_TOML)
        .unwrap_or_else(|e| {
            eprintln!("Warning: Failed to parse built-in aliases: {}", e);
            HashMap::new()
        });

    // Load global config overrides
    if let Some(global_path) = dirs::config_dir().map(|d| d.join("forge").join("aliases.toml")) {
        if global_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&global_path) {
                if let Ok(override_aliases) = parse_alias_file(&content) {
                    for (k, v) in override_aliases {
                        aliases.insert(k, v);
                    }
                }
            }
        }
    }

    // Load project-local config overrides
    if let Ok(cwd) = std::env::current_dir() {
        let project_path = cwd.join("forge-aliases.toml");
        if project_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&project_path) {
                if let Ok(override_aliases) = parse_alias_file(&content) {
                    for (k, v) in override_aliases {
                        aliases.insert(k, v);
                    }
                }
            }
        }
    }

    // Generate fuzzy suggestions
    let fuzzy_suggestions = generate_fuzzy_suggestions(&aliases);

    AliasMap {
        version: 1,
        canonical_tools: extract_canonical_tools(&aliases),
        aliases,
        fuzzy_suggestions,
    }
}

/// Parse alias file in TOML format.
fn parse_alias_file(content: &str) -> Result<HashMap<String, String>, toml::de::Error> {
    #[derive(Deserialize)]
    struct AliasFile {
        alias: Vec<AliasEntry>,
    }

    #[derive(Deserialize)]
    struct AliasEntry {
        from: String,
        to: String,
    }

    let file: AliasFile = toml::from_str(content)?;
    Ok(file.alias.into_iter().map(|a| (a.from, a.to)).collect())
}

/// Extract unique canonical tool names from the alias map.
fn extract_canonical_tools(aliases: &HashMap<String, String>) -> Vec<String> {
    let mut tools: Vec<String> = aliases.values().cloned().collect();
    tools.sort();
    tools.dedup();
    tools
}

/// Generate fuzzy suggestions for common typos (Levenshtein distance ≤ 3).
fn generate_fuzzy_suggestions(aliases: &HashMap<String, String>) -> HashMap<String, String> {
    let mut suggestions = HashMap::new();

    // Common typo patterns
    let typo_patterns = [
        ("edit", "edt"),
        ("write", "wrte"),
        ("read", "rad"),
        ("glob", "globb"),
        ("grep", "grepp"),
        ("search", "serch"),
        ("run", "rn"),
        ("execute", "execut"),
    ];

    for (canonical, typo) in &typo_patterns {
        // Find if there's an alias pointing to a tool containing the canonical name
        for (alias, target) in aliases {
            if target.contains(canonical) || alias.contains(canonical) {
                suggestions.insert(typo.to_string(), target.clone());
            }
        }
    }

    suggestions
}

/// Format alias map as a human-readable table.
pub fn format_as_table(aliases: &AliasMap) -> String {
    let mut output = String::new();
    output.push_str("Forge Tool Aliases\n");
    output.push_str("==================\n\n");

    output.push_str("Canonical Tools:\n");
    for tool in &aliases.canonical_tools {
        output.push_str(&format!("  - {}\n", tool));
    }

    output.push_str("\nAliases:\n");
    let mut alias_pairs: Vec<_> = aliases.aliases.iter().collect();
    alias_pairs.sort_by(|a, b| a.0.cmp(b.0));
    for (from, to) in alias_pairs {
        output.push_str(&format!("  {:20} -> {}\n", from, to));
    }

    if !aliases.fuzzy_suggestions.is_empty() {
        output.push_str("\nFuzzy Suggestions (not auto-resolved):\n");
        let mut fuzzy_pairs: Vec<_> = aliases.fuzzy_suggestions.iter().collect();
        fuzzy_pairs.sort_by(|a, b| a.0.cmp(b.0));
        for (typo, target) in fuzzy_pairs {
            output.push_str(&format!("  {:20} -> {}\n", typo, target));
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_alias_map_returns_valid_structure() {
        let map = get_alias_map();
        assert_eq!(map.version, 1);
        assert!(!map.canonical_tools.is_empty());
        assert!(!map.aliases.is_empty());
    }

    #[test]
    fn builtin_aliases_contain_expected_mappings() {
        let map = get_alias_map();
        // Verify some key aliases exist
        assert!(map.aliases.contains_key("Edit"));
        assert!(map.aliases.contains_key("Bash"));
        assert!(map.aliases.contains_key("Read"));
        assert!(map.aliases.contains_key("Write"));
    }
}
