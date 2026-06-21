use crate::cluster::ClusterResult;
use crate::cycles::CycleInfo;
use crate::directed::DirectedResult;
use crate::matrix::DsmMatrix;
use crate::metrics::DsmMetrics;
use crate::partition::PartitionResult;
use serde::{Deserialize, Serialize};

/// Kind of refactoring suggestion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SuggestionKind {
    ExtractInterface,
    MoveElement,
    SplitPackage,
    MergePackages,
    IntroduceLayer,
    RemoveDependency,
    CreateModuleBoundary,
    InternalizeDetail,
    ExtractSharedKernel,
}

/// Priority level.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Priority {
    Critical,
    High,
    Medium,
    Low,
}

/// Estimated effort.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Effort {
    Small,
    Medium,
    Large,
}

/// A refactoring suggestion with DSM evidence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Suggestion {
    pub kind: SuggestionKind,
    pub priority: Priority,
    pub source: String,
    pub target: String,
    pub rationale: String,
    pub impact: String,
    pub dsm_evidence: String,
    pub estimated_effort: Effort,
    pub migration_step: Option<usize>,
}

/// Generate refactoring suggestions from DSM analysis results.
pub fn generate_suggestions(
    matrix: &DsmMatrix,
    cycles: &CycleInfo,
    clusters: &ClusterResult,
    metrics: &DsmMetrics,
    partition: &PartitionResult,
    directed: Option<&DirectedResult>,
) -> Vec<Suggestion> {
    let mut suggestions = Vec::new();

    // 1. Cycle-breaking suggestions
    suggest_cycle_breaks(matrix, cycles, &mut suggestions);

    // 2. Cluster-based suggestions
    suggest_cluster_improvements(matrix, clusters, &mut suggestions);

    // 3. Metrics-based suggestions (god elements, unstable elements)
    suggest_from_metrics(metrics, &mut suggestions);

    // 4. Partition-based suggestions (layer violations)
    suggest_layer_fixes(matrix, partition, &mut suggestions);

    // 5. Directed-mode suggestions
    if let Some(dir) = directed {
        suggest_from_directed(dir, &mut suggestions);
    }

    // Sort by priority
    suggestions.sort_by(|a, b| a.priority.cmp(&b.priority));

    suggestions
}

fn suggest_cycle_breaks(matrix: &DsmMatrix, cycles: &CycleInfo, suggestions: &mut Vec<Suggestion>) {
    use crate::cycles::find_tear_edges;
    let tears = find_tear_edges(matrix, cycles);

    for (i, j, weight) in tears {
        let source = &matrix.labels[i];
        let target = &matrix.labels[j];
        suggestions.push(Suggestion {
            kind: SuggestionKind::ExtractInterface,
            priority: if cycles.max_cycle_size > 10 {
                Priority::Critical
            } else {
                Priority::High
            },
            source: source.clone(),
            target: target.clone(),
            rationale: format!(
                "Break dependency cycle: {} -> {} (cycle size: {})",
                source, target, cycles.max_cycle_size
            ),
            impact: format!(
                "Removing this edge breaks a cycle of {} elements",
                cycles.max_cycle_size
            ),
            dsm_evidence: format!(
                "Tear edge identified by Tarjan SCC analysis (weight: {:.1})",
                weight
            ),
            estimated_effort: Effort::Medium,
            migration_step: None,
        });
    }
}

fn suggest_cluster_improvements(
    _matrix: &DsmMatrix,
    clusters: &ClusterResult,
    suggestions: &mut Vec<Suggestion>,
) {
    // Flag inter-cluster deps that could be eliminated
    for dep in &clusters.inter_cluster_deps {
        if dep.weight >= 3.0 {
            let from_name = clusters
                .clusters
                .iter()
                .find(|c| c.id == dep.from_cluster)
                .and_then(|c| c.name.clone())
                .unwrap_or_else(|| format!("Cluster {}", dep.from_cluster));
            let to_name = clusters
                .clusters
                .iter()
                .find(|c| c.id == dep.to_cluster)
                .and_then(|c| c.name.clone())
                .unwrap_or_else(|| format!("Cluster {}", dep.to_cluster));

            suggestions.push(Suggestion {
                kind: SuggestionKind::CreateModuleBoundary,
                priority: Priority::Medium,
                source: from_name.clone(),
                target: to_name.clone(),
                rationale: format!(
                    "Strong coupling between {} and {} ({} edges)",
                    from_name, to_name, dep.weight as usize
                ),
                impact: "Reduce inter-module coupling by formalizing boundary".to_string(),
                dsm_evidence: format!(
                    "{} inter-cluster edges: {:?}",
                    dep.edges.len(),
                    dep.edges.iter().take(3).collect::<Vec<_>>()
                ),
                estimated_effort: Effort::Large,
                migration_step: None,
            });
        }
    }

    // Flag large clusters that should be split
    for c in &clusters.clusters {
        if c.elements.len() > 15 && c.cohesion < 0.3 {
            suggestions.push(Suggestion {
                kind: SuggestionKind::SplitPackage,
                priority: Priority::Medium,
                source: c
                    .name
                    .clone()
                    .unwrap_or_else(|| format!("Cluster {}", c.id)),
                target: String::new(),
                rationale: format!(
                    "Large cluster ({} elements) with low cohesion ({:.2})",
                    c.elements.len(),
                    c.cohesion
                ),
                impact: "Split into smaller, more cohesive modules".to_string(),
                dsm_evidence: format!(
                    "{} internal deps, cohesion {:.2}",
                    c.internal_deps, c.cohesion
                ),
                estimated_effort: Effort::Large,
                migration_step: None,
            });
        }
    }
}

