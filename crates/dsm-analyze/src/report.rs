use crate::cluster::ClusterResult;
use crate::cycles::CycleInfo;
use crate::directed::DirectedResult;
use crate::metrics::DsmMetrics;
use crate::partition::PartitionResult;
use crate::suggest::Suggestion;
use serde::{Deserialize, Serialize};

/// Output format for reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputFormat {
    Mermaid,
    Markdown,
    Svg,
    Html,
    Json,
    Csv,
}

/// Full DSM analysis report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DsmReport {
    pub project_name: String,
    pub element_count: usize,
    pub edge_count: usize,
    pub metrics: DsmMetrics,
    pub cycles: CycleInfo,
    pub clusters: ClusterResult,
    pub partition: PartitionResult,
    pub suggestions: Vec<Suggestion>,
    pub directed: Option<DirectedResult>,
}

/// Render a report in the specified format.
pub fn render(report: &DsmReport, format: &OutputFormat) -> String {
    match format {
        OutputFormat::Mermaid => render_mermaid(report),
        OutputFormat::Markdown => render_markdown(report),
        OutputFormat::Svg => render_svg(report),
        OutputFormat::Html => render_html(report),
        OutputFormat::Json => render_json(report),
        OutputFormat::Csv => render_csv(report),
    }
}

fn render_mermaid(report: &DsmReport) -> String {
    let mut out = String::new();

    // Use collapsed view for large graphs (>50 elements) to stay within
    // GitHub's Mermaid rendering limits (~100 nodes max)
    let collapsed = report.element_count > 50;

    // Module dependency graph
    out.push_str("```mermaid\ngraph LR\n");

    if collapsed {
        render_mermaid_collapsed(report, &mut out);
    } else if let Some(dir) = &report.directed {
        // Use directed modules as subgraphs
        for module in &dir.modules {
            out.push_str(&format!("    subgraph {}\n", module.name));
            for member in &module.current_members {
                let short = short_name(member);
                out.push_str(&format!("        {}[{}]\n", sanitize_id(member), short));
            }
            out.push_str("    end\n");
        }
        render_mermaid_edges(report, &mut out);
    } else {
        // Use clusters as subgraphs
        for cluster in &report.clusters.clusters {
            let fallback = format!("Cluster {}", cluster.id);
            let name = cluster.name.as_deref().unwrap_or(&fallback);
            out.push_str(&format!("    subgraph \"{}\"\n", name));
            for name in &cluster.element_names {
                let short = short_name(name);
                out.push_str(&format!("        {}[{}]\n", sanitize_id(name), short));
            }
            out.push_str("    end\n");
        }
        render_mermaid_edges(report, &mut out);
    }

    out.push_str("```\n");

    // For collapsed graphs, add a note about the full module list
    if collapsed {
        out.push_str("\n> Diagram collapsed: showing clusters as single nodes. ");
        out.push_str(&format!(
            "{} modules across {} clusters.\n",
            report.element_count,
            report.clusters.clusters.len()
        ));
    }

    // Layer diagram
    if report.partition.layers.len() >= 2 {
        out.push_str("\n```mermaid\ngraph TB\n");
        for (i, (layer, name)) in report
            .partition
            .layers
            .iter()
            .zip(&report.partition.layer_names)
            .enumerate()
        {
            out.push_str(&format!("    subgraph \"Band {}: {}\"\n", i + 1, name));
            for &idx in layer {
                if idx
                    < report
                        .clusters
                        .clusters
                        .iter()
                        .flat_map(|c| &c.element_names)
                        .count()
                {
                    // Map index back to label
                }
            }
            out.push_str("    end\n");
        }
        out.push_str("```\n");
    }

    out
}

