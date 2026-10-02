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
    /// Split a function that is hard to follow (high cognitive complexity),
    /// optionally inside an element that also has structural problems.
    ReduceCognitiveComplexity,
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
    generate_suggestions_with_cognitive(
        matrix, cycles, clusters, metrics, partition, directed, None,
    )
}

/// Generate refactoring suggestions, folding in cognitive hot spots when a
/// cognitive scan has been run.
///
/// A structural problem (cycle, god element) inside an element that is also
/// cognitively dense is a stronger refactoring signal than either alone, so
/// those suggestions are raised in priority and their evidence line names the
/// hot spot responsible.
pub fn generate_suggestions_with_cognitive(
    matrix: &DsmMatrix,
    cycles: &CycleInfo,
    clusters: &ClusterResult,
    metrics: &DsmMetrics,
    partition: &PartitionResult,
    directed: Option<&DirectedResult>,
    cognitive: Option<&crate::hotspots::CognitiveSummary>,
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

    // 6. Cognitive-complexity suggestions (function-level evidence)
    let mut cognitive_targets: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Some(summary) = cognitive {
        cognitive_targets = suggest_from_cognitive(summary, &mut suggestions);
    }

    // Structural problems inside a cognitively dense element are promoted:
    // the reader is fighting both coupling and density in the same place.
    // Cognitive suggestions are excluded — they are already ranked by their own
    // severity, and promoting them here would double-count their score.
    if !cognitive_targets.is_empty() {
        for suggestion in suggestions.iter_mut() {
            if suggestion.kind == SuggestionKind::ReduceCognitiveComplexity {
                continue;
            }
            if cognitive_targets.contains(&suggestion.source) {
                suggestion.priority = match suggestion.priority {
                    Priority::Critical => Priority::Critical,
                    Priority::High => Priority::Critical,
                    Priority::Medium => Priority::High,
                    Priority::Low => Priority::Medium,
                };
                suggestion.rationale.push_str(
                    " — this element also contains a cognitive complexity hot spot, so the refactor is higher value",
                );
            }
        }
    }

    // Sort by priority
    suggestions.sort_by(|a, b| a.priority.cmp(&b.priority));

    suggestions
}