fn suggest_from_metrics(metrics: &DsmMetrics, suggestions: &mut Vec<Suggestion>) {
    for elem in &metrics.elements {
        // God elements: high fan-in from many clusters
        if elem.fan_in > 20 {
            suggestions.push(Suggestion {
                kind: SuggestionKind::ExtractInterface,
                priority: Priority::High,
                source: elem.name.clone(),
                target: String::new(),
                rationale: format!("{} has fan-in of {} (god element)", elem.name, elem.fan_in),
                impact: "Extract interface to reduce coupling to implementation".to_string(),
                dsm_evidence: format!(
                    "Fan-in: {}, fan-out: {}, instability: {:.2}",
                    elem.fan_in, elem.fan_out, elem.instability
                ),
                estimated_effort: Effort::Medium,
                migration_step: None,
            });
        }

        // Highly unstable elements that are depended upon
        if elem.instability > 0.8 && elem.fan_in > 5 {
            suggestions.push(Suggestion {
                kind: SuggestionKind::InternalizeDetail,
                priority: Priority::Medium,
                source: elem.name.clone(),
                target: String::new(),
                rationale: format!(
                    "{} is highly unstable ({:.2}) but has {} dependents",
                    elem.name, elem.instability, elem.fan_in
                ),
                impact: "Stabilize by reducing outgoing dependencies".to_string(),
                dsm_evidence: format!(
                    "Instability {:.2} with fan-in {}",
                    elem.instability, elem.fan_in
                ),
                estimated_effort: Effort::Medium,
                migration_step: None,
            });
        }
    }
}

fn suggest_layer_fixes(
    matrix: &DsmMatrix,
    partition: &PartitionResult,
    suggestions: &mut Vec<Suggestion>,
) {
    if partition.feedback_count > 0 {
        // Count feedback edges per element
        let reordered = matrix.reorder(&partition.order);
        let n = reordered.size();
        for i in 0..n {
            for j in (i + 1)..n {
                if reordered.data[i][j] > 0.0 {
                    suggestions.push(Suggestion {
                        kind: SuggestionKind::IntroduceLayer,
                        priority: Priority::Medium,
                        source: reordered.labels[i].clone(),
                        target: reordered.labels[j].clone(),
                        rationale: format!(
                            "Layer violation: {} depends on {} (lower layer depends on upper)",
                            reordered.labels[i], reordered.labels[j]
                        ),
                        impact: "Fix dependency direction to respect layer boundaries".to_string(),
                        dsm_evidence: "Above-diagonal mark in partitioned DSM".to_string(),
                        estimated_effort: Effort::Small,
                        migration_step: None,
                    });
                }
            }
        }
    }
}

fn suggest_from_directed(directed: &DirectedResult, suggestions: &mut Vec<Suggestion>) {
    for step in &directed.migration_plan {
        let kind = match step.action {
            crate::directed::MigrationAction::MoveElement => SuggestionKind::MoveElement,
            crate::directed::MigrationAction::ExtractInterface => SuggestionKind::ExtractInterface,
            crate::directed::MigrationAction::SplitElement => SuggestionKind::SplitPackage,
            crate::directed::MigrationAction::IntroduceMediator => SuggestionKind::IntroduceLayer,
        };
        let priority = match step.risk {
            crate::directed::Risk::Low => Priority::Low,
            crate::directed::Risk::Medium => Priority::Medium,
            crate::directed::Risk::High => Priority::High,
        };
        suggestions.push(Suggestion {
            kind,
            priority,
            source: step.element.clone(),
            target: step.to_module.clone(),
            rationale: step.rationale.clone(),
            impact: format!(
                "Migration step {} of {}",
                step.order,
                directed.migration_plan.len()
            ),
            dsm_evidence: "User-directed module extraction analysis".to_string(),
            estimated_effort: match step.risk {
                crate::directed::Risk::Low => Effort::Small,
                crate::directed::Risk::Medium => Effort::Medium,
                crate::directed::Risk::High => Effort::Large,
            },
            migration_step: Some(step.order),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::{cluster, ClusterConfig};
    use crate::cycles::find_cycles;
    use crate::extract::{Edge, EdgeKind};
    use crate::metrics::compute_metrics;
    use crate::partition::partition;

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
    fn generates_cycle_break_suggestions() {
        let edges = vec![
            make_edge("a", "b"),
            make_edge("b", "c"),
            make_edge("c", "a"),
        ];
        let m = DsmMatrix::from_edges(&edges);
        let ci = find_cycles(&m);
        let cr = cluster(
            &m,
            &ClusterConfig {
                seed: Some(1),
                ..Default::default()
            },
        );
        let metrics = compute_metrics(&m, &ci, &cr);
        let part = partition(&m);
        let suggestions = generate_suggestions(&m, &ci, &cr, &metrics, &part, None);
        assert!(suggestions
            .iter()
            .any(|s| s.kind == SuggestionKind::ExtractInterface));
    }

    #[test]
    fn suggestions_sorted_by_priority() {
        let edges = vec![
            make_edge("a", "b"),
            make_edge("b", "c"),
            make_edge("c", "a"),
            make_edge("d", "a"),
        ];
        let m = DsmMatrix::from_edges(&edges);
        let ci = find_cycles(&m);
        let cr = cluster(
            &m,
            &ClusterConfig {
                seed: Some(1),
                ..Default::default()
            },
        );
        let metrics = compute_metrics(&m, &ci, &cr);
        let part = partition(&m);
        let suggestions = generate_suggestions(&m, &ci, &cr, &metrics, &part, None);
        // Should be sorted by priority (Critical < High < Medium < Low)
        for i in 1..suggestions.len() {
            assert!(suggestions[i].priority >= suggestions[i - 1].priority);
        }
    }
}
