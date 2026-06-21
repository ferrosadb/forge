use crate::cycles::CycleInfo;
use crate::matrix::DsmMatrix;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// User-defined module specification (from modules.toml).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleSpec {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub seeds: Vec<String>,
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

/// Configuration for directed module extraction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectedConfig {
    pub modules: Vec<ModuleSpec>,
}

/// Result of directed module extraction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectedResult {
    pub modules: Vec<DirectedModule>,
    pub unassigned: Vec<UnassignedElement>,
    pub violations: Vec<BoundaryViolation>,
    pub migration_plan: Vec<MigrationStep>,
}

/// A module as analyzed by directed extraction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectedModule {
    pub name: String,
    pub description: String,
    pub current_members: Vec<String>,
    pub suggested_additions: Vec<SuggestedMove>,
    pub internal_cohesion: f64,
    pub external_coupling: f64,
    pub boundary_deps: Vec<BoundaryDep>,
}

/// A suggested move of an element into a module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuggestedMove {
    pub element: String,
    pub fit_score: f64,
    pub rationale: String,
}

/// A dependency that crosses a module boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoundaryDep {
    pub from: String,
    pub to: String,
    pub from_module: String,
    pub to_module: String,
    pub is_necessary: bool,
}

/// An element not assigned to any module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnassignedElement {
    pub name: String,
    pub best_fit_module: String,
    pub fit_score: f64,
    pub dep_counts: HashMap<String, usize>,
}

/// A violation of module boundaries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoundaryViolation {
    pub element: String,
    pub current_module: String,
    pub violation_kind: ViolationKind,
    pub details: String,
}

/// Kind of boundary violation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ViolationKind {
    WrongModule,
    CrossModuleCycle,
    LeakyAbstraction,
    GodElement,
}

/// A step in the migration plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationStep {
    pub order: usize,
    pub action: MigrationAction,
    pub element: String,
    pub from_module: Option<String>,
    pub to_module: String,
    pub rationale: String,
    pub risk: Risk,
    pub deps_to_update: Vec<String>,
}

/// Kind of migration action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MigrationAction {
    MoveElement,
    ExtractInterface,
    SplitElement,
    IntroduceMediator,
}

/// Risk level for a migration step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Risk {
    Low,
    Medium,
    High,
}