fn render_markdown(report: &DsmReport) -> String {
    let mut out = String::new();

    out.push_str(&format!("# DSM Analysis: {}\n\n", report.project_name));

    // Summary metrics
    out.push_str("## Summary Metrics\n\n");
    out.push_str("| Metric | Value | Status |\n");
    out.push_str("|--------|-------|--------|\n");
    out.push_str(&format!("| Elements | {} | - |\n", report.element_count));
    out.push_str(&format!("| Dependencies | {} | - |\n", report.edge_count));
    out.push_str(&format!(
        "| Propagation Cost | {:.1}% | {} |\n",
        report.metrics.propagation_cost * 100.0,
        crate::metrics::categorize_pc(report.metrics.propagation_cost)
    ));
    out.push_str(&format!(
        "| Max Cycle Size | {} | {} |\n",
        report.metrics.max_cycle_size,
        crate::metrics::categorize_cycle_size(report.metrics.max_cycle_size)
    ));
    out.push_str(&format!(
        "| Number of Cycles | {} | - |\n",
        report.metrics.num_cycles
    ));
    out.push_str(&format!(
        "| Cluster Quality | {:.1}% | {} |\n",
        report.metrics.cluster_quality * 100.0,
        crate::metrics::categorize_quality(report.metrics.cluster_quality)
    ));

    // Module dependency diagram (Mermaid)
    out.push_str("\n## Module Dependencies\n\n");
    out.push_str(&render_mermaid(report));

    // Cycles
    if !report.cycles.cycles.is_empty() {
        out.push_str("\n## Dependency Cycles\n\n");
        for (i, cycle) in report.cycles.cycles.iter().enumerate() {
            out.push_str(&format!("### Cycle {} ({} elements)\n", i + 1, cycle.len()));
            for &idx in cycle {
                if idx < report.metrics.elements.len() {
                    out.push_str(&format!("- {}\n", report.metrics.elements[idx].name));
                }
            }
            out.push('\n');
        }
    }

    // Clusters
    out.push_str("## Identified Modules\n\n");
    for cluster in &report.clusters.clusters {
        let fallback = format!("Cluster {}", cluster.id);
        let name = cluster.name.as_deref().unwrap_or(&fallback);
        out.push_str(&format!(
            "### {} ({} elements, cohesion: {:.2})\n",
            name,
            cluster.elements.len(),
            cluster.cohesion
        ));
        for name in &cluster.element_names {
            out.push_str(&format!("- {}\n", name));
        }
        out.push('\n');
    }

    // Suggestions
    if !report.suggestions.is_empty() {
        out.push_str("## Refactoring Suggestions\n\n");
        out.push_str("| Priority | Type | Source | Target | Rationale |\n");
        out.push_str("|----------|------|--------|--------|-----------|\n");
        for s in &report.suggestions {
            out.push_str(&format!(
                "| {:?} | {:?} | {} | {} | {} |\n",
                s.priority, s.kind, s.source, s.target, s.rationale
            ));
        }
    }

    // Directed analysis
    if let Some(dir) = &report.directed {
        out.push_str("\n## Directed Module Analysis\n\n");
        for module in &dir.modules {
            out.push_str(&format!(
                "### {} (cohesion: {:.2}, coupling: {:.2})\n",
                module.name, module.internal_cohesion, module.external_coupling
            ));
            out.push_str(&format!("{}\n\n", module.description));
            out.push_str("**Members:**\n");
            for m in &module.current_members {
                out.push_str(&format!("- {}\n", m));
            }
            if !module.suggested_additions.is_empty() {
                out.push_str("\n**Suggested additions:**\n");
                for s in &module.suggested_additions {
                    out.push_str(&format!(
                        "- {} (fit: {:.0}%) — {}\n",
                        s.element,
                        s.fit_score * 100.0,
                        s.rationale
                    ));
                }
            }
            out.push('\n');
        }

        if !dir.migration_plan.is_empty() {
            out.push_str("### Migration Plan\n\n");
            out.push_str("| Step | Action | Element | From | To | Risk |\n");
            out.push_str("|------|--------|---------|------|----|------|\n");
            for step in &dir.migration_plan {
                out.push_str(&format!(
                    "| {} | {:?} | {} | {} | {} | {:?} |\n",
                    step.order,
                    step.action,
                    step.element,
                    step.from_module.as_deref().unwrap_or("-"),
                    step.to_module,
                    step.risk
                ));
            }
        }
    }

    // Element details
    out.push_str("\n## Element Details\n\n");
    out.push_str("| Element | Fan-In | Fan-Out | Instability | In Cycle |\n");
    out.push_str("|---------|--------|---------|-------------|----------|\n");

    let mut sorted_elements = report.metrics.elements.clone();
    sorted_elements.sort_by_key(|e| std::cmp::Reverse(e.fan_in));
    for elem in sorted_elements.iter().take(50) {
        out.push_str(&format!(
            "| {} | {} | {} | {:.2} | {} |\n",
            elem.name,
            elem.fan_in,
            elem.fan_out,
            elem.instability,
            if elem.in_cycle { "Yes" } else { "No" }
        ));
    }

    out
}