/// Turn cognitive hot spots into refactoring suggestions.
///
/// Returns the set of elements that contain a hot spot, for priority promotion.
fn suggest_from_cognitive(
    summary: &crate::hotspots::CognitiveSummary,
    suggestions: &mut Vec<Suggestion>,
) -> std::collections::HashSet<String> {
    use std::collections::HashMap;

    // One suggestion per element, citing its worst hot spot; the rest are
    // summarised in the evidence line so the list stays actionable.
    let mut worst_per_element: HashMap<&str, Vec<&crate::hotspots::HotspotEvidence>> =
        HashMap::new();
    for hot_spot in &summary.hot_spots {
        worst_per_element
            .entry(hot_spot.element.as_str())
            .or_default()
            .push(hot_spot);
    }

    let mut targets = std::collections::HashSet::new();
    for (element, hot_spots) in worst_per_element {
        let worst = hot_spots
            .iter()
            .max_by_key(|h| h.cognitive)
            .expect("entry exists, so there is at least one hot spot");
        let others = hot_spots.len() - 1;

        let (priority, effort) = if worst.cognitive >= forge_cognitive_complexity::SEVERE_THRESHOLD
        {
            (Priority::High, Effort::Medium)
        } else if worst.cognitive >= forge_cognitive_complexity::HIGH_THRESHOLD {
            (Priority::Medium, Effort::Medium)
        } else {
            (Priority::Low, Effort::Small)
        };

        let evidence = if others == 0 {
            format!(
                "Cognitive complexity {} (nesting {}) in {} at {}:{}",
                worst.cognitive, worst.nesting, worst.name, worst.file, worst.line
            )
        } else {
            format!(
                "Cognitive complexity {} (nesting {}) in {} at {}:{}; {} more hot spot(s) in this element",
                worst.cognitive,
                worst.nesting,
                worst.name,
                worst.file,
                worst.line,
                others
            )
        };

        suggestions.push(Suggestion {
            kind: SuggestionKind::ReduceCognitiveComplexity,
            priority,
            source: element.to_string(),
            target: worst.name.clone(),
            rationale: format!(
                "{} is hard to follow (cognitive complexity {}, threshold {})",
                worst.name,
                worst.cognitive,
                forge_cognitive_complexity::MODERATE_THRESHOLD
            ),
            impact: format!(
                "Splitting this function reduces the reading cost of {}",
                element
            ),
            dsm_evidence: evidence,
            estimated_effort: effort,
            migration_step: None,
        });
        targets.insert(element.to_string());
    }

    targets
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

    #[test]
    fn cognitive_hot_spots_become_suggestions() {
        let edges = vec![make_edge("crate::engine", "crate::storage")];
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

        let summary = crate::hotspots::CognitiveSummary {
            scanned: true,
            functions_analyzed: 3,
            failures: 0,
            hot_spots: vec![crate::hotspots::HotspotEvidence {
                element: "crate::engine".to_string(),
                file: "src/engine.rs".to_string(),
                name: "Engine::run".to_string(),
                // At/above the severe threshold, which is what drives the High
                // priority asserted below.
                cognitive: forge_cognitive_complexity::SEVERE_THRESHOLD,
                nesting: 4,
                line: 42,
            }],
            unattributed: 0,
            warnings: vec![],
        };

        let suggestions = generate_suggestions_with_cognitive(
            &m,
            &ci,
            &cr,
            &metrics,
            &part,
            None,
            Some(&summary),
        );

        let found = suggestions
            .iter()
            .find(|s| s.kind == SuggestionKind::ReduceCognitiveComplexity)
            .expect("a cognitive hot spot should produce a suggestion");
        assert_eq!(found.source, "crate::engine");
        assert_eq!(found.target, "Engine::run");
        assert_eq!(found.priority, Priority::High, ">= severe threshold");
        assert!(
            found
                .dsm_evidence
                .contains(&forge_cognitive_complexity::SEVERE_THRESHOLD.to_string()),
            "{}",
            found.dsm_evidence
        );
        assert!(
            found.dsm_evidence.contains("src/engine.rs:42"),
            "{}",
            found.dsm_evidence
        );
    }

    /// A structural problem inside a cognitively dense element is promoted —
    /// that combination is the point of the integration.
    #[test]
    fn cognitive_density_promotes_structural_suggestions_in_the_same_element() {
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

        let baseline =
            generate_suggestions_with_cognitive(&m, &ci, &cr, &metrics, &part, None, None);

        let summary = crate::hotspots::CognitiveSummary {
            scanned: true,
            functions_analyzed: 1,
            failures: 0,
            hot_spots: vec![crate::hotspots::HotspotEvidence {
                element: "a".to_string(),
                file: "a.rs".to_string(),
                name: "dense".to_string(),
                cognitive: 30,
                nesting: 5,
                line: 1,
            }],
            unattributed: 0,
            warnings: vec![],
        };
        let promoted = generate_suggestions_with_cognitive(
            &m,
            &ci,
            &cr,
            &metrics,
            &part,
            None,
            Some(&summary),
        );

        let before = baseline
            .iter()
            .find(|s| s.source == "a" && s.kind != SuggestionKind::ReduceCognitiveComplexity)
            .expect("element a has a structural suggestion");
        let after = promoted
            .iter()
            .find(|s| s.source == "a" && s.kind == before.kind)
            .expect("the same structural suggestion is still present");
        assert!(
            after.priority < before.priority,
            "priority should be promoted (lower enum value = higher priority): {:?} -> {:?}",
            before.priority,
            after.priority
        );
        assert!(
            after.rationale.contains("cognitive complexity hot spot"),
            "{}",
            after.rationale
        );
    }

    #[test]
    fn no_cognitive_summary_changes_nothing() {
        let edges = vec![make_edge("a", "b"), make_edge("b", "a")];
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
        let legacy = generate_suggestions(&m, &ci, &cr, &metrics, &part, None);
        let extended =
            generate_suggestions_with_cognitive(&m, &ci, &cr, &metrics, &part, None, None);
        assert_eq!(
            serde_json::to_string(&legacy).unwrap(),
            serde_json::to_string(&extended).unwrap(),
            "absent summary is backward compatible"
        );
        assert!(!legacy
            .iter()
            .any(|s| s.kind == SuggestionKind::ReduceCognitiveComplexity));
    }
}