/// Perform directed module extraction.
///
/// Algorithm:
/// 1. Seed expansion: assign elements matching seeds/include patterns
/// 2. Affinity computation: for unassigned elements, count deps to each module
/// 3. Conflict detection: flag elements with strong ties to multiple modules
/// 4. Boundary analysis: identify cross-module deps (necessary vs accidental)
/// 5. Migration plan: order refactoring steps
pub fn directed_analysis(
    matrix: &DsmMatrix,
    config: &DirectedConfig,
    cycles: &CycleInfo,
) -> DirectedResult {
    let n = matrix.size();

    // Step 1: Seed expansion — assign elements to modules
    let mut assignment: HashMap<String, String> = HashMap::new();
    for module_spec in &config.modules {
        for label in &matrix.labels {
            if is_member(label, module_spec) {
                assignment.insert(label.clone(), module_spec.name.clone());
            }
        }
    }

    // Step 2: Affinity-based assignment for unassigned elements
    let mut unassigned = Vec::new();
    for (i, label) in matrix.labels.iter().enumerate() {
        if assignment.contains_key(label) {
            continue;
        }

        // Count deps to each module
        let mut dep_counts: HashMap<String, usize> = HashMap::new();
        for (j, other) in matrix.labels.iter().enumerate() {
            if let Some(module) = assignment.get(other) {
                if matrix.data[i][j] > 0.0 || matrix.data[j][i] > 0.0 {
                    *dep_counts.entry(module.clone()).or_default() += 1;
                }
            }
        }

        let total: usize = dep_counts.values().sum();
        let (best_module, best_count) = dep_counts
            .iter()
            .max_by_key(|(_, &c)| c)
            .map(|(m, &c)| (m.clone(), c))
            .unwrap_or_else(|| ("unassigned".to_string(), 0));

        let fit_score = if total > 0 {
            best_count as f64 / total as f64
        } else {
            0.0
        };

        unassigned.push(UnassignedElement {
            name: label.clone(),
            best_fit_module: best_module,
            fit_score,
            dep_counts,
        });
    }

    // Step 3: Build directed modules
    let mut modules = Vec::new();
    for module_spec in &config.modules {
        let members: Vec<String> = assignment
            .iter()
            .filter(|(_, m)| *m == &module_spec.name)
            .map(|(l, _)| l.clone())
            .collect();

        let suggested_additions: Vec<SuggestedMove> = unassigned
            .iter()
            .filter(|u| u.best_fit_module == module_spec.name && u.fit_score > 0.5)
            .map(|u| SuggestedMove {
                element: u.name.clone(),
                fit_score: u.fit_score,
                rationale: format!(
                    "{} has {} deps to {} ({:.0}% of total deps)",
                    u.name,
                    u.dep_counts.get(&module_spec.name).unwrap_or(&0),
                    module_spec.name,
                    u.fit_score * 100.0
                ),
            })
            .collect();

        // Compute internal cohesion
        let member_indices: Vec<usize> = members
            .iter()
            .filter_map(|m| matrix.label_index.get(m).copied())
            .collect();
        let internal_deps = count_internal_deps(matrix, &member_indices);
        let k = member_indices.len();
        let max_deps = if k > 1 { k * (k - 1) } else { 1 };
        let internal_cohesion = internal_deps as f64 / max_deps as f64;

        // Count external dependencies
        let external_deps = count_external_deps(matrix, &member_indices);
        let total_possible = if k > 0 && n > k { k * (n - k) * 2 } else { 1 };
        let external_coupling = external_deps as f64 / total_possible as f64;

        // Boundary deps
        let boundary_deps =
            find_boundary_deps(matrix, &member_indices, &assignment, &module_spec.name);

        modules.push(DirectedModule {
            name: module_spec.name.clone(),
            description: module_spec.description.clone(),
            current_members: members,
            suggested_additions,
            internal_cohesion,
            external_coupling,
            boundary_deps,
        });
    }

    // Step 4: Detect violations
    let violations = detect_violations(matrix, &assignment, &config.modules, cycles);

    // Step 5: Generate migration plan
    let migration_plan =
        generate_migration_plan(matrix, &modules, &unassigned, &violations, cycles);

    DirectedResult {
        modules,
        unassigned,
        violations,
        migration_plan,
    }
}

/// Check if a label is a member of a module spec (via seeds, include, exclude).
fn is_member(label: &str, spec: &ModuleSpec) -> bool {
    // Check exclude first
    for pattern in &spec.exclude {
        if glob_match(pattern, label) {
            return false;
        }
    }
    // Check seeds
    for seed in &spec.seeds {
        if label.starts_with(seed) {
            return true;
        }
    }
    // Check include patterns
    for pattern in &spec.include {
        if glob_match(pattern, label) {
            return true;
        }
    }
    false
}

/// Simple glob matching (supports * wildcard).
fn glob_match(pattern: &str, text: &str) -> bool {
    let re_str = format!("^{}$", regex::escape(pattern).replace(r"\*", ".*"));
    regex::Regex::new(&re_str)
        .map(|re| re.is_match(text))
        .unwrap_or(false)
}

fn count_internal_deps(matrix: &DsmMatrix, indices: &[usize]) -> usize {
    let mut count = 0;
    for &i in indices {
        for &j in indices {
            if i != j && matrix.data[i][j] > 0.0 {
                count += 1;
            }
        }
    }
    count
}

fn count_external_deps(matrix: &DsmMatrix, indices: &[usize]) -> usize {
    let n = matrix.size();
    let set: std::collections::HashSet<usize> = indices.iter().copied().collect();
    let mut count = 0;
    for &i in indices {
        for j in 0..n {
            if !set.contains(&j) && (matrix.data[i][j] > 0.0 || matrix.data[j][i] > 0.0) {
                count += 1;
            }
        }
    }
    count
}