fn render_svg(report: &DsmReport) -> String {
    let n = report.element_count;
    let cell_size = 12;
    let label_width = 200;
    let width = label_width + n * cell_size + 20;
    let height = label_width + n * cell_size + 20;

    let mut out = String::new();
    out.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {} {}\">\n",
        width, height
    ));
    out.push_str("<style>\n");
    out.push_str("  .cell { stroke: #ccc; stroke-width: 0.5; }\n");
    out.push_str("  .label { font-size: 8px; font-family: monospace; }\n");
    out.push_str("  .dep { fill: #4a90d9; }\n");
    out.push_str("  .cycle { fill: #e74c3c; }\n");
    out.push_str("  .self { fill: #ddd; }\n");
    out.push_str("</style>\n");

    // Note: actual matrix rendering would require the raw matrix data
    // This generates a placeholder structure
    out.push_str(&format!(
        "<text x=\"10\" y=\"15\" class=\"label\">DSM: {} ({} elements)</text>\n",
        report.project_name, n
    ));

    out.push_str("</svg>\n");
    out
}

fn render_html(report: &DsmReport) -> String {
    let mut out = String::new();
    out.push_str("<!DOCTYPE html>\n<html><head>\n");
    out.push_str("<title>DSM Analysis Report</title>\n");
    out.push_str("<style>\n");
    out.push_str(
        "body { font-family: sans-serif; max-width: 1200px; margin: 0 auto; padding: 20px; }\n",
    );
    out.push_str("table { border-collapse: collapse; } th, td { border: 1px solid #ddd; padding: 4px 8px; }\n");
    out.push_str(".good { color: green; } .warning { color: orange; } .critical { color: red; }\n");
    out.push_str("</style>\n</head><body>\n");

    // Embed markdown content as HTML
    let md = render_markdown(report);
    out.push_str("<pre>");
    out.push_str(&md.replace('<', "&lt;").replace('>', "&gt;"));
    out.push_str("</pre>\n");

    out.push_str("</body></html>\n");
    out
}

fn render_json(report: &DsmReport) -> String {
    serde_json::to_string_pretty(report).unwrap_or_else(|_| "{}".to_string())
}

fn render_csv(report: &DsmReport) -> String {
    let mut out = String::new();
    out.push_str("element,fan_in,fan_out,instability,in_cycle,cluster_id\n");
    for elem in &report.metrics.elements {
        out.push_str(&format!(
            "{},{},{},{:.3},{},{}\n",
            elem.name, elem.fan_in, elem.fan_out, elem.instability, elem.in_cycle, elem.cluster_id
        ));
    }
    out
}