fn find_boundary_deps(
    matrix: &DsmMatrix,
    member_indices: &[usize],
    assignment: &HashMap<String, String>,
    module_name: &str,
) -> Vec<BoundaryDep> {
    let set: std::collections::HashSet<usize> = member_indices.iter().copied().collect();
    let n = matrix.size();
    let mut deps = Vec::new();

    for &i in member_indices {
        for j in 0..n {
            if !set.contains(&j) && matrix.data[i][j] > 0.0 {
                let other_module = assignment
                    .get(&matrix.labels[j])
                    .cloned()
                    .unwrap_or_else(|| "unassigned".to_string());
                deps.push(BoundaryDep {
                    from: matrix.labels[i].clone(),
                    to: matrix.labels[j].clone(),
                    from_module: module_name.to_string(),
                    to_module: other_module,
                    is_necessary: true, // conservative default
                });
            }
        }
    }

    deps
}

fn detect_violations(
    matrix: &DsmMatrix,
    assignment: &HashMap<String, String>,
    _module_specs: &[ModuleSpec],
    cycles: &CycleInfo,
) -> Vec<BoundaryViolation> {
    let n = matrix.size();
    let mut violations = Vec::new();

    // Check for elements assigned to wrong module (more deps elsewhere)
    for (label, module) in assignment {
        if let Some(&idx) = matrix.label_index.get(label) {
            let mut deps_to_own = 0usize;
            let mut deps_to_other: HashMap<String, usize> = HashMap::new();

            for j in 0..n {
                if let Some(other_mod) = assignment.get(&matrix.labels[j]) {
                    if matrix.data[idx][j] > 0.0 || matrix.data[j][idx] > 0.0 {
                        if other_mod == module {
                            deps_to_own += 1;
                        } else {
                            *deps_to_other.entry(other_mod.clone()).or_default() += 1;
                        }
                    }
                }
            }

            // If more deps to another module, flag as wrong module
            for (other_mod, count) in &deps_to_other {
                if *count > deps_to_own && deps_to_own > 0 {
                    violations.push(BoundaryViolation {
                        element: label.clone(),
                        current_module: module.clone(),
                        violation_kind: ViolationKind::WrongModule,
                        details: format!(
                            "{} has {} deps to {} but only {} to {}",
                            label, count, other_mod, deps_to_own, module
                        ),
                    });
                }
            }

            // Check for god elements (depended on by 3+ modules)
            let modules_depending: std::collections::HashSet<&String> = (0..n)
                .filter(|&j| matrix.data[j][idx] > 0.0)
                .filter_map(|j| assignment.get(&matrix.labels[j]))
                .collect();
            if modules_depending.len() >= 3 {
                violations.push(BoundaryViolation {
                    element: label.clone(),
                    current_module: module.clone(),
                    violation_kind: ViolationKind::GodElement,
                    details: format!(
                        "{} is depended on by {} modules: {:?}",
                        label,
                        modules_depending.len(),
                        modules_depending
                            .iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                    ),
                });
            }
        }
    }

    // Check for cross-module cycles
    for cycle in &cycles.cycles {
        let cycle_modules: std::collections::HashSet<&String> = cycle
            .iter()
            .filter_map(|&i| assignment.get(&matrix.labels[i]))
            .collect();
        if cycle_modules.len() > 1 {
            for &elem in cycle {
                if let Some(module) = assignment.get(&matrix.labels[elem]) {
                    violations.push(BoundaryViolation {
                        element: matrix.labels[elem].clone(),
                        current_module: module.clone(),
                        violation_kind: ViolationKind::CrossModuleCycle,
                        details: format!(
                            "{} participates in cycle crossing modules: {:?}",
                            matrix.labels[elem],
                            cycle_modules.iter().map(|s| s.as_str()).collect::<Vec<_>>()
                        ),
                    });
                }
            }
        }
    }

    violations
}

fn generate_migration_plan(
    _matrix: &DsmMatrix,
    _modules: &[DirectedModule],
    unassigned: &[UnassignedElement],
    violations: &[BoundaryViolation],
    _cycles: &CycleInfo,
) -> Vec<MigrationStep> {
    let mut steps = Vec::new();
    let mut order = 0;

    // Phase 1: Break cross-module cycles (extract interfaces)
    for v in violations {
        if v.violation_kind == ViolationKind::CrossModuleCycle {
            // Only add one step per unique element
            if !steps.iter().any(|s: &MigrationStep| s.element == v.element) {
                order += 1;
                steps.push(MigrationStep {
                    order,
                    action: MigrationAction::ExtractInterface,
                    element: v.element.clone(),
                    from_module: Some(v.current_module.clone()),
                    to_module: v.current_module.clone(),
                    rationale: format!("Break cross-module cycle: {}", v.details),
                    risk: Risk::Medium,
                    deps_to_update: vec![],
                });
            }
        }
    }

    // Phase 2: Move misplaced elements
    for v in violations {
        if v.violation_kind == ViolationKind::WrongModule {
            order += 1;
            // Find which module it should go to
            let target = v
                .details
                .split(" deps to ")
                .nth(1)
                .and_then(|s| s.split(" but").next())
                .unwrap_or("unknown")
                .to_string();
            steps.push(MigrationStep {
                order,
                action: MigrationAction::MoveElement,
                element: v.element.clone(),
                from_module: Some(v.current_module.clone()),
                to_module: target,
                rationale: v.details.clone(),
                risk: Risk::Low,
                deps_to_update: vec![],
            });
        }
    }

    // Phase 3: Assign unassigned elements with strong affinity
    for u in unassigned {
        if u.fit_score > 0.6 {
            order += 1;
            steps.push(MigrationStep {
                order,
                action: MigrationAction::MoveElement,
                element: u.name.clone(),
                from_module: None,
                to_module: u.best_fit_module.clone(),
                rationale: format!(
                    "{} has {:.0}% dep affinity to {}",
                    u.name,
                    u.fit_score * 100.0,
                    u.best_fit_module
                ),
                risk: Risk::Low,
                deps_to_update: vec![],
            });
        }
    }

    // Phase 4: Split god elements
    for v in violations {
        if v.violation_kind == ViolationKind::GodElement
            && !steps.iter().any(|s| s.element == v.element)
        {
            order += 1;
            steps.push(MigrationStep {
                order,
                action: MigrationAction::SplitElement,
                element: v.element.clone(),
                from_module: Some(v.current_module.clone()),
                to_module: v.current_module.clone(),
                rationale: v.details.clone(),
                risk: Risk::High,
                deps_to_update: vec![],
            });
        }
    }

    steps
}