/// Render collapsed Mermaid where each cluster is a single node.
/// Used for large graphs (>50 elements) to stay within renderer limits.
fn render_mermaid_collapsed(report: &DsmReport, out: &mut String) {
    // Render each cluster as a single node with member count
    for cluster in &report.clusters.clusters {
        let fallback = format!("Cluster {}", cluster.id);
        let label = cluster.name.as_deref().unwrap_or(&fallback);
        let count = cluster.element_names.len();
        // Pick a representative name from common prefix or largest member
        let representative = cluster_display_name(&cluster.element_names, label);
        let node_id = format!("c{}", cluster.id);
        if count == 1 {
            out.push_str(&format!("    {}[\"{}\"]\n", node_id, representative));
        } else {
            out.push_str(&format!(
                "    {}[\"{} ({})\"]\n",
                node_id, representative, count
            ));
        }
    }

    // Build a set of cycle cluster pairs for annotation
    let cycle_cluster_pairs: std::collections::HashSet<(usize, usize)> = report
        .clusters
        .inter_cluster_deps
        .iter()
        .filter(|dep| {
            // Check if any edge in this inter-cluster dep is a cycle edge
            dep.edges.iter().any(|(from, to)| {
                report.cycles.cycle_edges.iter().any(|(ci, cj)| {
                    report
                        .clusters
                        .clusters
                        .iter()
                        .any(|c| c.element_names.contains(from) && c.elements.contains(ci))
                        && report
                            .clusters
                            .clusters
                            .iter()
                            .any(|c| c.element_names.contains(to) && c.elements.contains(cj))
                })
            })
        })
        .map(|dep| (dep.from_cluster, dep.to_cluster))
        .collect();

    // Sort edges by significance (cycles first, then by edge count)
    let mut sorted_deps: Vec<_> = report.clusters.inter_cluster_deps.iter().collect();
    sorted_deps.sort_by(|a, b| {
        let a_cycle = cycle_cluster_pairs.contains(&(a.from_cluster, a.to_cluster));
        let b_cycle = cycle_cluster_pairs.contains(&(b.from_cluster, b.to_cluster));
        b_cycle
            .cmp(&a_cycle)
            .then_with(|| b.edges.len().cmp(&a.edges.len()))
    });

    // Limit total edges for readability (cap at 60)
    let max_edges = 60;
    let mut edge_count_total = 0;

    for dep in &sorted_deps {
        if edge_count_total >= max_edges {
            break;
        }
        let from_id = format!("c{}", dep.from_cluster);
        let to_id = format!("c{}", dep.to_cluster);
        let edge_count = dep.edges.len();
        let is_cycle = cycle_cluster_pairs.contains(&(dep.from_cluster, dep.to_cluster));

        // For very large graphs, skip single-edge non-cycle connections
        if !is_cycle && edge_count == 1 && sorted_deps.len() > 60 {
            continue;
        }

        if is_cycle {
            if edge_count > 1 {
                out.push_str(&format!(
                    "    {} -.->|\"CYCLE ({})\"| {}\n",
                    from_id, edge_count, to_id
                ));
            } else {
                out.push_str(&format!("    {} -.->|CYCLE| {}\n", from_id, to_id));
            }
        } else if edge_count > 1 {
            out.push_str(&format!(
                "    {} -->|\"{}\"| {}\n",
                from_id, edge_count, to_id
            ));
        } else {
            out.push_str(&format!("    {} --> {}\n", from_id, to_id));
        }
        edge_count_total += 1;
    }
}

/// Render inter-cluster edges for expanded (non-collapsed) Mermaid diagrams.
fn render_mermaid_edges(report: &DsmReport, out: &mut String) {
    for dep in &report.clusters.inter_cluster_deps {
        for (from, to) in dep.edges.iter().take(5) {
            let is_cycle = report.cycles.cycle_edges.iter().any(|(ci, cj)| {
                report
                    .clusters
                    .clusters
                    .iter()
                    .any(|c| c.element_names.contains(from) && c.elements.contains(ci))
                    && report
                        .clusters
                        .clusters
                        .iter()
                        .any(|c| c.element_names.contains(to) && c.elements.contains(cj))
            });
            if is_cycle {
                out.push_str(&format!(
                    "    {} -.->|CYCLE| {}\n",
                    sanitize_id(from),
                    sanitize_id(to)
                ));
            } else {
                out.push_str(&format!(
                    "    {} --> {}\n",
                    sanitize_id(from),
                    sanitize_id(to)
                ));
            }
        }
    }
}