/// Parse a modules.toml file into DirectedConfig.
pub fn parse_modules_toml(input: &str) -> anyhow::Result<DirectedConfig> {
    #[derive(Deserialize)]
    struct TomlRoot {
        modules: HashMap<String, TomlModule>,
    }

    #[derive(Deserialize)]
    struct TomlModule {
        #[serde(default)]
        description: String,
        seeds: Vec<String>,
        #[serde(default)]
        include: Vec<String>,
        #[serde(default)]
        exclude: Vec<String>,
    }

    let root: TomlRoot = toml::from_str(input)?;
    let modules = root
        .modules
        .into_iter()
        .map(|(name, m)| ModuleSpec {
            name,
            description: m.description,
            seeds: m.seeds,
            include: m.include,
            exclude: m.exclude,
        })
        .collect();

    Ok(DirectedConfig { modules })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cycles::find_cycles;
    use crate::extract::{Edge, EdgeKind};

    fn make_edge(src: &str, tgt: &str) -> Edge {
        Edge {
            source: src.to_string(),
            target: tgt.to_string(),
            weight: 1.0,
            kind: EdgeKind::Import,
            cross_language: None,
        }
    }

    #[test]
    fn parse_modules_toml_basic() {
        let toml = r#"
[modules.storage]
description = "Storage layer"
seeds = ["db", "io"]
include = ["*cache*"]

[modules.query]
description = "Query processing"
seeds = ["cql3"]
"#;
        let config = parse_modules_toml(toml).unwrap();
        assert_eq!(config.modules.len(), 2);
    }

    #[test]
    fn directed_analysis_assigns_seeds() {
        let edges = vec![
            make_edge("db.core", "db.io"),
            make_edge("cql.parser", "db.core"),
            make_edge("util.common", "db.core"),
        ];
        let m = DsmMatrix::from_edges(&edges);
        let ci = find_cycles(&m);
        let config = DirectedConfig {
            modules: vec![
                ModuleSpec {
                    name: "storage".to_string(),
                    description: "Storage".to_string(),
                    seeds: vec!["db".to_string()],
                    include: vec![],
                    exclude: vec![],
                },
                ModuleSpec {
                    name: "query".to_string(),
                    description: "Query".to_string(),
                    seeds: vec!["cql".to_string()],
                    include: vec![],
                    exclude: vec![],
                },
            ],
        };
        let result = directed_analysis(&m, &config, &ci);
        // db.core and db.io should be in storage
        let storage = result.modules.iter().find(|m| m.name == "storage").unwrap();
        assert!(storage.current_members.contains(&"db.core".to_string()));
        assert!(storage.current_members.contains(&"db.io".to_string()));
        // util.common should be unassigned
        assert!(result.unassigned.iter().any(|u| u.name == "util.common"));
    }

    #[test]
    fn glob_match_works() {
        assert!(glob_match("*cache*", "my.cache.service"));
        assert!(glob_match("*cache*", "cache"));
        assert!(!glob_match("*cache*", "my.cach.service"));
        assert!(glob_match("db.*", "db.core"));
        assert!(!glob_match("db.*", "dba.core"));
    }

    #[test]
    fn directed_detects_cross_module_cycle() {
        let edges = vec![
            make_edge("db.core", "svc.main"),
            make_edge("svc.main", "db.core"),
        ];
        let m = DsmMatrix::from_edges(&edges);
        let ci = find_cycles(&m);
        let config = DirectedConfig {
            modules: vec![
                ModuleSpec {
                    name: "storage".to_string(),
                    description: "".to_string(),
                    seeds: vec!["db".to_string()],
                    include: vec![],
                    exclude: vec![],
                },
                ModuleSpec {
                    name: "service".to_string(),
                    description: "".to_string(),
                    seeds: vec!["svc".to_string()],
                    include: vec![],
                    exclude: vec![],
                },
            ],
        };
        let result = directed_analysis(&m, &config, &ci);
        assert!(result
            .violations
            .iter()
            .any(|v| v.violation_kind == ViolationKind::CrossModuleCycle));
    }

    #[test]
    fn migration_plan_ordered() {
        let edges = vec![
            make_edge("db.core", "svc.main"),
            make_edge("svc.main", "db.core"),
            make_edge("util.helper", "db.core"),
        ];
        let m = DsmMatrix::from_edges(&edges);
        let ci = find_cycles(&m);
        let config = DirectedConfig {
            modules: vec![
                ModuleSpec {
                    name: "storage".to_string(),
                    description: "".to_string(),
                    seeds: vec!["db".to_string()],
                    include: vec![],
                    exclude: vec![],
                },
                ModuleSpec {
                    name: "service".to_string(),
                    description: "".to_string(),
                    seeds: vec!["svc".to_string()],
                    include: vec![],
                    exclude: vec![],
                },
            ],
        };
        let result = directed_analysis(&m, &config, &ci);
        // Migration steps should be ordered
        for (i, step) in result.migration_plan.iter().enumerate() {
            assert_eq!(step.order, i + 1);
        }
    }
}