/// Generate a display name for a cluster based on common prefix of members.
fn cluster_display_name(names: &[String], fallback: &str) -> String {
    if names.is_empty() {
        return fallback.to_string();
    }
    if names.len() == 1 {
        return short_name(&names[0]);
    }

    // Find common prefix among all member names
    let first = &names[0];
    let mut prefix_len = first.len();
    for name in &names[1..] {
        prefix_len = first
            .chars()
            .zip(name.chars())
            .take(prefix_len)
            .take_while(|(a, b)| a == b)
            .count();
        if prefix_len == 0 {
            break;
        }
    }

    if prefix_len > 3 {
        // Trim to last underscore or dot boundary for clean prefix
        let prefix = &first[..prefix_len];
        let trimmed = prefix.trim_end_matches(|c: char| c != '_' && c != '.');
        let trimmed = trimmed.trim_end_matches(['_', '.']);
        // Only use prefix if it's specific enough (longer than the project prefix)
        // Count how many names would also match this prefix
        let matching = names.iter().filter(|n| n.starts_with(trimmed)).count();
        // Require the prefix to have enough segments to be distinctive.
        // Dot-separated (Java: org.apache.cassandra.gms) needs >= 4 segments
        // since root packages like org.apache.cassandra are 4 segments but too generic.
        // Underscore-separated (Erlang: riak_core_sysmon) needs >= 3 segments.
        let dot_count = trimmed.matches('.').count();
        let underscore_count = trimmed.matches('_').count();
        let segment_count = dot_count + underscore_count + 1;
        let min_segments = if dot_count > underscore_count { 5 } else { 3 };
        if trimmed.len() > 3 && matching == names.len() && segment_count >= min_segments {
            // For dot-separated names, show last 2 segments for distinctiveness
            let display = if dot_count > 0 {
                let parts: Vec<&str> = trimmed.split('.').collect();
                if parts.len() >= 2 {
                    parts[parts.len() - 2..].join(".")
                } else {
                    short_name(trimmed)
                }
            } else {
                short_name(trimmed)
            };
            return format!("{}*", display);
        }
    }

    // Fall back to most representative member: prefer non-test, shortest name
    names
        .iter()
        .filter(|n| !n.contains("test") && !n.contains("eqc"))
        .min_by_key(|n| n.len())
        .or_else(|| names.iter().min_by_key(|n| n.len()))
        .map(|n| short_name(n))
        .unwrap_or_else(|| fallback.to_string())
}

/// Get short display name from fully qualified name.
fn short_name(full: &str) -> String {
    full.split('.').next_back().unwrap_or(full).to_string()
}

/// Sanitize a string for use as a Mermaid node ID.
fn sanitize_id(s: &str) -> String {
    s.replace(['.', '-', ' '], "_")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::{cluster, ClusterConfig};
    use crate::cycles::find_cycles;
    use crate::extract::{Edge, EdgeKind};
    use crate::matrix::DsmMatrix;
    use crate::metrics::compute_metrics;
    use crate::partition::partition;
    use crate::suggest::generate_suggestions;

    fn make_report() -> DsmReport {
        let edges = vec![
            Edge {
                source: "a".into(),
                target: "b".into(),
                weight: 1.0,
                kind: EdgeKind::Import,
                cross_language: None,
            },
            Edge {
                source: "b".into(),
                target: "c".into(),
                weight: 1.0,
                kind: EdgeKind::Import,
                cross_language: None,
            },
            Edge {
                source: "c".into(),
                target: "a".into(),
                weight: 1.0,
                kind: EdgeKind::Import,
                cross_language: None,
            },
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

        DsmReport {
            project_name: "test-project".to_string(),
            element_count: m.size(),
            edge_count: edges.len(),
            metrics,
            cycles: ci,
            clusters: cr,
            partition: part,
            suggestions,
            directed: None,
        }
    }

    #[test]
    fn render_mermaid_output() {
        let report = make_report();
        let out = render(&report, &OutputFormat::Mermaid);
        assert!(out.contains("```mermaid"));
        assert!(out.contains("graph"));
    }

    #[test]
    fn render_markdown_output() {
        let report = make_report();
        let out = render(&report, &OutputFormat::Markdown);
        assert!(out.contains("# DSM Analysis"));
        assert!(out.contains("Propagation Cost"));
    }

    #[test]
    fn render_json_output() {
        let report = make_report();
        let out = render(&report, &OutputFormat::Json);
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(parsed.get("project_name").is_some());
    }

    #[test]
    fn render_csv_output() {
        let report = make_report();
        let out = render(&report, &OutputFormat::Csv);
        assert!(out.starts_with("element,fan_in"));
    }

    #[test]
    fn render_svg_output() {
        let report = make_report();
        let out = render(&report, &OutputFormat::Svg);
        assert!(out.contains("<svg"));
    }

    #[test]
    fn render_html_output() {
        let report = make_report();
        let out = render(&report, &OutputFormat::Html);
        assert!(out.contains("<!DOCTYPE html>"));
    }
}
